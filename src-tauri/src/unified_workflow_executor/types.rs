//! Types for the unified workflow executor.
//!
//! This module contains all data structures used by the verification-agentic loop.

use serde::{Deserialize, Serialize};

/// Result of a single iteration of the verification-agentic loop.
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
pub struct LoopResult {
    /// Total number of iterations executed
    pub iterations_run: u32,
    /// Whether the loop exited due to verification passing
    pub verification_passed: bool,
    /// Whether the loop exited due to hitting max iterations
    pub max_iterations_reached: bool,
    /// Whether the loop exited due to a critical failure
    pub critical_failure: bool,
    /// All iteration results
    pub iteration_results: Vec<IterationResult>,
}

impl LoopResult {
    /// Check if the workflow should proceed to completion phase.
    /// Only proceeds if verification passed (not max_iterations or critical failure).
    pub fn should_run_completion(&self) -> bool {
        self.verification_passed && !self.critical_failure
    }

    /// Get a human-readable summary of the loop result.
    pub fn summary(&self) -> String {
        if self.verification_passed {
            format!(
                "Verification PASSED after {} iteration(s)",
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
    /// Timeout in seconds for AI execution
    pub timeout_seconds: u64,
    /// The base prompt for the AI
    pub base_prompt: String,
    /// Workflow name (for logging)
    pub workflow_name: String,
    /// Workflow ID
    pub workflow_id: String,
    /// Execution ID (task_run_id)
    pub execution_id: String,
}

/// Phase of workflow execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
            _ => None,
        }
    }
}
