//! The `EvaluateVerification` state of the verification-agentic loop.
//!
//! [`handle_evaluate_verification`] turns one iteration's verification result
//! into the loop's next state. Every side effect it has — the output log the
//! summary AI reads, the Recap store and its web sync, the replay snapshots,
//! the canvas panel, the workflow state row, the dashboard metrics and the
//! knowledge resolution — goes through [`EvaluationEffects`], whose production
//! implementation is [`LoopController`]. That seam is what lets the handler
//! itself, rather than a copy of its branching, run in a unit test against a
//! recording fake: a `LoopController` needs a full `AppState` and a live Tauri
//! `AppHandle`, neither of which a unit test can build.

use std::collections::HashMap;

use tracing::{error, info, warn};

use crate::event_system::EventBroadcaster;
use crate::step_executor::VerificationPhaseResult;
use crate::str_utils::truncate_str;

use super::convergence::{ConvergenceAction, ConvergencePattern};
use super::loop_controller::LoopController;
use super::loop_handlers_decisions::{
    decide_evaluate_verification, required_step_count, FixProgressUpdate, VerificationEvalDecision,
    VerificationEvalSnapshot,
};
use super::loop_state_machine::{CompletionReason, LoopContext, LoopState};
use super::states::UnifiedWorkflowState;
use super::types::{get_parent_task_id, IterationResult, LoopConfig};

/// One iteration's convergence metrics, as the dashboard receives them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IterationMetrics {
    pub failed: u32,
    pub passed: u32,
    pub skipped: u32,
    pub new_failures: u32,
    pub repeated_failures: u32,
    pub is_stalled: bool,
}

/// The side effects of [`handle_evaluate_verification`].
///
/// Each method is one effect the handler had inline. None of them decides
/// anything: the branching stays in the handler, which is the point of the
/// seam.
#[async_trait::async_trait]
pub(crate) trait EvaluationEffects: Send + Sync {
    /// Append text to the run's output log, which the summary AI reads.
    async fn append_output(&self, execution_id: &str, text: &str);

    /// Store one iteration's verification result for the Recap page, then
    /// sync it to the web backend (best-effort, non-blocking).
    async fn store_verification_result(
        &self,
        parent_id: &str,
        iteration: u32,
        result_json: &serde_json::Value,
    );

    /// Record a replay snapshot of `kind`.
    async fn record_state_snapshot(
        &self,
        execution_id: &str,
        kind: &str,
        summary: &str,
        context: Option<&str>,
    );

    /// Emit the canvas panel for a completed verification.
    async fn on_verification_complete(
        &self,
        iteration: u32,
        result: &VerificationPhaseResult,
        history: &HashMap<String, Vec<bool>>,
    );

    /// Persist the workflow state row.
    fn persist_state(&self, execution_id: &str, state: &UnifiedWorkflowState);

    /// Emit the iteration's convergence metrics for the dashboard.
    fn emit_iteration_metrics(&self, execution_id: &str, iteration: u32, metrics: IterationMetrics);

    /// Resolve every unresolved knowledge entry of the run.
    async fn resolve_open_knowledge(&self, parent_id: &str);
}

#[async_trait::async_trait]
impl EvaluationEffects for LoopController {
    async fn append_output(&self, execution_id: &str, text: &str) {
        let _ = self
            .app_state
            .pg_db
            .append_task_output_ex(execution_id, text, false, false)
            .await;
    }

    async fn store_verification_result(
        &self,
        parent_id: &str,
        iteration: u32,
        result_json: &serde_json::Value,
    ) {
        let _ = self
            .app_state
            .pg_db
            .store_verification_phase_result(parent_id, iteration, result_json)
            .await;

        // Sync to web backend (best-effort, non-blocking)
        let parent_id = parent_id.to_string();
        let result_json = result_json.clone();
        tokio::spawn(async move {
            let sync_service = crate::commands::task_sync::AITaskSyncService::new();
            if let Err(e) = sync_service
                .sync_verification_result(&parent_id, iteration, &result_json)
                .await
            {
                warn!("Failed to sync verification result to backend: {}", e);
            }
        });
    }

