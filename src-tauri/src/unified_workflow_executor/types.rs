//! Types for the unified workflow executor.
//!
//! This module contains all data structures used by the verification-agentic loop.

use serde::{Deserialize, Serialize};

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

/// Configuration for the verification-agentic loop.
#[derive(Debug, Clone)]
pub struct LoopConfig {
    /// Maximum number of iterations before giving up
    pub max_iterations: u32,
    /// Optional inactivity timeout in seconds for AI execution.
    /// None = no timeout (recommended), Some(N) = kill AI after N seconds of inactivity.
    pub timeout_seconds: Option<u64>,
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
    Success { output: String },
    /// AI ran but reported failure
    Failed { output: String, error: String },
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
            AgenticOutcome::Success { output } => Some(output),
            AgenticOutcome::Failed { output, .. } => Some(output),
            AgenticOutcome::Error { error } => Some(error),
            AgenticOutcome::Skipped => None,
        }
    }
}

/// Extract the parent task ID from a workflow sequence child ID.
///
/// For workflow sequences, individual workflows have IDs like:
/// `workflow-sequence-{timestamp}-workflow-{n}`
///
/// But only the parent `workflow-sequence-{timestamp}` exists in task_runs.
/// This function extracts the parent ID for use in database operations that
/// reference task_runs (step checkpoints, task_run_events, etc.).
///
/// For non-sequence IDs (e.g., `unified-workflow-{id}-{timestamp}`), returns
/// the original ID unchanged.
///
/// # Examples
///
/// ```
/// use qontinui_runner::unified_workflow_executor::get_parent_task_id;
///
/// // Sequence child → parent
/// assert_eq!(
///     get_parent_task_id("workflow-sequence-1234567890-workflow-1"),
///     "workflow-sequence-1234567890"
/// );
///
/// // Non-sequence → unchanged
/// assert_eq!(
///     get_parent_task_id("unified-workflow-abc-1234567890"),
///     "unified-workflow-abc-1234567890"
/// );
/// ```
pub fn get_parent_task_id(execution_id: &str) -> String {
    // Check if this is a sequence child: workflow-sequence-{timestamp}-workflow-{n}
    if let Some(pos) = execution_id.rfind("-workflow-") {
        let potential_parent = &execution_id[..pos];
        // Verify it looks like a sequence parent (starts with workflow-sequence-)
        if potential_parent.starts_with("workflow-sequence-") {
            return potential_parent.to_string();
        }
    }
    execution_id.to_string()
}

/// Check if an execution ID is a workflow sequence child.
///
/// Returns true for IDs like `workflow-sequence-{timestamp}-workflow-{n}`.
pub fn is_sequence_child(execution_id: &str) -> bool {
    get_parent_task_id(execution_id) != execution_id
}
