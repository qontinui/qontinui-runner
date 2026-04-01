//! Realtime Dashboard Events Module
//!
//! Provides event emission for real-time UI updates in the Learning Dashboard
//! and Checkpoint Browser. These events enable live updates without polling.
//!
//! ## Events
//!
//! - `learning-update`: Emitted when a new learning outcome is recorded
//! - `checkpoint-created`: Emitted when a new checkpoint is saved
//! - `task-status-change`: Emitted when task status changes (started, iteration, completed)

use serde::{Deserialize, Serialize};
use tauri::Emitter;
use tracing::debug;

// ============================================================================
// Event Constants
// ============================================================================

/// Event channel for learning updates
pub const EVENT_LEARNING_UPDATE: &str = "learning-update";

/// Event channel for checkpoint creation
pub const EVENT_CHECKPOINT_CREATED: &str = "checkpoint-created";

/// Event channel for task status changes
pub const EVENT_TASK_STATUS_CHANGE: &str = "task-status-change";

/// Event channel for performance drift detection (online learning)
pub const EVENT_DRIFT_DETECTED: &str = "drift-detected";

/// Event channel for model routing decisions (online learning)
pub const EVENT_MODEL_ROUTING_DECISION: &str = "model-routing-decision";

// ============================================================================
// Learning Update Events
// ============================================================================

/// Payload for learning update events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningUpdatePayload {
    /// Task ID that generated this outcome
    pub task_id: String,
    /// Outcome status (success, failure, partial, abandoned)
    pub status: String,
    /// Duration in seconds if completed
    pub duration_secs: Option<f64>,
    /// Number of iterations
    pub iterations: Option<u32>,
    /// Strategy used
    pub strategy: Option<String>,
    /// Tools used during the task
    pub tools_used: Vec<String>,
    /// Timestamp when this outcome was recorded
    pub timestamp: i64,
}

/// Emit a learning update event when a new outcome is recorded.
pub fn emit_learning_update(
    app_handle: &tauri::AppHandle,
    task_id: &str,
    status: &str,
    duration_secs: Option<f64>,
    iterations: Option<u32>,
    strategy: Option<&str>,
    tools_used: Vec<String>,
) {
    let payload = LearningUpdatePayload {
        task_id: task_id.to_string(),
        status: status.to_string(),
        duration_secs,
        iterations,
        strategy: strategy.map(|s| s.to_string()),
        tools_used,
        timestamp: chrono::Utc::now().timestamp_millis(),
    };

    debug!(
        "Emitting learning update: task={}, status={}",
        task_id, status
    );

    if let Err(e) = app_handle.emit(EVENT_LEARNING_UPDATE, &payload) {
        debug!("Failed to emit learning update event: {}", e);
    }
}

// ============================================================================
// Checkpoint Created Events
// ============================================================================

/// Payload for checkpoint creation events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointCreatedPayload {
    /// Unique checkpoint ID
    pub checkpoint_id: String,
    /// Task run ID this checkpoint belongs to
    pub task_run_id: String,
    /// Iteration number at the time of checkpoint
    pub iteration: u32,
    /// What triggered this checkpoint
    pub trigger: String,
    /// Optional human-readable name
    pub name: Option<String>,
    /// Current state name
    pub state: String,
    /// Timestamp when checkpoint was created
    pub timestamp: i64,
}

/// Emit a checkpoint created event when a new checkpoint is saved.
pub fn emit_checkpoint_created(
    app_handle: &tauri::AppHandle,
    checkpoint_id: &str,
    task_run_id: &str,
    iteration: u32,
    trigger: &str,
    name: Option<&str>,
    state: &str,
) {
    let payload = CheckpointCreatedPayload {
        checkpoint_id: checkpoint_id.to_string(),
        task_run_id: task_run_id.to_string(),
        iteration,
        trigger: trigger.to_string(),
        name: name.map(|s| s.to_string()),
        state: state.to_string(),
        timestamp: chrono::Utc::now().timestamp_millis(),
    };

    debug!(
        "Emitting checkpoint created: id={}, task={}, iteration={}",
        checkpoint_id, task_run_id, iteration
    );

    if let Err(e) = app_handle.emit(EVENT_CHECKPOINT_CREATED, &payload) {
        debug!("Failed to emit checkpoint created event: {}", e);
    }
}

// ============================================================================
// Task Status Change Events
// ============================================================================

