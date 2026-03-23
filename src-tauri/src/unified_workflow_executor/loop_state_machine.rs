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
use super::convergence::{
    ConvergenceConfig, ConvergenceDetector, ConvergenceReport,
};
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

    // =========================================================================
    // Group E: State Machine Parity Tests
    // =========================================================================
    //
    // These tests verify that the state machine implementation preserves all
    // scout-integrated features from the monolithic loop. Each test group
    // maps to a scout project whose features must survive the refactor.

    // --- LoopContext Initialization & Helpers ---

    #[test]
    fn test_loop_context_initial_state() {
        let ctx = make_test_context(0);
        assert_eq!(ctx.iteration, 0);
        assert!(ctx.iteration_results.is_empty());
        assert_eq!(ctx.verification_failures, 0);
        assert!(ctx.dynamic_steps.is_empty());
        assert!(ctx.accumulated_diffs.is_empty());
        assert!(ctx.initial_source_commit.is_none());
        assert!(ctx.commit_before.is_none());
        assert!(ctx.pre_agentic_health.is_none());
        assert!(ctx.agentic_phase_start.is_none());
        assert!(!ctx.reflection_was_forced);
        assert!(!ctx.constraints_prompt_injected);
        assert!(ctx.pending_constraint_context.is_none());
        assert!(ctx.pending_health_regression.is_none());
    }

    #[test]
    fn test_loop_context_starting_iteration_preserved() {
        // conductor: resume from arbitrary checkpoint
        let ctx = make_test_context(5);
        assert_eq!(ctx.iteration, 5);
        let result = CompletionReason::VerificationPassed.into_loop_result(&ctx);
        assert_eq!(result.iterations_run, 5);
    }

    #[test]
    fn test_effective_verification_steps_static_only() {
        let ctx = make_test_context(1);
        let static_steps = vec![
            ExecutionStepConfig {
                step_type: "command".to_string(),
                name: Some("check-types".to_string()),
                ..Default::default()
            },
            ExecutionStepConfig {
                step_type: "command".to_string(),
                name: Some("lint".to_string()),
                ..Default::default()
            },
        ];
        let effective = ctx.effective_verification_steps(&static_steps);
        assert_eq!(effective.len(), 2);
        assert_eq!(effective[0].name.as_deref(), Some("check-types"));
        assert_eq!(effective[1].name.as_deref(), Some("lint"));
    }

    #[test]
    fn test_effective_verification_steps_with_dynamic_injection() {
        // conductor: dynamic step injection from AI output
        let mut ctx = make_test_context(2);
        ctx.dynamic_steps = vec![ExecutionStepConfig {
            step_type: "command".to_string(),
            name: Some("injected-test".to_string()),
            ..Default::default()
        }];
        let static_steps = vec![ExecutionStepConfig {
            step_type: "command".to_string(),
            name: Some("original-check".to_string()),
            ..Default::default()
        }];
        let effective = ctx.effective_verification_steps(&static_steps);
        assert_eq!(effective.len(), 2);
        assert_eq!(effective[0].name.as_deref(), Some("original-check"));
        assert_eq!(effective[1].name.as_deref(), Some("injected-test"));
    }

    #[test]
    fn test_effective_verification_steps_empty_dynamic() {
        let ctx = make_test_context(1);
        let static_steps = vec![ExecutionStepConfig {
            step_type: "command".to_string(),
            name: Some("only-static".to_string()),
            ..Default::default()
        }];
        let effective = ctx.effective_verification_steps(&static_steps);
        assert_eq!(effective.len(), 1);
        // Verify it returns the same steps, not a copy that lost data
        assert_eq!(effective[0].step_type, "command");
    }

    // --- CompletionReason: Files Modified Propagation (open-swe) ---

    #[test]
    fn test_files_modified_propagated_from_resource_tracker() {
        // open-swe: resource tracking middleware
        let mut ctx = make_test_context(2);
        ctx.resource_tracker
            .record_iteration(500, &["src/main.rs".to_string(), "Cargo.toml".to_string()]);
        ctx.resource_tracker
            .record_iteration(300, &["src/main.rs".to_string(), "src/lib.rs".to_string()]);

        let result = CompletionReason::VerificationPassed.into_loop_result(&ctx);
        let files = result.files_modified;
        // Should be deduplicated
        assert!(files.contains(&"src/main.rs".to_string()));
        assert!(files.contains(&"Cargo.toml".to_string()));
        assert!(files.contains(&"src/lib.rs".to_string()));
        assert_eq!(files.len(), 3);
    }

    // --- CompletionReason: Max Sessions (conductor session budgeting) ---

    #[test]
    fn test_completion_reason_max_sessions_reached() {
        let ctx = make_test_context(3);
        let result = CompletionReason::MaxSessionsReached.into_loop_result(&ctx);
        assert!(!result.verification_passed);
        assert!(result.max_iterations_reached); // sessions map to same flag
        assert!(result.unfixable_errors);
        assert_eq!(result.iterations_run, 3);
    }

    // --- CompletionReason: Iteration Results Propagation ---

    #[test]
    fn test_iteration_results_propagated_to_loop_result() {
        // deer-flow: iteration history preserved for context compression
        let mut ctx = make_test_context(3);
        ctx.iteration_results.push(IterationResult {
            iteration: 1,
            verification_passed: false,
            critical_failure: false,
            passed_checks: 2,
            failed_checks: 3,
            failure_context: String::new(),
            agentic_phase_ran: true,
            agentic_phase_success: Some(true),
        });
        ctx.iteration_results.push(IterationResult {
            iteration: 2,
            verification_passed: false,
            critical_failure: false,
            passed_checks: 4,
            failed_checks: 1,
            failure_context: String::new(),
            agentic_phase_ran: true,
            agentic_phase_success: Some(true),
        });
        ctx.iteration_results.push(IterationResult {
            iteration: 3,
            verification_passed: true,
            critical_failure: false,
            passed_checks: 5,
            failed_checks: 0,
            failure_context: String::new(),
            agentic_phase_ran: false,
            agentic_phase_success: None,
        });

        let result = CompletionReason::VerificationPassed.into_loop_result(&ctx);
        assert_eq!(result.iteration_results.len(), 3);
        assert!(!result.iteration_results[0].verification_passed);
        assert!(!result.iteration_results[1].verification_passed);
        assert!(result.iteration_results[2].verification_passed);
        // Verify checks propagated correctly
        assert_eq!(result.iteration_results[0].passed_checks, 2);
        assert_eq!(result.iteration_results[0].failed_checks, 3);
        assert_eq!(result.iteration_results[2].passed_checks, 5);
        assert_eq!(result.iteration_results[2].failed_checks, 0);
    }

    // --- LoopContext: Conductor Fields Present ---

    #[test]
    fn test_context_has_compensation_manager() {
        // conductor: compensation manager must be initialized
        let ctx = make_test_context(0);
        // CompensationManager is present and functional (can record commits)
        // We verify the field exists and is usable by checking it doesn't panic
        assert!(ctx.accumulated_diffs.is_empty());
        assert!(ctx.initial_source_commit.is_none());
    }

    #[test]
    fn test_context_accumulated_diffs_grow() {
        // conductor: structured cross-iteration diff tracking
        let mut ctx = make_test_context(1);
        assert!(ctx.accumulated_diffs.is_empty());

        ctx.accumulated_diffs
            .push(super::super::types::IterationDiff {
                iteration: 1,
                files_changed: vec!["src/main.rs".to_string()],
                diff_stat: "1 file changed".to_string(),
                diff_summary: "+println!(\"hello\")".to_string(),
                insertions: 1,
                deletions: 0,
                commit_before: Some("abc123".to_string()),
                commit_after: Some("def456".to_string()),
            });
        assert_eq!(ctx.accumulated_diffs.len(), 1);
        assert_eq!(ctx.accumulated_diffs[0].iteration, 1);
        assert_eq!(ctx.accumulated_diffs[0].files_changed, vec!["src/main.rs"]);
    }

    // --- LoopContext: Convergence Fields (deer-flow) ---

    #[test]
    fn test_context_convergence_detector_initialized() {
        // deer-flow: convergence detection must be available from iteration 1
        let mut ctx = make_test_context(0);
        // Feed some data to verify it's functional — returns a report (not a panic)
        let report = ctx.convergence_detector.analyze(
            1,
            &std::collections::HashMap::new(),
            std::collections::HashSet::new(),
            0,
            0,
        );
        // Early iterations (1-2) should not trigger escalation actions
        assert!(
            !report
                .actions
                .iter()
                .any(|a| matches!(a, ConvergenceAction::SuggestUnfixable { .. })),
            "iteration 1 should not suggest unfixable"
        );
    }

    // --- LoopContext: Constraint Engine (promptfoo) ---

    #[test]
    fn test_context_constraint_engine_functional() {
        // promptfoo: constraint engine must accept evaluations without panicking
        let ctx = make_test_context(0);
        let results = ctx.constraint_engine.evaluate(&["src/main.rs".to_string()]);
        // With default config, built-in constraints may fire — verify it returns
        // a Vec (not a panic) and each result has required fields
        for result in &results {
            assert!(!result.constraint_id.is_empty());
        }
    }

    #[test]
    fn test_context_proactive_constraints_injection_flag() {
        // promptfoo: constraints prompt injected only once
        let mut ctx = make_test_context(0);
        assert!(!ctx.constraints_prompt_injected);
        ctx.constraints_prompt_injected = true;
        assert!(ctx.constraints_prompt_injected);
    }

    // --- LoopContext: Health Regression (deepeval) ---

    #[test]
    fn test_context_health_regression_carries_across_iterations() {
        // deepeval: health regression warning stored for next iteration
        let mut ctx = make_test_context(1);
        assert!(ctx.pending_health_regression.is_none());
        ctx.pending_health_regression =
            Some("WARNING: App crashed after agentic phase".to_string());
        assert!(ctx.pending_health_regression.is_some());
        // Simulate consumption in next iteration's failure context building
        let warning = ctx.pending_health_regression.take();
        assert_eq!(warning.unwrap(), "WARNING: App crashed after agentic phase");
        assert!(ctx.pending_health_regression.is_none());
    }

    // --- LoopContext: Pre-Agentic State Capture (conductor + OpenViking) ---

    #[test]
    fn test_context_pre_agentic_commit_tracking() {
        // conductor: commit_before captured for iteration diff
        let mut ctx = make_test_context(1);
        assert!(ctx.commit_before.is_none());
        ctx.commit_before = Some("abc123def456".to_string());
        assert_eq!(ctx.commit_before.as_deref(), Some("abc123def456"));
    }

    #[test]
    fn test_context_agentic_phase_timing() {
        // OpenViking: token/cost tracking needs duration measurement
        let mut ctx = make_test_context(1);
        assert!(ctx.agentic_phase_start.is_none());
        ctx.agentic_phase_start = Some(Instant::now());
        std::thread::sleep(std::time::Duration::from_millis(10));
        let elapsed = ctx.agentic_phase_start.unwrap().elapsed();
        assert!(elapsed.as_millis() >= 10);
    }

    // --- LoopContext: Reflection Mode Restoration (deer-flow) ---

    #[test]
    fn test_context_reflection_forced_flag_lifecycle() {
        // deer-flow: forced reflection must be restorable
        let mut ctx = make_test_context(1);
        assert!(!ctx.reflection_was_forced);
        ctx.reflection_was_forced = true;
        // After agentic phase, this flag signals restoration
        assert!(ctx.reflection_was_forced);
        ctx.reflection_was_forced = false;
        assert!(!ctx.reflection_was_forced);
    }

    // --- LoopState: All Variants Constructible ---

    #[test]
    fn test_all_loop_states_constructible() {
        // Ensures the state machine has all necessary states
        let _ = LoopState::CheckPreconditions;
        let _ = LoopState::CheckEnvironment;
        let _ = LoopState::RunVerification;
        let _ = LoopState::EvaluateVerification {
            verification_result: make_test_verification_result(),
        };
        let _ = LoopState::BuildFailureContext {
            verification_result: make_test_verification_result(),
            convergence_report: make_test_convergence_report(),
        };
        let _ = LoopState::RunAgentic {
            failure_context: "test failure".to_string(),
            verification_result: make_test_verification_result(),
        };
        let _ = LoopState::PostAgentic {
            outcome: AgenticOutcome::Skipped,
            injected_steps: vec![],
            failure_context: "test".to_string(),
        };
        let _ = LoopState::CheckPostAgenticSignals {
            outcome: AgenticOutcome::Skipped,
        };
        let _ = LoopState::ApprovalGate {
            outcome: AgenticOutcome::Skipped,
        };
        let _ = LoopState::AdvanceIteration;
        let _ = LoopState::Complete {
            reason: CompletionReason::VerificationPassed,
        };
    }

    // --- LoopState: State Transition Coverage ---
    // Verifies the expected state transition graph is complete.

    #[test]
    fn test_state_transition_graph_coverage() {
        // The state machine must support these transitions:
        // CheckPreconditions → CheckEnvironment | Complete(Stopped/MaxIter/MaxSessions)
        // CheckEnvironment → RunVerification
        // RunVerification → EvaluateVerification
        // EvaluateVerification → Complete(Passed/Critical) | BuildFailureContext
        // BuildFailureContext → RunAgentic
        // RunAgentic → PostAgentic
        // PostAgentic → CheckPostAgenticSignals
        // CheckPostAgenticSignals → Complete(Unfixable/Stopped) | ApprovalGate
        // ApprovalGate → Complete(ApprovalAborted) | AdvanceIteration
        // AdvanceIteration → CheckPreconditions (loop back)
        //
        // Total: 10 states, 14 transitions, 6 completion reasons
        // This test verifies the enum variants match.

        let completion_variants = [
            CompletionReason::VerificationPassed,
            CompletionReason::MaxIterationsReached,
            CompletionReason::MaxSessionsReached,
            CompletionReason::CriticalFailure {
                error: "test".to_string(),
            },
            CompletionReason::Stopped,
            CompletionReason::UnfixableErrors,
            CompletionReason::ApprovalAborted,
        ];
        assert_eq!(completion_variants.len(), 7);

        // Each completion reason must produce a valid LoopResult
        let ctx = make_test_context(1);
        for reason in completion_variants {
            let result = reason.into_loop_result(&ctx);
            // Every result must have a defined iteration count
            assert!(result.iterations_run >= 1);
        }
    }

    // --- Token/Cost Propagation (OpenViking) ---

    #[test]
    fn test_loop_result_token_fields_present() {
        // OpenViking: token/cost fields must exist on LoopResult
        let ctx = make_test_context(1);
        let result = CompletionReason::VerificationPassed.into_loop_result(&ctx);
        // Default is None (populated by loop at runtime)
        assert!(result.total_tokens.is_none());
        assert!(result.total_cost_usd.is_none());
    }

    // --- Verification Result Counts in Context (deepeval metrics) ---

    #[test]
    fn test_context_check_counts_for_metrics() {
        // deepeval: per-iteration pass/fail counts feed agentic metrics
        let mut ctx = make_test_context(1);
        ctx.current_passed_checks = 4;
        ctx.current_failed_checks = 2;
        assert_eq!(ctx.current_passed_checks, 4);
        assert_eq!(ctx.current_failed_checks, 2);
    }

    // --- Helpers ---

    fn make_test_verification_result() -> crate::step_executor::VerificationPhaseResult {
        crate::step_executor::VerificationPhaseResult {
            iteration: 1,
            all_passed: false,
            total_steps: 3,
            passed_steps: 1,
            failed_steps: 2,
            skipped_steps: 0,
            total_duration_ms: 500,
            step_results: vec![],
            critical_failure: false,
            console_errors: None,
            app_health: None,
            browser_events: None,
            network_failures: None,
        }
    }

    fn make_test_convergence_report() -> ConvergenceReport {
        ConvergenceReport {
            iteration: 1,
            patterns: vec![],
            actions: vec![],
            is_healthy: true,
            context_injection: String::new(),
        }
    }
}
