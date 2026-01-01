//! Task Monitor Module
//!
//! Monitors Claude session output for task completion by watching log files
//! and detecting the [TASK_COMPLETE] marker.
//!
//! This module implements the simplified task model where tasks run until
//! the AI signals completion with [TASK_COMPLETE].

use crate::database::{session_start_marker, CheckpointDb, TASK_COMPLETE_MARKER};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Polling interval for log file monitoring (in milliseconds)
const POLL_INTERVAL_MS: u64 = 2000;

/// Context usage patterns that indicate high context consumption
/// Claude CLI outputs these when context is getting full
const CONTEXT_WARNING_PATTERNS: &[&str] = &[
    "context window",
    "running low on context",
    "context limit",
    "approaching the limit",
    "context is nearly full",
    "max_tokens",
];

/// State for an individual task monitor
#[derive(Debug)]
#[allow(dead_code)]
struct MonitorState {
    /// Task run ID being monitored
    task_run_id: String,
    /// Path to the log file being monitored
    log_file: PathBuf,
    /// Path to dev-logs directory (for completion marker files)
    dev_logs_path: PathBuf,
    /// Current session number
    session_num: u32,
    /// Last read position in the log file
    last_position: u64,
    /// Whether monitoring is active
    active: bool,
    /// Handle to cancel the monitoring task
    cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

/// Task Monitor manages background monitoring of Claude session output
pub struct TaskMonitor {
    /// Reference to the checkpoint database
    db: Arc<CheckpointDb>,
    /// Active monitors, keyed by task_run_id
    monitors: Arc<RwLock<HashMap<String, MonitorState>>>,
}

impl TaskMonitor {
    /// Create a new TaskMonitor with a reference to the database
    pub fn new(db: Arc<CheckpointDb>) -> Self {
        Self {
            db,
            monitors: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Start monitoring a task's log file for completion
    ///
    /// # Arguments
    /// * `task_run_id` - The ID of the task run to monitor
    /// * `log_file` - Path to the log file to monitor
    /// * `dev_logs_path` - Path to the dev-logs directory
    ///
    /// # Returns
    /// Ok(()) if monitoring started successfully, Err if already monitoring or other error
    pub async fn start_monitoring(
        &self,
        task_run_id: &str,
        log_file: PathBuf,
        dev_logs_path: PathBuf,
    ) -> Result<(), String> {
        // Check if already monitoring
        {
            let monitors = self.monitors.read().await;
            if monitors.contains_key(task_run_id) {
                return Err(format!("Already monitoring task run: {}", task_run_id));
            }
        }

        // Create cancel channel
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();

        // Create monitor state
        let state = MonitorState {
            task_run_id: task_run_id.to_string(),
            log_file: log_file.clone(),
            dev_logs_path: dev_logs_path.clone(),
            session_num: 1,
            last_position: 0,
            active: true,
            cancel_tx: Some(cancel_tx),
        };

        // Store the state
        {
            let mut monitors = self.monitors.write().await;
            monitors.insert(task_run_id.to_string(), state);
        }

        // Clone values for the spawned task
        let db = self.db.clone();
        let monitors = self.monitors.clone();
        let task_id = task_run_id.to_string();

        // Spawn the monitoring task
        tokio::spawn(async move {
            Self::monitor_loop(db, monitors, task_id, log_file, dev_logs_path, cancel_rx).await;
        });

        info!("Started monitoring task run: {}", task_run_id);
        Ok(())
    }

    /// Stop monitoring a task
    pub async fn stop_monitoring(&self, task_run_id: &str) -> Result<(), String> {
        let mut monitors = self.monitors.write().await;

        if let Some(mut state) = monitors.remove(task_run_id) {
            state.active = false;
            if let Some(tx) = state.cancel_tx.take() {
                let _ = tx.send(()); // Signal cancellation
            }
            info!("Stopped monitoring task run: {}", task_run_id);
            Ok(())
        } else {
            Err(format!("Not monitoring task run: {}", task_run_id))
        }
    }

    /// Check if a task is currently being monitored
    #[allow(dead_code)]
    pub async fn is_monitoring(&self, task_run_id: &str) -> bool {
        let monitors = self.monitors.read().await;
        monitors.contains_key(task_run_id)
    }

    /// Get all currently monitored task IDs
    #[allow(dead_code)]
    pub async fn get_monitored_tasks(&self) -> Vec<String> {
        let monitors = self.monitors.read().await;
        monitors.keys().cloned().collect()
    }

    /// The main monitoring loop - runs in a background task
    async fn monitor_loop(
        db: Arc<CheckpointDb>,
        monitors: Arc<RwLock<HashMap<String, MonitorState>>>,
        task_run_id: String,
        log_file: PathBuf,
        dev_logs_path: PathBuf,
        mut cancel_rx: tokio::sync::oneshot::Receiver<()>,
    ) {
        info!("Monitor loop started for task: {}", task_run_id);

        let mut interval =
            tokio::time::interval(tokio::time::Duration::from_millis(POLL_INTERVAL_MS));
        let mut last_position: u64 = 0;
        let mut session_num: u32 = 1;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    // Check if still active
                    {
                        let monitors_guard = monitors.read().await;
                        if !monitors_guard.contains_key(&task_run_id) {
                            debug!("Monitor removed for task: {}", task_run_id);
                            break;
                        }
                    }

                    // Process new content from log file
                    match Self::read_new_content(&log_file, last_position).await {
                        Ok((new_content, new_position)) => {
                            if !new_content.is_empty() {
                                last_position = new_position;

                                // Update monitor state
                                {
                                    let mut monitors_guard = monitors.write().await;
                                    if let Some(state) = monitors_guard.get_mut(&task_run_id) {
                                        state.last_position = new_position;
                                    }
                                }

                                // Check for context warnings (for proactive monitoring)
                                if Self::check_for_context_warning(&new_content) {
                                    warn!(
                                        "Context warning detected for task {}, session {} may end soon",
                                        task_run_id, session_num
                                    );
                                    // The session will end naturally and handle_session_end will trigger continuation
                                    // This log helps track when context is running low
                                }

                                // Process the new output
                                match Self::process_new_output(&db, &task_run_id, &new_content).await {
                                    Ok(is_complete) => {
                                        if is_complete {
                                            info!("Task complete marker detected for: {}", task_run_id);
                                            if let Err(e) = Self::handle_task_complete(&db, &task_run_id).await {
                                                error!("Failed to handle task completion: {}", e);
                                            }
                                            // Remove from monitors
                                            let mut monitors_guard = monitors.write().await;
                                            monitors_guard.remove(&task_run_id);
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        error!("Failed to process output for task {}: {}", task_run_id, e);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            // Log file might not exist yet or be temporarily unavailable
                            debug!("Could not read log file for task {}: {}", task_run_id, e);
                        }
                    }

                    // Check for session end marker file
                    let session_end_file = dev_logs_path.join(format!("session-{}-complete", task_run_id));
                    if session_end_file.exists() {
                        info!("Session end marker detected for task: {}", task_run_id);

                        // Clean up the marker file
                        let _ = tokio::fs::remove_file(&session_end_file).await;

                        match Self::handle_session_end(&db, &task_run_id, session_num).await {
                            Ok(should_continue) => {
                                if should_continue {
                                    session_num += 1;

                                    // Add session start marker to output
                                    let marker = session_start_marker(session_num);
                                    let _ = db.append_task_output(&task_run_id, &format!("\n\n{}\n", marker), true);

                                    // Update monitor state
                                    {
                                        let mut monitors_guard = monitors.write().await;
                                        if let Some(state) = monitors_guard.get_mut(&task_run_id) {
                                            state.session_num = session_num;
                                        }
                                    }

                                    info!("Continuing task {} with session {}", task_run_id, session_num);
                                } else {
                                    info!("Task {} should not continue, stopping monitor", task_run_id);
                                    let mut monitors_guard = monitors.write().await;
                                    monitors_guard.remove(&task_run_id);
                                    break;
                                }
                            }
                            Err(e) => {
                                error!("Failed to handle session end for task {}: {}", task_run_id, e);
                            }
                        }
                    }
                }
                _ = &mut cancel_rx => {
                    info!("Monitor cancelled for task: {}", task_run_id);
                    break;
                }
            }
        }

        info!("Monitor loop ended for task: {}", task_run_id);
    }

    /// Read new content from the log file since the last position
    async fn read_new_content(
        log_file: &PathBuf,
        last_position: u64,
    ) -> Result<(String, u64), String> {
        let mut file = File::open(log_file)
            .await
            .map_err(|e| format!("Failed to open log file: {}", e))?;

        // Get current file size
        let metadata = file
            .metadata()
            .await
            .map_err(|e| format!("Failed to get file metadata: {}", e))?;
        let file_size = metadata.len();

        // If file is smaller than last position, it was truncated - reset
        let start_position = if file_size < last_position {
            warn!("Log file was truncated, resetting position");
            0
        } else {
            last_position
        };

        // Seek to last position
        file.seek(SeekFrom::Start(start_position))
            .await
            .map_err(|e| format!("Failed to seek in log file: {}", e))?;

        // Read new content
        let mut content = String::new();
        file.read_to_string(&mut content)
            .await
            .map_err(|e| format!("Failed to read log file: {}", e))?;

        Ok((content, file_size))
    }

    /// Check if a task's output contains [TASK_COMPLETE]
    #[allow(dead_code)]
    pub fn check_for_completion(output: &str) -> bool {
        output.contains(TASK_COMPLETE_MARKER)
    }

    /// Check if output contains context warning patterns
    /// Returns true if context appears to be running low
    pub fn check_for_context_warning(output: &str) -> bool {
        let lower = output.to_lowercase();
        CONTEXT_WARNING_PATTERNS
            .iter()
            .any(|pattern| lower.contains(pattern))
    }

    /// Append new output to the task's output_log in database
    async fn process_new_output(
        db: &CheckpointDb,
        task_run_id: &str,
        new_content: &str,
    ) -> Result<bool, String> {
        // Append to database (don't increment session here, we do it on session end)
        let is_complete = db.append_task_output(task_run_id, new_content, false)?;

        if is_complete {
            debug!("Task {} marked complete via output", task_run_id);
        }

        Ok(is_complete)
    }

    /// Called when [TASK_COMPLETE] is detected
    async fn handle_task_complete(db: &CheckpointDb, task_run_id: &str) -> Result<(), String> {
        // Mark the task as complete in the database
        db.complete_task_run(task_run_id)?;
        info!("Task {} marked as complete", task_run_id);
        Ok(())
    }

    /// Called when session ends without [TASK_COMPLETE] (needs continuation)
    ///
    /// # Returns
    /// Ok(true) if the task should continue with a new session
    /// Ok(false) if the task should stop (max sessions reached or task stopped)
    async fn handle_session_end(
        db: &CheckpointDb,
        task_run_id: &str,
        session_num: u32,
    ) -> Result<bool, String> {
        info!(
            "Session {} ended for task {}, checking if should continue",
            session_num, task_run_id
        );

        // Check if we should continue
        let should_continue = db.should_continue_task(task_run_id)?;

        if !should_continue {
            // Get the task to check why we're stopping
            if let Some(task) = db.get_task_run(task_run_id)? {
                if task.status != "running" {
                    info!(
                        "Task {} has status '{}', not continuing",
                        task_run_id, task.status
                    );
                } else if let Some(max) = task.max_sessions {
                    if task.sessions_count >= max {
                        warn!(
                            "Task {} reached max sessions ({}/{}), marking as failed",
                            task_run_id, task.sessions_count, max
                        );
                        db.fail_task_run(
                            task_run_id,
                            &format!("Max sessions reached ({}) without task completion", max),
                        )?;
                    }
                }
            }
        }

        Ok(should_continue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_for_completion() {
        assert!(TaskMonitor::check_for_completion(
            "Some output [TASK_COMPLETE] more text"
        ));
        assert!(TaskMonitor::check_for_completion("[TASK_COMPLETE]"));
        assert!(TaskMonitor::check_for_completion(
            "Done!\n[TASK_COMPLETE]\n"
        ));
        assert!(!TaskMonitor::check_for_completion(
            "Some output without marker"
        ));
        assert!(!TaskMonitor::check_for_completion("TASK_COMPLETE")); // Missing brackets
        assert!(!TaskMonitor::check_for_completion("[TASK COMPLETE]")); // Space instead of underscore
    }

    #[test]
    fn test_session_start_marker() {
        assert_eq!(session_start_marker(1), "[SESSION_START:1]");
        assert_eq!(session_start_marker(5), "[SESSION_START:5]");
        assert_eq!(session_start_marker(100), "[SESSION_START:100]");
    }

    #[test]
    fn test_check_for_context_warning() {
        // Should detect various context warning patterns
        assert!(TaskMonitor::check_for_context_warning(
            "Running low on context, will need to continue"
        ));
        assert!(TaskMonitor::check_for_context_warning(
            "The context window is almost full"
        ));
        assert!(TaskMonitor::check_for_context_warning(
            "Approaching the limit of available context"
        ));
        assert!(TaskMonitor::check_for_context_warning(
            "Context is nearly full"
        ));
        assert!(TaskMonitor::check_for_context_warning(
            "max_tokens limit reached"
        ));

        // Case insensitive
        assert!(TaskMonitor::check_for_context_warning(
            "CONTEXT WINDOW usage is high"
        ));
        assert!(TaskMonitor::check_for_context_warning(
            "Context Limit warning"
        ));

        // Should not match normal content
        assert!(!TaskMonitor::check_for_context_warning(
            "This is normal output"
        ));
        assert!(!TaskMonitor::check_for_context_warning("[TASK_COMPLETE]"));
        assert!(!TaskMonitor::check_for_context_warning(
            "Processing data..."
        ));
    }
}
