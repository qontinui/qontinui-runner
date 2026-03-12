//! Types for the unified workflow executor.
//!
//! This module contains all data structures used by the verification-agentic loop.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use super::conditional_routing::{evaluate_routing_rules, ResolvedModelConfig, RoutingContext};
use crate::unified_workflows::ModelOverrideConfig;

/// Result of a single iteration of the verification-agentic loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationResult {
    /// Iteration number (1-indexed)
    pub iteration: u32,
    /// Whether verification passed in this iteration
    pub verification_passed: bool,
    /// Whether there was a critical failure that should stop the loop
    pub critical_failure: bool,
    /// Number of checks that passed
    pub passed_checks: usize,
    /// Number of checks that failed
    pub failed_checks: usize,
    /// Failure context to pass to the AI (if verification failed)
    pub failure_context: String,
    /// Whether the agentic phase ran
    pub agentic_phase_ran: bool,
    /// Whether the agentic phase succeeded (if it ran)
    pub agentic_phase_success: Option<bool>,
}

/// Final result of the entire verification-agentic loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopResult {
    /// Total number of iterations executed
    pub iterations_run: u32,
    /// Whether the loop exited due to verification passing
    pub verification_passed: bool,
    /// Whether the loop exited due to hitting max iterations
    pub max_iterations_reached: bool,
    /// Whether the loop exited due to a critical failure
    pub critical_failure: bool,
    /// Whether the loop was stopped externally (via stop_ai_analysis endpoint)
    pub was_stopped: bool,
    /// Whether the AI signaled that errors are unfixable (via [UNFIXABLE_ERRORS] marker)
    #[serde(default)]
    pub unfixable_errors: bool,
    /// All iteration results
    pub iteration_results: Vec<IterationResult>,
}

impl LoopResult {
    /// Check if the workflow should proceed to completion phase.
    /// Proceeds if:
    /// - Verification passed (normal success path)
    /// - AI signaled unfixable errors (graceful exit path)
    ///   Does NOT proceed if:
    /// - Critical failure occurred
    /// - User stopped the workflow
    /// - Max iterations reached without unfixable signal
    pub fn should_run_completion(&self) -> bool {
        !self.critical_failure
            && !self.was_stopped
            && (self.verification_passed || self.unfixable_errors)
    }

    /// Get a human-readable summary of the loop result.
    pub fn summary(&self) -> String {
        if self.was_stopped {
            format!("STOPPED by user after {} iteration(s)", self.iterations_run)
        } else if self.verification_passed {
            format!(
                "Verification PASSED after {} iteration(s)",
                self.iterations_run
            )
        } else if self.unfixable_errors {
            format!(
                "AI determined errors UNFIXABLE after {} iteration(s) - proceeding to completion",
                self.iterations_run
            )
        } else if self.critical_failure {
            format!(
                "CRITICAL FAILURE after {} iteration(s) - loop stopped",
                self.iterations_run
            )
        } else if self.max_iterations_reached {
            format!(
                "Max iterations ({}) reached - verification still failing",
                self.iterations_run
            )
        } else {
            format!("Loop ended after {} iteration(s)", self.iterations_run)
        }
    }
}

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

