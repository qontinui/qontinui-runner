//! Error monitoring service for continuous log watching.
//!
//! This service provides:
//! - Continuous file watching for configured log sources
//! - Incremental parsing (only new content)
//! - Automatic error storage with deduplication
//! - Workflow-scoped error collection

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::RwLock;

use crate::database::CheckpointDb;
use crate::error_monitor::pipeline::exporters::event_bus::EventBusExporter;
use crate::error_monitor::pipeline::exporters::sqlite::SqliteExporter;
use crate::error_monitor::pipeline::processors::dedup::DedupProcessor;
use crate::error_monitor::pipeline::processors::jsonl_preprocess::JsonlPreprocessor;
use crate::error_monitor::pipeline::processors::parser::ParserProcessor;
use crate::error_monitor::pipeline::traits::{Exporter, Processor};
use crate::error_monitor::pipeline::types::{LogRecord, SourceMeta};
use crate::error_monitor::storage::LogSourceStorage;
use crate::error_monitor::types::{
    ErrorEvent, LogFormat, LogSourceConfig, ParserType, PathType, StoredErrorEvent,
};

/// File position tracker for incremental parsing
#[derive(Debug, Clone)]
struct FileState {
    /// Last read position in bytes
    position: u64,
    /// Path to the file
    path: PathBuf,
    /// Source name (for lookup)
    source_name: String,
    /// Cached parser type for this source
    parser_type: ParserType,
    /// Log format (plaintext, json, jsonl)
    format: LogFormat,
}

/// Events emitted by the error monitor
#[derive(Debug, Clone)]
pub enum ErrorMonitorEvent {
    /// New error detected
    NewError(Box<StoredErrorEvent>),
    /// Multiple errors detected at once
    NewErrors(Vec<StoredErrorEvent>),
    /// Error parsing log content
    ParseError { source: String, error: String },
    /// Source added to monitoring
    SourceAdded { name: String, path: String },
    /// Source removed from monitoring
    SourceRemoved { name: String },
    /// Service started
    Started,
    /// Service stopped
    Stopped,
}

/// Configuration for the error monitor service
#[derive(Debug, Clone)]
pub struct ErrorMonitorConfig {
    /// How often to poll for changes (if not using native file watching)
    pub poll_interval: Duration,
    /// Maximum number of errors to queue before dropping
    pub max_queue_size: usize,
    /// Whether to start monitoring immediately on creation
    pub auto_start: bool,
    /// Debounce duration for file change events
    pub debounce_duration: Duration,
}

impl Default for ErrorMonitorConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            max_queue_size: 1000,
            auto_start: false,
            debounce_duration: Duration::from_millis(100),
        }
    }
}

/// Internal message for coordinating the service
#[derive(Debug)]
pub enum ServiceCommand {
    /// Add a source to monitor
    AddSource(Box<LogSourceConfig>),
    /// Remove a source by name
    RemoveSource(String),
    /// Set the current workflow context
    SetWorkflowContext {
        task_run_id: Option<String>,
        workflow_name: Option<String>,
    },
    /// Clear workflow context
    ClearWorkflowContext,
    /// Request manual scan of all sources
    ScanAll,
    /// Ingest errors from a managed process stream (no file needed)
    IngestStreamErrors {
        source_name: String,
        errors: Vec<ErrorEvent>,
    },
    /// Stop the service
    Stop,
}

/// Handle for controlling the error monitor service
#[derive(Clone)]
pub struct ErrorMonitorHandle {
    command_tx: Sender<ServiceCommand>,
    event_rx: Arc<RwLock<Option<Receiver<ErrorMonitorEvent>>>>,
}

impl ErrorMonitorHandle {
    /// Add a log source to monitor.
    pub async fn add_source(&self, source: LogSourceConfig) -> Result<(), String> {
        self.command_tx
            .send(ServiceCommand::AddSource(Box::new(source)))
            .await
            .map_err(|e| format!("Failed to send add source command: {}", e))
    }

    /// Remove a log source by name.
    pub async fn remove_source(&self, name: &str) -> Result<(), String> {
        self.command_tx
            .send(ServiceCommand::RemoveSource(name.to_string()))
            .await
            .map_err(|e| format!("Failed to send remove source command: {}", e))
    }

