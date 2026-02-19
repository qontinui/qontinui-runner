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
use tauri::Manager;
use tracing::{debug, error, info, instrument, warn};

use crate::config_storage::ConfigStorage;
use crate::doctor::DoctorHandle;
use crate::event_system::EventBroadcaster;
use crate::orchestrator::integration::StageTransition;
use crate::orchestrator::knowledge::{parse_findings_from_output, AgentType, KnowledgeBase};
use crate::step_executor::ExecutionStepConfig;
use crate::step_registry::StepEventLogger;
use crate::summary_generator::generate_task_summary_async;
use crate::workflow_state::{StateMachine, WorkflowState};
use crate::AppState;

use super::phases::{AgenticExecutor, CompletionExecutor, SetupExecutor, VerificationExecutor};
use super::resume::{ResumeManager, ResumePoint};
use super::states::UnifiedWorkflowState;
use super::types::{
    get_parent_task_id, AgenticOutcome, IterationResult, LoopConfig, LoopResult, SweepResult,
};

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
    knowledge_base: KnowledgeBase,
    app_state: Arc<AppState>,
    config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
    app_handle: tauri::AppHandle,
    pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    doctor_handle: Option<DoctorHandle>,
    reflection_fix_ctx: Option<crate::mcp::shared::ReflectionFixContext>,
    step_injection_ctx: Option<crate::step_injection::types::StepInjectionContext>,
}