/// Configuration for the verification-agentic loop.
#[derive(Debug, Clone)]
pub struct LoopConfig {
    /// Maximum number of iterations before giving up
    pub max_iterations: u32,
    /// The base prompt for the AI
    pub base_prompt: String,
    /// Workflow name (for logging)
    pub workflow_name: String,
    /// Workflow ID
    pub workflow_id: String,
    /// Execution ID (task_run_id)
    pub execution_id: String,
    /// Error IDs targeted by this workflow (for auto-resolution on success).
    /// When the workflow completes successfully, these errors will be marked as resolved.
    pub targeted_error_ids: Vec<i64>,
    /// Starting iteration for resumed workflows (0 = start fresh, N = resume from iteration N)
    pub starting_iteration: u32,
    /// Run agentic phase before verification on first iteration.
    /// This is useful for error-fix workflows where we want the AI to attempt
    /// a fix before verification runs (since verification may pass immediately
    /// if it only checks current state, not whether fixes were applied).
    pub run_agentic_first: bool,
    /// Artifact directory for sharing files between phases.
    /// Created automatically at the start of execution.
    /// Steps can reference it via `{{artifact_dir}}` variable substitution.
    pub artifact_dir: Option<PathBuf>,
    /// Whether this is a dev mode execution (enables dev-only features like
    /// self-analysis steps and learning outcome recording).
    pub is_dev_mode: bool,
    /// Whether to run a completion sweep after verification passes.
    pub enable_sweep: bool,
    /// Maximum number of sweep iterations.
    pub max_sweep_iterations: u32,
    /// Stages for multi-stage workflows. Empty = single-stage (backward compat).
    pub stages: Vec<StageConfig>,
    /// Whether to stop execution if a stage fails verification (default: false).
    pub stop_on_failure: bool,
    /// Whether to enable reflection mode during agentic iterations.
    /// When true, the AI investigates root causes before fixing failures.
    pub reflection_mode: bool,
    /// Optional AI provider override for this loop (from stage config).
    pub provider_override: Option<String>,
    /// Optional AI model override for this loop (from stage config).
    pub model_override: Option<String>,
    /// Per-phase model overrides (from stage/workflow config).
    pub model_overrides: HashMap<String, ModelOverrideConfig>,
    /// Stage index when running as part of a multi-stage workflow (None = single-stage).
    pub stage_index: Option<u32>,
    /// Maximum total AI sessions across all stages (None = unlimited).
    /// Checked against the task_run's sessions_count in the database.
    pub max_sessions: Option<u32>,
    /// Whether to auto-run the generated workflow after a meta-workflow completes.
    /// When true, the backend reads `result_data.generated_workflow_id` and spawns
    /// the generated workflow without relying on frontend polling.
    pub auto_run_generated: bool,
    /// Whether to pause for human approval after each agentic phase.
    /// When true, the loop controller registers an approval request and waits
    /// for a human response before continuing to the next verification iteration.
    pub approval_gate: bool,
    /// Maximum token budget for iteration context building.
    /// Context sections are added in priority order until this budget is reached.
    /// Default: 100_000 (~400K chars). Set lower to reduce prompt size.
    pub max_context_tokens: usize,
    /// Whether to include knowledge from similar workflows (cross-workflow learning).
    /// When true, the first iteration queries the knowledge base for insights
    /// from other workflows with similar names.
    pub cross_workflow_learning: bool,
    /// Per-step verification history tracking pass/fail across iterations.
    /// Key: step name, Value: vector of pass (true) / fail (false) results per iteration.
    /// Runtime-only state — not persisted. Populated during loop execution.
    pub verification_history: HashMap<String, Vec<bool>>,
    /// Runtime routing context for conditional model routing.
    /// Updated by the loop controller before each phase invocation.
    pub routing_context: RoutingContext,
    /// Project/workspace path for project-scoped learning.
    /// When set, project reflection knowledge is loaded into AI context.
    pub project_path: Option<String>,
}

impl LoopConfig {
    /// Update the routing context with current loop state.
    pub fn set_routing_context(&mut self, iteration: u32, verification_failures: u32) {
        self.routing_context = RoutingContext {
            iteration,
            verification_failures,
            stage_index: self.stage_index,
        };
    }

    /// Resolve the model for a specific phase, considering routing rules.
    /// If routing rules exist and the routing context is populated, evaluates rules first.
    /// Fallback chain: routing rules → phase-specific override → workflow-level model_override → None.
    /// The sentinel value `"__smart__"` is treated as None (let the router decide).
    pub fn resolve_model_for_phase(&self, phase: &str) -> Option<String> {
        // Check if there are routing rules for this phase
        if let Some(config) = self.model_overrides.get(phase) {
            if let Some(ref rules) = config.routing_rules {
                if !rules.is_empty() && self.routing_context.iteration > 0 {
                    let resolved = evaluate_routing_rules(rules, &self.routing_context, config);
                    let model = resolved.model.or_else(|| self.model_override.clone());
                    return match model.as_deref() {
                        Some("__smart__") => None,
                        _ => model,
                    };
                }
            }
        }

        let model = self
            .model_overrides
            .get(phase)
            .and_then(|c| c.model.clone())
            .or_else(|| self.model_override.clone());

        // "__smart__" sentinel means "let the router decide"
        match model.as_deref() {
            Some("__smart__") => None,
            _ => model,
        }
    }

    /// Resolve the provider for a specific phase, considering routing rules.
    /// If routing rules exist and the routing context is populated, evaluates rules first.
    /// Fallback chain: routing rules → phase-specific override → workflow-level provider_override → None.
    pub fn resolve_provider_for_phase(&self, phase: &str) -> Option<String> {
        // Check if there are routing rules for this phase
        if let Some(config) = self.model_overrides.get(phase) {
            if let Some(ref rules) = config.routing_rules {
                if !rules.is_empty() && self.routing_context.iteration > 0 {
                    let resolved = evaluate_routing_rules(rules, &self.routing_context, config);
                    return resolved.provider.or_else(|| self.provider_override.clone());
                }
            }
        }

        self.model_overrides
            .get(phase)
            .and_then(|c| c.provider.clone())
            .or_else(|| self.provider_override.clone())
    }

    /// Resolve the temperature override for a specific phase.
    pub fn resolve_temperature_for_phase(&self, phase: &str) -> Option<f32> {
        self.model_overrides.get(phase).and_then(|c| c.temperature)
    }

    /// Resolve the max_tokens override for a specific phase.
    pub fn resolve_max_tokens_for_phase(&self, phase: &str) -> Option<u32> {
        self.model_overrides.get(phase).and_then(|c| c.max_tokens)
    }

    /// Resolve the fallback model for a specific phase.
    pub fn resolve_fallback_model_for_phase(&self, phase: &str) -> Option<String> {
        self.model_overrides
            .get(phase)
            .and_then(|c| c.fallback_model.clone())
    }

