//! Verification phase executor.
//!
//! Runs verification/test steps and reports results including UI Bridge
//! diagnostics (console errors, app health, browser events, network failures).

use std::sync::Arc;
use tracing::{debug, info, instrument, warn};

use crate::config_storage::ConfigStorage;
use crate::step_executor::{
    ExecutionStepConfig, StepExecutionResult, StepExecutor, VerificationPhaseResult,
};
use crate::step_metadata::{StepDetails, StepMetadata};
use crate::step_registry::{StepEventKind, StepEventLogger};
use crate::step_types::StepType;
use crate::workflow_state::{in_stage, CheckpointManager, StepCheckpoint};
use crate::AppState;

use super::super::phase_helpers::{
    clear_console_errors, fetch_browser_events_from_ui_bridge, fetch_console_errors_from_ui_bridge,
    fetch_health_from_ui_bridge, fetch_network_failures_from_ui_bridge,
};

// =============================================================================
// Verification Phase Executor
// =============================================================================

/// The result of a verification with no step to run: nothing was checked.
///
/// `all_passed` is `false`. Nothing passed, so the result must not say that
/// everything did. Both empty arms return it: an empty step list,
/// including one emptied by the `dev_mode_only` filter of a build without
/// debug assertions, which is how the published runner is built, and a
/// targeted run whose step indices selected nothing. The verdict on such a
/// result is `NoVerificationSteps`, which `decide_evaluate_verification`
/// reaches from its zero `required_steps` whatever `all_passed` says. This
/// keeps the producer from asserting a pass it never observed.
fn nothing_to_verify(iteration: u32) -> VerificationPhaseResult {
    VerificationPhaseResult {
        iteration,
        all_passed: false,
        critical_failure: false,
        total_steps: 0,
        passed_steps: 0,
        failed_steps: 0,
        skipped_steps: 0,
        total_duration_ms: 0,
        step_results: Vec::new(),
        console_errors: None,
        app_health: None,
        browser_events: None,
        network_failures: None,
    }
}

/// Executes verification steps and determines if they all pass.
pub struct VerificationExecutor {
    pub(crate) app_state: Arc<AppState>,
    executor: StepExecutor,
}

impl VerificationExecutor {
    pub fn new(
        app_state: Arc<AppState>,
        config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
    ) -> Self {
        Self {
            app_state: app_state.clone(),
            executor: StepExecutor::with_app_handle(app_state.clone(), config_storage, app_handle),
        }
    }

    /// Set the task run ID on the inner step executor for database logging.
    pub fn set_task_run_id(&mut self, task_run_id: String) {
        self.executor.set_task_run_id(task_run_id);
    }

    /// Set the path scope policy on the inner step executor.
    pub fn set_path_scope_policy(&mut self, policy: crate::paths::PathScopePolicy) {
        self.executor.set_path_scope_policy(policy);
    }