    async fn record_state_snapshot(
        &self,
        execution_id: &str,
        kind: &str,
        summary: &str,
        context: Option<&str>,
    ) {
        let ts = chrono::Utc::now().to_rfc3339();
        let _ = self
            .app_state
            .pg_db
            .record_state_snapshot(execution_id, "", &ts, kind, Some(summary), context)
            .await;
    }

    async fn on_verification_complete(
        &self,
        iteration: u32,
        result: &VerificationPhaseResult,
        history: &HashMap<String, Vec<bool>>,
    ) {
        self.canvas_manager
            .lock()
            .await
            .on_verification_complete(iteration, result, history)
            .await;
    }

    fn persist_state(&self, execution_id: &str, state: &UnifiedWorkflowState) {
        self.persist_workflow_state(execution_id, state);
    }

    fn emit_iteration_metrics(
        &self,
        execution_id: &str,
        iteration: u32,
        metrics: IterationMetrics,
    ) {
        EventBroadcaster::new(self.app_handle.clone()).iteration_metrics(
            execution_id,
            iteration,
            metrics.failed,
            metrics.passed,
            metrics.skipped,
            metrics.new_failures,
            metrics.repeated_failures,
            metrics.is_stalled,
        );
    }

    async fn resolve_open_knowledge(&self, parent_id: &str) {
        match self.knowledge_base.get_all_knowledge(parent_id).await {
            Ok(entries) => {
                let unresolved: Vec<_> = entries.iter().filter(|k| !k.is_resolved).collect();
                if !unresolved.is_empty() {
                    info!(
                        "Resolving {} unresolved knowledge entries after verification pass",
                        unresolved.len()
                    );
                    for entry in &unresolved {
                        if let Err(e) = self
                            .knowledge_base
                            .resolve_finding(&entry.id, Some("Resolved: verification passed"))
                            .await
                        {
                            warn!("Failed to resolve knowledge entry {}: {}", entry.id, e);
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Failed to fetch knowledge entries for resolution: {}", e);
            }
        }
    }
}

// =========================================================================
// Handler: EvaluateVerification
// =========================================================================

/// The iteration-result context recorded when nothing was checked.
const NOTHING_CHECKED_CONTEXT: &str = "No required verification step produced a result, so \
nothing was checked and the run is not verified. The verification step list was empty, \
every step was filtered out of this build, or every step was `required: false`.";

/// The verdict word the output log, the replay snapshot and the logs use.
fn verdict_label(decision: &VerificationEvalDecision) -> &'static str {
    match decision {
        VerificationEvalDecision::NoVerificationSteps => {
            "NOTHING CHECKED (no required verification step)"
        }
        VerificationEvalDecision::VerificationPassed => "PASSED",
        VerificationEvalDecision::CriticalFailure => "CRITICAL FAILURE",
        VerificationEvalDecision::FixEscalation { .. }
        | VerificationEvalDecision::ContinueToFix { .. } => "FAILED",
    }
}

/// Apply the decision's fix-progress bookkeeping to the loop context.
fn apply_fix_progress(
    ctx: &mut LoopContext,
    config: &LoopConfig,
    progress: &FixProgressUpdate,
    passed_steps: usize,
) {
    match progress {
        FixProgressUpdate::Reset { new_best } => {
            ctx.best_passed_checks = *new_best;
            ctx.fix_attempts = 0;
            ctx.last_progress_iteration = ctx.iteration;
            info!(
                "Fix loop: progress detected (passed {} > previous best), reset fix_attempts",
                new_best,
            );
        }
        FixProgressUpdate::Increment => {
            ctx.fix_attempts = ctx.fix_attempts.saturating_add(1);
            info!(
                "Fix loop: no progress (passed {} <= best {}), fix_attempts = {}/{}",
                passed_steps, ctx.best_passed_checks, ctx.fix_attempts, config.max_fix_attempts
            );
        }
    }
}

/// Convergence analysis, resource limits, then the verdict:
/// `decide_evaluate_verification` picks the next state, and this handler
/// carries it out (knowledge resolution on a pass, fix-progress bookkeeping on
/// a failure).
///
/// The decision is made FIRST, from a snapshot. Every field it reads
/// (`best_passed_checks`, `fix_attempts`, `max_fix_attempts` and the result's
/// counts) stays fixed until the arm it picks writes it, so every effect below
/// reports the verdict the loop acts on. A verification with no required step
/// reports `NOTHING CHECKED` rather than `FAILED` or, as the inline branching
/// that preceded this did, `PASSED`.
pub(crate) async fn handle_evaluate_verification(
    effects: &dyn EvaluationEffects,
    ctx: &mut LoopContext,
    config: &mut LoopConfig,
    verification_result: VerificationPhaseResult,
) -> LoopState {
    // ---- Decide -------------------------------------------------------------
    let required_steps = required_step_count(&verification_result.step_results);
    let decision = decide_evaluate_verification(&VerificationEvalSnapshot {
        all_passed: verification_result.all_passed,
        required_steps,
        critical_failure: verification_result.critical_failure,
        passed_steps: verification_result.passed_steps,
        best_passed_checks: ctx.best_passed_checks,
        fix_attempts: ctx.fix_attempts,
        max_fix_attempts: config.max_fix_attempts,
    });
    let passed = decision == VerificationEvalDecision::VerificationPassed;
    let status = verdict_label(&decision);

    // Log verification results to output_log so the summary AI has context
    {
        let summary_line = format!(
            "\n--- Verification (Iteration {}): {} ({} passed, {} failed, {} total, {} required) ---\n",
            ctx.iteration,
            status,
            verification_result.passed_steps,
            verification_result.failed_steps,
            verification_result.total_steps,
            required_steps,
        );

        // Include brief details of failed steps
        let mut details = String::new();
        for sr in &verification_result.step_results {
            if !sr.success {
                let err = sr.error.as_deref().unwrap_or("unknown error");
                // Truncate long error messages
                let truncated = truncate_str(err, 200);
                details.push_str(&format!("  - {} [FAILED]: {}\n", sr.step_name, truncated));
            }
        }

        effects
            .append_output(
                &config.execution_id,
                &format!("{}{}", summary_line, details),
            )
            .await;
    }

    // Store verification result in database for Recap page
    // Use parent task ID for workflow sequences (same remapping as step checkpoints)
    if let Ok(result_json) = serde_json::to_value(&verification_result) {
        let parent_id = get_parent_task_id(&config.execution_id);
        effects
            .store_verification_result(&parent_id, ctx.iteration, &result_json)
            .await;
    }

    // Emit state snapshot for replay
    {
        let snapshot_summary = format!(
            "Verification iteration {} {}: {}/{} passed",
            ctx.iteration,
            status,
            verification_result.passed_steps,
            verification_result.total_steps,
        );
        let context = serde_json::json!({
            "type": "verification_result",
            "iteration": ctx.iteration,
            "all_passed": verification_result.all_passed,
            "passed_steps": verification_result.passed_steps,
            "failed_steps": verification_result.failed_steps,
            "total_steps": verification_result.total_steps,
            "required_steps": required_steps,
            "critical_failure": verification_result.critical_failure,
        });
        effects
            .record_state_snapshot(
                &config.execution_id,
                "verification_result",
                &snapshot_summary,
                Some(&context.to_string()),
            )
            .await;
    }

    // Emit canvas panel for verification completion
    effects
        .on_verification_complete(
            ctx.iteration,
            &verification_result,
            &config.verification_history,
        )
        .await;

    // Persist workflow state: VerificationComplete. `passed` is the decision's,
    // not the producer's `all_passed`, which is vacuously true over a set with
    // no required step.
    effects.persist_state(
        &config.execution_id,
        &UnifiedWorkflowState::verification_complete(
            ctx.iteration,
            passed,
            config.stage_index,
            ctx.prior_stages_passed,
        ),
    );
    effects
        .record_state_snapshot(
            &config.execution_id,
            "phase_transition",
            &format!(
                "Entered verification_complete phase, iteration {}",
                ctx.iteration
            ),
            None,
        )
        .await;

    // Run convergence detector and emit metrics for the frontend dashboard
    let convergence_report = {
        let failed_count = verification_result.failed_steps as u32;
        let passed_count = verification_result.passed_steps as u32;
        let skipped_count = verification_result.skipped_steps as u32;

        // Compute current failed step names
        let current_failed_names: std::collections::HashSet<String> = verification_result
            .step_results
            .iter()
            .filter(|sr| !sr.success)
            .map(|sr| sr.step_name.clone())
            .collect();

        // Run the convergence detector
        let report = ctx.convergence_detector.analyze(
            ctx.iteration,
            &config.verification_history,
            current_failed_names.clone(),
            failed_count,
            passed_count,
        );

        // Derive new/repeated from the detector's snapshot history for
        // backward-compatible metrics emission.
        let new_failures = current_failed_names.len() as u32;
        let repeated_failures = 0u32;
        let is_stalled = !report.is_healthy;

        effects.emit_iteration_metrics(
            &config.execution_id,
            ctx.iteration,
            IterationMetrics {
                failed: failed_count,
                passed: passed_count,
                skipped: skipped_count,
                new_failures,
                repeated_failures,
                is_stalled,
            },
        );

        // Log convergence status
        if report.has_concerns() {
            let pattern_names: Vec<&str> = report
                .patterns
                .iter()
                .map(|p| match p {
                    ConvergencePattern::Stuck { .. } => "stuck",
                    ConvergencePattern::Diverging { .. } => "diverging",
                    ConvergencePattern::Oscillating { .. } => "oscillating",
                    ConvergencePattern::Plateau { .. } => "plateau",
                    ConvergencePattern::Converging => "converging",
                })
                .collect();
            warn!(
                "CONVERGENCE: iteration={}, failed={}, passed={}, patterns={:?}, actions={}",
                ctx.iteration,
                failed_count,
                passed_count,
                pattern_names,
                report.actions.len(),
            );
        } else {
            info!(
                "CONVERGENCE: iteration={}, failed={}, passed={}, healthy=true",
                ctx.iteration, failed_count, passed_count,
            );
        }

        // Increment cumulative verification failures for routing context
        if !passed {
            ctx.verification_failures += 1;
        }

        // If escalation was recommended, bump verification_failures to trigger
        // routing rules that use the verification_failures threshold
        if report.should_escalate_model() {
            info!(
                "CONVERGENCE-ESCALATE: Bumping verification_failures for routing (was {})",
                ctx.verification_failures
            );
            // Ensure it's at least 3 so routing rules like
            // "verification_failures >= 3 → use opus" fire
            if ctx.verification_failures < 3 {
                ctx.verification_failures = 3;
            }
        }

        // Check resource limits and merge any actions into the report.
        // Resource actions use the same ConvergenceAction type so the loop
        // controller handles them uniformly with convergence pattern actions.
        let resource_actions = ctx.resource_tracker.check_limits(ctx.iteration);
        if !resource_actions.is_empty() {
            // Merge resource actions into the report
            let mut merged = report;
            merged.is_healthy = false;
            for action in &resource_actions {
                if let ConvergenceAction::InjectContext { context, .. } = action {
                    if merged.context_injection.is_empty() {
                        merged
                            .context_injection
                            .push_str("\n## Resource Constraints\n\n");
                    }
                    merged.context_injection.push_str(context);
                    merged.context_injection.push('\n');
                }
            }
            merged.actions.extend(resource_actions);
            merged
        } else {
            report
        }
    };

    // Store current iteration's verification counts for PostAgentic
    ctx.current_passed_checks = verification_result.passed_steps;
    ctx.current_failed_checks = verification_result.failed_steps;

    // ---- Act on the decision ------------------------------------------------
    match decision {
        VerificationEvalDecision::NoVerificationSteps => {
            warn!(
                "*** NOTHING CHECKED on iteration {}: {} step result(s), none required - the run is not verified ***",
                ctx.iteration,
                verification_result.step_results.len()
            );
            // No knowledge is resolved here: a run that checked nothing has
            // shown nothing fixed.
            ctx.iteration_results.push(IterationResult {
                iteration: ctx.iteration,
                verification_passed: false,
                critical_failure: false,
                passed_checks: verification_result.passed_steps,
                failed_checks: verification_result.failed_steps,
                failure_context: NOTHING_CHECKED_CONTEXT.to_string(),
                agentic_phase_ran: false,
                agentic_phase_success: None,
                blame_json: None,
                contingent_on: Vec::new(),
                node_rollups: Vec::new(),
            });

            LoopState::Complete {
                reason: CompletionReason::NoVerificationSteps,
            }
        }

        VerificationEvalDecision::VerificationPassed => {
            info!("*** VERIFICATION PASSED on iteration {} ***", ctx.iteration);

            // Resolve all unresolved knowledge entries — the issues they describe
            // have been addressed since verification now passes. On this arm
            // ONLY: every other verdict has shown nothing resolved.
            effects
                .resolve_open_knowledge(&get_parent_task_id(&config.execution_id))
                .await;

            ctx.iteration_results.push(IterationResult {
                iteration: ctx.iteration,
                verification_passed: true,
                critical_failure: false,
                passed_checks: verification_result.passed_steps,
                failed_checks: verification_result.failed_steps,
                failure_context: String::new(),
                agentic_phase_ran: false,
                agentic_phase_success: None,
                blame_json: None,
                contingent_on: Vec::new(),
                node_rollups: Vec::new(),
            });

            LoopState::Complete {
                reason: CompletionReason::VerificationPassed,
            }
        }

        VerificationEvalDecision::CriticalFailure => {
            error!(
                "*** CRITICAL FAILURE on iteration {} - stopping loop ***",
                ctx.iteration
            );

            ctx.iteration_results.push(IterationResult {
                iteration: ctx.iteration,
                verification_passed: false,
                critical_failure: true,
                passed_checks: verification_result.passed_steps,
                failed_checks: verification_result.failed_steps,
                failure_context: verification_result.build_failure_context(),
                agentic_phase_ran: false,
                agentic_phase_success: None,
                blame_json: None,
                contingent_on: Vec::new(),
                node_rollups: Vec::new(),
            });

            LoopState::Complete {
                reason: CompletionReason::CriticalFailure {
                    error: verification_result.build_failure_context(),
                },
            }
        }

        // --- Fix attempt tracking (Phase 1: Bounded Fix Loop) ---
        VerificationEvalDecision::FixEscalation { progress } => {
            apply_fix_progress(ctx, config, &progress, verification_result.passed_steps);
            warn!(
                "Fix loop: {} consecutive non-improving iterations (max {}), escalating",
                ctx.fix_attempts, config.max_fix_attempts
            );
            LoopState::FixEscalation {
                verification_result,
                convergence_report,
            }
        }

        VerificationEvalDecision::ContinueToFix { progress } => {
            apply_fix_progress(ctx, config, &progress, verification_result.passed_steps);
            // Verification failed (non-critical) — continue to build failure context
            LoopState::BuildFailureContext {
                verification_result,
                convergence_report,
            }
        }
    }
}

/// Fixtures that drive [`handle_evaluate_verification`] itself, and its arming test.
///
/// Everything here names only what existed before plan
/// 2026-11-06-a-product-workflow-verification-with-nothing-to-check-never-reports-passed
/// Phase 1 changed the verdict. So the arming test compiles against the
/// handler as it was, and it fails there by assertion.
#[cfg(test)]
pub(super) mod evaluate_verification_arming {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::step_executor::StepExecutionResult;

    /// Records the effects the verdict is read from, and performs none.
    #[derive(Default)]
    pub(in super::super) struct RecordingEffects {
        pub outputs: Mutex<Vec<String>>,
        pub states: Mutex<Vec<UnifiedWorkflowState>>,
        pub knowledge_resolutions: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl EvaluationEffects for RecordingEffects {
        async fn append_output(&self, _execution_id: &str, text: &str) {
            self.outputs.lock().unwrap().push(text.to_string());
        }

        async fn store_verification_result(
            &self,
            _parent_id: &str,
            _iteration: u32,
            _result_json: &serde_json::Value,
        ) {
        }

        async fn record_state_snapshot(
            &self,
            _execution_id: &str,
            _kind: &str,
            _summary: &str,
            _context: Option<&str>,
        ) {
        }

        async fn on_verification_complete(
            &self,
            _iteration: u32,
            _result: &VerificationPhaseResult,
            _history: &HashMap<String, Vec<bool>>,
        ) {
        }

        fn persist_state(&self, _execution_id: &str, state: &UnifiedWorkflowState) {
            self.states.lock().unwrap().push(state.clone());
        }

        fn emit_iteration_metrics(
            &self,
            _execution_id: &str,
            _iteration: u32,
            _metrics: IterationMetrics,
        ) {
        }

        async fn resolve_open_knowledge(&self, parent_id: &str) {
            self.knowledge_resolutions
                .lock()
                .unwrap()
                .push(parent_id.to_string());
        }
    }

    /// One step result. `required: None` is required: the shared fold's default.
    pub(in super::super) fn step(
        name: &str,
        success: bool,
        required: Option<bool>,
    ) -> StepExecutionResult {
        StepExecutionResult {
            step_index: 0,
            step_type: "command".to_string(),
            step_name: name.to_string(),
            step_id: None,
            success,
            error: if success {
                None
            } else {
                Some(format!("{} failed", name))
            },
            screenshot_path: None,
            started_at: None,
            ended_at: None,
            duration_ms: 0,
            config: Default::default(),
            verification_details: None,
            output_data: None,
            required,
            resolved_inputs: None,
            extracted_values: None,
            failure_category: None,
            interrupted: None,
        }
    }

    /// A verification result over `step_results`, reporting `all_passed` as given:
    /// the producer's word, which the handler must not take on trust.
    pub(in super::super) fn phase_result(
        step_results: Vec<StepExecutionResult>,
        all_passed: bool,
        critical_failure: bool,
    ) -> VerificationPhaseResult {
        VerificationPhaseResult {
            iteration: 1,
            all_passed,
            total_steps: step_results.len(),
            passed_steps: step_results.iter().filter(|s| s.success).count(),
            failed_steps: step_results.iter().filter(|s| !s.success).count(),
            skipped_steps: 0,
            total_duration_ms: 0,
            step_results,
            critical_failure,
            console_errors: None,
            app_health: None,
            browser_events: None,
            network_failures: None,
        }
    }

    pub(in super::super) fn loop_config() -> LoopConfig {
        let mut config = crate::follow_up::workflow::build_follow_up_config(
            "exec-evaluate",
            "Evaluate",
            "Source",
        );
        // No constraint config is read from disk, and a fixed fix-attempt ceiling.
        config.project_path = None;
        config.max_fix_attempts = 3;
        config
    }

    pub(in super::super) fn loop_context(config: &LoopConfig) -> LoopContext {
        let mut ctx = LoopContext::new(
            config,
            crate::database::pg::PgDb::new_noop_for_test(),
            Vec::new(),
            false,
        );
        ctx.iteration = 1;
        ctx
    }

    /// The next state's name, for assertion messages (`LoopState` is not `Debug`).
    pub(in super::super) fn describe(state: &LoopState) -> String {
        match state {
            LoopState::Complete { reason } => format!("Complete({:?})", reason),
            LoopState::BuildFailureContext { .. } => "BuildFailureContext".to_string(),
            LoopState::FixEscalation { .. } => "FixEscalation".to_string(),
            _ => "another state".to_string(),
        }
    }

    /// ARMING (plan Phase 1, seam class `gate`). A unified-workflow verification
    /// that checked nothing does not complete the loop as a pass. The input is
    /// an empty step list fed the `all_passed: true` that both empty producer
    /// arms returned. An all-`dev_mode_only` verification reaches the handler in
    /// exactly this shape in a build without debug assertions, because
    /// `run_verification` filters every step out first.
    ///
    /// It enters through the handler that every unified workflow run calls
    /// (`run_loop_state_machine` → `handle_evaluate_verification`). Before
    /// Phase 1 that handler ended the loop `VerificationPassed` and resolved the
    /// run's knowledge.
    #[tokio::test]
    async fn an_empty_verification_fed_all_passed_does_not_complete_as_a_pass() {
        let effects = RecordingEffects::default();
        let mut config = loop_config();
        let mut ctx = loop_context(&config);

        let state = handle_evaluate_verification(
            &effects,
            &mut ctx,
            &mut config,
            phase_result(Vec::new(), true, false),
        )
        .await;

        assert!(
            !matches!(
                state,
                LoopState::Complete {
                    reason: CompletionReason::VerificationPassed
                }
            ),
            "a verification that checked nothing completed as a pass: {}",
            describe(&state)
        );
        assert!(
            matches!(state, LoopState::Complete { .. }),
            "nothing checked must end the loop, since no fix iteration can change it: {}",
            describe(&state)
        );
        assert!(
            effects.knowledge_resolutions.lock().unwrap().is_empty(),
            "a verification that checked nothing must not resolve knowledge"
        );
        assert_eq!(ctx.iteration_results.len(), 1);
        assert!(!ctx.iteration_results[0].verification_passed);
        let states = effects.states.lock().unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].verification_passed(), Some(false));
    }
}

// The handler's verdicts, driven through it with the arming module's fixtures.
#[cfg(test)]
mod evaluate_verification_tests {
    use super::evaluate_verification_arming::{
        describe, loop_config, loop_context, phase_result, step, RecordingEffects,
    };
    use super::*;

