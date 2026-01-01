//! Workflow Monitor
//!
//! NOTE: This module is being deprecated in favor of the simplified task model.
//! The new model runs tasks until [TASK_COMPLETE] marker is found, without
//! checkpoint-based phase tracking.
//!
//! This module is kept for backward compatibility but should not be used
//! for new code.

use crate::prompts::SavedPrompt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::{info, warn};

// ============================================================================
// Data Types
// ============================================================================

/// Status of a workflow run
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    /// Workflow is actively running (has active session)
    Running,
    /// Workflow is idle but not complete (waiting or between sessions)
    Idle,
    /// Session ended with progress, ready for continuation (no wait needed)
    ReadyToContinue,
    /// Workflow appears stalled (no progress for stall_threshold)
    Stalled,
    /// Workflow completed successfully
    Completed,
    /// Workflow failed with error
    Failed,
}

/// Information about an active workflow run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRun {
    /// Unique ID for this workflow run
    pub id: String,
    /// ID of the prompt being run
    pub prompt_id: String,
    /// Name of the prompt (for display)
    pub prompt_name: String,
    /// Current status
    pub status: WorkflowStatus,
    /// Current phase (from checkpoint)
    pub current_phase: u32,
    /// Previous phase (to detect progress)
    pub previous_phase: u32,
    /// Completion value (target phase)
    pub completion_value: u32,
    /// Path to checkpoint file
    pub checkpoint_path: String,
    /// ID of the currently active AI Developer session (if any)
    pub active_session_id: Option<String>,
    /// Timestamp when workflow started
    pub started_at: u64,
    /// Timestamp of last checkpoint update
    pub last_checkpoint_update: Option<u64>,
    /// Timestamp of last activity (session spawn or checkpoint update)
    pub last_activity: u64,
    /// Number of sessions spawned so far
    pub sessions_spawned: u32,
    /// Error message if status is Failed
    pub error_message: Option<String>,
    /// Log of events during this workflow run
    pub event_log: Vec<WorkflowEvent>,
}

/// An event in the workflow log
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowEvent {
    pub timestamp: u64,
    pub event_type: String,
    pub message: String,
}

impl WorkflowRun {
    /// Create a new workflow run
    /// NOTE: This is deprecated - use the new task model instead
    #[allow(dead_code)]
    #[deprecated(note = "Use the new task model instead")]
    pub fn new(prompt: &SavedPrompt) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            id: uuid::Uuid::new_v4().to_string(),
            prompt_id: prompt.id.clone(),
            prompt_name: prompt.name.clone(),
            status: WorkflowStatus::Idle,
            current_phase: 0,
            previous_phase: 0,
            completion_value: 0,            // Deprecated - no phases in new model
            checkpoint_path: String::new(), // Deprecated - no checkpoint in new model
            active_session_id: None,
            started_at: now,
            last_checkpoint_update: None,
            last_activity: now,
            sessions_spawned: 0,
            error_message: None,
            event_log: vec![WorkflowEvent {
                timestamp: now,
                event_type: "started".to_string(),
                message: format!("Workflow '{}' started", prompt.name),
            }],
        }
    }

    /// Add an event to the log
    pub fn log_event(&mut self, event_type: &str, message: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.event_log.push(WorkflowEvent {
            timestamp: now,
            event_type: event_type.to_string(),
            message: message.to_string(),
        });
        self.last_activity = now;
    }

    /// Update status and log it
    pub fn set_status(&mut self, status: WorkflowStatus, message: &str) {
        self.status = status.clone();
        let status_str = format!("{:?}", status).to_lowercase();
        self.log_event(&status_str, message);
    }
}

// ============================================================================
// Checkpoint Reading
// ============================================================================

