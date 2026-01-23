//! Type definitions for the Recap module.
//!
//! Contains all data structures used for building and returning recap data.

use serde::{Deserialize, Serialize};

/// A step in the recap timeline.
#[derive(Debug, Serialize, Clone)]
pub struct RecapStep {
    /// Step name
    pub name: String,
    /// Step type: "workflow", "action", "ai_session", "test", "check"
    pub step_type: String,
    /// Status: "success", "failed", "running", "skipped"
    pub status: String,
    /// Workflow phase: "setup", "verification", "agentic", "completion"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Icon type for frontend rendering (maps to STEP_ICON_CONFIG).
    /// More specific than step_type (e.g., "check_lint" vs "check").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_type: Option<String>,
    /// AI-generated summary of work done (for AI steps).
    /// Extracted from structured output's work_summary field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_summary: Option<String>,
    /// Brief summary of what happened (deterministic summary for non-AI steps)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Duration in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// Error message if failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Nested steps (for workflows containing actions)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<RecapStep>,
}

/// Information about why a run failed.
#[derive(Debug, Serialize)]
pub struct FailureInfo {
    /// Primary reason for failure
    pub reason: String,
    /// Name of the step that failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_step: Option<String>,
    /// Detailed error message or stack trace
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_details: Option<String>,
    /// Error type category
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
}

/// Quick statistics about the run.
#[derive(Debug, Serialize, Default)]
pub struct RecapStats {
    /// Total number of actions executed
    pub total_actions: i32,
    /// Number of successful actions
    pub successful_actions: i32,
    /// Number of failed actions
    pub failed_actions: i32,
    /// Number of skipped actions
    pub skipped_actions: i32,
    /// Total number of AI sessions
    pub ai_sessions: i32,
    /// Number of tests run
    pub tests_run: i32,
    /// Number of tests passed
    pub tests_passed: i32,
}

/// A stage in the recap timeline, grouping related steps.
/// Stages represent the 4 workflow phases: setup, verification, agentic, completion.
#[derive(Debug, Serialize, Clone)]
pub struct StageRecap {
    /// Stage identifier: "setup", "verification", "agentic", "completion"
    pub stage: String,
    /// Display name: "Setup", "Agentic", "Verification", "Completion"
    pub display_name: String,
    /// Status: "success", "failed", "running", "skipped", "pending"
    pub status: String,
    /// When this stage started (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// When this stage ended (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// Duration in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// Steps in this stage
    pub steps: Vec<RecapStep>,
    /// Iteration number (for agentic/verification in loop)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iteration: Option<u32>,
}

/// Complete recap data for a task run.
#[derive(Debug, Serialize)]
pub struct RecapData {
    /// Task run ID
    pub task_run_id: String,
    /// Task name
    pub task_name: String,
    /// Overall status: "running", "complete", "failed", "stopped"
    pub status: String,
    /// Total duration in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// When the run started
    pub created_at: String,
    /// When the run completed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,

    /// Failure info (prominent if failed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_info: Option<FailureInfo>,

    /// AI-generated or extracted summary
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,

    /// Whether the goal was achieved (from orchestrator)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_achieved: Option<bool>,

    /// Steps grouped by stage (with timing from transition_history)
    pub stages: Vec<StageRecap>,

    /// Steps overview (timeline) - flat list for backwards compatibility
    pub steps: Vec<RecapStep>,

    /// Quick statistics
    pub stats: RecapStats,
}

/// Response wrapper for recap commands.
#[derive(Debug, Serialize)]
pub struct RecapResponse<T> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T> RecapResponse<T> {
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(message: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message),
        }
    }
}

/// Stage transition data parsed from transition_history_json.
#[derive(Debug, Deserialize)]
pub struct StageTransition {
    pub from: String,
    pub to: String,
    pub timestamp: String,
    pub iteration: u32,
}

/// Information about a stage occurrence (may repeat for iterations).
#[derive(Debug)]
pub struct StageOccurrence {
    pub stage: String,
    pub iteration: u32,
    pub started_at: String,
    pub ended_at: Option<String>,
}

/// Parsed execution step from execution_steps_json.
#[derive(Deserialize)]
pub struct ExecutionStep {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, alias = "id")]
    pub step_id: Option<String>,
    #[serde(default, rename = "type")]
    pub step_type: Option<String>,
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub config: Option<serde_json::Value>,
}
