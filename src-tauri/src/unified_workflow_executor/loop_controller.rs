//! Loop controller for the verification-agentic loop.
//!
//! This module contains the core loop logic with a single responsibility:
//! coordinate the verification → agentic cycle until verification passes
//! or max iterations is reached.
//!
//! Key design principles:
//! 1. The loop ONLY exits when verification passes or max_iterations is hit
//! 2. The AI CANNOT bypass verification by claiming [TASK_COMPLETE]
//! 3. Completion phase ONLY runs if verification passed
//! 4. All decisions are logged clearly for debugging

use std::sync::Arc;
use tracing::{error, info, warn};

use crate::config_storage::ConfigStorage;
use crate::orchestrator::integration::StageTransition;
use crate::step_executor::ExecutionStepConfig;
use crate::AppState;

use super::phases::{AgenticExecutor, CompletionExecutor, SetupExecutor, VerificationExecutor};
use super::types::{AgenticOutcome, IterationResult, LoopConfig, LoopResult};

/// The main loop controller for unified workflows.
///
/// This controller manages the verification-agentic loop with strict guarantees:
/// - Verification is the ONLY authority on whether work is complete
/// - The AI cannot self-declare completion
/// - Completion phase only runs when verification passes
pub struct LoopController {
    setup_executor: SetupExecutor,
    verification_executor: VerificationExecutor,
    agentic_executor: AgenticExecutor,
    completion_executor: CompletionExecutor,
    checkpoint_db: Arc<crate::database::CheckpointDb>,
}

impl LoopController {
    pub fn new(
        app_state: Arc<AppState>,
        config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
        pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    ) -> Self {
        Self {
            setup_executor: SetupExecutor::new(
                app_state.clone(),
                config_storage.clone(),
                app_handle.clone(),
                pid_tracker.clone(),
            ),
            verification_executor: VerificationExecutor::new(
                app_state.clone(),
                config_storage.clone(),
                app_handle.clone(),
            ),
            agentic_executor: AgenticExecutor::new(
                app_state.clone(),
                app_handle.clone(),
                pid_tracker.clone(),
            ),
            completion_executor: CompletionExecutor::new(
                app_state.clone(),
                config_storage,
                app_handle,
                pid_tracker,
            ),
            checkpoint_db: app_state.checkpoint_db.clone(),
        }
    }