    #[tokio::test]
    async fn an_empty_verification_ends_no_verification_steps() {
        let effects = RecordingEffects::default();
        let mut config = loop_config();
        let mut ctx = loop_context(&config);

        let state = handle_evaluate_verification(
            &effects,
            &mut ctx,
            &mut config,
            phase_result(Vec::new(), true, false),
        )
        .await;

        assert!(
            matches!(
                state,
                LoopState::Complete {
                    reason: CompletionReason::NoVerificationSteps
                }
            ),
            "{}",
            describe(&state)
        );
        assert_eq!(
            ctx.iteration_results[0].failure_context,
            NOTHING_CHECKED_CONTEXT
        );
        let outputs = effects.outputs.lock().unwrap();
        assert_eq!(
            outputs[0],
            "\n--- Verification (Iteration 1): NOTHING CHECKED (no required verification step) (0 passed, 0 failed, 0 total, 0 required) ---\n"
        );
    }

    /// Two informational steps that both passed: the fold says `all_passed`,
    /// but no required step ran.
    #[tokio::test]
    async fn an_all_informational_verification_fed_all_passed_ends_no_verification_steps() {
        let effects = RecordingEffects::default();
        let mut config = loop_config();
        let mut ctx = loop_context(&config);

        let state = handle_evaluate_verification(
            &effects,
            &mut ctx,
            &mut config,
            phase_result(
                vec![
                    step("lint", true, Some(false)),
                    step("format", true, Some(false)),
                ],
                true,
                false,
            ),
        )
        .await;

        assert!(
            matches!(
                state,
                LoopState::Complete {
                    reason: CompletionReason::NoVerificationSteps
                }
            ),
            "{}",
            describe(&state)
        );
        assert!(effects.knowledge_resolutions.lock().unwrap().is_empty());
        assert_eq!(
            effects.states.lock().unwrap()[0].verification_passed(),
            Some(false)
        );
    }

