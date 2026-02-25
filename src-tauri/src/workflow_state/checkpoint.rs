//! Step-Level Checkpointing
//!
//! Provides fine-grained checkpointing at the step level for resume capability.
//! When a workflow is interrupted mid-execution, this allows it to resume
//! from the exact step where it stopped.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::debug;

use crate::database::CheckpointDb;
use crate::unified_workflow_executor::get_parent_task_id;

/// Status of a step checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepCheckpointStatus {
    /// Step is pending execution.
    Pending,
    /// Step is currently running.
    Running,
    /// Step completed successfully.
    Success,
    /// Step failed.
    Failed,
    /// Step was skipped.
    Skipped,
}

impl std::fmt::Display for StepCheckpointStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepCheckpointStatus::Pending => write!(f, "pending"),
            StepCheckpointStatus::Running => write!(f, "running"),
            StepCheckpointStatus::Success => write!(f, "success"),
            StepCheckpointStatus::Failed => write!(f, "failed"),
            StepCheckpointStatus::Skipped => write!(f, "skipped"),
        }
    }
}

impl std::str::FromStr for StepCheckpointStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(StepCheckpointStatus::Pending),
            "running" => Ok(StepCheckpointStatus::Running),
            "success" => Ok(StepCheckpointStatus::Success),
            "failed" => Ok(StepCheckpointStatus::Failed),
            "skipped" => Ok(StepCheckpointStatus::Skipped),
            _ => Err(format!("Unknown step checkpoint status: {}", s)),
        }
    }
}

/// A checkpoint for a single step execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepCheckpoint {
    /// Unique ID for this checkpoint.
    pub id: String,
    /// Execution ID (task_run_id) this checkpoint belongs to.
    pub execution_id: String,
    /// Workflow type.
    pub workflow_type: String,
    /// Phase of the workflow (e.g., "setup", "verification", "agentic", "completion").
    pub phase: String,
    /// Iteration number (for phases that repeat).
    pub iteration: Option<u32>,
    /// Step index within the phase.
    pub step_index: usize,
    /// Stage index for multi-stage phased workflows (0-indexed).
    pub stage_index: Option<u32>,
    /// Step type (e.g., "playwright", "automation", "ai").
    pub step_type: String,
    /// Step name for display.
    pub step_name: Option<String>,
    /// Current status.
    pub status: StepCheckpointStatus,
    /// Result data as JSON (if completed).
    pub result_json: Option<String>,
    /// Step configuration as JSON (single source of truth for step config).
    /// This eliminates the need to look up config in task_runs.execution_steps_json.
    pub step_config_json: Option<String>,
    /// When the step started.
    pub started_at: Option<String>,
    /// When the step completed.
    pub completed_at: Option<String>,
    /// Duration in milliseconds.
    pub duration_ms: Option<i64>,
    /// Error message if failed.
    pub error: Option<String>,
}

