//! Workflow Monitor
//!
//! Monitors multi-session workflow prompts and automatically spawns
//! continuation sessions when workflows stall.

use crate::prompts::{SavedPrompt, WorkflowConfig};
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
    pub fn new(prompt: &SavedPrompt) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            id: uuid::Uuid::new_v4().to_string(),
            prompt_id: prompt.id.clone(),
            prompt_name: prompt.name.clone(),
            status: WorkflowStatus::Idle,
            current_phase: 0,
            completion_value: prompt.workflow.completion_value,
            checkpoint_path: prompt.workflow.checkpoint_path.clone(),
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
            .unwrap()
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
pub fn read_checkpoint_phase(checkpoint_path: &str, phase_field: &str) -> Result<u32, String> {
    let path = Path::new(checkpoint_path);
    if !path.exists() {
        return Err("Checkpoint file does not exist".to_string());
    }

    let content =
        fs::read_to_string(path).map_err(|e| format!("Failed to read checkpoint: {}", e))?;

    let json: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse checkpoint JSON: {}", e))?;

    json.get(phase_field)
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .ok_or_else(|| format!("Field '{}' not found or not a number", phase_field))
}

/// Get the modification time of the checkpoint file
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
// Workflow Manager
// ============================================================================

/// Manages active workflow runs
pub struct WorkflowManager {
    /// Active workflow runs, keyed by workflow run ID
    runs: Arc<RwLock<HashMap<String, WorkflowRun>>>,
    /// Workflow config cache (prompt_id -> WorkflowConfig)
    configs: Arc<RwLock<HashMap<String, WorkflowConfig>>>,
}

impl WorkflowManager {
    pub fn new() -> Self {
        Self {
            runs: Arc::new(RwLock::new(HashMap::new())),
            configs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Start a new workflow run for a prompt
    pub async fn start_workflow(&self, prompt: &SavedPrompt) -> Result<WorkflowRun, String> {
        if !prompt.workflow.enabled {
            return Err("Workflow mode not enabled for this prompt".to_string());
        }

        let run = WorkflowRun::new(prompt);
        let run_id = run.id.clone();

        // Store the config
        {
            let mut configs = self.configs.write().await;
            configs.insert(prompt.id.clone(), prompt.workflow.clone());
        }

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
    #[allow(dead_code)]
    pub async fn update_run(&self, run: WorkflowRun) {
        let mut runs = self.runs.write().await;
        runs.insert(run.id.clone(), run);
    }

    /// Remove a completed/failed workflow run
    #[allow(dead_code)]
    pub async fn remove_run(&self, run_id: &str) {
        let mut runs = self.runs.write().await;
        runs.remove(run_id);
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
    pub async fn check_workflow_status(
        &self,
        run_id: &str,
        prompt: &SavedPrompt,
    ) -> Option<WorkflowRun> {
        let mut runs = self.runs.write().await;
        let run = runs.get_mut(run_id)?;

        let config = &prompt.workflow;

        // Check if checkpoint exists and read current phase
        match read_checkpoint_phase(&config.checkpoint_path, &config.phase_field) {
            Ok(phase) => {
                let old_phase = run.current_phase;
                run.current_phase = phase;

                if phase != old_phase {
                    run.log_event("phase_update", &format!("Phase {} -> {}", old_phase, phase));
                }

                // Update checkpoint mtime
                if let Some(mtime) = get_checkpoint_mtime(&config.checkpoint_path) {
                    run.last_checkpoint_update = Some(mtime);
                }

                // Check if complete
                if phase >= config.completion_value {
                    run.set_status(
                        WorkflowStatus::Completed,
                        &format!("Reached phase {}", phase),
                    );
                    return Some(run.clone());
                }
            }
            Err(e) => {
                // Checkpoint might not exist yet if workflow just started
                if run.sessions_spawned > 0 {
                    warn!("Failed to read checkpoint for run {}: {}", run_id, e);
                }
            }
        }

        // Check for stall condition
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let last_activity = run.last_checkpoint_update.unwrap_or(run.last_activity);
        let seconds_since_activity = now.saturating_sub(last_activity);

        // Only consider stalled if no active session AND exceeded threshold
        if run.active_session_id.is_none()
            && seconds_since_activity > config.stall_threshold_secs as u64
            && run.sessions_spawned > 0
        {
            run.set_status(
                WorkflowStatus::Stalled,
                &format!("No activity for {}s", seconds_since_activity),
            );
        } else if run.active_session_id.is_some() {
            if run.status != WorkflowStatus::Running {
                run.status = WorkflowStatus::Running;
            }
        } else if run.status == WorkflowStatus::Stalled {
            // Keep stalled status
        } else {
            run.status = WorkflowStatus::Idle;
        }

        Some(run.clone())
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
    fn test_workflow_run_creation() {
        let prompt = SavedPrompt {
            id: "test-id".to_string(),
            name: "Test Prompt".to_string(),
            description: String::new(),
            content: "Test content".to_string(),
            category: String::new(),
            tags: vec![],
            max_iterations: 50,
            workflow: WorkflowConfig {
                enabled: true,
                checkpoint_path: "/tmp/test.json".to_string(),
                phase_field: "current_phase".to_string(),
                completion_value: 12,
                stall_threshold_secs: 300,
                continuation_prompt: "Continue".to_string(),
            },
            created_at: String::new(),
            modified_at: String::new(),
        };

        let run = WorkflowRun::new(&prompt);
        assert_eq!(run.prompt_id, "test-id");
        assert_eq!(run.status, WorkflowStatus::Idle);
        assert_eq!(run.completion_value, 12);
        assert_eq!(run.sessions_spawned, 0);
    }

    #[test]
    fn test_workflow_event_logging() {
        let prompt = SavedPrompt {
            id: "test-id".to_string(),
            name: "Test".to_string(),
            description: String::new(),
            content: String::new(),
            category: String::new(),
            tags: vec![],
            max_iterations: 10,
            workflow: WorkflowConfig::default(),
            created_at: String::new(),
            modified_at: String::new(),
        };

        let mut run = WorkflowRun::new(&prompt);
        run.log_event("test", "Test event");

        assert_eq!(run.event_log.len(), 2); // started + test
        assert_eq!(run.event_log[1].event_type, "test");
    }
}