/// Read the current phase from a checkpoint file
///
/// Supports both numeric phases (e.g., 1, 2, 3) and completion strings
/// (e.g., "COMPLETE", "DONE"). Completion strings return u32::MAX.
///
/// Also checks for "status" and "workflow_status" fields to detect completion
/// even when current_phase hasn't been updated.
#[allow(dead_code)]
pub fn read_checkpoint_phase(checkpoint_path: &str, phase_field: &str) -> Result<u32, String> {
    let path = Path::new(checkpoint_path);
    if !path.exists() {
        return Err("Checkpoint file does not exist".to_string());
    }

    let content =
        fs::read_to_string(path).map_err(|e| format!("Failed to read checkpoint: {}", e))?;

    let json: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse checkpoint JSON: {}", e))?;

    // FIRST check for explicit "completed" boolean field
    // Many checkpoint formats use { "completed": true, "current_phase": N }
    if let Some(completed) = json.get("completed").and_then(|v| v.as_bool()) {
        if completed {
            info!("Workflow completion detected via 'completed' boolean field");
            return Ok(u32::MAX); // Signal completion
        }
    }

    // Also check for completion status fields - AI might mark status=COMPLETED
    // before updating current_phase
    let status_fields = ["status", "workflow_status"];
    for field in &status_fields {
        if let Some(status) = json.get(*field).and_then(|v| v.as_str()) {
            let upper = status.to_uppercase();
            if upper == "COMPLETE" || upper == "COMPLETED" || upper == "DONE" || upper == "FINISHED"
            {
                info!(
                    "Workflow completion detected via '{}' field: {}",
                    field, status
                );
                return Ok(u32::MAX); // Signal completion
            }
        }
    }

    let phase_value = json.get(phase_field);

    // If phase is a string like "COMPLETE", "DONE", etc., treat as completed
    if let Some(s) = phase_value.and_then(|v| v.as_str()) {
        let upper = s.to_uppercase();
        if upper == "COMPLETE" || upper == "COMPLETED" || upper == "DONE" || upper == "FINISHED" {
            return Ok(u32::MAX); // Signal completion
        }
    }

    // Read numeric phase value
    if let Some(num) = phase_value.and_then(|v| v.as_u64()) {
        return Ok(num as u32);
    }

    Err(format!(
        "Field '{}' not found or not a valid phase value",
        phase_field
    ))
}

/// Get the modification time of the checkpoint file
#[allow(dead_code)]
pub fn get_checkpoint_mtime(checkpoint_path: &str) -> Option<u64> {
    let path = Path::new(checkpoint_path);
    if !path.exists() {
        return None;
    }

    path.metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

// ============================================================================
// Restart Permission
// ============================================================================

/// Information about a restart permission in the checkpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestartPermission {
    /// Whether restart and auto-continuation is permitted
    pub permitted: bool,
    /// When the permission was requested
    pub requested_at: Option<String>,
    /// Reason for the restart
    pub reason: Option<String>,
}

/// Check if the checkpoint has restart_permitted set to true.
///
/// Agents should write this field before triggering a runner restart
/// to allow the workflow to auto-continue after restart.
///
/// Checkpoint format:
/// ```json
/// {
///   "current_phase": 5,
///   "restart_permitted": {
///     "permitted": true,
///     "requested_at": "2025-12-22T18:15:00Z",
///     "reason": "Applying code changes to runner"
///   }
/// }
/// ```
#[allow(dead_code)]
pub fn read_restart_permission(checkpoint_path: &str) -> Option<RestartPermission> {
    let path = Path::new(checkpoint_path);
    if !path.exists() {
        return None;
    }

    let content = fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    // Check for restart_permitted field
    let restart_permitted = json.get("restart_permitted")?;

    // Handle both simple boolean and object format
    if let Some(permitted) = restart_permitted.as_bool() {
        if permitted {
            return Some(RestartPermission {
                permitted: true,
                requested_at: None,
                reason: None,
            });
        }
        return None;
    }

    // Object format
    if let Some(obj) = restart_permitted.as_object() {
        let permitted = obj
            .get("permitted")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if permitted {
            return Some(RestartPermission {
                permitted: true,
                requested_at: obj
                    .get("requested_at")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                reason: obj.get("reason").and_then(|v| v.as_str()).map(String::from),
            });
        }
    }

    None
}