impl StepCheckpoint {
    /// Create a new step checkpoint in pending state.
    pub fn new(
        execution_id: impl Into<String>,
        workflow_type: impl Into<String>,
        phase: impl Into<String>,
        iteration: Option<u32>,
        step_index: usize,
        step_type: impl Into<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            execution_id: execution_id.into(),
            workflow_type: workflow_type.into(),
            phase: phase.into(),
            iteration,
            step_index,
            stage_index: None,
            step_type: step_type.into(),
            step_name: None,
            status: StepCheckpointStatus::Pending,
            result_json: None,
            step_config_json: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
            error: None,
        }
    }

    /// Set the step name.
    pub fn with_step_name(mut self, name: impl Into<String>) -> Self {
        self.step_name = Some(name.into());
        self
    }

    /// Set the stage index for multi-stage workflows.
    pub fn with_stage_index(mut self, stage_index: Option<u32>) -> Self {
        self.stage_index = stage_index;
        self
    }

    /// Set the step configuration JSON.
    /// This stores the full step configuration as the single source of truth.
    pub fn with_step_config<T: serde::Serialize>(mut self, config: &T) -> Self {
        self.step_config_json = serde_json::to_string(config).ok();
        self
    }

    /// Set the step configuration JSON from a raw string.
    pub fn with_step_config_json(mut self, config_json: impl Into<String>) -> Self {
        self.step_config_json = Some(config_json.into());
        self
    }

    /// Mark the step as started.
    pub fn mark_started(&mut self) {
        self.status = StepCheckpointStatus::Running;
        self.started_at = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Mark the step as completed successfully.
    pub fn mark_success(&mut self, result_json: Option<String>, duration_ms: i64) {
        self.status = StepCheckpointStatus::Success;
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());
        self.duration_ms = Some(duration_ms);
        self.result_json = result_json;
    }

    /// Mark the step as failed.
    pub fn mark_failed(&mut self, error: impl Into<String>, duration_ms: i64) {
        self.status = StepCheckpointStatus::Failed;
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());
        self.duration_ms = Some(duration_ms);
        self.error = Some(error.into());
    }

    /// Mark the step as skipped.
    pub fn mark_skipped(&mut self) {
        self.status = StepCheckpointStatus::Skipped;
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());
    }
}

/// Manages step-level checkpoints for workflow execution.
///
/// The CheckpointManager provides:
/// - Saving checkpoints as steps execute
/// - Querying completed steps for resume
/// - Clearing checkpoints for re-execution
#[derive(Clone)]
pub struct CheckpointManager {
    db: Arc<CheckpointDb>,
    workflow_type: String,
}

impl std::fmt::Debug for CheckpointManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckpointManager")
            .field("workflow_type", &self.workflow_type)
            .field("db", &"<CheckpointDb>")
            .finish()
    }
}

impl CheckpointManager {
    /// Create a new checkpoint manager.
    pub fn new(db: Arc<CheckpointDb>, workflow_type: impl Into<String>) -> Self {
        Self {
            db,
            workflow_type: workflow_type.into(),
        }
    }

    /// Save a step checkpoint.
    ///
    /// This is called when a step starts (status=running) and when it completes
    /// (status=success/failed/skipped).
    ///
    /// Note: For composed run children (e.g., composed-run-X-workflow-N),
    /// the execution_id is automatically remapped to the parent task ID because
    /// only parent IDs exist in task_runs (required by foreign key constraint).
    pub fn save_step(&self, checkpoint: &StepCheckpoint) -> Result<(), String> {
        // For composed run children, remap to parent ID to satisfy FK constraint
        let parent_id = get_parent_task_id(&checkpoint.execution_id);

        debug!(
            execution_id = %checkpoint.execution_id,
            parent_id = %parent_id,
            phase = %checkpoint.phase,
            step_index = %checkpoint.step_index,
            status = %checkpoint.status,
            "Saving step checkpoint"
        );

        // Create a modified checkpoint with the parent ID if needed
        if parent_id != checkpoint.execution_id {
            let mut modified = checkpoint.clone();
            modified.execution_id = parent_id;
            self.db.save_workflow_step_checkpoint(&modified)
        } else {
            self.db.save_workflow_step_checkpoint(checkpoint)
        }
    }

    /// Get all completed steps for a given execution, phase, and iteration.
    ///
    /// This is used during resume to determine which steps have already been executed.
    pub fn get_completed_steps(
        &self,
        execution_id: &str,
        phase: &str,
        iteration: Option<u32>,
    ) -> Result<Vec<StepCheckpoint>, String> {
        self.db
            .get_workflow_step_checkpoints(execution_id, phase, iteration)
    }