    /// Run the complete workflow with verification-agentic loop.
    ///
    /// Flow:
    /// 1. SETUP (once)
    /// 2. LOOP until verification passes or max_iterations:
    ///    a. Run verification
    ///    b. If all pass → exit loop with success
    ///    c. If critical failure → exit loop with failure
    ///    d. Run agentic phase with failure context
    /// 3. COMPLETION (only if verification passed)
    /// 4. Return final result
    pub async fn run(
        &self,
        config: LoopConfig,
        setup_automation_steps: Vec<ExecutionStepConfig>,
        setup_prompt_steps: Vec<ExecutionStepConfig>,
        verification_steps: Vec<ExecutionStepConfig>,
        agentic_steps: Vec<ExecutionStepConfig>,
        completion_automation_steps: Vec<ExecutionStepConfig>,
        completion_prompt_steps: Vec<ExecutionStepConfig>,
    ) -> WorkflowResult {
        let start = std::time::Instant::now();
        let mut all_step_results = Vec::new();

        // Stage transition tracking for recap timeline
        let mut transitions: Vec<StageTransition> = Vec::new();
        let mut current_stage = "init".to_string();

        info!(
            "=== UNIFIED WORKFLOW START: {} (id: {}) ===",
            config.workflow_name, config.execution_id
        );
        info!(
            "Configuration: max_iterations={}, timeout={}s",
            config.max_iterations, config.timeout_seconds
        );
        info!(
            "Steps: setup_auto={}, setup_prompt={}, verification={}, agentic={}, completion_auto={}, completion_prompt={}",
            setup_automation_steps.len(),
            setup_prompt_steps.len(),
            verification_steps.len(),
            agentic_steps.len(),
            completion_automation_steps.len(),
            completion_prompt_steps.len()
        );

        // =====================================================================
        // PHASE 1: SETUP (runs once)
        // =====================================================================
        info!("=== PHASE 1: SETUP ===");
        self.record_stage_transition(
            &config.execution_id,
            &mut transitions,
            &mut current_stage,
            "setup",
            0,
        );

        let (setup_success, setup_results) = self
            .setup_executor
            .execute(
                &setup_automation_steps,
                &setup_prompt_steps,
                &config.execution_id,
                &config.workflow_name,
                config.timeout_seconds,
            )
            .await;
        all_step_results.extend(setup_results);

        if !setup_success {
            error!("Setup phase failed - aborting workflow");
            self.mark_task_failed(&config.execution_id, "Setup phase failed")
                .await;
            return WorkflowResult {
                success: false,
                verification_passed: false,
                step_results: all_step_results,
                duration_ms: start.elapsed().as_millis() as u64,
                loop_result: None,
            };
        }

        // =====================================================================
        // PHASE 2: VERIFICATION-AGENTIC LOOP
        // =====================================================================
        info!("=== PHASE 2: VERIFICATION-AGENTIC LOOP ===");
        let loop_result = self
            .run_verification_agentic_loop(
                &config,
                &verification_steps,
                !agentic_steps.is_empty(),
                &mut all_step_results,
                &mut transitions,
                &mut current_stage,
            )
            .await;

        info!(
            "Loop completed: {}",
            loop_result.summary()
        );

        // =====================================================================
        // PHASE 3: COMPLETION (only if verification passed!)
        // =====================================================================
        if loop_result.should_run_completion() {
            info!("=== PHASE 3: COMPLETION (verification PASSED) ===");
            self.record_stage_transition(
                &config.execution_id,
                &mut transitions,
                &mut current_stage,
                "completion",
                loop_result.iterations_run,
            );

            let (completion_success, completion_results) = self
                .completion_executor
                .execute(
                    &completion_automation_steps,
                    &completion_prompt_steps,
                    &config.execution_id,
                    &config.workflow_name,
                    config.timeout_seconds,
                )
                .await;
            all_step_results.extend(completion_results);

            if !completion_success {
                warn!("Completion phase had failures, but verification passed so task is complete");
            }

            // Mark task as completed (verification passed)
            self.mark_task_completed(&config.execution_id).await;

            info!("=== WORKFLOW COMPLETED SUCCESSFULLY ===");
        } else {
            info!("=== PHASE 3: COMPLETION SKIPPED (verification did not pass) ===");
            info!(
                "Reason: verification_passed={}, critical_failure={}, max_iterations_reached={}",
                loop_result.verification_passed,
                loop_result.critical_failure,
                loop_result.max_iterations_reached
            );

            // Mark task as failed
            let reason = if loop_result.critical_failure {
                "Critical verification failure"
            } else {
                "Verification failed after max iterations"
            };
            self.mark_task_failed(&config.execution_id, reason).await;

            info!("=== WORKFLOW FAILED ===");
        }

        WorkflowResult {
            success: loop_result.verification_passed,
            verification_passed: loop_result.verification_passed,
            step_results: all_step_results,
            duration_ms: start.elapsed().as_millis() as u64,
            loop_result: Some(loop_result),
        }
    }