    /// Set the current workflow context for error association.
    pub async fn set_workflow_context(
        &self,
        task_run_id: Option<String>,
        workflow_name: Option<String>,
    ) -> Result<(), String> {
        self.command_tx
            .send(ServiceCommand::SetWorkflowContext {
                task_run_id,
                workflow_name,
            })
            .await
            .map_err(|e| format!("Failed to send workflow context command: {}", e))
    }

    /// Clear the workflow context (errors will be logged as continuous monitoring).
    pub async fn clear_workflow_context(&self) -> Result<(), String> {
        self.command_tx
            .send(ServiceCommand::ClearWorkflowContext)
            .await
            .map_err(|e| format!("Failed to send clear context command: {}", e))
    }

    /// Request a manual scan of all monitored sources.
    pub async fn scan_all(&self) -> Result<(), String> {
        self.command_tx
            .send(ServiceCommand::ScanAll)
            .await
            .map_err(|e| format!("Failed to send scan command: {}", e))
    }

    /// Ingest errors from a managed process stream.
    /// These errors are stored and emitted just like file-based errors.
    pub async fn ingest_stream_errors(
        &self,
        source_name: String,
        errors: Vec<ErrorEvent>,
    ) -> Result<(), String> {
        self.command_tx
            .send(ServiceCommand::IngestStreamErrors {
                source_name,
                errors,
            })
            .await
            .map_err(|e| format!("Failed to send ingest stream errors command: {}", e))
    }

    /// Stop the error monitor service.
    pub async fn stop(&self) -> Result<(), String> {
        self.command_tx
            .send(ServiceCommand::Stop)
            .await
            .map_err(|e| format!("Failed to send stop command: {}", e))
    }

    /// Take the event receiver (can only be called once).
    pub async fn take_event_receiver(&self) -> Option<Receiver<ErrorMonitorEvent>> {
        self.event_rx.write().await.take()
    }
}

/// Error monitoring service
pub struct ErrorMonitorService {
    db: Arc<CheckpointDb>,
    pg_db: Arc<crate::database::pg::PgDb>,
    config: ErrorMonitorConfig,
    /// Currently monitored files with their state
    file_states: HashMap<String, FileState>,
    /// Current workflow context (shared with SqliteExporter)
    current_task_run_id: Arc<RwLock<Option<String>>>,
    current_workflow_name: Arc<RwLock<Option<String>>>,
    /// Event sender (kept for non-pipeline events like SourceAdded)
    event_tx: Sender<ErrorMonitorEvent>,
    /// Pipeline processors
    jsonl_preprocessor: JsonlPreprocessor,
    parser_processor: ParserProcessor,
    dedup_processor: DedupProcessor,
    /// Pipeline exporters
    sqlite_exporter: SqliteExporter,
    event_bus_exporter: EventBusExporter,
}

impl ErrorMonitorService {
    /// Create a new error monitor service.
    pub fn new(
        db: Arc<CheckpointDb>,
        pg_db: Arc<crate::database::pg::PgDb>,
        config: ErrorMonitorConfig,
    ) -> (Self, ErrorMonitorHandle, Receiver<ServiceCommand>) {
        let (command_tx, command_rx) = mpsc::channel(100);
        let (event_tx, event_rx) = mpsc::channel(config.max_queue_size);

        // Shared workflow context for SqliteExporter
        let current_task_run_id = Arc::new(RwLock::new(None));
        let current_workflow_name = Arc::new(RwLock::new(None));

        // Pipeline components
        let jsonl_preprocessor = JsonlPreprocessor;
        let parser_processor = ParserProcessor::new();
        let dedup_processor = DedupProcessor::new(10_000);
        let sqlite_exporter = SqliteExporter::new(
            db.clone(),
            current_task_run_id.clone(),
            current_workflow_name.clone(),
        );
        let event_bus_exporter = EventBusExporter::new(event_tx.clone());

        let service = Self {
            db,
            pg_db,
            config,
            file_states: HashMap::new(),
            current_task_run_id,
            current_workflow_name,
            event_tx,
            jsonl_preprocessor,
            parser_processor,
            dedup_processor,
            sqlite_exporter,
            event_bus_exporter,
        };

        let handle = ErrorMonitorHandle {
            command_tx,
            event_rx: Arc::new(RwLock::new(Some(event_rx))),
        };

        (service, handle, command_rx)
    }