/// Clear the restart_permitted field from a checkpoint file.
///
/// Call this after successfully resuming a workflow to prevent
/// the permission from being reused on subsequent restarts.
#[allow(dead_code)]
pub fn clear_restart_permission(checkpoint_path: &str) -> Result<(), String> {
    let path = Path::new(checkpoint_path);
    if !path.exists() {
        return Ok(()); // Nothing to clear
    }

    let content =
        fs::read_to_string(path).map_err(|e| format!("Failed to read checkpoint: {}", e))?;

    let mut json: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse checkpoint JSON: {}", e))?;

    // Remove the restart_permitted field if it exists
    if let Some(obj) = json.as_object_mut() {
        if obj.remove("restart_permitted").is_some() {
            let updated = serde_json::to_string_pretty(&json)
                .map_err(|e| format!("Failed to serialize checkpoint: {}", e))?;

            fs::write(path, updated).map_err(|e| format!("Failed to write checkpoint: {}", e))?;

            info!(
                "Cleared restart_permitted from checkpoint: {}",
                checkpoint_path
            );
        }
    }

    Ok(())
}

/// Write restart permission to a checkpoint file.
///
/// Agents should call this before triggering a runner restart
/// to allow the workflow to auto-continue after restart.
#[allow(dead_code)]
pub fn write_restart_permission(checkpoint_path: &str, reason: &str) -> Result<(), String> {
    let path = Path::new(checkpoint_path);

    // Read existing checkpoint or create new one
    let mut json: serde_json::Value = if path.exists() {
        let content =
            fs::read_to_string(path).map_err(|e| format!("Failed to read checkpoint: {}", e))?;
        serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse checkpoint JSON: {}", e))?
    } else {
        serde_json::json!({})
    };

    // Add restart_permitted field
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(obj) = json.as_object_mut() {
        obj.insert(
            "restart_permitted".to_string(),
            serde_json::json!({
                "permitted": true,
                "requested_at": now,
                "reason": reason
            }),
        );
    }

    let updated = serde_json::to_string_pretty(&json)
        .map_err(|e| format!("Failed to serialize checkpoint: {}", e))?;

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create checkpoint directory: {}", e))?;
    }

    fs::write(path, updated).map_err(|e| format!("Failed to write checkpoint: {}", e))?;

    info!(
        "Wrote restart_permitted to checkpoint: {} (reason: {})",
        checkpoint_path, reason
    );
    Ok(())
}

// ============================================================================
// Workflow Manager
// ============================================================================

/// Manages active workflow runs
#[allow(dead_code)]
pub struct WorkflowManager {
    /// Active workflow runs, keyed by workflow run ID
    runs: Arc<RwLock<HashMap<String, WorkflowRun>>>,
    /// Prompt ID cache for workflow runs (deprecated)
    #[allow(dead_code)]
    configs: Arc<RwLock<HashMap<String, String>>>,
}