    /// Run verification steps.
    ///
    /// Returns (verification_result, step_results)
    /// The logger is required for consistent step event logging.
    ///
    /// Step checkpointing is now integrated to enable resume after crashes:
    /// - Each step is checkpointed before (running) and after (success/failed) execution
    /// - On resume, completed steps can be skipped based on checkpoint data
    #[instrument(
        name = "qontinui.workflow.phase.verification",
        skip(self, steps, logger),
        fields(
            execution_id = %execution_id,
            iteration = iteration,
            workflow_name = %workflow_name,
            step_count = steps.len()
        )
    )]
    pub async fn run_verification(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        iteration: u32,
        workflow_name: &str,
        logger: &StepEventLogger,
        stage_index: Option<u32>,
    ) -> (VerificationPhaseResult, Vec<StepExecutionResult>) {
        // Filter out dev_mode_only steps when not in dev mode
        let steps: Vec<ExecutionStepConfig> = steps
            .iter()
            .filter(|step| {
                if !step.runs_in_build(cfg!(debug_assertions)) {
                    info!(
                        "VERIFICATION-PHASE: Skipping dev-mode-only step: {:?}",
                        step.name
                    );
                    false
                } else {
                    true
                }
            })
            .cloned()
            .collect();
        let steps = steps.as_slice();

        // Clear console errors before running verification so each iteration
        // only captures its own errors
        clear_console_errors().await;

        // Record verification start time for browser event filtering
        let verification_start_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        if steps.is_empty() {
            info!(
                "VERIFICATION-PHASE: No verification steps to run (iteration {}) - nothing is checked",
                iteration
            );
            return (nothing_to_verify(iteration), Vec::new());
        }

        // Pre-verification health check: detect if the SDK app is already broken
        // (e.g., showing a framework error overlay) before running expensive verification steps.
        // This gives the AI faster feedback about app-breaking changes.
        let pre_check_health = fetch_health_from_ui_bridge().await;
        if let Some(ref health) = pre_check_health {
            let status = health
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            if status == "broken" {
                let summary = health.get("summary").and_then(|v| v.as_str()).unwrap_or("");
                warn!(
                    "VERIFICATION-PHASE: SDK app health is BROKEN before verification (iteration {}): {}",
                    iteration, summary
                );
            }
        }

        info!(
            "VERIFICATION-PHASE: Running {} steps (iteration {})",
            steps.len(),
            iteration
        );

        // Create checkpoint manager for step-level checkpointing
        let checkpoint_mgr = CheckpointManager::new("unified");

        // Crash-recovery replay is DELIBERATELY NOT wired into this phase.
        // `super::phase_is_replayable` excludes "verification" and carries the
        // full reasoning; in short, a verification step is a MEASUREMENT of the
        // system under test, so its journalled result describes a world the
        // resumed process never observed, and it costs no AI tokens to take the
        // measurement again. The journal is still read here so that a resumed
        // run which re-observes says so, instead of looking like the silent
        // re-execution this plan exists to close.
        debug_assert!(
            !super::phase_is_replayable("verification"),
            "verification must stay out of the replay set"
        );
        match checkpoint_mgr.get_completed_steps(execution_id, "verification", Some(iteration)) {
            Ok(prior) => {
                // Stage-scoped for the same reason the replay and narration
                // readers are: `(execution_id, phase, iteration)` is NOT
                // unique — `stage_index` is part of the table's uniqueness
                // key, and every stage of a multi-stage workflow writes the
                // same `(iteration, step_index)` pairs. A stage-blind count
                // here reported stage 0's rows as this stage's, so a stage
                // that had journalled nothing still announced steps it was
                // about to "RE-OBSERVE".
                let already_journalled = prior
                    .iter()
                    .filter(|cp| in_stage(cp, stage_index) && cp.status.is_replayable())
                    .count();
                if already_journalled > 0 {
                    info!(
                        "VERIFICATION-PHASE: {} step(s) of iteration {} (stage {:?}) are journalled as complete and will be RE-OBSERVED (a verification result is a measurement, not a reusable output)",
                        already_journalled, iteration, stage_index
                    );
                }
            }
            Err(e) => {
                debug!(
                    "VERIFICATION-PHASE: could not read prior checkpoints: {}",
                    e
                );
            }
        }

        // Log START events and save checkpoints for each step before execution
        for (idx, step) in steps.iter().enumerate() {
            let step_type =
                StepType::from_str_compat(&step.step_type).unwrap_or(StepType::Playwright);
            let step_name = step.name.as_deref().unwrap_or(&step.step_type);
            let metadata =
                StepMetadata::verification(execution_id, step_type, step_name, idx, iteration);
            let details = StepDetails::default();

            if let Err(e) =
                logger.log_start(StepEventKind::VerificationStepStart, metadata, details)
            {
                warn!("Failed to log verification step start event: {}", e);
            }

            // Save step checkpoint as "running"
            let mut checkpoint = StepCheckpoint::new(
                execution_id,
                "unified",
                "verification",
                Some(iteration),
                idx,
                step_type.as_str(),
            )
            .with_step_name(step_name)
            .with_stage_index(stage_index);
            checkpoint.mark_started();
            if let Err(e) = checkpoint_mgr.save_step(&checkpoint) {
                warn!("Failed to save verification step checkpoint: {}", e);
            }
        }

        // Use the new method that emits completion events as each step finishes
        // This allows the UI to show real-time progress instead of waiting for all steps
        let result = self
            .executor
            .execute_verification_steps_with_events(
                steps,
                execution_id,
                iteration,
                Some(workflow_name),
            )
            .await;

        info!(
            "VERIFICATION-PHASE: Iteration {} result: all_passed={}, critical_failure={}, passed={}/{}, failed={}",
            iteration,
            result.all_passed,
            result.critical_failure,
            result.passed_steps,
            result.total_steps,
            result.failed_steps
        );

        // Save completion checkpoints for each step
        for (idx, step_result) in result.step_results.iter().enumerate() {
            let step = &steps[idx];
            let step_type =
                StepType::from_str_compat(&step.step_type).unwrap_or(StepType::Playwright);
            let step_name = step.name.as_deref().unwrap_or(&step.step_type);

            let mut checkpoint = StepCheckpoint::new(
                execution_id,
                "unified",
                "verification",
                Some(iteration),
                idx,
                step_type.as_str(),
            )
            .with_step_name(step_name)
            .with_stage_index(stage_index);

            let duration_ms = step_result.duration_ms as i64;
            if step_result.success {
                checkpoint.mark_success(serde_json::to_string(step_result).ok(), duration_ms);
            } else {
                // Annotate failure with structured error code for AI consumption
                let raw_error = step_result.error.as_deref().unwrap_or("Unknown error");
                let error_code = crate::mcp::ui_bridge::classify_assertion_failure(raw_error);
                let recovery = crate::mcp::ui_bridge::recovery_hint_for(&error_code);
                let annotated_error = format!("[{:?} → {:?}] {}", error_code, recovery, raw_error);
                checkpoint.mark_failed(&annotated_error, duration_ms);
            }

            if let Err(e) = checkpoint_mgr.save_step(&checkpoint) {
                warn!(
                    "Failed to save verification step completion checkpoint: {}",
                    e
                );
            }
        }

        // Fetch UI Bridge diagnostics concurrently (all best-effort)
        let (console_errors_result, app_health, browser_events, network_failures) = tokio::join!(
            fetch_console_errors_from_ui_bridge(),
            fetch_health_from_ui_bridge(),
            fetch_browser_events_from_ui_bridge(verification_start_ms),
            fetch_network_failures_from_ui_bridge(verification_start_ms),
        );

        let console_errors = match console_errors_result {
            Ok(errors) => {
                if !errors.is_empty() {
                    debug!(
                        "VERIFICATION-PHASE: Captured {} console errors during verification",
                        errors.len()
                    );
                }
                Some(errors).filter(|e| !e.is_empty())
            }
            Err(e) => {
                debug!("VERIFICATION-PHASE: Could not fetch console errors: {}", e);
                None
            }
        };

        if app_health.is_some() || !browser_events.is_empty() || !network_failures.is_empty() {
            debug!(
                "VERIFICATION-PHASE: UI Bridge diagnostics: health={}, browser_events={}, network_failures={}",
                app_health.is_some(),
                browser_events.len(),
                network_failures.len()
            );
        }

        // Use post-verification health if available, fall back to pre-check health
        let effective_health = app_health.or(pre_check_health);

        let mut result = result;
        result.console_errors = console_errors;
        result.app_health = effective_health;
        result.browser_events = Some(browser_events).filter(|e| !e.is_empty());
        result.network_failures = Some(network_failures).filter(|e| !e.is_empty());

        let step_results = result.step_results.clone();
        (result, step_results)
    }

    /// Run a subset of verification steps by their indices.
    ///
    /// Used by the multi-agent fixer to run targeted verification after each
    /// fix agent completes, providing fast feedback without running all steps.
    pub async fn run_targeted_verification(
        &self,
        all_steps: &[ExecutionStepConfig],
        step_indices: &[usize],
        execution_id: &str,
        iteration: u32,
        workflow_name: &str,
    ) -> VerificationPhaseResult {
        let targeted_steps: Vec<ExecutionStepConfig> = step_indices
            .iter()
            .filter_map(|&idx| all_steps.get(idx).cloned())
            .collect();

        if targeted_steps.is_empty() {
            return nothing_to_verify(iteration);
        }

        info!(
            "MULTI-AGENT: Running targeted verification for {} step(s) (iteration {})",
            targeted_steps.len(),
            iteration
        );

        let result = self
            .executor
            .execute_verification_steps_with_events(
                &targeted_steps,
                execution_id,
                iteration,
                Some(workflow_name),
            )
            .await;

        info!(
            "MULTI-AGENT: Targeted verification: passed={}/{}, failed={}",
            result.passed_steps, result.total_steps, result.failed_steps
        );

        result
    }
}

#[cfg(test)]
mod nothing_to_verify_tests {
    use super::nothing_to_verify;

    /// Both empty arms return this result. Before plan
    /// 2026-11-06-a-product-workflow-verification-with-nothing-to-check-never-reports-passed
    /// they returned `all_passed: true`, which is a pass that nothing observed.
    #[test]
    fn an_empty_verification_does_not_report_all_passed() {
        let result = nothing_to_verify(4);
        assert!(
            !result.all_passed,
            "nothing checked must not report all_passed"
        );
        assert_eq!(result.iteration, 4);
        assert_eq!(result.total_steps, 0);
        assert_eq!(result.passed_steps, 0);
        assert!(result.step_results.is_empty());
        assert!(!result.critical_failure);
    }
}