    /// Start the error monitor service.
    ///
    /// This will:
    /// 1. Load all configured sources from the database
    /// 2. Start watching files for changes
    /// 3. Process commands from the handle
    pub async fn run(mut self, mut command_rx: Receiver<ServiceCommand>) {
        // Load sources from global settings (single source of truth)
        if let Err(e) = self.load_sources_from_settings().await {
            tracing::error!("Failed to load sources from settings: {}", e);
        }

        // Clean up stale spec verification errors that were stored before the filter was added.
        // Spec events (action_failed with "SPEC: " prefix) are verification test results,
        // not application errors, and should not trigger Quick Fix workflows.
        {
            let db = self.db.clone();
            match tokio::task::spawn_blocking(move || -> Result<usize, String> {
                let conn = db.get_conn_string()?;
                conn.execute(
                    "UPDATE error_events \
                     SET status = 'resolved', resolved_at = datetime('now') \
                     WHERE log_source_name = 'Runner Actions' \
                       AND message LIKE 'SPEC: %' \
                       AND status IN ('new', 'acknowledged')",
                    [],
                )
                .map_err(|e| format!("SQL error: {}", e))
            })
            .await
            {
                Ok(Ok(count)) => {
                    if count > 0 {
                        tracing::info!(
                            "Cleaned up {} stale spec verification error(s) on startup",
                            count
                        );
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!("Failed to clean up spec verification errors: {}", e);
                }
                Err(e) => {
                    tracing::warn!("Task join error cleaning up spec errors: {}", e);
                }
            }
        }

        // Emit started event
        let _ = self.event_tx.send(ErrorMonitorEvent::Started).await;

        // Set up file watcher
        let (watcher_tx, mut watcher_rx) = mpsc::channel::<PathBuf>(100);
        let _watcher = self.setup_watcher(watcher_tx.clone());

        // Main event loop
        loop {
            tokio::select! {
                // Handle commands
                Some(cmd) = command_rx.recv() => {
                    match cmd {
                        ServiceCommand::Stop => {
                            tracing::info!("Error monitor service stopping");
                            break;
                        }
                        ServiceCommand::AddSource(source) => {
                            self.add_source(*source).await;
                        }
                        ServiceCommand::RemoveSource(name) => {
                            self.remove_source(&name).await;
                        }
                        ServiceCommand::SetWorkflowContext { task_run_id, workflow_name } => {
                            *self.current_task_run_id.write().await = task_run_id;
                            *self.current_workflow_name.write().await = workflow_name;
                        }
                        ServiceCommand::ClearWorkflowContext => {
                            *self.current_task_run_id.write().await = None;
                            *self.current_workflow_name.write().await = None;
                        }
                        ServiceCommand::ScanAll => {
                            self.scan_all_sources().await;
                        }
                        ServiceCommand::IngestStreamErrors { source_name: _, errors } => {
                            if !errors.is_empty() {
                                let records: Vec<LogRecord> = errors.into_iter().map(LogRecord::from).collect();
                                self.process_records(records).await;
                            }
                        }
                    }
                }

                // Handle file change events
                Some(path) = watcher_rx.recv() => {
                    self.handle_file_change(&path).await;
                }

                // Periodic polling (backup for file watchers)
                _ = tokio::time::sleep(self.config.poll_interval) => {
                    self.poll_all_sources().await;
                }
            }
        }

        // Emit stopped event
        let _ = self.event_tx.send(ErrorMonitorEvent::Stopped).await;
    }

    /// Load configured sources from global settings (single source of truth).
    ///
    /// Reads from the global log source settings, converts to error monitor types,
    /// and syncs to the `log_sources` DB table for FK integrity with `error_events`.
    async fn load_sources_from_settings(&mut self) -> Result<(), String> {
        let global_settings = crate::settings::get_global_log_source_settings();

        // Convert GlobalLogSource -> error_monitor::types::LogSourceConfig
        let sources: Vec<LogSourceConfig> = global_settings
            .sources
            .iter()
            .filter(|s| s.enabled)
            .map(|gs| {
                let path_type = match gs.source_type.as_str() {
                    "directory" => PathType::Directory,
                    "glob" => PathType::Glob,
                    _ => PathType::File,
                };
                let format = crate::error_monitor::types::LogFormat::from_str(&gs.format)
                    .unwrap_or_default();
                let parser = crate::error_monitor::types::ParserType::from_str(&gs.parser)
                    .unwrap_or_default();

                LogSourceConfig {
                    id: None,
                    name: gs.name.clone(),
                    description: Some(gs.description.clone()),
                    path: gs.path.clone(),
                    path_type,
                    format,
                    parser,
                    timestamp_pattern: gs.timestamp_pattern.clone(),
                    timezone: gs.timezone.clone(),
                    error_patterns: if gs.error_patterns.is_empty() {
                        None
                    } else {
                        Some(gs.error_patterns.clone())
                    },
                    warning_patterns: if gs.warning_patterns.is_empty() {
                        None
                    } else {
                        Some(gs.warning_patterns.clone())
                    },
                    ignore_patterns: if gs.ignore_patterns.is_empty() {
                        None
                    } else {
                        Some(gs.ignore_patterns.clone())
                    },
                    enabled: gs.enabled,
                    poll_interval_ms: gs.poll_interval_ms,
                    created_at: None,
                    updated_at: None,
                }
            })
            .collect();

        // Sync to database for FK integrity (error_events references log_sources.id)
        let sources_for_db = sources.clone();
        if let Err(e) = self.pg_db.sync_log_sources(&sources_for_db).await {
            tracing::error!("Failed to sync log sources to PG: {}", e);
        }

        for source in sources {
            self.add_source_internal(source);
        }

        Ok(())
    }

    /// Add a source to monitoring.
    async fn add_source(&mut self, source: LogSourceConfig) {
        let name = source.name.clone();
        let path = source.path.clone();

        // Save to database
        let source_clone = source.clone();
        if let Err(e) = self.pg_db.create_log_source(&source_clone).await {
            tracing::error!("Failed to save source to PG: {}", e);
        }

        self.add_source_internal(source);

        let _ = self
            .event_tx
            .send(ErrorMonitorEvent::SourceAdded { name, path })
            .await;
    }

    /// Internal source addition without database save.
    fn add_source_internal(&mut self, source: LogSourceConfig) {
        let paths = self.resolve_paths(&source);
        let parser_type = source.parser.clone();
        let source_name = source.name.clone();
        let format = source.format.clone();

        for path in paths {
            let state = FileState {
                position: self.get_file_size(&path).unwrap_or(0),
                path: path.clone(),
                source_name: source_name.clone(),
                parser_type: parser_type.clone(),
                format: format.clone(),
            };

            // Use path as key for file state
            self.file_states
                .insert(path.to_string_lossy().to_string(), state);
        }
    }

    /// Remove a source from monitoring.
    async fn remove_source(&mut self, name: &str) {
        // Remove from file states
        self.file_states
            .retain(|_, state| state.source_name != name);

        // Remove from database
        if let Err(e) = self.pg_db.delete_log_source_by_name(name).await {
            tracing::error!("Failed to delete source from PG: {}", e);
        }

        let _ = self
            .event_tx
            .send(ErrorMonitorEvent::SourceRemoved {
                name: name.to_string(),
            })
            .await;
    }

    /// Resolve paths for a source configuration.
    fn resolve_paths(&self, source: &LogSourceConfig) -> Vec<PathBuf> {
        match source.path_type {
            PathType::File => vec![PathBuf::from(&source.path)],
            PathType::Glob => glob::glob(&source.path)
                .map(|paths| paths.filter_map(|p| p.ok()).collect())
                .unwrap_or_default(),
            PathType::Directory => {
                // Watch all files in directory
                std::fs::read_dir(&source.path)
                    .map(|entries| {
                        entries
                            .filter_map(|e| e.ok())
                            .filter(|e| e.path().is_file())
                            .map(|e| e.path())
                            .collect()
                    })
                    .unwrap_or_default()
            }
        }
    }

    /// Get the current size of a file.
    fn get_file_size(&self, path: &PathBuf) -> Option<u64> {
        std::fs::metadata(path).ok().map(|m| m.len())
    }

    /// Set up the file watcher.
    fn setup_watcher(&self, tx: Sender<PathBuf>) -> Option<RecommendedWatcher> {
        let tx = tx.clone();

        let watcher = notify::recommended_watcher(move |result: Result<Event, notify::Error>| {
            if let Ok(event) = result {
                for path in event.paths {
                    // Use try_send since we're in notify's thread which is outside the Tokio runtime.
                    // blocking_send requires a Tokio context. try_send is non-blocking and will
                    // drop the message if the channel is full - acceptable since we poll periodically.
                    let _ = tx.try_send(path);
                }
            }
        });

        match watcher {
            Ok(mut w) => {
                // Watch all current paths
                for state in self.file_states.values() {
                    if let Some(parent) = state.path.parent() {
                        let _ = w.watch(parent, RecursiveMode::NonRecursive);
                    }
                }
                Some(w)
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to create file watcher: {}. Falling back to polling.",
                    e
                );
                None
            }
        }
    }

