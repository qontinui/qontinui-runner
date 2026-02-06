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
use crate::error_monitor::parsers::{create_parser, LogParser};
use crate::error_monitor::storage::{ErrorEventStorage, LogSourceStorage};
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
    config: ErrorMonitorConfig,
    /// Currently monitored files with their state
    file_states: HashMap<String, FileState>,
    /// Current workflow context
    current_task_run_id: Option<String>,
    current_workflow_name: Option<String>,
    /// Event sender
    event_tx: Sender<ErrorMonitorEvent>,
    /// Cached parsers
    parsers: HashMap<ParserType, Box<dyn LogParser>>,
}

impl ErrorMonitorService {
    /// Create a new error monitor service.
    pub fn new(
        db: Arc<CheckpointDb>,
        config: ErrorMonitorConfig,
    ) -> (Self, ErrorMonitorHandle, Receiver<ServiceCommand>) {
        let (command_tx, command_rx) = mpsc::channel(100);
        let (event_tx, event_rx) = mpsc::channel(config.max_queue_size);

        let service = Self {
            db,
            config,
            file_states: HashMap::new(),
            current_task_run_id: None,
            current_workflow_name: None,
            event_tx,
            parsers: HashMap::new(),
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
                            self.current_task_run_id = task_run_id;
                            self.current_workflow_name = workflow_name;
                        }
                        ServiceCommand::ClearWorkflowContext => {
                            self.current_task_run_id = None;
                            self.current_workflow_name = None;
                        }
                        ServiceCommand::ScanAll => {
                            self.scan_all_sources().await;
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
        let db = self.db.clone();
        let sources_for_db = sources.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || {
            let conn = db.connection()?;
            // Clear existing sources and re-insert from settings
            conn.execute("DELETE FROM log_sources", [])
                .map_err(|e| format!("Failed to clear log_sources: {}", e))?;
            for source in &sources_for_db {
                LogSourceStorage::insert(&conn, source)?;
            }
            Ok::<(), String>(())
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
        {
            tracing::error!("Failed to sync log sources to database: {}", e);
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
        let db = self.db.clone();
        let source_clone = source.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || {
            let conn = db.connection()?;
            LogSourceStorage::insert(&conn, &source_clone)
        })
        .await
        .unwrap_or_else(|e| Err(format!("Task join error: {}", e)))
        {
            tracing::error!("Failed to save source to database: {}", e);
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

        // Ensure parser is cached
        if !self.parsers.contains_key(&parser_type) {
            self.parsers
                .insert(parser_type.clone(), create_parser(&parser_type));
        }
    }

    /// Remove a source from monitoring.
    async fn remove_source(&mut self, name: &str) {
        // Remove from file states
        self.file_states
            .retain(|_, state| state.source_name != name);

        // Remove from database
        let db = self.db.clone();
        let name_owned = name.to_string();
        if let Err(e) = tokio::task::spawn_blocking(move || {
            let conn = db.connection()?;
            LogSourceStorage::delete_by_name(&conn, &name_owned)
        })
        .await
        .unwrap_or_else(|e| Err(format!("Task join error: {}", e)))
        {
            tracing::error!("Failed to delete source from database: {}", e);
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
                    let tx = tx.clone();
                    // Use blocking send since we're in a sync callback
                    let _ = tx.blocking_send(path);
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

        // For JSONL format, preprocess to filter out success events and extract errors
        let content_to_parse = match format {
            LogFormat::Jsonl => self.preprocess_jsonl_content(&content),
            _ => content,
        };

        if content_to_parse.is_empty() {
            return Ok(current_size);
        }

        // Parse content
        let parser = self
            .parsers
            .get(parser_type)
            .ok_or_else(|| format!("No parser for type {:?}", parser_type))?;

        let errors = parser.parse_content(&content_to_parse, source_name);

        if errors.is_empty() {
            return Ok(current_size);
        }

        // Store errors and emit events
        let stored_errors = self.store_errors(errors).await?;

        if stored_errors.len() == 1 {
            let _ = self
                .event_tx
                .send(ErrorMonitorEvent::NewError(Box::new(
                    stored_errors.into_iter().next().unwrap(),
                )))
                .await;
        } else if !stored_errors.is_empty() {
            let _ = self
                .event_tx
                .send(ErrorMonitorEvent::NewErrors(stored_errors))
                .await;
        }

        Ok(current_size)
    }

    /// Preprocess JSONL content to filter out success events and extract meaningful errors.
    ///
    /// For JSONL files, we parse each line as JSON and:
    /// 1. Skip events where status indicates success (at any nesting level)
    /// 2. Only keep events that indicate actual errors
    /// 3. Extract meaningful error messages from JSON structure
    fn preprocess_jsonl_content(&self, content: &str) -> String {
        let mut error_lines = Vec::new();

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Try to parse as JSON
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                // Check if this event indicates an error
                if let Some(error_message) = self.extract_jsonl_error(&json) {
                    // Return a formatted error line that the generic parser can handle
                    error_lines.push(format!("ERROR: {}", error_message));
                }
                // If no error found, skip this line (don't pass success events to parser)
            } else {
                // If JSON parsing fails, pass the line through as-is (might be malformed)
                error_lines.push(line.to_string());
            }
        }

        error_lines.join("\n")
    }

    /// Extract error message from a JSONL event if it indicates an error.
    /// Returns None for success events, Some(message) for error events.
    fn extract_jsonl_error(&self, json: &serde_json::Value) -> Option<String> {
        // Check for status fields that indicate success - skip these
        let success_statuses = [
            "success",
            "completed",
            "pending",
            "running",
            "started",
            "ok",
        ];

        // Check status at various common nesting levels
        let status = self.find_status_field(json);

        if let Some(status) = status {
            let status_lower = status.to_lowercase();
            if success_statuses.iter().any(|s| status_lower == *s) {
                return None; // This is a success event, skip it
            }
            // Check for error statuses
            if status_lower == "failed" || status_lower == "error" || status_lower == "failure" {
                // This is an error event - extract the message
                return Some(self.build_jsonl_error_message(json, &status));
            }
        }

        // Check for explicit error fields
        if let Some(error) = json.get("error") {
            if let Some(msg) = error.as_str() {
                return Some(msg.to_string());
            }
            if let Some(msg) = error.get("message").and_then(|v| v.as_str()) {
                return Some(msg.to_string());
            }
        }

        // Check for exception fields
        if let Some(exception) = json.get("exception").and_then(|v| v.as_str()) {
            return Some(exception.to_string());
        }

        // Check for error_type or error_message fields
        if let Some(error_msg) = json.get("error_message").and_then(|v| v.as_str()) {
            return Some(error_msg.to_string());
        }

        // No error indicators found
        None
    }

    /// Find the status field in a JSON object, checking common nesting patterns.
    fn find_status_field(&self, json: &serde_json::Value) -> Option<String> {
        // Check direct status field
        if let Some(status) = json.get("status").and_then(|v| v.as_str()) {
            return Some(status.to_string());
        }
        // Check nested under "node"
        if let Some(node) = json.get("node") {
            if let Some(status) = node.get("status").and_then(|v| v.as_str()) {
                return Some(status.to_string());
            }
        }
        // Check nested under "result"
        if let Some(result) = json.get("result") {
            if let Some(status) = result.get("status").and_then(|v| v.as_str()) {
                return Some(status.to_string());
            }
        }
        // Check nested under "data"
        if let Some(data) = json.get("data") {
            if let Some(status) = data.get("status").and_then(|v| v.as_str()) {
                return Some(status.to_string());
            }
        }
        None
    }

    /// Build a meaningful error message from a JSONL error event.
    fn build_jsonl_error_message(&self, json: &serde_json::Value, status: &str) -> String {
        let mut parts = Vec::new();

        // Add event type if available
        if let Some(event_type) = json.get("event_type").and_then(|v| v.as_str()) {
            parts.push(event_type.to_string());
        }

        // Add name if available (check various locations)
        let name = json
            .get("name")
            .and_then(|v| v.as_str())
            .or_else(|| {
                json.get("node")
                    .and_then(|n| n.get("name"))
                    .and_then(|v| v.as_str())
            })
            .or_else(|| {
                json.get("action")
                    .and_then(|a| a.get("name"))
                    .and_then(|v| v.as_str())
            });
        if let Some(name) = name {
            parts.push(name.to_string());
        }

        // Add status
        parts.push(format!("status={}", status));

        // Add error message if available (check various locations)
        let error_msg = json
            .get("error")
            .and_then(|v| v.as_str())
            .or_else(|| json.get("message").and_then(|v| v.as_str()))
            .or_else(|| json.get("error_message").and_then(|v| v.as_str()))
            .or_else(|| {
                json.get("node")
                    .and_then(|n| n.get("error"))
                    .and_then(|v| v.as_str())
            })
            .or_else(|| {
                json.get("node")
                    .and_then(|n| n.get("metadata"))
                    .and_then(|m| m.get("error"))
                    .and_then(|v| v.as_str())
            })
            .or_else(|| {
                json.get("result")
                    .and_then(|r| r.get("error"))
                    .and_then(|v| v.as_str())
            });
        if let Some(err) = error_msg {
            parts.push(err.to_string());
        }

        if parts.is_empty() {
            status.to_string()
        } else {
            parts.join(" - ")
        }
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

    /// Store errors in the database.
    async fn store_errors(&self, errors: Vec<ErrorEvent>) -> Result<Vec<StoredErrorEvent>, String> {
        let db = self.db.clone();
        let task_run_id = self.current_task_run_id.clone();
        let workflow_name = self.current_workflow_name.clone();

        tokio::task::spawn_blocking(move || {
            let conn = db.connection()?;
            let mut stored = Vec::new();

            for error in errors {
                match ErrorEventStorage::insert(
                    &conn,
                    &error,
                    task_run_id.as_deref(),
                    workflow_name.as_deref(),
                ) {
                    Ok(stored_error) => stored.push(stored_error),
                    Err(e) => tracing::warn!("Failed to store error: {}", e),
                }
            }

            Ok(stored)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }
}

/// Start the error monitor service in a background task.
/// Returns the handle immediately for use by the caller, and spawns the service
/// in the background within the provided async block.
///
/// MUST be called from within a Tokio runtime context (e.g., inside tauri::async_runtime::spawn).
pub async fn start_error_monitor_async(
    db: Arc<CheckpointDb>,
    config: ErrorMonitorConfig,
) -> ErrorMonitorHandle {
    let (service, handle, command_rx) = ErrorMonitorService::new(db, config);

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

    #[tokio::test]
    async fn test_service_creation() {
        let dir = tempdir().unwrap();
        let _db_path = dir.path().join("test.db");
        let db = Arc::new(CheckpointDb::new_in_memory().unwrap());

        let config = ErrorMonitorConfig::default();
        let (_service, handle, _command_rx) = ErrorMonitorService::new(db, config);

        // Verify handle can receive events
        let _event_rx = handle.take_event_receiver().await;
        assert!(handle.take_event_receiver().await.is_none());
    }

    #[test]
    fn test_resolve_paths_file() {
        let dir = tempdir().unwrap();
        let _db_path = dir.path().join("test.db");
        let db = Arc::new(CheckpointDb::new_in_memory().unwrap());

        let config = ErrorMonitorConfig::default();
        let (event_tx, _) = mpsc::channel(100);

        let service = ErrorMonitorService {
            db,
            config,
            file_states: HashMap::new(),
            current_task_run_id: None,
            current_workflow_name: None,
            event_tx,
            parsers: HashMap::new(),
        };

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
        let _db_path = dir.path().join("test.db");

        // Create initial log file
        let mut file = std::fs::File::create(&log_path).unwrap();
        writeln!(file, "Initial content").unwrap();
        file.flush().unwrap();

        let initial_size = std::fs::metadata(&log_path).unwrap().len();

        // Add more content
        writeln!(file, "ERROR: Something went wrong").unwrap();
        file.flush().unwrap();

        let db = Arc::new(CheckpointDb::new_in_memory().unwrap());
        let config = ErrorMonitorConfig::default();
        let (event_tx, _) = mpsc::channel(100);

        let service = ErrorMonitorService {
            db,
            config,
            file_states: HashMap::new(),
            current_task_run_id: None,
            current_workflow_name: None,
            event_tx,
            parsers: HashMap::new(),
        };

        // Read only new content
        let content = service
            .read_file_from_position(&log_path, initial_size)
            .unwrap();
        assert!(content.contains("ERROR: Something went wrong"));
        assert!(!content.contains("Initial content"));
    }

    #[test]
    fn test_preprocess_jsonl_filters_success_events() {
        let db = Arc::new(CheckpointDb::new_in_memory().unwrap());
        let config = ErrorMonitorConfig::default();
        let (event_tx, _) = mpsc::channel(100);

        let service = ErrorMonitorService {
            db,
            config,
            file_states: HashMap::new(),
            current_task_run_id: None,
            current_workflow_name: None,
            event_tx,
            parsers: HashMap::new(),
        };

        // JSONL with success events - should be filtered out
        let jsonl_content = r#"{"id":"act-1","node":{"status":"success","name":"Test Action"},"event_type":"action_completed"}
{"id":"act-2","node":{"status":"pending","name":"Pending Action"},"event_type":"action_started"}
{"id":"act-3","node":{"status":"running","name":"Running Action"},"event_type":"action_progress"}"#;

        let result = service.preprocess_jsonl_content(jsonl_content);
        assert!(
            result.is_empty(),
            "Success events should be filtered out: '{}'",
            result
        );
    }

    #[test]
    fn test_preprocess_jsonl_keeps_error_events() {
        let db = Arc::new(CheckpointDb::new_in_memory().unwrap());
        let config = ErrorMonitorConfig::default();
        let (event_tx, _) = mpsc::channel(100);

        let service = ErrorMonitorService {
            db,
            config,
            file_states: HashMap::new(),
            current_task_run_id: None,
            current_workflow_name: None,
            event_tx,
            parsers: HashMap::new(),
        };

        // JSONL with failed event - should be kept
        let jsonl_content = r#"{"id":"act-1","node":{"status":"failed","name":"Failed Action"},"event_type":"action_completed"}"#;

        let result = service.preprocess_jsonl_content(jsonl_content);
        assert!(!result.is_empty(), "Failed events should be kept");
        assert!(result.contains("ERROR:"), "Should format as ERROR: message");
    }

    #[test]
    fn test_preprocess_jsonl_extracts_error_field() {
        let db = Arc::new(CheckpointDb::new_in_memory().unwrap());
        let config = ErrorMonitorConfig::default();
        let (event_tx, _) = mpsc::channel(100);

        let service = ErrorMonitorService {
            db,
            config,
            file_states: HashMap::new(),
            current_task_run_id: None,
            current_workflow_name: None,
            event_tx,
            parsers: HashMap::new(),
        };

        // JSONL with explicit error field
        let jsonl_content = r#"{"error":"Connection timeout occurred"}"#;

        let result = service.preprocess_jsonl_content(jsonl_content);
        assert!(!result.is_empty(), "Error events should be kept");
        assert!(
            result.contains("Connection timeout"),
            "Should extract error message"
        );
    }

    #[test]
    fn test_preprocess_jsonl_mixed_content() {
        let db = Arc::new(CheckpointDb::new_in_memory().unwrap());
        let config = ErrorMonitorConfig::default();
        let (event_tx, _) = mpsc::channel(100);

        let service = ErrorMonitorService {
            db,
            config,
            file_states: HashMap::new(),
            current_task_run_id: None,
            current_workflow_name: None,
            event_tx,
            parsers: HashMap::new(),
        };

        // Mix of success and error events
        let jsonl_content = r#"{"id":"1","node":{"status":"success","name":"Good"}}
{"id":"2","node":{"status":"failed","name":"Bad"},"error":"Something broke"}
{"id":"3","node":{"status":"completed","name":"Done"}}"#;

        let result = service.preprocess_jsonl_content(jsonl_content);

        // Should only contain the failed event
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "Should only have 1 error line, got: {:?}",
            lines
        );
        assert!(lines[0].contains("ERROR:"), "Should be formatted as ERROR");
    }
}