    /// Get the last completed step index for a phase/iteration.
    ///
    /// Returns None if no steps have been completed.
    pub fn get_last_completed_step_index(
        &self,
        execution_id: &str,
        phase: &str,
        iteration: Option<u32>,
    ) -> Result<Option<usize>, String> {
        let checkpoints = self.get_completed_steps(execution_id, phase, iteration)?;

        let last_completed = checkpoints
            .iter()
            .filter(|cp| {
                matches!(
                    cp.status,
                    StepCheckpointStatus::Success | StepCheckpointStatus::Skipped
                )
            })
            .map(|cp| cp.step_index)
            .max();

        Ok(last_completed)
    }

    /// Clear all checkpoints for a specific iteration.
    ///
    /// This is used when retrying a failed iteration.
    pub fn clear_iteration_checkpoints(
        &self,
        execution_id: &str,
        phase: &str,
        iteration: u32,
    ) -> Result<(), String> {
        debug!(
            execution_id = %execution_id,
            phase = %phase,
            iteration = %iteration,
            "Clearing iteration checkpoints"
        );

        self.db
            .delete_workflow_step_checkpoints(execution_id, Some(phase), Some(iteration))
    }

    /// Clear all checkpoints for an execution.
    ///
    /// This is used when starting a fresh execution.
    pub fn clear_all_checkpoints(&self, execution_id: &str) -> Result<(), String> {
        debug!(
            execution_id = %execution_id,
            "Clearing all checkpoints"
        );

        self.db
            .delete_workflow_step_checkpoints(execution_id, None, None)
    }

    /// Check if a step has already been completed successfully.
    pub fn is_step_completed(
        &self,
        execution_id: &str,
        phase: &str,
        iteration: Option<u32>,
        step_index: usize,
    ) -> Result<bool, String> {
        let checkpoints = self.get_completed_steps(execution_id, phase, iteration)?;

        Ok(checkpoints.iter().any(|cp| {
            cp.step_index == step_index
                && matches!(
                    cp.status,
                    StepCheckpointStatus::Success | StepCheckpointStatus::Skipped
                )
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_step_checkpoint_lifecycle() {
        let mut checkpoint = StepCheckpoint::new(
            "exec-1",
            "unified",
            "verification",
            Some(1),
            0,
            "playwright",
        )
        .with_step_name("Test Login");

        assert_eq!(checkpoint.status, StepCheckpointStatus::Pending);

        checkpoint.mark_started();
        assert_eq!(checkpoint.status, StepCheckpointStatus::Running);
        assert!(checkpoint.started_at.is_some());

        checkpoint.mark_success(Some(r#"{"passed": true}"#.to_string()), 1500);
        assert_eq!(checkpoint.status, StepCheckpointStatus::Success);
        assert!(checkpoint.completed_at.is_some());
        assert_eq!(checkpoint.duration_ms, Some(1500));
    }

    #[test]
    fn test_step_checkpoint_failure() {
        let mut checkpoint = StepCheckpoint::new(
            "exec-2",
            "unified",
            "verification",
            Some(1),
            0,
            "playwright",
        );

        checkpoint.mark_started();
        checkpoint.mark_failed("Assertion failed: expected 'Submit' button", 2500);

        assert_eq!(checkpoint.status, StepCheckpointStatus::Failed);
        assert!(checkpoint.error.is_some());
        assert_eq!(checkpoint.duration_ms, Some(2500));
    }

    #[test]
    fn test_status_parsing() {
        assert_eq!(
            "pending".parse::<StepCheckpointStatus>().unwrap(),
            StepCheckpointStatus::Pending
        );
        assert_eq!(
            "running".parse::<StepCheckpointStatus>().unwrap(),
            StepCheckpointStatus::Running
        );
        assert_eq!(
            "success".parse::<StepCheckpointStatus>().unwrap(),
            StepCheckpointStatus::Success
        );
        assert_eq!(
            "failed".parse::<StepCheckpointStatus>().unwrap(),
            StepCheckpointStatus::Failed
        );
        assert_eq!(
            "skipped".parse::<StepCheckpointStatus>().unwrap(),
            StepCheckpointStatus::Skipped
        );
    }
}