    /// Handle a file change event.
    async fn handle_file_change(&mut self, path: &Path) {
        let path_str = path.to_string_lossy().to_string();

        // Clone the data we need to avoid borrow issues
        let state_data = self.file_states.get(&path_str).map(|s| {
            (
                s.position,
                s.path.clone(),
                s.source_name.clone(),
                s.parser_type.clone(),
                s.format.clone(),
            )
        });

        if let Some((position, file_path, source_name, parser_type, format)) = state_data {
            match self
                .process_file_changes_inner(
                    &file_path,
                    position,
                    &source_name,
                    &parser_type,
                    &format,
                )
                .await
            {
                Ok(new_position) => {
                    if let Some(state) = self.file_states.get_mut(&path_str) {
                        state.position = new_position;
                    }
                }
                Err(e) => {
                    let _ = self
                        .event_tx
                        .send(ErrorMonitorEvent::ParseError {
                            source: source_name,
                            error: e,
                        })
                        .await;
                }
            }
        }
    }

    /// Poll all sources for changes.
    async fn poll_all_sources(&mut self) {
        // Collect data to avoid borrow issues
        let states_data: Vec<(String, u64, PathBuf, String, ParserType, LogFormat)> = self
            .file_states
            .iter()
            .map(|(k, s)| {
                (
                    k.clone(),
                    s.position,
                    s.path.clone(),
                    s.source_name.clone(),
                    s.parser_type.clone(),
                    s.format.clone(),
                )
            })
            .collect();

        for (path_str, position, file_path, source_name, parser_type, format) in states_data {
            match self
                .process_file_changes_inner(
                    &file_path,
                    position,
                    &source_name,
                    &parser_type,
                    &format,
                )
                .await
            {
                Ok(new_position) => {
                    if let Some(state) = self.file_states.get_mut(&path_str) {
                        state.position = new_position;
                    }
                }
                Err(e) => {
                    let _ = self
                        .event_tx
                        .send(ErrorMonitorEvent::ParseError {
                            source: source_name,
                            error: e,
                        })
                        .await;
                }
            }
        }
    }