#[allow(dead_code)]
impl WorkflowManager {
    pub fn new() -> Self {
        Self {
            runs: Arc::new(RwLock::new(HashMap::new())),
            configs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Start a new workflow run for a prompt
    /// NOTE: This is deprecated - use the new task model instead
    #[allow(deprecated)]
    pub async fn start_workflow(&self, prompt: &SavedPrompt) -> Result<WorkflowRun, String> {
        // In the new model, all tasks run until [TASK_COMPLETE]
        // This function is kept for backward compatibility
        warn!("start_workflow is deprecated - use the new task model instead");

        let run = WorkflowRun::new(prompt);
        let run_id = run.id.clone();

        // Store the run
        {
            let mut runs = self.runs.write().await;
            runs.insert(run_id.clone(), run.clone());
        }

        info!(
            "Started workflow run {} for prompt '{}'",
            run_id, prompt.name
        );
        Ok(run)
    }

    /// Get a workflow run by ID
    pub async fn get_run(&self, run_id: &str) -> Option<WorkflowRun> {
        let runs = self.runs.read().await;
        runs.get(run_id).cloned()
    }

    /// Get all active workflow runs
    pub async fn get_all_runs(&self) -> Vec<WorkflowRun> {
        let runs = self.runs.read().await;
        runs.values().cloned().collect()
    }

    /// Update a workflow run
    pub async fn update_run(&self, run: WorkflowRun) {
        let mut runs = self.runs.write().await;
        runs.insert(run.id.clone(), run);
    }

    /// Remove a completed/failed workflow run
    pub async fn remove_run(&self, run_id: &str) -> bool {
        let mut runs = self.runs.write().await;
        runs.remove(run_id).is_some()
    }

    /// Clear all workflow runs
    pub async fn clear_all_runs(&self) -> usize {
        let mut runs = self.runs.write().await;
        let count = runs.len();
        runs.clear();
        count
    }

    /// Set the active session for a workflow run
    pub async fn set_active_session(&self, run_id: &str, session_id: Option<String>) {
        let mut runs = self.runs.write().await;
        if let Some(run) = runs.get_mut(run_id) {
            run.active_session_id = session_id.clone();
            if session_id.is_some() {
                run.sessions_spawned += 1;
                run.set_status(
                    WorkflowStatus::Running,
                    &format!("Session started (#{} total)", run.sessions_spawned),
                );
            }
        }
    }

    /// Check and update workflow status based on checkpoint
    /// NOTE: This is deprecated - the new task model uses [TASK_COMPLETE] marker
    #[allow(dead_code)]
    #[deprecated(note = "Use the new task model instead")]
    pub async fn check_workflow_status(
        &self,
        run_id: &str,
        _prompt: &SavedPrompt,
    ) -> Option<WorkflowRun> {
        // Deprecated - new model doesn't use checkpoint-based status
        warn!("check_workflow_status is deprecated - use the new task model instead");

        let runs = self.runs.read().await;
        runs.get(run_id).cloned()
    }

    /// Mark a workflow as failed
    pub async fn fail_workflow(&self, run_id: &str, error: &str) {
        let mut runs = self.runs.write().await;
        if let Some(run) = runs.get_mut(run_id) {
            run.error_message = Some(error.to_string());
            run.set_status(WorkflowStatus::Failed, error);
        }
    }
}

/// Persisted workflow state for recovery after runner restart
/// NOTE: This is deprecated - the new task model persists state in the database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedWorkflowState {
    /// Active workflow runs
    pub runs: Vec<WorkflowRun>,
    /// Prompt ID cache (deprecated - was prompt_id -> WorkflowConfig)
    #[serde(default)]
    pub configs: HashMap<String, String>,
    /// Timestamp when state was persisted
    pub persisted_at: u64,
}

#[allow(dead_code)]
impl WorkflowManager {
    /// Get the path to the workflow state file
    fn get_state_file_path() -> Result<std::path::PathBuf, String> {
        let exe_path =
            std::env::current_exe().map_err(|e| format!("Failed to get exe path: {}", e))?;

        let mut current = exe_path.as_path();
        let runner_dir = loop {
            if let Some(parent) = current.parent() {
                if parent.join("src-tauri").exists()
                    || parent.file_name().is_some_and(|n| n == "qontinui-runner")
                {
                    break parent.to_path_buf();
                }
                current = parent;
            } else {
                let cwd =
                    std::env::current_dir().map_err(|e| format!("Failed to get cwd: {}", e))?;
                break cwd;
            }
        };

        let workspace_root = runner_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| runner_dir.clone());

        let dev_logs = workspace_root.join(".dev-logs");
        fs::create_dir_all(&dev_logs).map_err(|e| format!("Failed to create .dev-logs: {}", e))?;

        Ok(dev_logs.join("workflow-state.json"))
    }