    /// One required step that passed is a pass, and the pass arm is the only
    /// one that resolves the run's knowledge.
    #[tokio::test]
    async fn one_required_step_passing_completes_verification_passed() {
        let effects = RecordingEffects::default();
        let mut config = loop_config();
        let mut ctx = loop_context(&config);

        let state = handle_evaluate_verification(
            &effects,
            &mut ctx,
            &mut config,
            phase_result(vec![step("tests", true, None)], true, false),
        )
        .await;

        assert!(
            matches!(
                state,
                LoopState::Complete {
                    reason: CompletionReason::VerificationPassed
                }
            ),
            "{}",
            describe(&state)
        );
        assert_eq!(
            *effects.knowledge_resolutions.lock().unwrap(),
            vec!["exec-evaluate".to_string()]
        );
        assert!(ctx.iteration_results[0].verification_passed);
        assert_eq!(
            effects.states.lock().unwrap()[0].verification_passed(),
            Some(true)
        );
        assert!(effects.outputs.lock().unwrap()[0].contains("): PASSED ("));
    }

    /// Many steps with a required failure keep fixing. The informational
    /// failure does not count. Progress over the best-so-far (0) resets the
    /// fix counter, and no knowledge is resolved.
    #[tokio::test]
    async fn many_steps_with_a_required_failure_continue_to_fix() {
        let effects = RecordingEffects::default();
        let mut config = loop_config();
        let mut ctx = loop_context(&config);

        let state = handle_evaluate_verification(
            &effects,
            &mut ctx,
            &mut config,
            phase_result(
                vec![
                    step("build", true, None),
                    step("tests", false, Some(true)),
                    step("lint", false, Some(false)),
                ],
                false,
                false,
            ),
        )
        .await;

        assert!(
            matches!(state, LoopState::BuildFailureContext { .. }),
            "{}",
            describe(&state)
        );
        assert_eq!(ctx.best_passed_checks, 1);
        assert_eq!(ctx.fix_attempts, 0);
        assert_eq!(ctx.last_progress_iteration, 1);
        assert!(ctx.iteration_results.is_empty());
        assert!(effects.knowledge_resolutions.lock().unwrap().is_empty());
        assert!(effects.outputs.lock().unwrap()[0].contains("): FAILED ("));
    }