    /// The core verification-agentic loop.
    ///
    /// CRITICAL: This loop is the ONLY authority on whether verification passes.
    /// The AI cannot bypass this by outputting [TASK_COMPLETE] or similar.
    async fn run_verification_agentic_loop(
        &self,
        config: &LoopConfig,
        verification_steps: &[ExecutionStepConfig],
        has_agentic_steps: bool,
        all_step_results: &mut Vec<crate::step_executor::StepExecutionResult>,
        transitions: &mut Vec<StageTransition>,
        current_stage: &mut String,
    ) -> LoopResult {
        let mut iteration_results = Vec::new();
        let mut iteration = 0u32;

        loop {
            iteration += 1;

            info!(
                "--- LOOP ITERATION {} of {} ---",
                iteration, config.max_iterations
            );

            // Check if we've exceeded max iterations BEFORE running verification
            if iteration > config.max_iterations {
                warn!(
                    "Max iterations ({}) exceeded - exiting loop",
                    config.max_iterations
                );
                return LoopResult {
                    iterations_run: iteration - 1, // Don't count this iteration
                    verification_passed: false,
                    max_iterations_reached: true,
                    critical_failure: false,
                    iteration_results,
                };
            }

            // Log iteration start to database
            // Use append_task_output_ex with check_completion_marker=false
            // because in unified workflows, VERIFICATION is the authority on completion,
            // not the [TASK_COMPLETE] marker from the AI
            let _ = self.checkpoint_db.append_task_output_ex(
                &config.execution_id,
                &format!("\n\n=== Verification-Agentic Loop: Iteration {} ===\n", iteration),
                false,
                false, // Don't check for completion marker - verification is the authority
            );

            // -----------------------------------------------------------------
            // VERIFICATION PHASE
            // -----------------------------------------------------------------
            info!("Running verification phase (iteration {})", iteration);
            self.record_stage_transition(
                &config.execution_id,
                transitions,
                current_stage,
                "verification",
                iteration,
            );

            let (verification_result, step_results) = self
                .verification_executor
                .execute(verification_steps, &config.execution_id, iteration, &config.workflow_name)
                .await;

            // Add step results to overall results
            all_step_results.extend(step_results);

            // Store verification result in database for Recap page
            if let Ok(result_json) = serde_json::to_value(&verification_result) {
                let _ = self.checkpoint_db.store_verification_phase_result(
                    &config.execution_id,
                    iteration,
                    &result_json,
                );
            }

            // Check verification outcome
            if verification_result.all_passed {
                info!(
                    "*** VERIFICATION PASSED on iteration {} ***",
                    iteration
                );

                let iter_result = IterationResult {
                    iteration,
                    verification_passed: true,
                    critical_failure: false,
                    passed_checks: verification_result.passed_steps,
                    failed_checks: verification_result.failed_steps,
                    failure_context: String::new(),
                    agentic_phase_ran: false,
                    agentic_phase_success: None,
                };
                iteration_results.push(iter_result);

                return LoopResult {
                    iterations_run: iteration,
                    verification_passed: true,
                    max_iterations_reached: false,
                    critical_failure: false,
                    iteration_results,
                };
            }

            // Check for critical failure
            if verification_result.critical_failure {
                error!(
                    "*** CRITICAL FAILURE on iteration {} - stopping loop ***",
                    iteration
                );

                let iter_result = IterationResult {
                    iteration,
                    verification_passed: false,
                    critical_failure: true,
                    passed_checks: verification_result.passed_steps,
                    failed_checks: verification_result.failed_steps,
                    failure_context: verification_result.build_failure_context(),
                    agentic_phase_ran: false,
                    agentic_phase_success: None,
                };
                iteration_results.push(iter_result);

                return LoopResult {
                    iterations_run: iteration,
                    verification_passed: false,
                    max_iterations_reached: false,
                    critical_failure: true,
                    iteration_results,
                };
            }

            // Verification failed (non-critical) - run agentic phase
            // LOOP CONTINUES: This is the expected path when verification fails
            info!(
                "LOOP-CONTINUE: Verification FAILED (passed={}, failed={}) on iteration {} - will run agentic phase then loop back",
                verification_result.passed_steps, verification_result.failed_steps, iteration
            );
            info!(
                "LOOP-DEBUG: all_passed={}, critical_failure={}, iteration={}/{} - loop will continue",
                verification_result.all_passed, verification_result.critical_failure, iteration, config.max_iterations
            );

            let failure_context = verification_result.build_failure_context();

            // -----------------------------------------------------------------
            // AGENTIC PHASE
            // -----------------------------------------------------------------
            self.record_stage_transition(
                &config.execution_id,
                transitions,
                current_stage,
                "agentic",
                iteration,
            );

            let agentic_outcome = self
                .agentic_executor
                .execute(config, iteration, &failure_context, has_agentic_steps)
                .await;

            // Log agentic output to database
            // CRITICAL: Use append_task_output_ex with check_completion_marker=false
            // The AI may output [TASK_COMPLETE] but that does NOT mean verification passed!
            // In unified workflows, only verification passing can mark the task complete.
            if let Some(output) = agentic_outcome.output() {
                let _ = self.checkpoint_db.append_task_output_ex(
                    &config.execution_id,
                    &format!("\n--- AI Output (Iteration {}) ---\n{}\n", iteration, output),
                    true,  // increment session count
                    false, // Don't check for completion marker - verification is the authority
                );
            }

            let iter_result = IterationResult {
                iteration,
                verification_passed: false,
                critical_failure: false,
                passed_checks: verification_result.passed_steps,
                failed_checks: verification_result.failed_steps,
                failure_context,
                agentic_phase_ran: !matches!(agentic_outcome, AgenticOutcome::Skipped),
                agentic_phase_success: Some(agentic_outcome.is_success()),
            };
            iteration_results.push(iter_result);

            info!(
                "LOOP-CONTINUE: Iteration {} complete - looping back to verification (next iteration: {})",
                iteration, iteration + 1
            );
            // The loop continues here naturally - no return statement
            // Control flows back to the top of the loop for the next iteration
        }
    }

