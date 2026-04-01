//! Verification phase executor.
//!
//! Runs verification/test steps and reports results including UI Bridge
//! diagnostics (console errors, app health, browser events, network failures).

use std::sync::Arc;
use tracing::{debug, info, instrument, warn};

use crate::config_storage::ConfigStorage;
use crate::executor::{ExecutionOutcome, IntoOutcome};
use crate::step_executor::{
    ExecutionStepConfig, StepExecutionResult, StepExecutor, VerificationPhaseResult,
};
use crate::step_metadata::{StepDetails, StepMetadata};
use crate::step_registry::{StepEventKind, StepEventLogger};
use crate::step_types::StepType;
use crate::workflow_state::{CheckpointManager, StepCheckpoint};
use crate::AppState;

use super::super::phase_configs::{VerificationConfig, VerificationResult};
use super::super::phase_helpers::{
    clear_console_errors, fetch_browser_events_from_ui_bridge, fetch_console_errors_from_ui_bridge,
    fetch_health_from_ui_bridge, fetch_network_failures_from_ui_bridge,
};

// =============================================================================
// Verification Phase Executor
// =============================================================================

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
                if step.dev_mode_only.unwrap_or(false) && !cfg!(debug_assertions) {
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
                "VERIFICATION-PHASE: No verification steps defined (iteration {})",
                iteration
            );
            // No verification steps = verification passes
            return (
                VerificationPhaseResult {
                    iteration,
                    all_passed: true,
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
                },
                Vec::new(),
            );
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

    /// Run verification and return a unified ExecutionOutcome.
    ///
    /// This uses the IntoOutcome trait to convert the VerificationResult into a
    /// standardized ExecutionOutcome, which is useful for consistent result handling.
    ///
    /// # Arguments
    /// * `config` - The verification configuration
    /// * `logger` - Logger for step events
    ///
    /// # Returns
    /// An `ExecutionOutcome` summarizing the verification phase execution.
    pub async fn run_verification_to_outcome(
        &self,
        config: &VerificationConfig,
        logger: &StepEventLogger,
    ) -> ExecutionOutcome {
        let start = std::time::Instant::now();

        let (phase_result, step_results) = self
            .run_verification(
                &config.steps,
                &config.execution_id,
                config.iteration,
                &config.workflow_name,
                logger,
                None,
            )
            .await;

        let duration_ms = start.elapsed().as_millis() as u64;

        // Use the IntoOutcome trait for consistent conversion
        let result = VerificationResult {
            phase_result,
            step_results,
        };
        result.into_outcome(duration_ms)
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
            return VerificationPhaseResult {
                iteration,
                all_passed: true,
                total_steps: 0,
                passed_steps: 0,
                failed_steps: 0,
                skipped_steps: 0,
                total_duration_ms: 0,
                step_results: vec![],
                critical_failure: false,
                console_errors: None,
                app_health: None,
                browser_events: None,
                network_failures: None,
            };
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
