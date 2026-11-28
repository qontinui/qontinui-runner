//! Per-stage execution configuration built from a `UnifiedWorkflow`'s stages.

use std::collections::HashMap;

use crate::unified_workflows::ModelOverrideConfig;

/// Configuration for a single stage within a multi-stage workflow.
///
/// Each stage has its own steps and execution settings. The loop controller
/// iterates over stages sequentially, running each stage's verification-agentic
/// loop independently.
#[derive(Debug, Clone)]
pub struct StageConfig {
    /// Unique identifier for this stage.
    pub id: String,
    /// Display name for this stage.
    pub name: String,
    /// Index of this stage (0-indexed).
    pub index: usize,
    /// Total number of stages in the workflow.
    pub total_stages: usize,
    /// Setup automation steps for this stage.
    pub setup_automation_steps: Vec<crate::step_executor::ExecutionStepConfig>,
    /// Setup prompt steps for this stage.
    pub setup_prompt_steps: Vec<crate::step_executor::ExecutionStepConfig>,
    /// Verification steps for this stage.
    pub verification_steps: Vec<crate::step_executor::ExecutionStepConfig>,
    /// Agentic steps for this stage.
    pub agentic_steps: Vec<crate::step_executor::ExecutionStepConfig>,
    /// Completion automation steps for this stage.
    pub completion_automation_steps: Vec<crate::step_executor::ExecutionStepConfig>,
    /// Completion prompt steps for this stage.
    pub completion_prompt_steps: Vec<crate::step_executor::ExecutionStepConfig>,
    /// Maximum iterations for this stage's loop.
    pub max_iterations: u32,
    /// AI provider override for this stage (None = use workflow default).
    pub provider: Option<String>,
    /// Model override for this stage (None = use workflow default).
    pub model: Option<String>,
    /// Per-phase model overrides for this stage.
    pub model_overrides: HashMap<String, ModelOverrideConfig>,
    /// Timeout in seconds for this stage's AI sessions.
    pub timeout_seconds: Option<u64>,
    /// Whether to pause for human approval after each agentic phase.
    pub approval_gate: bool,
    /// Optional condition for conditional stage execution.
    /// When set, the stage is evaluated against this condition before running.
    /// If the condition is not met, the stage is skipped.
    pub condition: Option<crate::unified_workflows::StageCondition>,
    /// When true, run completion prompt steps BEFORE automation steps.
    /// Default (false) runs automation first, then prompts.
    /// This is used by meta-workflows to ensure the AI hardener runs
    /// before save_workflow_artifact persists the final result.
    pub completion_prompts_first: bool,
}