    /// Resolve the fallback provider for a specific phase.
    pub fn resolve_fallback_provider_for_phase(&self, phase: &str) -> Option<String> {
        self.model_overrides
            .get(phase)
            .and_then(|c| c.fallback_provider.clone())
    }

    /// Resolve model for a phase with runtime context for conditional routing.
    ///
    /// If the phase has routing_rules, evaluates them against the context.
    /// Otherwise falls back to the static resolve chain.
    pub fn resolve_model_for_phase_with_context(
        &self,
        phase: &str,
        ctx: &RoutingContext,
    ) -> ResolvedModelConfig {
        if let Some(config) = self.model_overrides.get(phase) {
            if let Some(ref rules) = config.routing_rules {
                if !rules.is_empty() {
                    let resolved = evaluate_routing_rules(rules, ctx, config);
                    // Apply workflow-level fallback for model/provider
                    return ResolvedModelConfig {
                        model: resolved.model.or_else(|| self.model_override.clone()),
                        provider: resolved.provider.or_else(|| self.provider_override.clone()),
                        temperature: resolved.temperature,
                        max_tokens: resolved.max_tokens,
                    };
                }
            }
        }

        // No routing rules — use static resolution
        ResolvedModelConfig {
            model: self.resolve_model_for_phase(phase),
            provider: self.resolve_provider_for_phase(phase),
            temperature: self.resolve_temperature_for_phase(phase),
            max_tokens: self.resolve_max_tokens_for_phase(phase),
        }
    }
}

/// Result of the completion sweep phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweepResult {
    /// Number of sweep iterations executed.
    pub iterations_run: u32,
    /// Whether the AI signaled no more work needed.
    pub no_more_steps: bool,
}

/// Phase of workflow execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowPhase {
    Setup,
    Verification,
    Agentic,
    Completion,
}

impl WorkflowPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkflowPhase::Setup => "setup",
            WorkflowPhase::Verification => "verification",
            WorkflowPhase::Agentic => "agentic",
            WorkflowPhase::Completion => "completion",
        }
    }
}

/// Outcome of attempting to run the agentic phase.
#[derive(Debug, Clone)]
pub enum AgenticOutcome {
    /// AI ran successfully
    Success {
        output: String,
        parsed: Option<super::agentic_output::AgenticPhaseOutput>,
    },
    /// AI ran but reported failure
    Failed {
        output: String,
        error: String,
        parsed: Option<super::agentic_output::AgenticPhaseOutput>,
    },
    /// AI execution errored out
    Error { error: String },
    /// Skipped (no agentic steps defined)
    Skipped,
}

impl AgenticOutcome {
    pub fn is_success(&self) -> bool {
        matches!(self, AgenticOutcome::Success { .. })
    }

    pub fn output(&self) -> Option<&str> {
        match self {
            AgenticOutcome::Success { output, .. } => Some(output),
            AgenticOutcome::Failed { output, .. } => Some(output),
            AgenticOutcome::Error { error } => Some(error),
            AgenticOutcome::Skipped => None,
        }
    }

    /// Access the parsed structured output, if available.
    pub fn parsed(&self) -> Option<&super::agentic_output::AgenticPhaseOutput> {
        match self {
            AgenticOutcome::Success { parsed, .. } => parsed.as_ref(),
            AgenticOutcome::Failed { parsed, .. } => parsed.as_ref(),
            AgenticOutcome::Error { .. } | AgenticOutcome::Skipped => None,
        }
    }
}

/// Extract the parent task ID from a composed run child ID.
///
/// For composed runs, individual workflows have IDs like:
/// `composed-run-{timestamp}-workflow-{n}`
///
/// But only the parent `composed-run-{timestamp}` exists in task_runs.
/// This function extracts the parent ID for use in database operations that
/// reference task_runs (step checkpoints, task_run_events, etc.).
///
/// For non-composed IDs (e.g., `unified-workflow-{id}-{timestamp}`), returns
/// the original ID unchanged.
///
/// # Examples
///
/// ```
/// use qontinui_runner::unified_workflow_executor::get_parent_task_id;
///
/// // Composed run child → parent
/// assert_eq!(
///     get_parent_task_id("composed-run-1234567890-workflow-1"),
///     "composed-run-1234567890"
/// );
///
/// // Non-composed → unchanged
/// assert_eq!(
///     get_parent_task_id("unified-workflow-abc-1234567890"),
///     "unified-workflow-abc-1234567890"
/// );
/// ```
pub fn get_parent_task_id(execution_id: &str) -> String {
    // Check if this is a composed run child: composed-run-{timestamp}-workflow-{n}
    if let Some(pos) = execution_id.rfind("-workflow-") {
        let potential_parent = &execution_id[..pos];
        // Verify it looks like a composed run parent (starts with composed-run-)
        if potential_parent.starts_with("composed-run-") {
            return potential_parent.to_string();
        }
    }
    execution_id.to_string()
}

/// Check if an execution ID is a composed run child.
///
/// Returns true for IDs like `composed-run-{timestamp}-workflow-{n}`.
pub fn is_sequence_child(execution_id: &str) -> bool {
    get_parent_task_id(execution_id) != execution_id
}