/// Possible task statuses
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Task has started
    Started,
    /// Task is in planning phase
    Planning,
    /// Task is executing (with iteration number)
    Executing,
    /// Task is verifying
    Verifying,
    /// Task completed successfully
    Completed,
    /// Task failed
    Failed,
    /// Task was stopped
    Stopped,
    /// Task is paused
    Paused,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Started => write!(f, "started"),
            TaskStatus::Planning => write!(f, "planning"),
            TaskStatus::Executing => write!(f, "executing"),
            TaskStatus::Verifying => write!(f, "verifying"),
            TaskStatus::Completed => write!(f, "completed"),
            TaskStatus::Failed => write!(f, "failed"),
            TaskStatus::Stopped => write!(f, "stopped"),
            TaskStatus::Paused => write!(f, "paused"),
        }
    }
}

/// Payload for task status change events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStatusChangePayload {
    /// Task run ID
    pub task_run_id: String,
    /// New status
    pub status: TaskStatus,
    /// Current iteration number
    pub iteration: u32,
    /// Maximum iterations allowed
    pub max_iterations: u32,
    /// Optional task name
    pub task_name: Option<String>,
    /// Optional reason for status change
    pub reason: Option<String>,
    /// Whether verification passed (for completed/failed)
    pub verification_passed: Option<bool>,
    /// Timestamp of status change
    pub timestamp: i64,
}

/// Emit a task status change event.
pub fn emit_task_status_change(
    app_handle: &tauri::AppHandle,
    task_run_id: &str,
    status: TaskStatus,
    iteration: u32,
    max_iterations: u32,
    task_name: Option<&str>,
    reason: Option<&str>,
    verification_passed: Option<bool>,
) {
    let payload = TaskStatusChangePayload {
        task_run_id: task_run_id.to_string(),
        status,
        iteration,
        max_iterations,
        task_name: task_name.map(|s| s.to_string()),
        reason: reason.map(|s| s.to_string()),
        verification_passed,
        timestamp: chrono::Utc::now().timestamp_millis(),
    };

    debug!(
        "Emitting task status change: task={}, status={}, iteration={}/{}",
        task_run_id, status, iteration, max_iterations
    );

    if let Err(e) = app_handle.emit(EVENT_TASK_STATUS_CHANGE, &payload) {
        debug!("Failed to emit task status change event: {}", e);
    }
}

// ============================================================================
// Convenience Functions
// ============================================================================

/// Emit task started event
pub fn emit_task_started(
    app_handle: &tauri::AppHandle,
    task_run_id: &str,
    max_iterations: u32,
    task_name: Option<&str>,
) {
    emit_task_status_change(
        app_handle,
        task_run_id,
        TaskStatus::Started,
        0,
        max_iterations,
        task_name,
        None,
        None,
    );
}

/// Emit iteration started event
pub fn emit_iteration_started(
    app_handle: &tauri::AppHandle,
    task_run_id: &str,
    iteration: u32,
    max_iterations: u32,
    task_name: Option<&str>,
) {
    emit_task_status_change(
        app_handle,
        task_run_id,
        TaskStatus::Executing,
        iteration,
        max_iterations,
        task_name,
        None,
        None,
    );
}

/// Emit task completed event
pub fn emit_task_completed(
    app_handle: &tauri::AppHandle,
    task_run_id: &str,
    iteration: u32,
    max_iterations: u32,
    task_name: Option<&str>,
    verification_passed: bool,
) {
    emit_task_status_change(
        app_handle,
        task_run_id,
        if verification_passed {
            TaskStatus::Completed
        } else {
            TaskStatus::Failed
        },
        iteration,
        max_iterations,
        task_name,
        None,
        Some(verification_passed),
    );
}

// ============================================================================
// Cost Optimization Events
// ============================================================================

pub const EVENT_COST_UPDATE: &str = "cost-update";
pub const EVENT_BUDGET_WARNING: &str = "budget-warning";
pub const EVENT_COST_ANOMALY: &str = "cost-anomaly";

/// Payload for cost update events (emitted after each AI call).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostUpdatePayload {
    pub task_run_id: String,
    pub phase: String,
    pub iteration: Option<u32>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_usd: f64,
    pub cumulative_cost_usd: f64,
    pub cache_hit_rate: f64,
    pub timestamp: i64,
}