    async fn mark_task_completed(&self, execution_id: &str) {
        if let Err(e) = self.checkpoint_db.complete_task_run(execution_id) {
            error!("Failed to mark task {} as completed: {}", execution_id, e);
        } else {
            info!("Marked task {} as COMPLETED", execution_id);
        }
    }

    async fn mark_task_failed(&self, execution_id: &str, reason: &str) {
        if let Err(e) = self.checkpoint_db.fail_task_run(execution_id, reason) {
            error!("Failed to mark task {} as failed: {}", execution_id, e);
        } else {
            info!("Marked task {} as FAILED: {}", execution_id, reason);
        }
    }

    /// Record a stage transition and persist to database.
    fn record_stage_transition(
        &self,
        execution_id: &str,
        transitions: &mut Vec<StageTransition>,
        current_stage: &mut String,
        to_stage: &str,
        iteration: u32,
    ) {
        if current_stage.as_str() != to_stage {
            let transition = StageTransition {
                from: current_stage.clone(),
                to: to_stage.to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                iteration,
            };
            transitions.push(transition);
            *current_stage = to_stage.to_string();

            // Persist to database
            if let Ok(json) = serde_json::to_string(&transitions) {
                if let Err(e) = self
                    .checkpoint_db
                    .update_task_run_transition_history(execution_id, &json)
                {
                    warn!("Failed to persist transition history: {}", e);
                }
            }
        }
    }
}

/// Result of running a complete workflow.
#[derive(Debug)]
pub struct WorkflowResult {
    /// Whether the workflow succeeded (verification passed)
    pub success: bool,
    /// Whether verification passed
    pub verification_passed: bool,
    /// All step results from all phases
    pub step_results: Vec<crate::step_executor::StepExecutionResult>,
    /// Total duration in milliseconds
    pub duration_ms: u64,
    /// Loop result (if the loop ran)
    pub loop_result: Option<LoopResult>,
}

impl WorkflowResult {
    /// Convert to ExecutionResult for API response
    pub fn to_execution_result(self) -> crate::step_executor::ExecutionResult {
        let successful = self.step_results.iter().filter(|r| r.success).count();
        let failed = self.step_results.iter().filter(|r| !r.success).count();

        crate::step_executor::ExecutionResult {
            success: self.success,
            total_steps: self.step_results.len(),
            successful_steps: successful,
            failed_steps: failed,
            total_duration_ms: self.duration_ms,
            steps: self.step_results,
            captured_logs: None,
            captured_runner_logs: None,
        }
    }
}
