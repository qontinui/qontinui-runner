//! State machine for the verification-agentic loop.
//!
//! Replaces the monolithic `run_verification_agentic_loop` with an explicit
//! state machine where each state has a small handler that returns the next state.
//!
//! ## Design
//!
//! - `LoopState` enum: each variant carries exactly the data its handler needs
//! - `LoopContext`: accumulated state that spans multiple iterations
//! - `CompletionReason`: why the loop exited (success, max iterations, stopped, etc.)
//! - Dispatch loop: `match state { ... }` calls one handler per iteration step
//!
//! Each handler is 30-150 lines (vs 2,000+ in the monolith) and can be tested
//! independently by constructing its input state and asserting the returned state.

use std::time::Instant;

use crate::step_executor::{ExecutionStepConfig, VerificationPhaseResult};

use super::compensation::CompensationManager;
use super::convergence::{ConvergenceConfig, ConvergenceDetector, ConvergenceReport};
use super::types::{AgenticOutcome, IterationResult, LoopConfig, LoopResult};

// =============================================================================
// State Machine Types
// =============================================================================

/// Every state the verification-agentic loop can be in.
///
/// Each variant carries only the data its handler needs — illegal states
/// are unrepresentable.
pub(crate) enum LoopState {
    /// Entry: check stop/pause, max iterations, session budget
    CheckPreconditions,

    /// Check environment readiness and auto-connect SDK (iteration 1 only)
    CheckEnvironment,

    /// Execute verification steps
    RunVerification,

    /// Evaluate verification result — decide: pass, critical fail, or continue
    EvaluateVerification {
        verification_result: VerificationPhaseResult,
    },

    /// Build failure context for the AI from verification failures + history
    BuildFailureContext {
        verification_result: VerificationPhaseResult,
        convergence_report: ConvergenceReport,
    },

    /// Execute the agentic (AI) phase with failure context
    RunAgentic {
        failure_context: String,
        /// Carried from EvaluateVerification for multi-agent mode
        verification_result: VerificationPhaseResult,
    },

    /// Process agentic output: constraints, health, knowledge, findings
    PostAgentic {
        outcome: AgenticOutcome,
        injected_steps: Vec<ExecutionStepConfig>,
        failure_context: String,
    },

    /// Check for unfixable errors, stop signals
    CheckPostAgenticSignals { outcome: AgenticOutcome },

    /// Approval gate (if enabled and agentic succeeded)
    ApprovalGate { outcome: AgenticOutcome },

    /// Capture git diffs and advance to next iteration
    AdvanceIteration,

    /// Loop complete — build final result
    Complete { reason: CompletionReason },
}

/// Why the loop exited.
#[derive(Debug, Clone)]
pub(crate) enum CompletionReason {
    /// All verification steps passed
    VerificationPassed,
    /// Hit the configured max iterations
    MaxIterationsReached,
    /// Hit the global session budget
    MaxSessionsReached,
    /// A verification step had a critical/infrastructure failure
    CriticalFailure { error: String },
    /// User clicked Stop
    Stopped,
    /// AI signaled [UNFIXABLE_ERRORS]
    UnfixableErrors,
    /// Human aborted via approval gate
    ApprovalAborted,
}

// =============================================================================
// Loop Context — accumulated state across iterations
// =============================================================================

/// Mutable state that accumulates across iterations.
///
/// Per-iteration data flows through `LoopState` variants instead.
pub(crate) struct LoopContext {
    // Iteration tracking
    pub iteration: u32,
    pub iteration_results: Vec<IterationResult>,
    pub verification_failures: u32,

    // Convergence & constraints
    pub convergence_detector: ConvergenceDetector,
    pub constraint_engine: crate::constraint_engine::ConstraintEngine,
    pub pending_constraint_context: Option<String>,
    pub proactive_constraints_prompt: Option<String>,
    pub constraints_prompt_injected: bool,
    pub resource_tracker: super::convergence::ResourceTracker,

