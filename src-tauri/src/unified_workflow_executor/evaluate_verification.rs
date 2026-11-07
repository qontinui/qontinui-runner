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

/// Convergence analysis, resource limits, handle verification_passed / critical_failure,
/// record knowledge.
///
/// Original lines: 2557-2852
pub(crate) async fn handle_evaluate_verification(
    effects: &dyn EvaluationEffects,
    ctx: &mut LoopContext,
    config: &mut LoopConfig,
    verification_result: VerificationPhaseResult,
) -> LoopState {
    // Log verification results to output_log so the summary AI has context
    {
        let status = if verification_result.all_passed {
            "PASSED"
        } else if verification_result.critical_failure {
            "CRITICAL FAILURE"
        } else {
            "FAILED"
        };
        let summary_line = format!(
            "\n--- Verification (Iteration {}): {} ({} passed, {} failed, {} total) ---\n",
            ctx.iteration,
            status,
            verification_result.passed_steps,
            verification_result.failed_steps,
            verification_result.total_steps,
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
            if verification_result.all_passed {
                "PASSED"
            } else {
                "FAILED"
            },
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

    // Persist workflow state: VerificationComplete
    effects.persist_state(
        &config.execution_id,
        &UnifiedWorkflowState::verification_complete(
            ctx.iteration,
            verification_result.all_passed,
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
        if !verification_result.all_passed {
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

    // Check verification outcome
    if verification_result.all_passed {
        info!("*** VERIFICATION PASSED on iteration {} ***", ctx.iteration);

        // Resolve all unresolved knowledge entries — the issues they describe
        // have been addressed since verification now passes.
        effects
            .resolve_open_knowledge(&get_parent_task_id(&config.execution_id))
            .await;

        let iter_result = IterationResult {
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
        };
        ctx.iteration_results.push(iter_result);

        return LoopState::Complete {
            reason: CompletionReason::VerificationPassed,
        };
    }

    // Check for critical failure
    if verification_result.critical_failure {
        error!(
            "*** CRITICAL FAILURE on iteration {} - stopping loop ***",
            ctx.iteration
        );

        let iter_result = IterationResult {
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
        };
        ctx.iteration_results.push(iter_result);

        return LoopState::Complete {
            reason: CompletionReason::CriticalFailure {
                error: verification_result.build_failure_context(),
            },
        };
    }

    // --- Fix attempt tracking (Phase 1: Bounded Fix Loop) ---
    let current_passed = verification_result.passed_steps;
    if current_passed > ctx.best_passed_checks {
        // Progress detected — reset fix counter
        ctx.best_passed_checks = current_passed;
        ctx.fix_attempts = 0;
        ctx.last_progress_iteration = ctx.iteration;
        info!(
            "Fix loop: progress detected (passed {} > previous best), reset fix_attempts",
            current_passed,
        );
    } else {
        ctx.fix_attempts += 1;
        info!(
            "Fix loop: no progress (passed {} <= best {}), fix_attempts = {}/{}",
            current_passed, ctx.best_passed_checks, ctx.fix_attempts, config.max_fix_attempts
        );
    }

    // Check if fix attempts exhausted
    if config.max_fix_attempts > 0 && ctx.fix_attempts >= config.max_fix_attempts {
        warn!(
            "Fix loop: {} consecutive non-improving iterations (max {}), escalating",
            ctx.fix_attempts, config.max_fix_attempts
        );
        return LoopState::FixEscalation {
            verification_result,
            convergence_report,
        };
    }

    // Verification failed (non-critical) — continue to build failure context
    LoopState::BuildFailureContext {
        verification_result,
        convergence_report,
    }
}
