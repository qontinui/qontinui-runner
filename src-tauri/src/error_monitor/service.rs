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

use crate::error_monitor::pipeline::exporters::event_bus::EventBusExporter;
use crate::error_monitor::pipeline::processors::dedup::DedupProcessor;
use crate::error_monitor::pipeline::processors::jsonl_preprocess::JsonlPreprocessor;
use crate::error_monitor::pipeline::processors::parser::ParserProcessor;
use crate::error_monitor::pipeline::traits::{Exporter, Processor};
use crate::error_monitor::pipeline::types::{LogRecord, SourceMeta};
use crate::error_monitor::storage::LogSourceStorage;
use crate::error_monitor::types::{
    ErrorEvent, ErrorSeverity, LogFormat, LogSourceConfig, ParserType, PathType, StoredErrorEvent,
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

    /// Register a managed process stderr stream for error monitoring.
    ///
    /// Spawns a background tokio task that reads lines from the provided receiver
    /// (populated by the stderr reader thread) and ingests error/warning lines
    /// through the error monitor pipeline.
    ///
    /// The caller should send stderr lines to the returned channel; the background
    /// task will parse them and forward errors via `ingest_stream_errors`.
    pub fn spawn_stderr_ingestion_task(
        &self,
        source_name: String,
        mut line_rx: tokio::sync::mpsc::Receiver<String>,
    ) {
        let command_tx = self.command_tx.clone();
        tokio::spawn(async move {
            // Batch lines to avoid sending one-at-a-time through the command channel.
            // Flush every 50 lines or after 2 seconds of inactivity (whichever comes first).
            let mut batch: Vec<ErrorEvent> = Vec::new();
            const BATCH_SIZE: usize = 50;
            const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

            loop {
                let line = if batch.is_empty() {
                    // No partial batch — just wait for next line without timeout
                    match line_rx.recv().await {
                        Some(l) => l,
                        None => break,
                    }
                } else {
                    // Partial batch exists — apply timeout so slow trickles get flushed
                    tokio::select! {
                        result = line_rx.recv() => {
                            match result {
                                Some(l) => l,
                                None => {
                                    // Stream closed — flush remaining below
                                    break;
                                }
                            }
                        }
                        _ = tokio::time::sleep(FLUSH_TIMEOUT) => {
                            // Timeout: flush partial batch
                            let errors = std::mem::take(&mut batch);
                            let _ = command_tx
                                .send(ServiceCommand::IngestStreamErrors {
                                    source_name: source_name.clone(),
                                    errors,
                                })
                                .await;
                            continue;
                        }
                    }
                };
                // Only ingest lines that look like errors or warnings.
                // The pipeline processors will do full parsing/dedup, but we
                // pre-filter to avoid flooding the pipeline with debug/info lines.
                let dominated = line.to_lowercase();
                let is_error_like = dominated.contains("error")
                    || dominated.contains("exception")
                    || dominated.contains("traceback")
                    || dominated.contains("critical")
                    || dominated.contains("fatal")
                    || dominated.contains("panic");
                let is_warning_like = dominated.contains("warning") || dominated.contains("warn");

                if !is_error_like && !is_warning_like {
                    continue;
                }

                let severity = if is_error_like {
                    ErrorSeverity::Error
                } else {
                    ErrorSeverity::Warning
                };

                batch.push(ErrorEvent {
                    log_source_name: source_name.clone(),
                    severity,
                    error_type: None,
                    error_code: None,
                    message: line.clone(),
                    stack_trace: None,
                    location: None,
                    context_lines: None,
                    raw_entry: line,
                    log_timestamp: None,
                    trace_id: None,
                });

                if batch.len() >= BATCH_SIZE {
                    let errors = std::mem::take(&mut batch);
                    let _ = command_tx
                        .send(ServiceCommand::IngestStreamErrors {
                            source_name: source_name.clone(),
                            errors,
                        })
                        .await;
                }
            } // end loop

            // Flush remaining
            if !batch.is_empty() {
                let _ = command_tx
                    .send(ServiceCommand::IngestStreamErrors {
                        source_name: source_name.clone(),
                        errors: batch,
                    })
                    .await;
            }

            tracing::debug!(
                "Stderr ingestion task for '{}' ended (stream closed)",
                source_name
            );
        });
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
    pg_db: Arc<crate::database::pg::PgDb>,
    config: ErrorMonitorConfig,
    /// Currently monitored files with their state
    file_states: HashMap<String, FileState>,
    /// Current workflow context, attached to emitted events.
    current_task_run_id: Arc<RwLock<Option<String>>>,
    current_workflow_name: Arc<RwLock<Option<String>>>,
    /// Event sender (kept for non-pipeline events like SourceAdded)
    event_tx: Sender<ErrorMonitorEvent>,
    /// Pipeline processors
    jsonl_preprocessor: JsonlPreprocessor,
    parser_processor: ParserProcessor,
    dedup_processor: DedupProcessor,
    /// Pipeline exporter — dispatches parsed events onto the service event bus.
    event_bus_exporter: EventBusExporter,
}

impl ErrorMonitorService {
    /// Create a new error monitor service.
    pub fn new(
        pg_db: Arc<crate::database::pg::PgDb>,
        config: ErrorMonitorConfig,
    ) -> (Self, ErrorMonitorHandle, Receiver<ServiceCommand>) {
        let (command_tx, command_rx) = mpsc::channel::<ServiceCommand>(64);
        let (event_tx, event_rx) = mpsc::channel::<ErrorMonitorEvent>(config.max_queue_size);

        let current_task_run_id: Arc<RwLock<Option<String>>> = Arc::new(RwLock::new(None));
        let current_workflow_name: Arc<RwLock<Option<String>>> = Arc::new(RwLock::new(None));

        let event_bus_exporter = EventBusExporter::new(event_tx.clone());

        let handle = ErrorMonitorHandle {
            command_tx,
            event_rx: Arc::new(RwLock::new(Some(event_rx))),
        };

        let service = Self {
            pg_db,
            config,
            file_states: HashMap::new(),
            current_task_run_id,
            current_workflow_name,
            event_tx,
            jsonl_preprocessor: JsonlPreprocessor,
            parser_processor: ParserProcessor::new(),
            dedup_processor: DedupProcessor::new(10_000),
            event_bus_exporter,
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
        tracing::info!("Error monitor service starting");
        let _ = self.event_tx.send(ErrorMonitorEvent::Started).await;

        // Load sources from settings on startup
        if let Err(e) = self.load_sources_from_settings().await {
            tracing::error!("Failed to load error monitor sources: {}", e);
        }

        // Set up file watcher
        let (watch_tx, mut watch_rx) = mpsc::channel::<PathBuf>(256);
        let _watcher = self.setup_watcher(watch_tx);

        let poll_interval = self.config.poll_interval;

        loop {
            tokio::select! {
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
                        ServiceCommand::IngestStreamErrors { source_name, errors } => {
                            use crate::error_monitor::pipeline::types::{LogRecord, SourceMeta};
                            let records: Vec<LogRecord> = errors
                                .into_iter()
                                .map(|e| {
                                    LogRecord::new(
                                        e.raw_entry.clone(),
                                        source_name.clone(),
                                        SourceMeta {
                                            parser_type: crate::error_monitor::types::ParserType::Generic,
                                            format: crate::error_monitor::types::LogFormat::Plaintext,
                                            path: None,
                                        },
                                    )
                                })
                                .collect();
                            self.process_records(records).await;
                        }
                    }
                }
                Some(path) = watch_rx.recv() => {
                    self.handle_file_change(&path).await;
                }
                _ = tokio::time::sleep(poll_interval) => {
                    self.poll_all_sources().await;
                }
            }
        }

        let _ = self.event_tx.send(ErrorMonitorEvent::Stopped).await;
        tracing::info!("Error monitor service stopped");
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
    pg_db: Arc<crate::database::pg::PgDb>,
    config: ErrorMonitorConfig,
) -> ErrorMonitorHandle {
    let (service, handle, command_rx) = ErrorMonitorService::new(pg_db, config);

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
    fn create_test_service() -> (
        ErrorMonitorService,
        ErrorMonitorHandle,
        Receiver<ServiceCommand>,
    ) {
        panic!("SQLite tests disabled — use PG-based tests instead")
    }

    #[tokio::test]
    async fn test_service_creation() {
        let (_service, handle, _command_rx) = create_test_service();

        // Verify handle can receive events
        let _event_rx = handle.take_event_receiver().await;
        assert!(handle.take_event_receiver().await.is_none());
    }

    #[test]
    fn test_resolve_paths_file() {
        let (service, _handle, _command_rx) = create_test_service();

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

        let (service, _handle, _command_rx) = create_test_service();

        // Read only new content
        let content = service
            .read_file_from_position(&log_path, initial_size)
            .unwrap();
        assert!(content.contains("ERROR: Something went wrong"));
        assert!(!content.contains("Initial content"));
    }

    // ---------------------------------------------------------------
    // Stderr ingestion task: line filtering tests
    // ---------------------------------------------------------------
    //
    // spawn_stderr_ingestion_task sends filtered lines through the
    // command channel as IngestStreamErrors. We test the filtering
    // logic by creating the channel pair, sending lines, and verifying
    // which lines produce IngestStreamErrors commands.

    #[tokio::test]
    async fn test_stderr_ingestion_filters_error_lines() {
        // Construct a minimal ErrorMonitorHandle directly — no PgDb needed.
        // We only need the command channel to observe what spawn_stderr_ingestion_task sends.
        let (command_tx, mut command_rx) = mpsc::channel::<ServiceCommand>(100);
        let (_event_tx, event_rx) = mpsc::channel::<ErrorMonitorEvent>(10);
        let handle = ErrorMonitorHandle {
            command_tx,
            event_rx: Arc::new(RwLock::new(Some(event_rx))),
        };

        let (line_tx, line_rx) = tokio::sync::mpsc::channel::<String>(100);

        handle.spawn_stderr_ingestion_task("test-stderr".to_string(), line_rx);

        // Send a mix of error-like and non-error lines
        let lines = vec![
            "INFO: Starting up",                  // should be filtered out
            "DEBUG: connecting to database",      // should be filtered out
            "ERROR: connection refused",          // should be forwarded
            "  File \"main.py\", line 42",        // should be filtered out (no error keyword)
            "Traceback (most recent call last):", // should be forwarded
            "WARNING: deprecated API call",       // should be forwarded
            "All systems nominal",                // should be filtered out
            "CRITICAL: disk full",                // should be forwarded
            "fatal: not a git repository",        // should be forwarded
            "panic: runtime error",               // should be forwarded
        ];

        for line in &lines {
            line_tx.send(line.to_string()).await.unwrap();
        }
        // Drop sender to signal stream end, which flushes the batch
        drop(line_tx);

        // Collect all IngestStreamErrors commands
        let mut ingested_messages: Vec<String> = Vec::new();

        // The task will flush remaining batch when the channel closes,
        // then the task ends. We read until the command channel is empty.
        // Use a short timeout to avoid hanging if something goes wrong.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let result = tokio::time::timeout_at(deadline, command_rx.recv()).await;
            match result {
                Ok(Some(ServiceCommand::IngestStreamErrors { errors, .. })) => {
                    for e in errors {
                        ingested_messages.push(e.message.clone());
                    }
                }
                Ok(Some(ServiceCommand::Stop)) => break,
                Ok(Some(_)) => {}  // ignore other commands
                Ok(None) => break, // channel closed
                Err(_) => break,   // timeout
            }
        }

        // Verify error-like lines were forwarded
        assert!(
            ingested_messages
                .iter()
                .any(|m| m.contains("connection refused")),
            "ERROR line should be forwarded"
        );
        assert!(
            ingested_messages.iter().any(|m| m.contains("Traceback")),
            "Traceback line should be forwarded"
        );
        assert!(
            ingested_messages
                .iter()
                .any(|m| m.contains("deprecated API")),
            "WARNING line should be forwarded"
        );
        assert!(
            ingested_messages.iter().any(|m| m.contains("disk full")),
            "CRITICAL line should be forwarded"
        );
        assert!(
            ingested_messages
                .iter()
                .any(|m| m.contains("fatal: not a git")),
            "fatal line should be forwarded"
        );
        assert!(
            ingested_messages
                .iter()
                .any(|m| m.contains("panic: runtime")),
            "panic line should be forwarded"
        );

        // Verify non-error lines were NOT forwarded
        assert!(
            !ingested_messages.iter().any(|m| m.contains("Starting up")),
            "INFO line should be filtered out"
        );
        assert!(
            !ingested_messages
                .iter()
                .any(|m| m.contains("connecting to database")),
            "DEBUG line should be filtered out"
        );
        assert!(
            !ingested_messages
                .iter()
                .any(|m| m.contains("All systems nominal")),
            "Normal line should be filtered out"
        );

        // Total forwarded should be 6
        assert_eq!(
            ingested_messages.len(),
            6,
            "expected 6 error/warning-like lines to be forwarded, got {}",
            ingested_messages.len()
        );
    }
}