    // Dynamic steps (injected by AI across iterations)
    pub dynamic_steps: Vec<ExecutionStepConfig>,

    // Health monitoring
    pub pending_health_regression: Option<String>,

    // Compensation (git checkpoints, rollback)
    pub compensation_manager: CompensationManager,
    pub accumulated_diffs: Vec<crate::unified_workflow_executor::types::IterationDiff>,
    pub initial_source_commit: Option<String>,

    // Current iteration's verification counts (set in EvaluateVerification, consumed in PostAgentic)
    pub current_passed_checks: usize,
    pub current_failed_checks: usize,

    // Pre-agentic snapshot (set per iteration, consumed in PostAgentic)
    pub commit_before: Option<String>,
    pub pre_agentic_health: Option<super::health_monitor::HealthBaseline>,
    pub agentic_phase_start: Option<Instant>,
    pub reflection_was_forced: bool,
}

impl LoopContext {
    pub fn new(
        config: &LoopConfig,
        checkpoint_db: std::sync::Arc<crate::database::CheckpointDb>,
        initial_dynamic_steps: Vec<ExecutionStepConfig>,
    ) -> Self {
        // Load constraint config
        let constraint_config = config
            .project_path
            .as_ref()
            .and_then(|p| crate::constraint_engine::config::load_config(std::path::Path::new(p)));

        let resource_limits = constraint_config
            .as_ref()
            .and_then(|c| c.resource_limits.clone())
            .unwrap_or_default();

        let resource_tracker =
            super::convergence::ResourceTracker::new(resource_limits, Instant::now());

        let mut constraint_engine = if let Some(ref loaded) = constraint_config {
            crate::constraint_engine::ConstraintEngine::from_config(loaded)
        } else {
            crate::constraint_engine::ConstraintEngine::new()
        };

        if let Some(ref p) = config.project_path {
            constraint_engine = constraint_engine.with_project_root(std::path::PathBuf::from(p));
        }

        // Apply per-workflow constraint overrides
        for (constraint_id, enabled) in &config.constraint_overrides {
            if *enabled {
                constraint_engine.enable_constraint(constraint_id);
            } else {
                constraint_engine.disable_constraint(constraint_id);
            }
        }

        let proactive_constraints_prompt = {
            let prompt = constraint_engine.constraints_prompt();
            if !prompt.is_empty() {
                Some(prompt)
            } else {
                None
            }
        };

        Self {
            iteration: config.starting_iteration,
            iteration_results: Vec::new(),
            verification_failures: 0,
            convergence_detector: ConvergenceDetector::new(ConvergenceConfig::default()),
            constraint_engine,
            pending_constraint_context: None,
            proactive_constraints_prompt,
            constraints_prompt_injected: false,
            resource_tracker,
            dynamic_steps: initial_dynamic_steps,
            pending_health_regression: None,
            compensation_manager: CompensationManager::new(checkpoint_db),
            accumulated_diffs: Vec::new(),
            initial_source_commit: None,
            current_passed_checks: 0,
            current_failed_checks: 0,
            commit_before: None,
            pre_agentic_health: None,
            agentic_phase_start: None,
            reflection_was_forced: false,
        }
    }

    /// Merge static verification steps with any dynamically injected steps.
    pub fn effective_verification_steps(
        &self,
        static_steps: &[ExecutionStepConfig],
    ) -> Vec<ExecutionStepConfig> {
        if self.dynamic_steps.is_empty() {
            static_steps.to_vec()
        } else {
            let mut combined = static_steps.to_vec();
            combined.extend(self.dynamic_steps.iter().cloned());
            combined
        }
    }
}