    /// No progress at the ceiling escalates, after counting the attempt.
    #[tokio::test]
    async fn a_failure_without_progress_at_the_ceiling_escalates() {
        let effects = RecordingEffects::default();
        let mut config = loop_config();
        let mut ctx = loop_context(&config);
        ctx.best_passed_checks = 1;
        ctx.fix_attempts = 2;

        let state = handle_evaluate_verification(
            &effects,
            &mut ctx,
            &mut config,
            phase_result(
                vec![step("build", true, None), step("tests", false, None)],
                false,
                false,
            ),
        )
        .await;

        assert!(
            matches!(state, LoopState::FixEscalation { .. }),
            "{}",
            describe(&state)
        );
        assert_eq!(ctx.fix_attempts, 3);
    }

    #[tokio::test]
    async fn a_critical_failure_completes_critical() {
        let effects = RecordingEffects::default();
        let mut config = loop_config();
        let mut ctx = loop_context(&config);

        let state = handle_evaluate_verification(
            &effects,
            &mut ctx,
            &mut config,
            phase_result(vec![step("health", false, None)], false, true),
        )
        .await;

        assert!(
            matches!(
                state,
                LoopState::Complete {
                    reason: CompletionReason::CriticalFailure { .. }
                }
            ),
            "{}",
            describe(&state)
        );
        assert!(ctx.iteration_results[0].critical_failure);
        assert!(effects.knowledge_resolutions.lock().unwrap().is_empty());
    }
}