pub fn emit_cost_update(
    app_handle: &tauri::AppHandle,
    task_run_id: &str,
    phase: &str,
    iteration: Option<u32>,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    cost_usd: f64,
    cumulative_cost_usd: f64,
) {
    let total_input = input_tokens + cache_creation_tokens + cache_read_tokens;
    let cache_hit_rate = if total_input > 0 {
        cache_read_tokens as f64 / total_input as f64
    } else {
        0.0
    };

    let payload = CostUpdatePayload {
        task_run_id: task_run_id.to_string(),
        phase: phase.to_string(),
        iteration,
        input_tokens,
        output_tokens,
        cache_creation_tokens,
        cache_read_tokens,
        cost_usd,
        cumulative_cost_usd,
        cache_hit_rate,
        timestamp: chrono::Utc::now().timestamp_millis(),
    };

    debug!(
        "Emitting cost update: task={}, phase={}, cost=${:.4}",
        task_run_id, phase, cost_usd
    );

    if let Err(e) = app_handle.emit(EVENT_COST_UPDATE, &payload) {
        debug!("Failed to emit cost update event: {}", e);
    }
}

/// Payload for budget warning events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetWarningPayload {
    pub task_run_id: String,
    pub remaining_fraction: f64,
    pub total_cost_usd: f64,
    pub budget_limit_usd: f64,
    pub message: String,
    pub timestamp: i64,
}

pub fn emit_budget_warning(
    app_handle: &tauri::AppHandle,
    task_run_id: &str,
    remaining_fraction: f64,
    total_cost_usd: f64,
    budget_limit_usd: f64,
    message: &str,
) {
    let payload = BudgetWarningPayload {
        task_run_id: task_run_id.to_string(),
        remaining_fraction,
        total_cost_usd,
        budget_limit_usd,
        message: message.to_string(),
        timestamp: chrono::Utc::now().timestamp_millis(),
    };

    debug!(
        "Emitting budget warning: task={}, remaining={:.0}%",
        task_run_id,
        remaining_fraction * 100.0
    );

    if let Err(e) = app_handle.emit(EVENT_BUDGET_WARNING, &payload) {
        debug!("Failed to emit budget warning event: {}", e);
    }
}

/// Payload for cost anomaly events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostAnomalyPayload {
    pub task_run_id: String,
    pub cost_usd: f64,
    pub mean_cost_usd: f64,
    pub std_dev: f64,
    pub z_score: f64,
    pub message: String,
    pub timestamp: i64,
}

pub fn emit_cost_anomaly(
    app_handle: &tauri::AppHandle,
    task_run_id: &str,
    cost_usd: f64,
    mean_cost_usd: f64,
    std_dev: f64,
    z_score: f64,
) {
    let payload = CostAnomalyPayload {
        task_run_id: task_run_id.to_string(),
        cost_usd,
        mean_cost_usd,
        std_dev,
        z_score,
        message: format!(
            "Cost anomaly detected: ${:.4} is {:.1} std devs above mean ${:.4}",
            cost_usd, z_score, mean_cost_usd
        ),
        timestamp: chrono::Utc::now().timestamp_millis(),
    };

    debug!(
        "Emitting cost anomaly: task={}, z_score={:.1}",
        task_run_id, z_score
    );

    if let Err(e) = app_handle.emit(EVENT_COST_ANOMALY, &payload) {
        debug!("Failed to emit cost anomaly event: {}", e);
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_status_display() {
        assert_eq!(TaskStatus::Started.to_string(), "started");
        assert_eq!(TaskStatus::Executing.to_string(), "executing");
        assert_eq!(TaskStatus::Completed.to_string(), "completed");
    }

    #[test]
    fn test_payload_serialization() {
        let payload = LearningUpdatePayload {
            task_id: "task-1".to_string(),
            status: "success".to_string(),
            duration_secs: Some(120.5),
            iterations: Some(3),
            strategy: Some("incremental".to_string()),
            tools_used: vec!["grep".to_string(), "edit".to_string()],
            timestamp: 1234567890,
        };

        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("task-1"));
        assert!(json.contains("success"));
    }

    #[test]
    fn test_checkpoint_payload_serialization() {
        let payload = CheckpointCreatedPayload {
            checkpoint_id: "cp-123".to_string(),
            task_run_id: "task-1".to_string(),
            iteration: 5,
            trigger: "iteration_boundary:5".to_string(),
            name: Some("Before verification".to_string()),
            state: "executing".to_string(),
            timestamp: 1234567890,
        };

        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("cp-123"));
        assert!(json.contains("iteration_boundary"));
    }
}