impl CompletionReason {
    /// Build a LoopResult from the completion reason and context.
    pub fn into_loop_result(self, ctx: &LoopContext) -> LoopResult {
        let (
            verification_passed,
            was_stopped,
            max_iterations_reached,
            critical_failure,
            unfixable_errors,
        ) = match &self {
            CompletionReason::VerificationPassed => (true, false, false, false, false),
            CompletionReason::MaxIterationsReached => (false, false, true, false, true),
            CompletionReason::MaxSessionsReached => (false, false, true, false, true),
            CompletionReason::CriticalFailure { .. } => (false, false, false, true, false),
            CompletionReason::Stopped => (false, true, false, false, false),
            CompletionReason::UnfixableErrors => (false, false, false, false, true),
            CompletionReason::ApprovalAborted => (false, true, false, false, false),
        };

        LoopResult {
            verification_passed,
            iterations_run: ctx.iteration,
            iteration_results: ctx.iteration_results.clone(),
            was_stopped,
            max_iterations_reached,
            critical_failure,
            unfixable_errors,
            total_tokens: None,
            total_cost_usd: None,
            files_modified: ctx.resource_tracker.files_modified(),
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_completion_reason_verification_passed() {
        let ctx = make_test_context(3);
        let result = CompletionReason::VerificationPassed.into_loop_result(&ctx);
        assert!(result.verification_passed);
        assert!(!result.was_stopped);
        assert!(!result.max_iterations_reached);
        assert!(!result.critical_failure);
        assert!(!result.unfixable_errors);
        assert_eq!(result.iterations_run, 3);
    }

    #[test]
    fn test_completion_reason_max_iterations() {
        let ctx = make_test_context(5);
        let result = CompletionReason::MaxIterationsReached.into_loop_result(&ctx);
        assert!(!result.verification_passed);
        assert!(result.max_iterations_reached);
        assert!(result.unfixable_errors);
        assert_eq!(result.iterations_run, 5);
    }

    #[test]
    fn test_completion_reason_stopped() {
        let ctx = make_test_context(2);
        let result = CompletionReason::Stopped.into_loop_result(&ctx);
        assert!(!result.verification_passed);
        assert!(result.was_stopped);
        assert_eq!(result.iterations_run, 2);
    }

    #[test]
    fn test_completion_reason_critical_failure() {
        let ctx = make_test_context(1);
        let result = CompletionReason::CriticalFailure {
            error: "infra down".to_string(),
        }
        .into_loop_result(&ctx);
        assert!(!result.verification_passed);
        assert!(result.critical_failure);
        assert_eq!(result.iterations_run, 1);
    }

    #[test]
    fn test_completion_reason_unfixable() {
        let ctx = make_test_context(4);
        let result = CompletionReason::UnfixableErrors.into_loop_result(&ctx);
        assert!(!result.verification_passed);
        assert!(result.unfixable_errors);
    }

    #[test]
    fn test_completion_reason_approval_aborted() {
        let ctx = make_test_context(2);
        let result = CompletionReason::ApprovalAborted.into_loop_result(&ctx);
        assert!(!result.verification_passed);
        assert!(result.was_stopped);
    }

    /// Helper: build a minimal LoopContext with a given iteration count.
    fn make_test_context(iteration: u32) -> LoopContext {
        LoopContext {
            iteration,
            iteration_results: Vec::new(),
            verification_failures: 0,
            convergence_detector: ConvergenceDetector::new(ConvergenceConfig::default()),
            constraint_engine: crate::constraint_engine::ConstraintEngine::new(),
            pending_constraint_context: None,
            proactive_constraints_prompt: None,
            constraints_prompt_injected: false,
            resource_tracker: crate::unified_workflow_executor::convergence::ResourceTracker::new(
                Default::default(),
                Instant::now(),
            ),
            dynamic_steps: Vec::new(),
            pending_health_regression: None,
            compensation_manager: CompensationManager::new(std::sync::Arc::new(
                crate::database::CheckpointDb::new_in_memory().unwrap(),
            )),
            accumulated_diffs: Vec::new(),
            initial_source_commit: None,
            current_passed_checks: 0,
            current_failed_checks: 0,
            commit_before: None,
            pre_agentic_health: None,
            agentic_phase_start: None,
            reflection_was_forced: false,
        }
    }
}