    /// Scan all sources from the beginning.
    async fn scan_all_sources(&mut self) {
        // Reset all positions
        for state in self.file_states.values_mut() {
            state.position = 0;
        }

        self.poll_all_sources().await;
    }

    /// Process log records through the pipeline (preprocessor → parser → dedup → exporters).
    async fn process_records(&self, records: Vec<LogRecord>) {
        if records.is_empty() {
            return;
        }

        // Run through processor chain
        let records = self.jsonl_preprocessor.process(records).await;
        let records = self.parser_processor.process(records).await;
        let records = self.dedup_processor.process(records).await;

        if records.is_empty() {
            return;
        }

        // Fan out to exporters
        if let Err(e) = self.sqlite_exporter.export(&records).await {
            tracing::warn!("SQLite export failed: {}", e);
        }
        if let Err(e) = self.event_bus_exporter.export(&records).await {
            tracing::warn!("Event bus export failed: {}", e);
        }
    }

    /// Process changes in a file (inner implementation to avoid borrow issues).
    async fn process_file_changes_inner(
        &self,
        path: &PathBuf,
        mut position: u64,
        source_name: &str,
        parser_type: &ParserType,
        format: &LogFormat,
    ) -> Result<u64, String> {
        let current_size = self.get_file_size(path).unwrap_or(0);

        // Check if file was truncated (e.g., log rotation)
        if current_size < position {
            position = 0;
        }

        // Nothing new to read
        if current_size == position {
            return Ok(position);
        }

        // Read new content
        let content = self.read_file_from_position(path, position)?;

        if content.is_empty() {
            return Ok(current_size);
        }

        // Convert raw lines to LogRecords
        let source_meta = SourceMeta {
            parser_type: parser_type.clone(),
            format: format.clone(),
            path: Some(path.to_string_lossy().to_string()),
        };

        let records: Vec<LogRecord> = content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                LogRecord::new(
                    line.to_string(),
                    source_name.to_string(),
                    source_meta.clone(),
                )
            })
            .collect();

        self.process_records(records).await;

        Ok(current_size)
    }

    /// Read file content from a specific position.
    fn read_file_from_position(&self, path: &PathBuf, position: u64) -> Result<String, String> {
        use std::io::{Read, Seek, SeekFrom};

        let mut file = std::fs::File::open(path)
            .map_err(|e| format!("Failed to open file {}: {}", path.display(), e))?;

        file.seek(SeekFrom::Start(position))
            .map_err(|e| format!("Failed to seek in file: {}", e))?;

        let mut content = String::new();
        file.read_to_string(&mut content)
            .map_err(|e| format!("Failed to read file: {}", e))?;

        Ok(content)
    }
}

