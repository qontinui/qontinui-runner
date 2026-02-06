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
//! 5. Step events are logged through a centralized registry to prevent duplicates
//! 6. The loop respects external stop requests (via stop_ai_analysis endpoint)

use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

use crate::config_storage::ConfigStorage;
use crate::event_system::EventBroadcaster;
use crate::orchestrator::integration::StageTransition;
use crate::step_executor::ExecutionStepConfig;
use crate::step_registry::StepEventLogger;
use crate::workflow_state::{StateMachine, WorkflowState};
use crate::AppState;

use super::phases::{AgenticExecutor, CompletionExecutor, SetupExecutor, VerificationExecutor};
use super::resume::{ResumeManager, ResumePoint};
use super::states::UnifiedWorkflowState;
use super::types::{get_parent_task_id, AgenticOutcome, IterationResult, LoopConfig, LoopResult};

/// The main loop controller for unified workflows.
///
/// This controller manages the verification-agentic loop with strict guarantees:
/// - Verification is the ONLY authority on whether work is complete
/// - The AI cannot self-declare completion
/// - Completion phase only runs when verification passes
/// - Step events are logged through a centralized registry to prevent duplicates
pub struct LoopController {
    setup_executor: SetupExecutor,
    verification_executor: VerificationExecutor,
    agentic_executor: AgenticExecutor,
    completion_executor: CompletionExecutor,
    checkpoint_db: Arc<crate::database::CheckpointDb>,
    app_handle: tauri::AppHandle,
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
                app_handle.clone(),
                pid_tracker,
            ),
            checkpoint_db: app_state.checkpoint_db.clone(),
            app_handle,
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
    #[instrument(
        name = "workflow.execute",
        skip(self, setup_automation_steps, setup_prompt_steps, verification_steps, agentic_steps, completion_automation_steps, completion_prompt_steps),
        fields(
            execution_id = %config.execution_id,
            workflow_name = %config.workflow_name,
            max_iterations = config.max_iterations,
            setup_steps = setup_automation_steps.len() + setup_prompt_steps.len(),
            verification_steps = verification_steps.len(),
            agentic_steps = agentic_steps.len(),
            completion_steps = completion_automation_steps.len() + completion_prompt_steps.len()
        )
    )]
    pub async fn run(
        &mut self,
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

        // =====================================================================
        // DETERMINE RESUME POINT (must happen before transition loading)
        // =====================================================================
        let resume_manager = ResumeManager::new(self.checkpoint_db.clone());
        let resume_point = resume_manager
            .determine_resume_point(&config.execution_id)
            .unwrap_or_else(|e| {
                warn!(
                    "Failed to determine resume point: {} - starting from beginning",
                    e
                );
                ResumePoint::FromStart
            });

        info!("Resume point: {}", resume_point.description());

        // Stage transition tracking for recap timeline
        // Load existing transitions when resuming to preserve prior phase history
        let (mut transitions, mut current_stage) =
            if !matches!(resume_point, ResumePoint::FromStart) {
                // Try to load existing transition history from database
                if let Ok(Some(task_run)) = self.checkpoint_db.get_task_run(&config.execution_id) {
                    if let Some(ref history_json) = task_run.transition_history_json {
                        if let Ok(loaded_transitions) =
                            serde_json::from_str::<Vec<StageTransition>>(history_json)
                        {
                            // Get the last stage from transitions, or default to "init"
                            let last_stage = loaded_transitions
                                .last()
                                .map(|t| t.to.clone())
                                .unwrap_or_else(|| "init".to_string());
                            info!(
                                "Loaded {} existing transitions, current stage: {}",
                                loaded_transitions.len(),
                                last_stage
                            );
                            (loaded_transitions, last_stage)
                        } else {
                            (Vec::new(), "init".to_string())
                        }
                    } else {
                        (Vec::new(), "init".to_string())
                    }
                } else {
                    (Vec::new(), "init".to_string())
                }
            } else {
                // Fresh run - start with empty transitions
                (Vec::new(), "init".to_string())
            };

        info!(
            "=== UNIFIED WORKFLOW START: {} (id: {}) ===",
            config.workflow_name, config.execution_id
        );
        let timeout_str = match config.timeout_seconds {
            Some(t) => format!("{}s", t),
            None => "disabled".to_string(),
        };
        info!(
            "Configuration: max_iterations={}, timeout={}",
            config.max_iterations, timeout_str
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

        // Create centralized step event logger for this execution
        // This ensures consistent event format and prevents duplicate logging
        let logger = StepEventLogger::new(
            self.checkpoint_db.clone(),
            &config.execution_id,
            &config.workflow_name,
        );

        // Calculate effective starting iteration based on resume point
        let (
            skip_setup,
            effective_starting_iteration,
            run_agentic_first_from_resume,
            skip_to_completion,
        ) = match &resume_point {
            ResumePoint::FromStart => (false, 0, false, false),
            ResumePoint::SetupPhase { .. } => {
                // Resume in setup - re-run setup (could be smarter about partial resume)
                (false, 0, false, false)
            }
            ResumePoint::VerificationPhase { iteration, .. } => {
                // Skip setup, start verification at the given iteration
                // iteration is 1-indexed, starting_iteration is 0-indexed (number of completed iterations)
                (true, iteration.saturating_sub(1), false, false)
            }
            ResumePoint::AgenticPhase { iteration } => {
                // Skip setup, run agentic phase first, then continue verification loop
                // The agentic phase is at the given iteration, so we'll run it and then
                // continue with verification at iteration + 1
                (true, iteration.saturating_sub(1), true, false)
            }
            ResumePoint::CompletionPhase { .. } => {
                // Skip directly to completion - verification already passed
                (true, 0, false, true)
            }
        };

        // Determine if we should run agentic first:
        // - From resume: if we were interrupted during agentic phase
        // - From config: if this is an error-fix workflow (run_agentic_first=true in config)
        //   This ensures AI attempts to fix errors before verification runs, since log_watch
        //   verification may pass immediately if logs are currently clean.
        let run_agentic_first = run_agentic_first_from_resume
            || (matches!(resume_point, ResumePoint::FromStart) && config.run_agentic_first);

        // =====================================================================
        // CLEAR OLD DATA FOR FRESH RUNS
        // =====================================================================
        // When starting from the beginning (not resuming), clear any stale data
        // from previous interrupted runs to prevent data pollution.
        // This includes: checkpoints, transition history, verification results, workflow state
        if matches!(resume_point, ResumePoint::FromStart) {
            info!(
                "Fresh run - clearing old data for execution {}",
                config.execution_id
            );

            // Clear step checkpoints
            if let Err(e) = resume_manager.clear_all_checkpoints(&config.execution_id) {
                warn!("Failed to clear old checkpoints: {} - continuing anyway", e);
            }

            // Clear transition history
            if let Err(e) = self
                .checkpoint_db
                .update_task_run_transition_history(&config.execution_id, "[]")
            {
                warn!(
                    "Failed to clear transition history: {} - continuing anyway",
                    e
                );
            }

            // Clear verification phase results
            if let Err(e) = self
                .checkpoint_db
                .delete_verification_phase_results(&config.execution_id)
            {
                warn!(
                    "Failed to clear verification phase results: {} - continuing anyway",
                    e
                );
            }

            // Clear workflow execution state
            if let Err(e) = self
                .checkpoint_db
                .delete_workflow_execution_state(&config.execution_id)
            {
                warn!(
                    "Failed to clear workflow execution state: {} - continuing anyway",
                    e
                );
            }
        }

        // =====================================================================
        // PHASE 1: SETUP (runs once, unless resuming from later phase)
        // =====================================================================
        if !skip_setup {
            info!("=== PHASE 1: SETUP ===");

            // Persist workflow state: SetupRunning
            self.persist_workflow_state(
                &config.execution_id,
                &UnifiedWorkflowState::setup_running(),
            );

            self.record_stage_transition(
                &config.execution_id,
                &mut transitions,
                &mut current_stage,
                "setup",
                0,
            );

            let (setup_success, setup_results) = self
                .setup_executor
                .run_setup(
                    &setup_automation_steps,
                    &setup_prompt_steps,
                    &config.execution_id,
                    &config.workflow_name,
                    config.timeout_seconds,
                    &logger,
                )
                .await;
            all_step_results.extend(setup_results);

            if !setup_success {
                error!("Setup phase failed - aborting workflow");
                // Persist workflow state: Failed
                self.persist_workflow_state(
                    &config.execution_id,
                    &UnifiedWorkflowState::failed_in_phase("Setup phase failed", "setup", None),
                );
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

            // Persist workflow state: SetupComplete
            self.persist_workflow_state(
                &config.execution_id,
                &UnifiedWorkflowState::setup_complete(),
            );
        } else {
            info!("=== PHASE 1: SETUP SKIPPED (resuming from later phase) ===");
        }

        // =====================================================================
        // HANDLE AGENTIC PHASE RESUME (if we're resuming mid-agentic)
        // =====================================================================
        let loop_result = if run_agentic_first {
            // We're resuming in the middle of an agentic phase
            // Run the agentic phase first, then continue with verification loop
            let agentic_iteration = effective_starting_iteration + 1;
            info!(
                "=== RESUMING AGENTIC PHASE (iteration {}) ===",
                agentic_iteration
            );

            // Persist workflow state: AgenticRunning
            self.persist_workflow_state(
                &config.execution_id,
                &UnifiedWorkflowState::agentic_running(agentic_iteration),
            );

            self.record_stage_transition(
                &config.execution_id,
                &mut transitions,
                &mut current_stage,
                "agentic",
                agentic_iteration,
            );

            // Build context for agentic phase (we don't have the verification result,
            // so we'll use a generic context)
            let agentic_context = "Resuming from interrupted agentic phase. Please continue fixing the issues from where you left off.".to_string();

            let agentic_outcome = self
                .agentic_executor
                .run_agentic(
                    &config,
                    agentic_iteration,
                    &agentic_context,
                    !agentic_steps.is_empty(),
                    &logger,
                )
                .await;

            // Persist workflow state: AgenticComplete
            self.persist_workflow_state(
                &config.execution_id,
                &UnifiedWorkflowState::agentic_complete(agentic_iteration),
            );

            match &agentic_outcome {
                AgenticOutcome::Success { .. } => {
                    info!("Resumed agentic phase completed successfully");
                }
                AgenticOutcome::Failed { error, .. } => {
                    warn!(
                        "Resumed agentic phase failed: {}, but continuing with verification loop",
                        error
                    );
                }
                AgenticOutcome::Error { error } => {
                    warn!(
                        "Resumed agentic phase errored: {}, but continuing with verification loop",
                        error
                    );
                }
                AgenticOutcome::Skipped => {
                    info!("Resumed agentic phase was skipped (no agentic steps)");
                }
            }

            // Now continue with the verification loop from the next iteration
            let mut resumed_config = config.clone();
            resumed_config.starting_iteration = agentic_iteration; // Start from after the agentic phase

            info!("=== PHASE 2: VERIFICATION-AGENTIC LOOP (continuing after resumed agentic) ===");
            self.run_verification_agentic_loop(
                &resumed_config,
                &verification_steps,
                !agentic_steps.is_empty(),
                &mut all_step_results,
                &mut transitions,
                &mut current_stage,
                &logger,
            )
            .await
        } else if skip_to_completion {
            // Skip directly to completion - return a synthetic loop result indicating success
            info!("=== PHASE 2: VERIFICATION-AGENTIC LOOP SKIPPED (resuming to completion) ===");
            LoopResult {
                iterations_run: 0,         // We don't know how many iterations ran before
                verification_passed: true, // Must have passed if we're in completion phase
                max_iterations_reached: false,
                critical_failure: false,
                was_stopped: false,
                unfixable_errors: false,
                iteration_results: Vec::new(),
            }
        } else {
            // Normal flow - run the verification-agentic loop
            // Use effective_starting_iteration to resume from the right point
            let mut adjusted_config = config.clone();
            if effective_starting_iteration > 0 {
                adjusted_config.starting_iteration = effective_starting_iteration;
                info!(
                    "=== PHASE 2: VERIFICATION-AGENTIC LOOP (resuming from iteration {}) ===",
                    effective_starting_iteration + 1
                );
            } else {
                info!("=== PHASE 2: VERIFICATION-AGENTIC LOOP ===");
            }

            self.run_verification_agentic_loop(
                &adjusted_config,
                &verification_steps,
                !agentic_steps.is_empty(),
                &mut all_step_results,
                &mut transitions,
                &mut current_stage,
                &logger,
            )
            .await
        };

        info!("Loop completed: {}", loop_result.summary());

        // =====================================================================
        // PHASE 3: COMPLETION (only if verification passed!)
        // =====================================================================
        if loop_result.should_run_completion() {
            // Check if stopped before running completion phase
            if self.is_task_stopped(&config.execution_id) {
                warn!("Task was stopped before completion phase - aborting");
                // Persist workflow state: Stopped
                self.persist_workflow_state(
                    &config.execution_id,
                    &UnifiedWorkflowState::stopped_in_phase(
                        "verification",
                        Some(loop_result.iterations_run),
                    ),
                );
                return WorkflowResult {
                    success: false,
                    verification_passed: loop_result.verification_passed,
                    step_results: all_step_results,
                    duration_ms: start.elapsed().as_millis() as u64,
                    loop_result: Some(loop_result),
                };
            }

            info!("=== PHASE 3: COMPLETION (verification PASSED) ===");

            // Persist workflow state: CompletionRunning
            self.persist_workflow_state(
                &config.execution_id,
                &UnifiedWorkflowState::completion_running(),
            );

            self.record_stage_transition(
                &config.execution_id,
                &mut transitions,
                &mut current_stage,
                "completion",
                loop_result.iterations_run,
            );

            let (completion_success, completion_results) = self
                .completion_executor
                .run_completion(
                    &completion_automation_steps,
                    &completion_prompt_steps,
                    &config.execution_id,
                    &config.workflow_name,
                    config.timeout_seconds,
                    loop_result.iterations_run,
                    &logger,
                )
                .await;
            all_step_results.extend(completion_results);

            if !completion_success {
                warn!("Completion phase had failures, but verification passed so task is complete");
            }

            // Persist workflow state: CompletionComplete
            self.persist_workflow_state(
                &config.execution_id,
                &UnifiedWorkflowState::completion_complete(),
            );

            // Mark task as completed
            // Note: We mark as completed even for unfixable errors because the AI
            // did its job and determined the errors cannot be fixed automatically.
            self.mark_task_completed(&config.execution_id).await;

            // Resolve targeted errors on successful completion (verification passed)
            // Do NOT resolve errors if the AI signaled unfixable - they're still unresolved
            if loop_result.verification_passed && !config.targeted_error_ids.is_empty() {
                self.resolve_targeted_errors(&config.execution_id, &config.targeted_error_ids)
                    .await;
            }

            if loop_result.unfixable_errors {
                info!("=== WORKFLOW COMPLETED WITH UNFIXABLE ERRORS ===");
            } else {
                info!("=== WORKFLOW COMPLETED SUCCESSFULLY ===");
            }
        } else {
            info!("=== PHASE 3: COMPLETION SKIPPED (verification did not pass) ===");
            info!(
                "Reason: verification_passed={}, critical_failure={}, max_iterations_reached={}, was_stopped={}, unfixable_errors={}",
                loop_result.verification_passed,
                loop_result.critical_failure,
                loop_result.max_iterations_reached,
                loop_result.was_stopped,
                loop_result.unfixable_errors
            );

            // Mark task as failed (unless it was stopped - stop_ai_analysis already handled that)
            if loop_result.was_stopped {
                info!("=== WORKFLOW STOPPED BY USER ===");
                // Persist workflow state: Stopped
                self.persist_workflow_state(
                    &config.execution_id,
                    &UnifiedWorkflowState::stopped_in_phase(
                        "verification",
                        Some(loop_result.iterations_run),
                    ),
                );
                // Don't mark as failed - stop_ai_analysis already marked it as "stopped"
            } else {
                let reason = if loop_result.critical_failure {
                    "Critical verification failure"
                } else {
                    "Verification failed after max iterations"
                };
                // Persist workflow state: Failed
                self.persist_workflow_state(
                    &config.execution_id,
                    &UnifiedWorkflowState::failed_in_phase(
                        reason,
                        "verification",
                        Some(loop_result.iterations_run),
                    ),
                );
                self.mark_task_failed(&config.execution_id, reason).await;
                info!("=== WORKFLOW FAILED ===");
            }
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
    #[instrument(
        name = "workflow.verification_agentic_loop",
        skip(self, verification_steps, all_step_results, transitions, current_stage, logger),
        fields(
            execution_id = %config.execution_id,
            max_iterations = config.max_iterations,
            starting_iteration = config.starting_iteration,
            verification_step_count = verification_steps.len()
        )
    )]
    async fn run_verification_agentic_loop(
        &self,
        config: &LoopConfig,
        verification_steps: &[ExecutionStepConfig],
        has_agentic_steps: bool,
        all_step_results: &mut Vec<crate::step_executor::StepExecutionResult>,
        transitions: &mut Vec<StageTransition>,
        current_stage: &mut String,
        logger: &StepEventLogger,
    ) -> LoopResult {
        let mut iteration_results = Vec::new();
        // Start from the configured starting_iteration (for resume) or 0 (for fresh start)
        let mut iteration = config.starting_iteration;

        loop {
            iteration += 1;

            info!(
                "--- LOOP ITERATION {} of {} ---{}",
                iteration,
                config.max_iterations,
                if config.starting_iteration > 0 {
                    " (resumed)"
                } else {
                    ""
                }
            );

            // Check if the task has been stopped externally (e.g., user clicked Stop button)
            if self.is_task_stopped(&config.execution_id) {
                warn!("Task was stopped externally - exiting loop");
                return LoopResult {
                    iterations_run: iteration - 1,
                    verification_passed: false,
                    max_iterations_reached: false,
                    critical_failure: false,
                    was_stopped: true,
                    unfixable_errors: false,
                    iteration_results,
                };
            }

            // CRITICAL: Reset task status to "running" at the start of each iteration
            // This ensures that any external modifications (e.g., AI calling APIs) don't
            // prematurely mark the task as complete or failed. The loop controller is
            // the ONLY authority on task completion in unified workflows.
            if let Err(e) = self
                .checkpoint_db
                .update_task_run_status(&config.execution_id, "running")
            {
                warn!(
                    "Failed to reset task status to running for iteration {}: {}",
                    iteration, e
                );
            }

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
                    was_stopped: false,
                    unfixable_errors: false,
                    iteration_results,
                };
            }

            // Log iteration start to database
            // Use append_task_output_ex with check_completion_marker=false
            // because in unified workflows, VERIFICATION is the authority on completion,
            // not the [TASK_COMPLETE] marker from the AI
            let _ = self.checkpoint_db.append_task_output_ex(
                &config.execution_id,
                &format!(
                    "\n\n=== Verification-Agentic Loop: Iteration {} ===\n",
                    iteration
                ),
                false,
                false, // Don't check for completion marker - verification is the authority
            );

            // -----------------------------------------------------------------
            // VERIFICATION PHASE
            // -----------------------------------------------------------------
            info!("Running verification phase (iteration {})", iteration);

            // Persist workflow state: VerificationRunning
            self.persist_workflow_state(
                &config.execution_id,
                &UnifiedWorkflowState::verification_running(iteration),
            );

            self.record_stage_transition(
                &config.execution_id,
                transitions,
                current_stage,
                "verification",
                iteration,
            );

            let (verification_result, step_results) = self
                .verification_executor
                .run_verification(
                    verification_steps,
                    &config.execution_id,
                    iteration,
                    &config.workflow_name,
                    logger,
                )
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

            // Persist workflow state: VerificationComplete
            self.persist_workflow_state(
                &config.execution_id,
                &UnifiedWorkflowState::verification_complete(
                    iteration,
                    verification_result.all_passed,
                ),
            );

            // Check verification outcome
            if verification_result.all_passed {
                info!("*** VERIFICATION PASSED on iteration {} ***", iteration);

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
                    was_stopped: false,
                    unfixable_errors: false,
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
                    was_stopped: false,
                    unfixable_errors: false,
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

            // Persist workflow state: AgenticRunning
            self.persist_workflow_state(
                &config.execution_id,
                &UnifiedWorkflowState::agentic_running(iteration),
            );

            self.record_stage_transition(
                &config.execution_id,
                transitions,
                current_stage,
                "agentic",
                iteration,
            );

            let agentic_outcome = self
                .agentic_executor
                .run_agentic(
                    config,
                    iteration,
                    &failure_context,
                    has_agentic_steps,
                    logger,
                )
                .await;

            // Persist workflow state: AgenticComplete
            self.persist_workflow_state(
                &config.execution_id,
                &UnifiedWorkflowState::agentic_complete(iteration),
            );

            // Log agentic output to database
            // CRITICAL: Use append_task_output_ex with check_completion_marker=false
            // The AI may output [TASK_COMPLETE] but that does NOT mean verification passed!
            // In unified workflows, only verification passing can mark the task complete.
            // ALWAYS log and increment session count, even for Error outcomes, to prevent
            // the task from getting stuck with sessions_count=0 and status="running".
            {
                let output_text = match agentic_outcome.output() {
                    Some(text) => {
                        format!("\n--- AI Output (Iteration {}) ---\n{}\n", iteration, text)
                    }
                    None => format!(
                        "\n--- AI Output (Iteration {}) ---\n(no output - skipped)\n",
                        iteration
                    ),
                };
                let _ = self.checkpoint_db.append_task_output_ex(
                    &config.execution_id,
                    &output_text,
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

            // Log agentic outcome for debugging
            info!(
                "AGENTIC-OUTCOME: iteration={}, outcome={}",
                iteration,
                match &agentic_outcome {
                    AgenticOutcome::Success { .. } => "Success",
                    AgenticOutcome::Failed { .. } => "Failed",
                    AgenticOutcome::Error { error } => {
                        error!("AGENTIC-OUTCOME: Error details: {}", error);
                        "Error"
                    }
                    AgenticOutcome::Skipped => "Skipped",
                }
            );

            // Check if the AI signaled unfixable errors
            // This allows the AI to gracefully exit the loop when it determines
            // that the errors cannot be fixed (e.g., infrastructure issues,
            // missing dependencies, external service problems).
            if let Some(output) = agentic_outcome.output() {
                if output.contains("[UNFIXABLE_ERRORS]") || output.contains("[UNFIXABLE_ERROR]") {
                    warn!(
                        "AI signaled unfixable errors on iteration {} - exiting loop gracefully",
                        iteration
                    );

                    // Log the unfixable signal to the task output
                    let _ = self.checkpoint_db.append_task_output_ex(
                        &config.execution_id,
                        "\n=== AI SIGNALED UNFIXABLE ERRORS ===\nThe AI has determined that some errors cannot be fixed automatically. Proceeding to completion phase.\n",
                        false,
                        false,
                    );

                    return LoopResult {
                        iterations_run: iteration,
                        verification_passed: false,
                        max_iterations_reached: false,
                        critical_failure: false,
                        was_stopped: false,
                        unfixable_errors: true,
                        iteration_results,
                    };
                }
            }

            // Check if the task was stopped during the agentic phase
            if self.is_task_stopped(&config.execution_id) {
                warn!("Task was stopped during agentic phase - exiting loop");
                return LoopResult {
                    iterations_run: iteration,
                    verification_passed: false,
                    max_iterations_reached: false,
                    critical_failure: false,
                    was_stopped: true,
                    unfixable_errors: false,
                    iteration_results,
                };
            }

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

    /// Resolve targeted errors after successful workflow completion.
    ///
    /// This marks all errors that were targeted by the workflow as resolved,
    /// recording the task_run_id (execution_id) that fixed them for traceability.
    async fn resolve_targeted_errors(&self, execution_id: &str, error_ids: &[i64]) {
        info!(
            "Resolving {} targeted errors for successful workflow {}",
            error_ids.len(),
            execution_id
        );

        match self.checkpoint_db.connection() {
            Ok(conn) => {
                let mut resolved_count = 0;
                let mut failed_count = 0;

                for error_id in error_ids {
                    let resolution_note = format!(
                        "Auto-resolved by successful completion of workflow task {}",
                        execution_id
                    );

                    match crate::error_monitor::ErrorEventStorage::mark_resolved_by_task(
                        &conn,
                        *error_id,
                        execution_id,
                        Some(&resolution_note),
                    ) {
                        Ok(_) => {
                            resolved_count += 1;
                        }
                        Err(e) => {
                            failed_count += 1;
                            warn!("Failed to resolve error {}: {}", error_id, e);
                        }
                    }
                }

                if resolved_count > 0 {
                    info!(
                        "Successfully resolved {} errors (failed: {}) for workflow {}",
                        resolved_count, failed_count, execution_id
                    );
                }
            }
            Err(e) => {
                error!(
                    "Failed to get database connection for error resolution: {}",
                    e
                );
            }
        }
    }

    /// Check if the task has been stopped externally (via stop_ai_analysis endpoint).
    ///
    /// This allows the loop to gracefully abort when the user clicks the Stop button.
    fn is_task_stopped(&self, execution_id: &str) -> bool {
        // For workflow sequence children (e.g., workflow-sequence-X-workflow-N),
        // check the parent task instead since children don't have their own task_run records
        let task_id_to_check = get_parent_task_id(execution_id);

        match self.checkpoint_db.get_task_run(&task_id_to_check) {
            Ok(Some(task)) => {
                if task.status == "stopped" {
                    info!(
                        "Task {} has been stopped externally - aborting workflow",
                        task_id_to_check
                    );
                    true
                } else {
                    false
                }
            }
            Ok(None) => {
                // Only treat as stopped if this is not a sequence child
                // (sequence children are expected to not have task_run records)
                if task_id_to_check == execution_id {
                    warn!(
                        "Task {} not found in database - treating as stopped",
                        execution_id
                    );
                    true
                } else {
                    // Parent also not found - this shouldn't happen but continue anyway
                    warn!(
                        "Parent task {} not found for sequence child {} - continuing execution",
                        task_id_to_check, execution_id
                    );
                    false
                }
            }
            Err(e) => {
                warn!(
                    "Failed to check task {} status: {} - continuing execution",
                    task_id_to_check, e
                );
                false
            }
        }
    }

    /// Persist the workflow state to the database and broadcast a state change event.
    ///
    /// This creates or updates the workflow_execution_state record,
    /// enabling resume from the exact state after a restart.
    ///
    /// After successful persistence, broadcasts an `orchestrator-state-change` event
    /// to both Tauri frontend and WebSocket clients in real-time.
    ///
    /// Note: For workflow sequence children (e.g., workflow-sequence-X-workflow-N),
    /// we skip state persistence since they don't have task_run records.
    /// The parent sequence task_run tracks overall status.
    fn persist_workflow_state(&self, execution_id: &str, state: &UnifiedWorkflowState) {
        // Skip persistence for workflow sequence children - they don't have task_run records
        // and would fail the foreign key constraint
        let parent_id = get_parent_task_id(execution_id);
        if parent_id != execution_id {
            debug!(
                "Skipping state persistence for sequence child {} (parent: {})",
                execution_id, parent_id
            );
            return;
        }

        let state_machine = StateMachine::new(
            self.checkpoint_db.clone(),
            execution_id,
            "unified",
            state.clone(),
        );

        if let Err(e) = state_machine.persist() {
            warn!(
                "Failed to persist workflow state for {}: {}",
                execution_id, e
            );
        } else {
            debug!(
                "Persisted workflow state '{}' for execution {}",
                state.name(),
                execution_id
            );

            // Broadcast real-time event to notify all clients (Tauri + WebSocket) of state change
            let broadcaster = EventBroadcaster::new(self.app_handle.clone());
            broadcaster.orchestrator_state_change(
                execution_id,
                state.name(),
                state.iteration().unwrap_or(0),
                state.phase().unwrap_or("unknown"),
            );

            debug!(
                "Broadcast orchestrator-state-change event for {} (state: {})",
                execution_id,
                state.name()
            );
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
    #[allow(clippy::wrong_self_convention)]
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
            verification_passed: Some(self.verification_passed),
            loop_result: self.loop_result,
            task_summary: None, // Will be set by caller if available
        }
    }
}

/// Configuration for workflow resume on startup.
pub struct ResumeConfig {
    /// Whether to attempt resume of interrupted workflows (default: true)
    pub resume_enabled: bool,
}

impl Default for ResumeConfig {
    fn default() -> Self {
        Self {
            resume_enabled: true,
        }
    }
}

/// Resume interrupted unified workflows on startup.
///
/// This function should be called when the runner starts to handle any
/// workflows that were running when the runner was last stopped.
///
/// If `resume_enabled` is true (default), interrupted workflows will be resumed
/// from where they left off. If false, they will be marked as failed.
///
/// Returns the number of workflows that were processed (resumed or marked failed).
pub async fn resume_interrupted_workflows(
    db: Arc<crate::database::CheckpointDb>,
    app_state: Arc<AppState>,
    config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
    app_handle: tauri::AppHandle,
    pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    config: ResumeConfig,
) -> usize {
    // Get all running unified workflows
    let running_workflows = match db.get_running_unified_workflows() {
        Ok(workflows) => workflows,
        Err(e) => {
            warn!("Failed to query running unified workflows: {}", e);
            return 0;
        }
    };

    if running_workflows.is_empty() {
        info!("No interrupted unified workflows found to resume");
        return 0;
    }

    info!(
        "Found {} interrupted unified workflow(s) to {}",
        running_workflows.len(),
        if config.resume_enabled {
            "resume"
        } else {
            "mark as failed"
        }
    );

    let mut processed_count = 0;

    for task_run in &running_workflows {
        // Check per-task auto_continue setting - this determines whether to resume on startup
        // (auto_continue does NOT affect continuation after steps, only startup resume)
        let should_resume = config.resume_enabled && task_run.auto_continue;

        if !should_resume {
            // Mark as failed instead of resuming
            let reason = if !config.resume_enabled {
                "resume disabled globally"
            } else {
                "auto_continue disabled for this task"
            };
            info!(
                "Marking interrupted workflow '{}' (id: {}) as failed ({})",
                task_run.task_name, task_run.id, reason
            );
            if let Err(e) = db.mark_interrupted_workflow_failed(&task_run.id) {
                error!("Failed to mark workflow {} as failed: {}", task_run.id, e);
            } else {
                processed_count += 1;
            }
            continue;
        }

        // Extract workflow ID from task_run.id (format: unified-workflow-{uuid}-{timestamp})
        let workflow_id = extract_workflow_id_from_task_id(&task_run.id);

        if let Some(wf_id) = workflow_id {
            info!(
                "Resuming interrupted workflow '{}' (task_id: {}, workflow_id: {})",
                task_run.task_name, task_run.id, wf_id
            );

            // Fetch the workflow definition
            match db.get_unified_workflow(&wf_id) {
                Ok(Some(workflow)) => {
                    // Spawn the workflow resume in a background task with panic protection
                    let task_id = task_run.id.clone();
                    let task_name = task_run.task_name.clone();
                    let starting_iteration = task_run.sessions_count; // Resume from after completed iterations
                    let checkpoint_db_for_guard = db.clone();

                    // Capture values needed inside the async block
                    let app_state_for_spawn = app_state.clone();
                    let config_storage_for_spawn = config_storage.clone();
                    let app_handle_for_spawn = app_handle.clone();
                    let pid_tracker_for_spawn = pid_tracker.clone();
                    let wf_id_for_spawn = wf_id.clone();

                    super::spawn_workflow_with_panic_guard(
                        checkpoint_db_for_guard,
                        task_id.clone(),
                        task_name.clone(),
                        async move {
                            let mut controller = LoopController::new(
                                app_state_for_spawn,
                                config_storage_for_spawn,
                                app_handle_for_spawn,
                                pid_tracker_for_spawn,
                            );

                            info!(
                                "Starting resume of workflow '{}' from iteration {}",
                                task_name,
                                starting_iteration + 1
                            );

                            // Build execution steps from workflow definition with explicit phase assignment
                            let setup_automation_steps = convert_json_steps_with_phase(
                                &workflow.setup_steps,
                                0,
                                Some("setup"),
                            );
                            // Prepend pre-flight check if enabled (default: true)
                            // Pre-flight checks run FIRST to catch environment issues early
                            let setup_automation_steps =
                                crate::unified_workflows::prepend_preflight_check_step(
                                    setup_automation_steps,
                                    workflow.preflight_check_enabled,
                                );
                            let setup_prompt_steps = extract_prompt_steps_with_phase(
                                &workflow.setup_steps,
                                Some("setup"),
                            );
                            let verification_steps = convert_json_steps_with_phase(
                                &workflow.verification_steps,
                                0,
                                Some("verification"),
                            );
                            // Prepend health check steps if enabled (default: true)
                            // Health checks run BEFORE log_watch to catch server down before scanning logs
                            let verification_steps =
                                crate::unified_workflows::prepend_health_check_steps(
                                    verification_steps,
                                    workflow.health_check_enabled,
                                    &workflow.health_check_urls,
                                );
                            // Prepend log_watch step if enabled (default: true)
                            let verification_steps =
                                crate::unified_workflows::prepend_log_watch_step(
                                    verification_steps,
                                    workflow.log_watch_enabled,
                                );
                            // Agentic steps are prompt-type steps, use extract_prompt_steps_with_phase
                            // (convert_json_steps_with_phase filters out prompt steps)
                            let agentic_steps = extract_prompt_steps_with_phase(
                                &workflow.agentic_steps,
                                Some("agentic"),
                            );
                            let completion_automation_steps = convert_json_steps_with_phase(
                                &workflow.completion_steps,
                                0,
                                Some("completion"),
                            );
                            let completion_prompt_steps = extract_prompt_steps_with_phase(
                                &workflow.completion_steps,
                                Some("completion"),
                            );

                            // For error-fix workflows, run agentic first (only if starting fresh)
                            let run_agentic_first =
                                !workflow.targeted_error_ids.is_empty() && starting_iteration == 0;

                            let loop_config = super::types::LoopConfig {
                                max_iterations: workflow.max_iterations,
                                timeout_seconds: workflow.timeout_seconds, // Use workflow setting
                                base_prompt: String::new(),                // Not used for resume
                                workflow_name: task_name.clone(),
                                workflow_id: wf_id_for_spawn.to_string(),
                                execution_id: task_id.clone(),
                                targeted_error_ids: workflow.targeted_error_ids.clone(),
                                starting_iteration,
                                run_agentic_first,
                            };

                            controller
                                .run(
                                    loop_config,
                                    setup_automation_steps,
                                    setup_prompt_steps,
                                    verification_steps,
                                    agentic_steps,
                                    completion_automation_steps,
                                    completion_prompt_steps,
                                )
                                .await
                        },
                    );

                    processed_count += 1;
                }
                Ok(None) => {
                    warn!(
                        "Workflow definition {} not found for task {} - preserving 'running' status for manual investigation",
                        wf_id, task_run.id
                    );
                    // Don't mark as failed - the workflow definition might have been deleted
                    // but the task_run should be manually resolved by the user
                }
                Err(e) => {
                    error!("Failed to fetch workflow {} for resume: {}", wf_id, e);
                }
            }
        } else if let Some(ref wf_name) = task_run.workflow_name {
            // New format: workflow-sequence-{timestamp} or workflow-sequence-{timestamp}-workflow-{n}
            // Look up workflow by name instead
            info!(
                "Looking up workflow by name '{}' for task_id: {}",
                wf_name, task_run.id
            );

            match db.get_unified_workflow_by_name(wf_name) {
                Ok(Some(workflow)) => {
                    // Spawn the workflow resume in a background task with panic protection
                    let task_id = task_run.id.clone();
                    let task_name = task_run.task_name.clone();
                    let wf_id = workflow.id.clone();
                    let starting_iteration = task_run.sessions_count;
                    let checkpoint_db_for_guard = db.clone();

                    // Capture values needed inside the async block
                    let app_state_for_spawn = app_state.clone();
                    let config_storage_for_spawn = config_storage.clone();
                    let app_handle_for_spawn = app_handle.clone();
                    let pid_tracker_for_spawn = pid_tracker.clone();

                    info!(
                        "Resuming workflow '{}' (task_id: {}, workflow_id: {}, iteration: {})",
                        task_name,
                        task_id,
                        wf_id,
                        starting_iteration + 1
                    );

                    super::spawn_workflow_with_panic_guard(
                        checkpoint_db_for_guard,
                        task_id.clone(),
                        task_name.clone(),
                        async move {
                            let mut controller = LoopController::new(
                                app_state_for_spawn,
                                config_storage_for_spawn,
                                app_handle_for_spawn,
                                pid_tracker_for_spawn,
                            );

                            // Build execution steps from workflow definition with explicit phase assignment
                            let setup_automation_steps = convert_json_steps_with_phase(
                                &workflow.setup_steps,
                                0,
                                Some("setup"),
                            );
                            // Prepend pre-flight check if enabled (default: true)
                            let setup_automation_steps =
                                crate::unified_workflows::prepend_preflight_check_step(
                                    setup_automation_steps,
                                    workflow.preflight_check_enabled,
                                );
                            let setup_prompt_steps = extract_prompt_steps_with_phase(
                                &workflow.setup_steps,
                                Some("setup"),
                            );
                            let verification_steps = convert_json_steps_with_phase(
                                &workflow.verification_steps,
                                0,
                                Some("verification"),
                            );
                            let verification_steps =
                                crate::unified_workflows::prepend_health_check_steps(
                                    verification_steps,
                                    workflow.health_check_enabled,
                                    &workflow.health_check_urls,
                                );
                            let verification_steps =
                                crate::unified_workflows::prepend_log_watch_step(
                                    verification_steps,
                                    workflow.log_watch_enabled,
                                );
                            let agentic_steps = extract_prompt_steps_with_phase(
                                &workflow.agentic_steps,
                                Some("agentic"),
                            );
                            let completion_automation_steps = convert_json_steps_with_phase(
                                &workflow.completion_steps,
                                0,
                                Some("completion"),
                            );
                            let completion_prompt_steps = extract_prompt_steps_with_phase(
                                &workflow.completion_steps,
                                Some("completion"),
                            );

                            // For error-fix workflows, run agentic first (only if starting fresh)
                            let run_agentic_first =
                                !workflow.targeted_error_ids.is_empty() && starting_iteration == 0;

                            let loop_config = super::types::LoopConfig {
                                max_iterations: workflow.max_iterations,
                                timeout_seconds: workflow.timeout_seconds,
                                base_prompt: String::new(),
                                workflow_name: task_name.clone(),
                                workflow_id: wf_id.to_string(),
                                execution_id: task_id.clone(),
                                targeted_error_ids: workflow.targeted_error_ids.clone(),
                                starting_iteration,
                                run_agentic_first,
                            };

                            controller
                                .run(
                                    loop_config,
                                    setup_automation_steps,
                                    setup_prompt_steps,
                                    verification_steps,
                                    agentic_steps,
                                    completion_automation_steps,
                                    completion_prompt_steps,
                                )
                                .await
                        },
                    );

                    processed_count += 1;
                }
                Ok(None) => {
                    warn!(
                        "Workflow definition not found by name '{}' for task {} - preserving 'running' status",
                        wf_name, task_run.id
                    );
                }
                Err(e) => {
                    error!(
                        "Failed to fetch workflow by name '{}' for resume: {}",
                        wf_name, e
                    );
                }
            }
        } else {
            warn!(
                "Could not extract workflow ID from task_id '{}' and no workflow_name set - preserving 'running' status",
                task_run.id
            );
        }
    }

    processed_count
}

/// Extract workflow ID from task_run.id (format: unified-workflow-{uuid}-{timestamp})
pub fn extract_workflow_id_from_task_id(task_id: &str) -> Option<String> {
    // Format: unified-workflow-{uuid part 1}-{uuid part 2}-{uuid part 3}-{uuid part 4}-{uuid part 5}-{timestamp}
    // Example: unified-workflow-12345678-1234-5678-1234-567812345678-20250101120000
    if !task_id.starts_with("unified-workflow-") {
        return None;
    }

    let parts: Vec<&str> = task_id.split('-').collect();
    // "unified" = 0, "workflow" = 1, then 5 UUID parts, then timestamp
    if parts.len() >= 7 {
        // UUID is parts 2-6 (indices 2,3,4,5,6)
        let uuid = format!(
            "{}-{}-{}-{}-{}",
            parts[2], parts[3], parts[4], parts[5], parts[6]
        );
        Some(uuid)
    } else {
        None
    }
}

/// Convert JSON Value steps to ExecutionStepConfig (excluding prompt steps)
/// Prompt steps are handled separately by extract_prompt_steps_from_json
///
/// If `explicit_phase` is provided, it will be set on all steps that don't
/// already have a phase specified. This ensures steps from setup_steps array
/// get phase="setup", etc.
pub fn convert_json_steps_to_execution_steps(
    steps: &[serde_json::Value],
    monitor: i32,
) -> Vec<ExecutionStepConfig> {
    convert_json_steps_with_phase(steps, monitor, None)
}

/// Convert JSON Value steps to ExecutionStepConfig with explicit phase.
///
/// Sets the explicit phase on all steps that don't already have one.
/// This is the preferred function for unified workflow execution.
pub fn convert_json_steps_with_phase(
    steps: &[serde_json::Value],
    monitor: i32,
    explicit_phase: Option<&str>,
) -> Vec<ExecutionStepConfig> {
    use crate::step_executor::StepPhase;

    steps
        .iter()
        // Filter out prompt steps - they're handled separately to avoid duplicate logging
        .filter(|step| {
            let step_type = step.get("type").and_then(|t| t.as_str()).unwrap_or("");
            step_type != "prompt" && step_type != "ai_session"
        })
        .filter_map(|step| {
            let mut config = if let Ok(mut config) =
                serde_json::from_value::<ExecutionStepConfig>(step.clone())
            {
                if config.monitor_index.is_none() {
                    config.monitor_index = Some(monitor);
                }
                config
            } else {
                // Fall back to minimal conversion
                let step_type = step.get("type").and_then(|t| t.as_str())?;
                ExecutionStepConfig {
                    step_type: step_type.to_string(),
                    name: step
                        .get("name")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string()),
                    monitor_index: Some(monitor),
                    ..Default::default()
                }
            };

            // Set explicit phase if not already set
            if config.phase.is_none() {
                if let Some(phase_str) = explicit_phase {
                    if let Some(phase) = StepPhase::from_str_opt(phase_str) {
                        config.set_phase(phase);
                    }
                }
            }

            Some(config)
        })
        .collect()
}

/// Extract prompt steps from JSON Value array
///
/// If `explicit_phase` is provided, it will be set on all steps that don't
/// already have a phase specified.
pub fn extract_prompt_steps_from_json(steps: &[serde_json::Value]) -> Vec<ExecutionStepConfig> {
    extract_prompt_steps_with_phase(steps, None)
}

/// Extract prompt steps with explicit phase.
pub fn extract_prompt_steps_with_phase(
    steps: &[serde_json::Value],
    explicit_phase: Option<&str>,
) -> Vec<ExecutionStepConfig> {
    use crate::step_executor::StepPhase;

    steps
        .iter()
        .filter(|step| {
            step.get("type")
                .and_then(|t| t.as_str())
                .map(|t| t == "prompt")
                .unwrap_or(false)
        })
        .filter_map(|step| {
            let mut config = serde_json::from_value::<ExecutionStepConfig>(step.clone()).ok()?;

            // Set explicit phase if not already set
            if config.phase.is_none() {
                if let Some(phase_str) = explicit_phase {
                    if let Some(phase) = StepPhase::from_str_opt(phase_str) {
                        config.set_phase(phase);
                    }
                }
            }

            Some(config)
        })
        .collect()
}
