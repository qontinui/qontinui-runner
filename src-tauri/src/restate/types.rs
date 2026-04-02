//! Shared types for Restate durable execution integration.
//!
//! These types are serialized/deserialized across the Restate boundary,
//! so they must implement Serialize + Deserialize and be self-contained.

use serde::{Deserialize, Serialize};

/// Input to the Restate workflow `run()` handler.
/// Contains everything needed to execute a full workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowInput {
    /// Workflow execution configuration (max iterations, prompts, model overrides, etc.)
    pub loop_config_json: String,
    /// Setup phase automation steps (serialized)
    pub setup_automation_steps_json: String,
    /// Setup phase prompt steps (serialized)
    pub setup_prompt_steps_json: String,
    /// Verification phase steps (serialized)
    pub verification_steps_json: String,
    /// Agentic phase steps (serialized)
    pub agentic_steps_json: String,
    /// Completion phase automation steps (serialized)
    pub completion_automation_steps_json: String,
    /// Completion phase prompt steps (serialized)
    pub completion_prompt_steps_json: String,
}

/// Output from the Restate workflow `run()` handler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowOutput {
    pub success: bool,
    pub verification_passed: bool,
    pub iterations_run: u32,
    pub critical_failure: bool,
    pub was_stopped: bool,
    pub duration_ms: u64,
    pub files_modified: Vec<String>,
    pub error: Option<String>,
}

/// Durable workflow state stored in Restate's Virtual Object.
/// This is the source of truth for workflow progress when Restate is active.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableWorkflowState {
    pub execution_id: String,
    pub workflow_name: String,
    pub phase: DurablePhase,
    pub iteration: u32,
    pub stage_index: Option<u32>,
    pub verification_passed: bool,
    pub stop_requested: bool,
    pub approval_awakeable_id: Option<String>,
    pub total_steps_completed: u32,
    pub last_completed_step: Option<String>,
}

impl Default for DurableWorkflowState {
    fn default() -> Self {
        Self {
            execution_id: String::new(),
            workflow_name: String::new(),
            phase: DurablePhase::Created,
            iteration: 0,
            stage_index: None,
            verification_passed: false,
            stop_requested: false,
            approval_awakeable_id: None,
            total_steps_completed: 0,
            last_completed_step: None,
        }
    }
}

/// Workflow phase in the durable execution model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DurablePhase {
    Created,
    Setup,
    Verification,
    Agentic,
    Completion,
    Compensating,
    Completed,
    Failed,
    Stopped,
}

impl DurablePhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Setup => "setup",
            Self::Verification => "verification",
            Self::Agentic => "agentic",
            Self::Completion => "completion",
            Self::Compensating => "compensating",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Stopped)
    }
}

/// Result of executing a single step within a `ctx.run()` side effect.
/// This is what gets journaled by Restate for replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableStepResult {
    pub success: bool,
    pub step_index: usize,
    pub step_type: String,
    pub step_name: Option<String>,
    pub error: Option<String>,
    pub output_data: Option<serde_json::Value>,
    pub duration_ms: u64,
    /// Variables set by this step (for SharedVariableStore reconstruction on replay)
    pub variables_set: Vec<(String, String)>,
}

/// Approval response submitted via awakeable resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalResponse {
    pub approved: bool,
    pub comment: Option<String>,
    pub reviewer: Option<String>,
}

/// Event emitted to the frontend when a workflow reaches an approval gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalPendingEvent {
    pub execution_id: String,
    pub workflow_name: String,
    pub iteration: u32,
    pub awakeable_id: String,
    pub prompt: String,
}