/// Start the error monitor service in a background task.
/// Returns the handle immediately for use by the caller, and spawns the service
/// in the background within the provided async block.
///
/// MUST be called from within a Tokio runtime context (e.g., inside tauri::async_runtime::spawn).
pub async fn start_error_monitor_async(
    db: Arc<CheckpointDb>,
    pg_db: Arc<crate::database::pg::PgDb>,
    config: ErrorMonitorConfig,
) -> ErrorMonitorHandle {
    let (service, handle, command_rx) = ErrorMonitorService::new(db, pg_db, config);

    tokio::spawn(async move {
        service.run(command_rx).await;
    });

    handle
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    /// Helper to create a test service via the public constructor.
    /// Requires DATABASE_URL env var for PgDb connection.
    fn create_test_service(
        db: Arc<CheckpointDb>,
    ) -> (
        ErrorMonitorService,
        ErrorMonitorHandle,
        Receiver<ServiceCommand>,
    ) {
        let pg_db = crate::database::pg::PgDb::new_blocking_for_test();
        ErrorMonitorService::new(db, pg_db, ErrorMonitorConfig::default())
    }

    #[tokio::test]
    async fn test_service_creation() {
        let db = Arc::new(CheckpointDb::new_in_memory().unwrap());
        let (_service, handle, _command_rx) = create_test_service(db);

        // Verify handle can receive events
        let _event_rx = handle.take_event_receiver().await;
        assert!(handle.take_event_receiver().await.is_none());
    }

    #[test]
    fn test_resolve_paths_file() {
        let db = Arc::new(CheckpointDb::new_in_memory().unwrap());
        let (service, _handle, _command_rx) = create_test_service(db);

        let source = LogSourceConfig {
            id: None,
            name: "test".to_string(),
            description: None,
            path: "/path/to/file.log".to_string(),
            path_type: PathType::File,
            format: crate::error_monitor::types::LogFormat::Plaintext,
            parser: ParserType::Generic,
            timestamp_pattern: None,
            timezone: "local".to_string(),
            error_patterns: None,
            warning_patterns: None,
            ignore_patterns: None,
            enabled: true,
            poll_interval_ms: 5000,
            created_at: None,
            updated_at: None,
        };

        let paths = service.resolve_paths(&source);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], PathBuf::from("/path/to/file.log"));
    }

    #[tokio::test]
    async fn test_read_new_content() {
        let dir = tempdir().unwrap();
        let log_path = dir.path().join("test.log");

        // Create initial log file
        let mut file = std::fs::File::create(&log_path).unwrap();
        writeln!(file, "Initial content").unwrap();
        file.flush().unwrap();

        let initial_size = std::fs::metadata(&log_path).unwrap().len();

        // Add more content
        writeln!(file, "ERROR: Something went wrong").unwrap();
        file.flush().unwrap();

        let db = Arc::new(CheckpointDb::new_in_memory().unwrap());
        let (service, _handle, _command_rx) = create_test_service(db);

        // Read only new content
        let content = service
            .read_file_from_position(&log_path, initial_size)
            .unwrap();
        assert!(content.contains("ERROR: Something went wrong"));
        assert!(!content.contains("Initial content"));
    }
}