    /// Persist current workflow state to disk
    /// NOTE: Deprecated - use database-backed task runs instead
    pub async fn persist_state(&self) -> Result<(), String> {
        let state_file = Self::get_state_file_path()?;

        let runs: Vec<WorkflowRun> = {
            let runs_guard = self.runs.read().await;
            runs_guard.values().cloned().collect()
        };

        let configs: HashMap<String, String> = {
            let configs_guard = self.configs.read().await;
            configs_guard.clone()
        };

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let state = PersistedWorkflowState {
            runs,
            configs,
            persisted_at: now,
        };

        let json = serde_json::to_string_pretty(&state)
            .map_err(|e| format!("Failed to serialize state: {}", e))?;

        fs::write(&state_file, json).map_err(|e| format!("Failed to write state file: {}", e))?;

        info!("Persisted workflow state to {:?}", state_file);
        Ok(())
    }

    /// Restore workflow state from disk (call on startup)
    /// Returns the restored state if any active workflows were found
    pub async fn restore_state(&self) -> Result<Option<PersistedWorkflowState>, String> {
        let state_file = Self::get_state_file_path()?;

        if !state_file.exists() {
            info!("No persisted workflow state found");
            return Ok(None);
        }

        let json = fs::read_to_string(&state_file)
            .map_err(|e| format!("Failed to read state file: {}", e))?;

        let state: PersistedWorkflowState = serde_json::from_str(&json)
            .map_err(|e| format!("Failed to parse state file: {}", e))?;

        // Filter to only active workflows (Running or Idle)
        let active_runs: Vec<WorkflowRun> = state
            .runs
            .iter()
            .filter(|r| {
                matches!(
                    r.status,
                    WorkflowStatus::Running
                        | WorkflowStatus::Idle
                        | WorkflowStatus::ReadyToContinue
                )
            })
            .cloned()
            .collect();

        if active_runs.is_empty() {
            info!("No active workflows to restore");
            // Clear the state file since there's nothing active
            let _ = fs::remove_file(&state_file);
            return Ok(None);
        }

        info!(
            "Restoring {} active workflow(s) from state",
            active_runs.len()
        );

        // Restore runs
        {
            let mut runs_guard = self.runs.write().await;
            for run in &active_runs {
                runs_guard.insert(run.id.clone(), run.clone());
            }
        }

        // Restore configs
        {
            let mut configs_guard = self.configs.write().await;
            for (prompt_id, config) in &state.configs {
                configs_guard.insert(prompt_id.clone(), config.clone());
            }
        }

        Ok(Some(PersistedWorkflowState {
            runs: active_runs,
            configs: state.configs,
            persisted_at: state.persisted_at,
        }))
    }

    /// Clear persisted state file
    pub fn clear_persisted_state() -> Result<(), String> {
        let state_file = Self::get_state_file_path()?;
        if state_file.exists() {
            fs::remove_file(&state_file)
                .map_err(|e| format!("Failed to remove state file: {}", e))?;
            info!("Cleared persisted workflow state");
        }
        Ok(())
    }
}

impl Default for WorkflowManager {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(deprecated)]
    fn test_workflow_run_creation() {
        let prompt = SavedPrompt {
            id: "test-id".to_string(),
            name: "Test Prompt".to_string(),
            description: String::new(),
            content: "Test content".to_string(),
            category: String::new(),
            tags: vec![],
            max_sessions: Some(5),
            created_at: String::new(),
            modified_at: String::new(),
        };

        let run = WorkflowRun::new(&prompt);
        assert_eq!(run.prompt_id, "test-id");
        assert_eq!(run.status, WorkflowStatus::Idle);
        assert_eq!(run.sessions_spawned, 0);
    }

    #[test]
    #[allow(deprecated)]
    fn test_workflow_event_logging() {
        let prompt = SavedPrompt {
            id: "test-id".to_string(),
            name: "Test".to_string(),
            description: String::new(),
            content: String::new(),
            category: String::new(),
            tags: vec![],
            max_sessions: None,
            created_at: String::new(),
            modified_at: String::new(),
        };

        let mut run = WorkflowRun::new(&prompt);
        run.log_event("test", "Test event");

        assert_eq!(run.event_log.len(), 2); // started + test
        assert_eq!(run.event_log[1].event_type, "test");
    }
}