impl LoopController {
    pub fn new(
        app_state: Arc<AppState>,
        config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
        pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    ) -> Self {
        let doctor_handle = app_state
            .doctor_handle
            .try_lock()
            .ok()
            .and_then(|guard| guard.clone());
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
                config_storage.clone(),
                app_handle.clone(),
                pid_tracker.clone(),
            ),
            checkpoint_db: app_state.checkpoint_db.clone(),
            knowledge_base: KnowledgeBase::new(app_state.checkpoint_db.clone()),
            app_state,
            config_storage,
            app_handle,
            pid_tracker,
            doctor_handle,
            reflection_fix_ctx: None,
            step_injection_ctx: None,
        }
    }

    /// Enable interactive sessions on all phase executors via the session manager.
    pub fn with_session_manager(mut self, sm: Arc<crate::claude_session::SessionManager>) -> Self {
        self.setup_executor.set_session_manager(sm.clone());
        self.agentic_executor.set_session_manager(sm.clone());
        self.completion_executor.set_session_manager(sm);
        self
    }

    /// Set the reflection fix context for parsing [REFLECTION_FIX:...] markers during agentic phase.
    pub fn with_reflection_fix_ctx(
        mut self,
        ctx: crate::mcp::shared::ReflectionFixContext,
    ) -> Self {
        self.agentic_executor.set_reflection_fix_ctx(ctx.clone());
        self.reflection_fix_ctx = Some(ctx);
        self
    }

    /// Set the step injection context for parsing [INJECT_STEP]...[/INJECT_STEP] markers during agentic phase.
    pub fn with_step_injection_ctx(
        mut self,
        ctx: crate::step_injection::types::StepInjectionContext,
    ) -> Self {
        self.agentic_executor.set_step_injection_ctx(ctx.clone());
        self.step_injection_ctx = Some(ctx);
        self
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
        mut config: LoopConfig,
        mut setup_automation_steps: Vec<ExecutionStepConfig>,
        mut setup_prompt_steps: Vec<ExecutionStepConfig>,
        mut verification_steps: Vec<ExecutionStepConfig>,
        mut agentic_steps: Vec<ExecutionStepConfig>,
        mut completion_automation_steps: Vec<ExecutionStepConfig>,
        mut completion_prompt_steps: Vec<ExecutionStepConfig>,
    ) -> WorkflowResult {
        let start = std::time::Instant::now();
        let mut all_step_results = Vec::new();

        // =====================================================================
        // PROPAGATE TASK RUN ID to phase executors
        // =====================================================================
        // The execution_id is the task_run_id. Phase executors need it so their
        // StepExecutor/HandlerContext can write result_data (e.g. generated_workflow_id).
        self.setup_executor
            .set_task_run_id(config.execution_id.clone());
        self.verification_executor
            .set_task_run_id(config.execution_id.clone());
        self.completion_executor
            .set_task_run_id(config.execution_id.clone());

        // =====================================================================
        // CREATE ARTIFACT DIRECTORY
        // =====================================================================
        if config.artifact_dir.is_none() {
            if let Ok(app_data_dir) = self.app_handle.path().app_data_dir() {
                let artifact_dir = app_data_dir.join("artifacts").join(&config.execution_id);
                if let Err(e) = std::fs::create_dir_all(&artifact_dir) {
                    warn!(
                        "Failed to create artifact directory {:?}: {}",
                        artifact_dir, e
                    );
                } else {
                    info!("Artifact directory: {:?}", artifact_dir);
                    config.artifact_dir = Some(artifact_dir);
                }
            }
        }

        // =====================================================================
        // APPLY VARIABLE SUBSTITUTION to all step fields
        // =====================================================================
        if let Some(ref artifact_dir) = config.artifact_dir {
            let artifact_dir_str = artifact_dir.to_string_lossy().replace('\\', "/");
            let exec_id = &config.execution_id;
            let substitute = |steps: &mut Vec<ExecutionStepConfig>| {
                for step in steps.iter_mut() {
                    substitute_step_vars(step, &artifact_dir_str, exec_id);
                }
            };
            substitute(&mut setup_automation_steps);
            substitute(&mut setup_prompt_steps);
            substitute(&mut verification_steps);
            substitute(&mut agentic_steps);
            substitute(&mut completion_automation_steps);
            substitute(&mut completion_prompt_steps);
            info!(
                "Applied variable substitution (artifact_dir={}, execution_id={})",
                artifact_dir_str, exec_id
            );
        }

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
        info!("Configuration: max_iterations={}", config.max_iterations);
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
                    &logger,
                )
                .await;

            // Append setup phase results to output_log so output is visible immediately
            {
                let mut setup_output = format!(
                    "\n=== Setup Phase ===\nSteps: {}\nSuccess: {}\n",
                    setup_results.len(),
                    setup_success,
                );
                for sr in &setup_results {
                    setup_output.push_str(&format!(
                        "  [{}] {} - {} ({}ms)\n",
                        if sr.success { "OK" } else { "FAIL" },
                        sr.step_type,
                        sr.step_name,
                        sr.duration_ms,
                    ));
                    if let Some(ref details) = sr.verification_details {
                        if let Some(ref stdout) = details.stdout {
                            if !stdout.is_empty() {
                                setup_output.push_str(&format!("    stdout: {}\n", stdout));
                            }
                        }
                        if let Some(ref stderr) = details.stderr {
                            if !stderr.is_empty() {
                                setup_output.push_str(&format!("    stderr: {}\n", stderr));
                            }
                        }
                    }
                    if let Some(ref err) = sr.error {
                        setup_output.push_str(&format!("    error: {}\n", err));
                    }
                }
                let _ = self.checkpoint_db.append_task_output_ex(
                    &config.execution_id,
                    &setup_output,
                    false, // don't increment session count for setup
                    false,
                );
            }

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

                // Fire-and-forget summary generation for the failed task
                let db = self.checkpoint_db.clone();
                let exec_id = config.execution_id.clone();
                let doctor_handle = self.doctor_handle.clone();
                tokio::spawn(async move {
                    match generate_task_summary_async(db, exec_id.clone(), doctor_handle).await {
                        Ok(_) => info!("Generated summary for failed task {}", exec_id),
                        Err(e) => warn!(
                            "Failed to generate summary for failed task {}: {}",
                            exec_id, e
                        ),
                    }
                });

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
        // EXPAND RUNTIME VARIABLES in prompts from setup phase outputs
        // =====================================================================
        // Setup API steps store their outputs in the setup executor's SharedVariableStore.
        // These need to be substituted into {{variable_name}} patterns in:
        // - config.base_prompt (agentic phase prompt)
        // - completion prompt step content
        {
            let shared_vars = self.setup_executor.shared_variables().get_all();
            if !shared_vars.is_empty() {
                info!(
                    "Expanding {} runtime variables from setup phase into prompts",
                    shared_vars.len()
                );
                for (name, value) in &shared_vars {
                    let pattern = format!("{{{{{}}}}}", name);
                    if config.base_prompt.contains(&pattern) {
                        info!(
                            "  Substituting {{{{{}}}}} ({} chars) into base_prompt",
                            name,
                            value.len()
                        );
                        config.base_prompt = config.base_prompt.replace(&pattern, value);
                    }
                }
                // Also substitute into completion prompt step content
                for step in completion_prompt_steps.iter_mut() {
                    if let Some(ref mut content) = step.prompt_content {
                        for (name, value) in &shared_vars {
                            let pattern = format!("{{{{{}}}}}", name);
                            if content.contains(&pattern) {
                                *content = content.replace(&pattern, value);
                            }
                        }
                    }
                }
            }
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

            // Build context for agentic phase from stored verification data
            let agentic_context = build_resume_agentic_context(
                &self.checkpoint_db,
                &config.execution_id,
                agentic_iteration,
            );

            let (agentic_outcome, pre_loop_injected_steps) = self
                .agentic_executor
                .run_agentic(
                    &config,
                    agentic_iteration,
                    &agentic_context,
                    !agentic_steps.is_empty() || !config.base_prompt.is_empty(),
                    &agentic_steps,
                    &logger,
                )
                .await;

            if !pre_loop_injected_steps.is_empty() {
                info!(
                    "Pre-loop agentic phase injected {} dynamic verification step(s)",
                    pre_loop_injected_steps.len()
                );
            }

            // Persist workflow state: AgenticComplete
            self.persist_workflow_state(
                &config.execution_id,
                &UnifiedWorkflowState::agentic_complete(agentic_iteration),
            );

            // Log agentic output to database and increment session count
            {
                let output_text = match &agentic_outcome {
                    AgenticOutcome::Success { output } => {
                        info!("Resumed agentic phase completed successfully");
                        format!(
                            "\n=== Agentic Phase (iteration {}) ===\n{}",
                            agentic_iteration, output
                        )
                    }
                    AgenticOutcome::Failed { output, error } => {
                        warn!(
                            "Resumed agentic phase failed: {}, but continuing with verification loop",
                            error
                        );
                        format!(
                            "\n=== Agentic Phase (iteration {}, FAILED: {}) ===\n{}",
                            agentic_iteration, error, output
                        )
                    }
                    AgenticOutcome::Error { error } => {
                        warn!(
                            "Resumed agentic phase errored: {}, but continuing with verification loop",
                            error
                        );
                        format!(
                            "\n=== Agentic Phase (iteration {}, ERROR: {}) ===\n",
                            agentic_iteration, error
                        )
                    }
                    AgenticOutcome::Skipped => {
                        info!("Resumed agentic phase was skipped (no agentic steps)");
                        String::new()
                    }
                };
                if !output_text.is_empty() {
                    let _ = self.checkpoint_db.append_task_output_ex(
                        &config.execution_id,
                        &output_text,
                        true,  // increment session count
                        false, // Don't check for completion marker
                    );
                }
            }

            // Now continue with the verification loop from the next iteration
            let mut resumed_config = config.clone();
            resumed_config.starting_iteration = agentic_iteration; // Start from after the agentic phase

            info!("=== PHASE 2: VERIFICATION-AGENTIC LOOP (continuing after resumed agentic) ===");
            self.run_verification_agentic_loop(
                &resumed_config,
                &verification_steps,
                !agentic_steps.is_empty() || !resumed_config.base_prompt.is_empty(),
                &agentic_steps,
                &mut all_step_results,
                &mut transitions,
                &mut current_stage,
                &logger,
                pre_loop_injected_steps,
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
                !agentic_steps.is_empty() || !adjusted_config.base_prompt.is_empty(),
                &agentic_steps,
                &mut all_step_results,
                &mut transitions,
                &mut current_stage,
                &logger,
                Vec::new(), // No initial dynamic steps for normal flow
            )
            .await
        };

        info!("Loop completed: {}", loop_result.summary());

        // =====================================================================
        // PHASE 2.5: COMPLETION SWEEP (optional, runs after verification passes)
        // =====================================================================
        if config.enable_sweep
            && loop_result.verification_passed
            && !self.is_task_stopped(&config.execution_id)
        {
            info!(
                "=== PHASE 2.5: COMPLETION SWEEP (max {} iterations) ===",
                config.max_sweep_iterations
            );

            let sweep_result = self
                .run_sweep_loop(
                    &config,
                    &mut all_step_results,
                    &logger,
                    loop_result.iterations_run,
                )
                .await;

            info!(
                "Sweep completed: {} iteration(s), no_more_steps={}",
                sweep_result.iterations_run, sweep_result.no_more_steps
            );
        }

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
                    loop_result.iterations_run,
                    &logger,
                )
                .await;

            // Append completion phase results to output_log
            {
                let mut completion_output = format!(
                    "\n=== Completion Phase ===\nSteps: {}\nSuccess: {}\n",
                    completion_results.len(),
                    completion_success,
                );
                for sr in &completion_results {
                    completion_output.push_str(&format!(
                        "  [{}] {} - {} ({}ms)\n",
                        if sr.success { "OK" } else { "FAIL" },
                        sr.step_type,
                        sr.step_name,
                        sr.duration_ms,
                    ));
                    if let Some(ref details) = sr.verification_details {
                        if let Some(ref stdout) = details.stdout {
                            if !stdout.is_empty() {
                                completion_output.push_str(&format!("    stdout: {}\n", stdout));
                            }
                        }
                        if let Some(ref stderr) = details.stderr {
                            if !stderr.is_empty() {
                                completion_output.push_str(&format!("    stderr: {}\n", stderr));
                            }
                        }
                    }
                    if let Some(ref err) = sr.error {
                        completion_output.push_str(&format!("    error: {}\n", err));
                    }
                }
                let _ = self.checkpoint_db.append_task_output_ex(
                    &config.execution_id,
                    &completion_output,
                    false, // don't increment session count for completion
                    false,
                );
            }

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
            self.mark_task_completed(&config.execution_id, Some(&config.workflow_id))
                .await;

            // Fire-and-forget summary generation for the completed task
            let db = self.checkpoint_db.clone();
            let exec_id = config.execution_id.clone();
            let doctor_handle = self.doctor_handle.clone();
            tokio::spawn(async move {
                match generate_task_summary_async(db, exec_id.clone(), doctor_handle).await {
                    Ok(_) => info!("Generated summary for completed task {}", exec_id),
                    Err(e) => warn!(
                        "Failed to generate summary for completed task {}: {}",
                        exec_id, e
                    ),
                }
            });

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

                // Fire-and-forget summary generation for stopped task
                let db = self.checkpoint_db.clone();
                let exec_id = config.execution_id.clone();
                let doctor_handle = self.doctor_handle.clone();
                tokio::spawn(async move {
                    match generate_task_summary_async(db, exec_id.clone(), doctor_handle).await {
                        Ok(_) => info!("Generated summary for stopped task {}", exec_id),
                        Err(e) => warn!(
                            "Failed to generate summary for stopped task {}: {}",
                            exec_id, e
                        ),
                    }
                });
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

                // Fire-and-forget summary generation for failed task
                let db = self.checkpoint_db.clone();
                let exec_id = config.execution_id.clone();
                let doctor_handle = self.doctor_handle.clone();
                tokio::spawn(async move {
                    match generate_task_summary_async(db, exec_id.clone(), doctor_handle).await {
                        Ok(_) => info!("Generated summary for failed task {}", exec_id),
                        Err(e) => warn!(
                            "Failed to generate summary for failed task {}: {}",
                            exec_id, e
                        ),
                    }
                });

                info!("=== WORKFLOW FAILED ===");
            }
        }

        // Record learning outcome for meta-workflows (fire-and-forget)
        if config.workflow_name.starts_with("AI Generate:") || config.is_dev_mode {
            let db = self.checkpoint_db.clone();
            let outcome = crate::orchestrator::learning_recorder::WorkflowOutcome {
                task_run_id: config.execution_id.clone(),
                workflow_name: config.workflow_name.clone(),
                category: "meta".to_string(),
                status: if loop_result.verification_passed {
                    "complete".to_string()
                } else {
                    "failed".to_string()
                },
                duration_secs: start.elapsed().as_secs_f64(),
                iterations: loop_result.iterations_run,
                verification_passed: loop_result.verification_passed,
                max_iterations_reached: loop_result.max_iterations_reached,
                was_stopped: loop_result.was_stopped,
                tools_used: Vec::new(),
                files_modified: Vec::new(),
                error_type: None,
                error_message: None,
            };
            tokio::spawn(async move {
                if let Err(e) = db.with_conn(|conn| {
                    crate::orchestrator::learning_recorder::record_workflow_learning(conn, &outcome)
                }) {
                    warn!("Failed to record learning outcome: {}", e);
                }
            });
        }

        // Trigger reflection workflow (dev mode only, non-reflection runs only)
        if config.is_dev_mode {
            let deps = crate::reflection::trigger::ReflectionDeps {
                app_state: self.app_state.clone(),
                config_storage: self.config_storage.clone(),
                app_handle: self.app_handle.clone(),
                pid_tracker: self.pid_tracker.clone(),
            };
            let source_task_run_id = config.execution_id.clone();
            tokio::spawn(async move {
                // Delay to allow summary generation to finish
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                // launch_reflection is sync — it spawns the workflow internally
                match crate::reflection::trigger::launch_reflection(
                    deps,
                    source_task_run_id.clone(),
                ) {
                    Ok(id) if id == "skipped" => {
                        debug!("Reflection skipped for {}", source_task_run_id);
                    }
                    Ok(id) => {
                        info!(
                            "Launched reflection {} for completed run {}",
                            id, source_task_run_id
                        );
                    }
                    Err(e) => {
                        warn!(
                            "Failed to launch reflection for {}: {}",
                            source_task_run_id, e
                        );
                    }
                }
            });
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
        skip(self, verification_steps, agentic_steps, all_step_results, transitions, current_stage, logger),
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
        agentic_steps: &[ExecutionStepConfig],
        all_step_results: &mut Vec<crate::step_executor::StepExecutionResult>,
        transitions: &mut Vec<StageTransition>,
        current_stage: &mut String,
        logger: &StepEventLogger,
        initial_dynamic_steps: Vec<ExecutionStepConfig>,
    ) -> LoopResult {
        let mut iteration_results = Vec::new();
        // Start from the configured starting_iteration (for resume) or 0 (for fresh start)
        let mut iteration = config.starting_iteration;

        // Dynamic verification steps accumulated from agentic phase outputs.
        // These are merged with the static verification steps for subsequent iterations.
        let mut dynamic_steps: Vec<ExecutionStepConfig> = initial_dynamic_steps;
        if !dynamic_steps.is_empty() {
            info!(
                "Starting verification-agentic loop with {} pre-injected dynamic step(s)",
                dynamic_steps.len()
            );
        }

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
            } else {
                // Broadcast task-run-update to both Tauri + WebSocket
                let broadcaster = EventBroadcaster::new(self.app_handle.clone());
                broadcaster.task_run_update(&config.execution_id, "running", Some(iteration), None);
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

            // Build effective verification steps: static steps + any dynamically injected steps
            let effective_verification_steps: Vec<ExecutionStepConfig> = if dynamic_steps.is_empty()
            {
                verification_steps.to_vec()
            } else {
                let mut combined = verification_steps.to_vec();
                combined.extend(dynamic_steps.iter().cloned());
                info!(
                    "Verification using {} static + {} dynamic = {} total steps",
                    verification_steps.len(),
                    dynamic_steps.len(),
                    combined.len()
                );
                combined
            };

            let (verification_result, step_results) = self
                .verification_executor
                .run_verification(
                    &effective_verification_steps,
                    &config.execution_id,
                    iteration,
                    &config.workflow_name,
                    logger,
                )
                .await;

            // Add step results to overall results
            all_step_results.extend(step_results);

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
                    iteration,
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
                        let truncated = if err.len() > 200 { &err[..200] } else { err };
                        details
                            .push_str(&format!("  - {} [FAILED]: {}\n", sr.step_name, truncated));
                    }
                }

                let _ = self.checkpoint_db.append_task_output_ex(
                    &config.execution_id,
                    &format!("{}{}", summary_line, details),
                    false,
                    false,
                );
            }

            // Store verification result in database for Recap page
            // Use parent task ID for workflow sequences (same remapping as step checkpoints)
            if let Ok(result_json) = serde_json::to_value(&verification_result) {
                let parent_id = get_parent_task_id(&config.execution_id);
                let _ = self.checkpoint_db.store_verification_phase_result(
                    &parent_id,
                    iteration,
                    &result_json,
                );

                // Sync to web backend (best-effort, non-blocking)
                let parent_id_clone = parent_id.clone();
                let result_json_clone = result_json.clone();
                tokio::spawn(async move {
                    let sync_service = crate::commands::task_sync::AITaskSyncService::new();
                    if let Err(e) = sync_service
                        .sync_verification_result(&parent_id_clone, iteration, &result_json_clone)
                        .await
                    {
                        warn!("Failed to sync verification result to backend: {}", e);
                    }
                });
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

                // Resolve all unresolved knowledge entries — the issues they describe
                // have been addressed since verification now passes.
                {
                    let parent_id = get_parent_task_id(&config.execution_id);
                    match self.knowledge_base.get_all_knowledge(&parent_id) {
                        Ok(entries) => {
                            let unresolved: Vec<_> =
                                entries.iter().filter(|k| !k.is_resolved).collect();
                            if !unresolved.is_empty() {
                                info!(
                                    "Resolving {} unresolved knowledge entries after verification pass",
                                    unresolved.len()
                                );
                                for entry in &unresolved {
                                    if let Err(e) = self.knowledge_base.resolve_finding(
                                        &entry.id,
                                        Some("Resolved: verification passed"),
                                    ) {
                                        warn!(
                                            "Failed to resolve knowledge entry {}: {}",
                                            entry.id, e
                                        );
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Failed to fetch knowledge entries for resolution: {}", e);
                        }
                    }
                }

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

            // Record verification feedback as knowledge for cross-iteration context
            {
                let failed_criteria: Vec<String> = verification_result
                    .step_results
                    .iter()
                    .filter(|sr| !sr.success)
                    .map(|sr| sr.step_name.clone())
                    .collect();

                let parent_id = get_parent_task_id(&config.execution_id);
                if let Err(e) = self.knowledge_base.record_verification_feedback(
                    &parent_id,
                    iteration,
                    &failure_context,
                    &failed_criteria,
                ) {
                    warn!(
                        "Failed to record verification feedback as knowledge (iteration {}): {}",
                        iteration, e
                    );
                } else {
                    debug!(
                        "Recorded verification feedback knowledge: {} failed criteria (iteration {})",
                        failed_criteria.len(),
                        iteration
                    );
                }
            }

            // Detect regressions from previous iteration (iteration 2+)
            let failure_context = if iteration > 1 {
                match detect_regression(
                    &self.checkpoint_db,
                    &config.execution_id,
                    iteration,
                    &verification_result,
                ) {
                    Some(warning) => format!("{}\n\n{}", warning, failure_context),
                    None => failure_context,
                }
            } else {
                failure_context
            };

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

            let (agentic_outcome, new_injected_steps) = self
                .agentic_executor
                .run_agentic(
                    config,
                    iteration,
                    &failure_context,
                    has_agentic_steps,
                    agentic_steps,
                    logger,
                )
                .await;

            // Accumulate any newly injected steps for future verification iterations
            if !new_injected_steps.is_empty() {
                info!(
                    "Injected {} dynamic verification step(s) from agentic phase (iteration {})",
                    new_injected_steps.len(),
                    iteration
                );
                dynamic_steps.extend(new_injected_steps);
            }

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

            // Record findings from AI output as knowledge entries
            if let Some(output) = agentic_outcome.output() {
                let findings = parse_findings_from_output(output);
                if !findings.is_empty() {
                    let parent_id = get_parent_task_id(&config.execution_id);
                    info!(
                        "Parsed {} finding(s) from agentic output (iteration {})",
                        findings.len(),
                        iteration
                    );
                    for finding in &findings {
                        if let Err(e) = self
                            .knowledge_base
                            .record_finding(&parent_id, finding, iteration)
                        {
                            warn!("Failed to record finding as knowledge: {}", e);
                        }
                    }
                }

                // Record agentic outcome as an observation
                let parent_id = get_parent_task_id(&config.execution_id);
                let observation = match &agentic_outcome {
                    AgenticOutcome::Success { .. } => {
                        format!(
                            "Iteration {}: Agentic phase completed successfully ({} chars of output)",
                            iteration,
                            output.len()
                        )
                    }
                    AgenticOutcome::Failed { error, .. } => {
                        format!("Iteration {}: Agentic phase failed: {}", iteration, error)
                    }
                    AgenticOutcome::Error { error } => {
                        format!("Iteration {}: Agentic phase error: {}", iteration, error)
                    }
                    AgenticOutcome::Skipped => String::new(),
                };
                if !observation.is_empty() {
                    if let Err(e) = self.knowledge_base.record_observation(
                        &parent_id,
                        AgentType::Worker,
                        iteration,
                        &observation,
                        &[],
                    ) {
                        warn!("Failed to record agentic observation: {}", e);
                    }
                }
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

    async fn mark_task_completed(&self, execution_id: &str, workflow_id: Option<&str>) {
        if let Err(e) = self.checkpoint_db.complete_task_run(execution_id) {
            error!("Failed to mark task {} as completed: {}", execution_id, e);
        } else {
            info!("Marked task {} as COMPLETED", execution_id);
            // Broadcast task-run-update to both Tauri + WebSocket
            let broadcaster = EventBroadcaster::new(self.app_handle.clone());
            broadcaster.task_run_update(execution_id, "completed", None, None);

            // Sync completion to web backend (best-effort, non-blocking)
            let db = self.checkpoint_db.clone();
            let eid = execution_id.to_string();
            tokio::spawn(async move {
                let sync_service = crate::commands::task_sync::AITaskSyncService::new();
                if let Ok(Some(task)) = db.get_task_run(&eid) {
                    if let Err(e) = sync_service.sync_task_completed(&task).await {
                        warn!("Failed to sync task completion to backend: {}", e);
                    }
                }
            });

            // Fire-and-forget: try to promote workflow to example library
            if let Some(wf_id) = workflow_id {
                let db = self.checkpoint_db.clone();
                let wf_id = wf_id.to_string();
                let _ = db.with_conn(|conn| {
                    crate::workflow_generation::example_workflows::try_promote_on_success(
                        conn, &wf_id,
                    );
                    Ok(())
                });
            }
        }
    }

    async fn mark_task_failed(&self, execution_id: &str, reason: &str) {
        if let Err(e) = self.checkpoint_db.fail_task_run(execution_id, reason) {
            error!("Failed to mark task {} as failed: {}", execution_id, e);
        } else {
            info!("Marked task {} as FAILED: {}", execution_id, reason);
            // Broadcast task-run-update to both Tauri + WebSocket
            let broadcaster = EventBroadcaster::new(self.app_handle.clone());
            broadcaster.task_run_update(
                execution_id,
                "failed",
                None,
                Some(serde_json::json!({ "reason": reason })),
            );

            // Sync failure to web backend (best-effort, non-blocking)
            let db = self.checkpoint_db.clone();
            let eid = execution_id.to_string();
            tokio::spawn(async move {
                let sync_service = crate::commands::task_sync::AITaskSyncService::new();
                if let Ok(Some(task)) = db.get_task_run(&eid) {
                    if let Err(e) = sync_service.sync_task_completed(&task).await {
                        warn!("Failed to sync task failure to backend: {}", e);
                    }
                }
            });
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
    /// For workflow sequence children (e.g., workflow-sequence-X-workflow-N),
    /// state is persisted under the parent sequence ID since children don't have
    /// their own task_run records.
    fn persist_workflow_state(&self, execution_id: &str, state: &UnifiedWorkflowState) {
        // For workflow sequence children (e.g., workflow-sequence-X-workflow-N),
        // persist under the parent ID since children don't have their own task_run records.
        let persist_id = get_parent_task_id(execution_id);

        let state_machine = StateMachine::new(
            self.checkpoint_db.clone(),
            &persist_id,
            "unified",
            state.clone(),
        );

        if let Err(e) = state_machine.persist() {
            warn!("Failed to persist workflow state for {}: {}", persist_id, e);
        } else {
            debug!(
                "Persisted workflow state '{}' for execution {} (db key: {})",
                state.name(),
                execution_id,
                persist_id
            );

            // Broadcast real-time event to notify all clients (Tauri + WebSocket) of state change
            let broadcaster = EventBroadcaster::new(self.app_handle.clone());
            broadcaster.orchestrator_state_change(
                &persist_id,
                state.name(),
                state.iteration().unwrap_or(0),
                state.phase().unwrap_or("unknown"),
            );

            // Also broadcast step-progress so widgets watching step data refetch
            broadcaster.step_progress(
                &persist_id,
                0,
                state.name(),
                state.phase().unwrap_or("unknown"),
                None,
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

            // Persist to database (use parent ID for sequence children)
            let persist_id = get_parent_task_id(execution_id);
            if let Ok(json) = serde_json::to_string(&transitions) {
                if let Err(e) = self
                    .checkpoint_db
                    .update_task_run_transition_history(&persist_id, &json)
                {
                    warn!("Failed to persist transition history: {}", e);
                }
            }
        }
    }

    /// Run the completion sweep loop.
    ///
    /// After verification passes, this reviews all completed work for gaps
    /// before proceeding to the completion phase. Each iteration runs an AI session
    /// that checks for overlooked items, incomplete implementations, and edge cases.
    ///
    /// The sweep exits when:
    /// - The AI outputs `[NO_MORE_STEPS]` (all work is complete)
    /// - Max sweep iterations are reached
    /// - The task is stopped externally
    /// - An AI session fails
    async fn run_sweep_loop(
        &self,
        config: &LoopConfig,
        _all_step_results: &mut Vec<crate::step_executor::StepExecutionResult>,
        logger: &StepEventLogger,
        loop_iterations_run: u32,
    ) -> SweepResult {
        let max_iterations = config.max_sweep_iterations.max(1);
        let mut iterations_run = 0u32;

        for iteration in 0..max_iterations {
            if self.is_task_stopped(&config.execution_id) {
                info!("SWEEP: Task stopped before iteration {}", iteration + 1);
                break;
            }

            info!(
                "SWEEP: Starting iteration {}/{}",
                iteration + 1,
                max_iterations
            );

            let sweep_prompt = Self::build_sweep_prompt(
                &config.workflow_name,
                &config.base_prompt,
                loop_iterations_run,
                iteration,
                max_iterations,
            );

            // Run the sweep as an agentic AI session
            let sweep_step = ExecutionStepConfig {
                name: Some(format!("Completion Sweep {}", iteration + 1)),
                step_type: "prompt".to_string(),
                phase: Some("agentic".to_string()),
                prompt_content: Some(sweep_prompt),
                prompt_mode: None, // Full session mode
                ..Default::default()
            };

            let (outcome, _injected) = self
                .agentic_executor
                .run_agentic(
                    config,
                    loop_iterations_run + iteration + 1, // Continue iteration numbering
                    "",                                  // No failure context for sweep
                    true,
                    &[sweep_step],
                    logger,
                )
                .await;

            // Append sweep output to task output_log
            let output_text = match &outcome {
                AgenticOutcome::Success { output } => {
                    format!(
                        "\n\n=== Completion Sweep (Iteration {}/{}) ===\n\n{}",
                        iteration + 1,
                        max_iterations,
                        output
                    )
                }
                AgenticOutcome::Failed { output, error } => {
                    warn!("SWEEP: Iteration {} failed: {}", iteration + 1, error);
                    format!(
                        "\n\n=== Completion Sweep (Iteration {}/{}, FAILED: {}) ===\n\n{}",
                        iteration + 1,
                        max_iterations,
                        error,
                        output
                    )
                }
                AgenticOutcome::Error { error } => {
                    error!("SWEEP: Iteration {} errored: {}", iteration + 1, error);
                    format!(
                        "\n\n=== Completion Sweep (Iteration {}/{}, ERROR) ===\n\n{}",
                        iteration + 1,
                        max_iterations,
                        error
                    )
                }
                AgenticOutcome::Skipped => String::new(),
            };

            if !output_text.is_empty() {
                let _ = self.checkpoint_db.append_task_output_ex(
                    &config.execution_id,
                    &output_text,
                    true,  // increment session count
                    false, // don't check for completion marker
                );
            }

            iterations_run = iteration + 1;

            // Check for stop signal
            let output_str = outcome.output().unwrap_or("");
            if output_str.contains("[NO_MORE_STEPS]") {
                info!("SWEEP: AI signaled [NO_MORE_STEPS], ending sweep loop");
                return SweepResult {
                    iterations_run,
                    no_more_steps: true,
                };
            }

            // Stop on failure
            if !outcome.is_success() {
                warn!(
                    "SWEEP: Iteration {} was not successful, ending loop",
                    iteration + 1
                );
                break;
            }
        }

        SweepResult {
            iterations_run,
            no_more_steps: false,
        }
    }

    /// Build the prompt for a completion sweep iteration.
    fn build_sweep_prompt(
        workflow_name: &str,
        base_prompt: &str,
        verification_iterations: u32,
        sweep_iteration: u32,
        max_sweep_iterations: u32,
    ) -> String {
        let mut prompt = format!(
            "## Completion Sweep for: {} (Iteration {}/{})\n\n",
            workflow_name,
            sweep_iteration + 1,
            max_sweep_iterations,
        );

        if !base_prompt.is_empty() {
            prompt.push_str(&format!("### Original Task\n\n{}\n\n", base_prompt));
        }

        prompt.push_str(&format!(
            "### Context\n\nThe verification-agentic loop passed after {} iteration(s). All verification checks are now passing.\n\n",
            verification_iterations
        ));

        prompt.push_str(
            r#"## Your Task

Review ALL work done during this workflow execution. Look for:

1. **Overlooked items** from the original task that weren't fully addressed
2. **Incomplete implementations** - partially done work, stub functions, placeholder code
3. **Edge cases** that weren't handled
4. **Integration gaps** - components that don't connect properly
5. **TODO/FIXME markers** left in the code

**IMPORTANT:** Do not just list items. Actually implement any fixes or missing pieces you find.

**STOP SIGNAL:** If ALL work is complete and there are no remaining items to address, output `[NO_MORE_STEPS]` at the end of your response. Only output this marker when you are confident everything is done.
If there IS remaining work, implement it now. Do NOT output `[NO_MORE_STEPS]` if you made changes or found issues.
"#,
        );

        prompt
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
                            let session_manager: Arc<crate::claude_session::SessionManager> =
                                app_handle_for_spawn
                                    .state::<Arc<crate::claude_session::SessionManager>>()
                                    .inner()
                                    .clone();
                            let mut controller = LoopController::new(
                                app_state_for_spawn,
                                config_storage_for_spawn,
                                app_handle_for_spawn,
                                pid_tracker_for_spawn,
                            )
                            .with_session_manager(session_manager);

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
                            let verification_steps = convert_all_json_steps_with_phase(
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
                                base_prompt: String::new(), // Not used for resume
                                workflow_name: task_name.clone(),
                                workflow_id: wf_id_for_spawn.to_string(),
                                execution_id: task_id.clone(),
                                targeted_error_ids: workflow.targeted_error_ids.clone(),
                                starting_iteration,
                                run_agentic_first,
                                artifact_dir: None,
                                is_dev_mode: cfg!(debug_assertions),
                                enable_sweep: workflow.enable_sweep,
                                max_sweep_iterations: workflow.max_sweep_iterations,
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
                            let session_manager: Arc<crate::claude_session::SessionManager> =
                                app_handle_for_spawn
                                    .state::<Arc<crate::claude_session::SessionManager>>()
                                    .inner()
                                    .clone();
                            let mut controller = LoopController::new(
                                app_state_for_spawn,
                                config_storage_for_spawn,
                                app_handle_for_spawn,
                                pid_tracker_for_spawn,
                            )
                            .with_session_manager(session_manager);

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
                            let verification_steps = convert_all_json_steps_with_phase(
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
                                base_prompt: String::new(),
                                workflow_name: task_name.clone(),
                                workflow_id: wf_id.to_string(),
                                execution_id: task_id.clone(),
                                targeted_error_ids: workflow.targeted_error_ids.clone(),
                                starting_iteration,
                                run_agentic_first,
                                artifact_dir: None,
                                is_dev_mode: cfg!(debug_assertions),
                                enable_sweep: workflow.enable_sweep,
                                max_sweep_iterations: workflow.max_sweep_iterations,
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
/// Variables available for substitution in step fields.
pub struct SubstitutionVars {
    pub artifact_dir: Option<String>,
    pub execution_id: String,
    pub iteration: u32,
}

/// Apply variable substitution to a JSON step value.
///
/// Replaces template variables in all string values within the JSON:
/// - `{{artifact_dir}}` → artifact directory path (forward slashes)
/// - `{{execution_id}}` → the task run ID
/// - `{{iteration}}` → current iteration number
pub fn apply_variable_substitution(
    step: &serde_json::Value,
    vars: &SubstitutionVars,
) -> serde_json::Value {
    let mut json_str = serde_json::to_string(step).unwrap_or_default();

    if let Some(ref artifact_dir) = vars.artifact_dir {
        // Use forward slashes on all platforms for consistency
        let normalized = artifact_dir.replace('\\', "/");
        json_str = json_str.replace("{{artifact_dir}}", &normalized);
    }
    json_str = json_str.replace("{{execution_id}}", &vars.execution_id);
    json_str = json_str.replace("{{iteration}}", &vars.iteration.to_string());

    serde_json::from_str(&json_str).unwrap_or_else(|_| step.clone())
}

/// Apply variable substitution to a slice of JSON step values.
pub fn apply_substitution_to_steps(
    steps: &[serde_json::Value],
    vars: &SubstitutionVars,
) -> Vec<serde_json::Value> {
    steps
        .iter()
        .map(|s| apply_variable_substitution(s, vars))
        .collect()
}

/// Apply variable substitution to an ExecutionStepConfig's string fields.
///
/// Replaces `{{artifact_dir}}` and `{{execution_id}}` in all relevant
/// Option<String> fields. This is called after the artifact directory
/// is created but before steps are executed.
fn substitute_step_vars(step: &mut ExecutionStepConfig, artifact_dir: &str, execution_id: &str) {
    let sub = |s: &mut Option<String>| {
        if let Some(val) = s {
            if val.contains("{{artifact_dir}}") || val.contains("{{execution_id}}") {
                *val = val
                    .replace("{{artifact_dir}}", artifact_dir)
                    .replace("{{execution_id}}", execution_id);
            }
        }
    };

    sub(&mut step.output_path);
    sub(&mut step.input_path);
    sub(&mut step.ai_review_input_path);
    sub(&mut step.shell_command);
    sub(&mut step.shell_command_working_directory);
    sub(&mut step.check_command);
    sub(&mut step.check_working_directory);

    // Also substitute in prompt content (may reference artifact paths)
    if let Some(ref mut content) = step.prompt_content {
        if content.contains("{{artifact_dir}}") || content.contains("{{execution_id}}") {
            *content = content
                .replace("{{artifact_dir}}", artifact_dir)
                .replace("{{execution_id}}", execution_id);
        }
    }
}

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
    _monitor: i32,
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
            let mut config =
                if let Ok(config) = serde_json::from_value::<ExecutionStepConfig>(step.clone()) {
                    config
                } else {
                    // Fall back to manual field extraction — preserve command, working directory,
                    // and other key fields so that check/test steps with inline commands still work
                    let step_type = step.get("type").and_then(|t| t.as_str())?;
                    ExecutionStepConfig {
                        step_type: step_type.to_string(),
                        name: step
                            .get("name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string()),
                        id: step
                            .get("id")
                            .and_then(|i| i.as_str())
                            .map(|s| s.to_string()),
                        shell_command: step
                            .get("command")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string()),
                        shell_command_working_directory: step
                            .get("working_directory")
                            .and_then(|w| w.as_str())
                            .map(|s| s.to_string()),
                        check_type: step
                            .get("check_type")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string()),
                        test_type: step
                            .get("test_type")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string()),
                        test_id: step
                            .get("test_id")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string()),
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

/// Convert ALL JSON steps (including prompt-type) to ExecutionStepConfig with explicit phase.
///
/// Unlike `convert_json_steps_with_phase` which filters out prompt steps,
/// this function preserves all step types in their original order.
/// This is needed for the verification phase where prompt-type steps
/// (AI-evaluated checks) must be included alongside automation steps.
pub fn convert_all_json_steps_with_phase(
    steps: &[serde_json::Value],
    _monitor: i32,
    explicit_phase: Option<&str>,
) -> Vec<ExecutionStepConfig> {
    use crate::step_executor::StepPhase;

    steps
        .iter()
        .filter_map(|step| {
            let mut config =
                if let Ok(config) = serde_json::from_value::<ExecutionStepConfig>(step.clone()) {
                    config
                } else {
                    // Fall back to manual field extraction — preserve command, working directory,
                    // and other key fields so that check/test steps with inline commands still work
                    let step_type = step.get("type").and_then(|t| t.as_str())?;
                    ExecutionStepConfig {
                        step_type: step_type.to_string(),
                        name: step
                            .get("name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string()),
                        id: step
                            .get("id")
                            .and_then(|i| i.as_str())
                            .map(|s| s.to_string()),
                        shell_command: step
                            .get("command")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string()),
                        shell_command_working_directory: step
                            .get("working_directory")
                            .and_then(|w| w.as_str())
                            .map(|s| s.to_string()),
                        check_type: step
                            .get("check_type")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string()),
                        test_type: step
                            .get("test_type")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string()),
                        test_id: step
                            .get("test_id")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string()),
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

// =============================================================================
// Regression Detection
// =============================================================================

/// Compare current verification results with the previous iteration to detect regressions.
///
/// Returns a warning string if regressions are found (steps that were passing before
/// but now fail), or None if no regressions detected.
fn detect_regression(
    checkpoint_db: &crate::database::CheckpointDb,
    execution_id: &str,
    current_iteration: u32,
    current_result: &crate::step_executor::VerificationPhaseResult,
) -> Option<String> {
    if current_iteration <= 1 {
        return None;
    }

    // Retrieve previous iteration result
    let prev_result =
        match checkpoint_db.get_verification_phase_result(execution_id, current_iteration - 1) {
            Ok(Some(val)) => val,
            _ => return None,
        };

    // Extract previous step results
    let prev_step_results = prev_result.get("step_results").and_then(|v| v.as_array())?;

    // Build a map of step_name -> success for previous iteration
    let mut prev_step_status: std::collections::HashMap<String, bool> =
        std::collections::HashMap::new();
    for step in prev_step_results {
        if let (Some(name), Some(success)) = (
            step.get("step_name").and_then(|v| v.as_str()),
            step.get("success").and_then(|v| v.as_bool()),
        ) {
            prev_step_status.insert(name.to_string(), success);
        }
    }

    // Find regressions: steps that were passing before but now fail
    let mut newly_broken: Vec<String> = Vec::new();
    for result in &current_result.step_results {
        if !result.success {
            if let Some(&prev_passed) = prev_step_status.get(&result.step_name) {
                if prev_passed {
                    newly_broken.push(result.step_name.clone());
                }
            }
        }
    }

    // Compare overall scores
    let prev_passed = prev_result
        .get("passed_steps")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let prev_total = prev_result
        .get("total_steps")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let curr_passed = current_result.passed_steps as u64;
    let curr_total = current_result.total_steps as u64;

    // Only warn if there are actual regressions
    if newly_broken.is_empty() && curr_passed >= prev_passed {
        return None;
    }

    let mut warning = String::new();
    warning.push_str("## REGRESSION WARNING\n\n");
    warning.push_str("Your changes in the previous iteration caused regressions.\n\n");

    if !newly_broken.is_empty() {
        warning.push_str(&format!(
            "**Previously passing, now failing:** {}\n",
            newly_broken.join(", ")
        ));
    }

    if curr_passed < prev_passed {
        warning.push_str(&format!(
            "**Score change:** {}/{} passed -> {}/{} passed ({} more failures)\n",
            prev_passed,
            prev_total,
            curr_passed,
            curr_total,
            prev_passed - curr_passed
        ));
    }

    warning.push_str(
        "\nConsider whether your changes were correct or if they had unintended side effects.\n",
    );

    info!(
        "REGRESSION-DETECT: iteration {} has {} newly broken steps (was {}/{}, now {}/{})",
        current_iteration,
        newly_broken.len(),
        prev_passed,
        prev_total,
        curr_passed,
        curr_total
    );

    Some(warning)
}

// =============================================================================
// Resume Context Builder
// =============================================================================

/// Build agentic context from stored verification data when resuming from an
/// interrupted agentic phase.
///
/// Tries multiple data sources in order:
/// 1. Verification phase result from database (full structured result)
/// 2. Step checkpoints from database (step names + error messages)
/// 3. Fallback generic message
fn build_resume_agentic_context(
    checkpoint_db: &Arc<crate::database::CheckpointDb>,
    execution_id: &str,
    iteration: u32,
) -> String {
    // Strategy 1: Try loading the full verification phase result.
    // The result may be stored under the execution_id or a child ID.
    if let Ok(Some(result_json)) =
        checkpoint_db.get_verification_phase_result(execution_id, iteration)
    {
        // Try to deserialize into VerificationPhaseResult and use build_failure_context()
        if let Ok(result) =
            serde_json::from_value::<crate::step_executor::VerificationPhaseResult>(result_json)
        {
            let context = result.build_failure_context();
            if !context.is_empty() {
                info!(
                    "RESUME: Built agentic context from verification phase result ({} chars)",
                    context.len()
                );
                return context;
            }
        }
    }

    // Strategy 2: Build context from step checkpoints (which remap to parent ID).
    let checkpoint_mgr =
        crate::workflow_state::CheckpointManager::new(checkpoint_db.clone(), "unified");
    if let Ok(checkpoints) =
        checkpoint_mgr.get_completed_steps(execution_id, "verification", Some(iteration))
    {
        if !checkpoints.is_empty() {
            let failed: Vec<_> = checkpoints
                .iter()
                .filter(|cp| {
                    matches!(
                        cp.status,
                        crate::workflow_state::StepCheckpointStatus::Failed
                    )
                })
                .collect();

            if !failed.is_empty() {
                let total = checkpoints.len();
                let passed = checkpoints
                    .iter()
                    .filter(|cp| {
                        matches!(
                            cp.status,
                            crate::workflow_state::StepCheckpointStatus::Success
                        )
                    })
                    .count();

                let mut context = String::new();
                context.push_str("## Verification Results (Resumed)\n\n");
                context.push_str(&format!(
                    "**Status:** {} of {} verification steps passed\n\n",
                    passed, total
                ));
                context.push_str("### Failed Steps\n\n");

                for cp in &failed {
                    let name = cp.step_name.as_deref().unwrap_or("unknown");
                    let step_type = &cp.step_type;
                    context.push_str(&format!("#### {} ({})\n", name, step_type));

                    if let Some(ref error) = cp.error {
                        context.push_str(&format!("**Error:** {}\n", error));
                    }

                    // If result_json is available (e.g., for successful steps that later
                    // became relevant), include it
                    if let Some(ref result_str) = cp.result_json {
                        if let Ok(result_data) =
                            serde_json::from_str::<serde_json::Value>(result_str)
                        {
                            // Extract stdout/output if present
                            if let Some(output) = result_data
                                .get("stdout")
                                .or_else(|| result_data.get("output"))
                                .and_then(|v| v.as_str())
                            {
                                if !output.is_empty() {
                                    let truncated = if output.len() > 2000 {
                                        format!(
                                            "{}...\n[truncated, {} more chars]",
                                            &output[..2000],
                                            output.len() - 2000
                                        )
                                    } else {
                                        output.to_string()
                                    };
                                    context.push_str(&format!(
                                        "**Output:**\n```\n{}\n```\n",
                                        truncated
                                    ));
                                }
                            }
                        }
                    }

                    context.push('\n');
                }

                info!(
                    "RESUME: Built agentic context from step checkpoints ({} chars, {} failed steps)",
                    context.len(),
                    failed.len()
                );
                return context;
            }
        }
    }

    // Strategy 3: Fallback
    info!("RESUME: No verification data found, using fallback context");
    "Resuming from interrupted agentic phase. The previous verification found failures. Please investigate and fix the issues.".to_string()
}
