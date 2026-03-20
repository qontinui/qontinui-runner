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
use tauri::{Emitter, Manager};
use tracing::{debug, error, info, instrument, warn};

use crate::config_storage::ConfigStorage;
use crate::database::CreateTaskRunEventInput;
use crate::doctor::DoctorHandle;
use crate::event_system::EventBroadcaster;
use crate::orchestrator::integration::StageTransition;
use crate::orchestrator::knowledge::{parse_findings_from_output, AgentType, KnowledgeBase};
use crate::step_executor::ExecutionStepConfig;
use crate::step_registry::StepEventLogger;
use crate::str_utils::truncate_str;
use crate::summary_generator::generate_task_summary_async;
use crate::workflow_state::{StateMachine, WorkflowState};
use crate::AppState;

use super::canvas_panels::CanvasPanelManager;
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
    canvas_manager: tokio::sync::Mutex<CanvasPanelManager>,
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
            canvas_manager: tokio::sync::Mutex::new(CanvasPanelManager::new(
                app_state.canvas_state.clone(),
                app_state.checkpoint_db.clone(),
                app_handle.clone(),
            )),
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
        name = "qontinui.workflow",
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
        let all_step_results = Vec::new();

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
        // CREATE WORKTREE (if enabled) — multi-repo support
        // =====================================================================
        if config.use_worktree {
            // Try multi-repo worktree creation first (across all sibling repos)
            let monorepo_root = crate::mcp::shared::get_monorepo_root();

            if let Some(ref root_str) = monorepo_root {
                let root_path = std::path::Path::new(root_str);
                info!("WORKTREE: Creating multi-repo worktrees under {}", root_str);

                match crate::worktree::create_multi_repo_worktrees(
                    root_path,
                    &config.execution_id,
                    &config.workflow_name,
                ) {
                    Ok(multi_result) => {
                        info!(
                            "WORKTREE: Created worktrees in {} repos ({} errors)",
                            multi_result.results.len(),
                            multi_result.errors.len()
                        );

                        // Store the primary worktree info (first successful result)
                        if let Some(primary) = multi_result.results.first() {
                            config.worktree_path =
                                Some(primary.worktree_path.to_string_lossy().to_string());
                            config.worktree_branch = Some(primary.branch_name.clone());
                        }

                        // Override project_path to the runner's worktree if available
                        let original_project_path = config.project_path.clone().unwrap_or_default();
                        for (original, worktree) in &multi_result.path_mappings {
                            if original_project_path.contains(original)
                                || original.contains("qontinui-runner")
                            {
                                config.project_path = Some(worktree.clone());
                                break;
                            }
                        }

                        // Remap all step working directories across ALL repos
                        let remap_steps =
                            |steps: &mut Vec<ExecutionStepConfig>,
                             mappings: &[(String, String)]| {
                                for step in steps.iter_mut() {
                                    if let Some(ref wd) = step.shell_command_working_directory {
                                        for (original, worktree) in mappings {
                                            if wd.contains(original) {
                                                step.shell_command_working_directory =
                                                    Some(wd.replace(original, worktree));
                                                break;
                                            }
                                        }
                                    }
                                    if let Some(ref wd) = step.check_working_directory {
                                        for (original, worktree) in mappings {
                                            if wd.contains(original) {
                                                step.check_working_directory =
                                                    Some(wd.replace(original, worktree));
                                                break;
                                            }
                                        }
                                    }
                                }
                            };
                        let mappings = &multi_result.path_mappings;
                        remap_steps(&mut setup_automation_steps, mappings);
                        remap_steps(&mut setup_prompt_steps, mappings);
                        remap_steps(&mut verification_steps, mappings);
                        remap_steps(&mut agentic_steps, mappings);
                        remap_steps(&mut completion_automation_steps, mappings);
                        remap_steps(&mut completion_prompt_steps, mappings);
                        for stage in &mut config.stages {
                            remap_steps(&mut stage.setup_automation_steps, mappings);
                            remap_steps(&mut stage.setup_prompt_steps, mappings);
                            remap_steps(&mut stage.verification_steps, mappings);
                            remap_steps(&mut stage.agentic_steps, mappings);
                            remap_steps(&mut stage.completion_automation_steps, mappings);
                            remap_steps(&mut stage.completion_prompt_steps, mappings);
                        }

                        // Insert worktree records for each repo
                        let now = chrono::Utc::now().to_rfc3339();
                        for (i, result) in multi_result.results.iter().enumerate() {
                            let original_path = &multi_result.path_mappings[i].0;
                            let record = crate::worktree::WorktreeRecord {
                                id: format!("{}-{}", config.execution_id, i),
                                worktree_path: result.worktree_path.to_string_lossy().to_string(),
                                branch_name: result.branch_name.clone(),
                                source_branch: result.source_branch.clone(),
                                source_commit: result.source_commit.clone(),
                                repo_path: original_path.clone(),
                                task_run_id: Some(config.execution_id.clone()),
                                workflow_name: Some(config.workflow_name.clone()),
                                status: crate::worktree::WorktreeStatus::Active,
                                created_at: now.clone(),
                                updated_at: now.clone(),
                            };
                            if let Err(e) = self.checkpoint_db.insert_worktree(&record) {
                                warn!(
                                    "WORKTREE: Failed to record worktree for {}: {}",
                                    original_path, e
                                );
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "WORKTREE: Multi-repo worktree creation failed ({}). Falling back to single-repo.",
                            e
                        );
                        // Fall back to single-repo worktree creation
                        Self::create_single_repo_worktree(
                            &mut config,
                            &mut setup_automation_steps,
                            &mut setup_prompt_steps,
                            &mut verification_steps,
                            &mut agentic_steps,
                            &mut completion_automation_steps,
                            &mut completion_prompt_steps,
                            &self.checkpoint_db,
                        );
                    }
                }
            } else {
                // No monorepo root found — fall back to single-repo behavior
                warn!("WORKTREE: Cannot determine monorepo root. Falling back to single-repo.");
                Self::create_single_repo_worktree(
                    &mut config,
                    &mut setup_automation_steps,
                    &mut setup_prompt_steps,
                    &mut verification_steps,
                    &mut agentic_steps,
                    &mut completion_automation_steps,
                    &mut completion_prompt_steps,
                    &self.checkpoint_db,
                );
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

            // Also apply substitution to steps within stages
            for stage in &mut config.stages {
                substitute(&mut stage.setup_automation_steps);
                substitute(&mut stage.setup_prompt_steps);
                substitute(&mut stage.verification_steps);
                substitute(&mut stage.agentic_steps);
                substitute(&mut stage.completion_automation_steps);
                substitute(&mut stage.completion_prompt_steps);
            }

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
        let (transitions, current_stage) = if !matches!(resume_point, ResumePoint::FromStart) {
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
        info!(
            "Configuration: max_iterations={}, stages={}, top_level_steps={}",
            config.max_iterations,
            config.stages.len(),
            setup_automation_steps.len()
                + setup_prompt_steps.len()
                + verification_steps.len()
                + agentic_steps.len()
                + completion_automation_steps.len()
                + completion_prompt_steps.len()
        );

        // Emit canvas panels for workflow start
        self.canvas_manager
            .lock()
            .await
            .on_workflow_start(&config)
            .await;

        // Create centralized step event logger for this execution
        // This ensures consistent event format and prevents duplicate logging
        let logger = StepEventLogger::new(
            self.checkpoint_db.clone(),
            &config.execution_id,
            &config.workflow_name,
        );

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

            // Clear constraint results
            if let Err(e) = self
                .checkpoint_db
                .delete_constraint_results(&config.execution_id)
            {
                warn!(
                    "Failed to clear constraint results: {} - continuing anyway",
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
        // NORMALIZE: All workflows are phased (multi-stage)
        // =====================================================================
        // If no explicit stages are defined, wrap the top-level steps into a
        // single stage. This means ALL execution goes through run_multi_stage().
        if config.stages.is_empty() {
            let has_any_steps = !setup_automation_steps.is_empty()
                || !setup_prompt_steps.is_empty()
                || !verification_steps.is_empty()
                || !agentic_steps.is_empty()
                || !completion_automation_steps.is_empty()
                || !completion_prompt_steps.is_empty();

            if has_any_steps {
                info!("Normalizing single-phase workflow into stages");
                let single_stage = super::StageConfig {
                    id: format!("{}-phase-1", config.workflow_id),
                    name: config.workflow_name.clone(),
                    index: 0,
                    total_stages: 1,
                    setup_automation_steps: std::mem::take(&mut setup_automation_steps),
                    setup_prompt_steps: std::mem::take(&mut setup_prompt_steps),
                    verification_steps: std::mem::take(&mut verification_steps),
                    agentic_steps: std::mem::take(&mut agentic_steps),
                    completion_automation_steps: std::mem::take(&mut completion_automation_steps),
                    completion_prompt_steps: std::mem::take(&mut completion_prompt_steps),
                    max_iterations: config.max_iterations,
                    provider: config.provider_override.clone(),
                    model: config.model_override.clone(),
                    model_overrides: config.model_overrides.clone(),
                    timeout_seconds: None,
                    approval_gate: config.approval_gate,
                    condition: None, // Single-phase normalization has no condition
                    completion_prompts_first: false,
                };
                config.stages = vec![single_stage];
            }
        }

        // All workflows go through the multi-stage path.
        // A single-phase workflow runs as stages: [single_phase].
        return self
            .run_multi_stage(
                config,
                all_step_results,
                transitions,
                current_stage,
                &logger,
                start,
                &resume_point,
            )
            .await;

        // NOTE: The single-stage execution path that was previously here has been removed.
        // All execution now goes through run_multi_stage(), which handles:
        // - Per-stage setup, verification-agentic loop, and completion
        // - Agentic-first for error-fix workflows
        // - Variable substitution from setup outputs
        // - Completion sweep
        // - Learning recording and reflection triggering
    }

    /// Run a multi-stage workflow where each stage has its own
    /// setup → verification-agentic loop → stage completion cycle.
    ///
    /// After all stages complete, the top-level completion phase runs.
    /// Context accumulates across stages: each stage's output is visible to later stages.
    async fn run_multi_stage(
        &mut self,
        mut config: LoopConfig,
        mut all_step_results: Vec<crate::step_executor::StepExecutionResult>,
        mut transitions: Vec<StageTransition>,
        mut current_stage: String,
        logger: &StepEventLogger,
        start: std::time::Instant,
        resume_point: &ResumePoint,
    ) -> WorkflowResult {
        let total_stages = config.stages.len();

        // Determine which stage to start from based on resume point
        let start_from_stage = match resume_point {
            ResumePoint::StageStart { from_stage } => *from_stage as usize,
            ResumePoint::VerificationPhase {
                stage_index: Some(si),
                ..
            } => *si as usize,
            ResumePoint::AgenticPhase {
                stage_index: Some(si),
                ..
            } => *si as usize,
            ResumePoint::SetupPhase {
                stage_index: Some(si),
                ..
            } => *si as usize,
            _ => 0,
        };

        if start_from_stage > 0 {
            info!(
                "=== MULTI-STAGE WORKFLOW: {} stages, stop_on_failure={}, RESUMING from stage {} ===",
                total_stages, config.stop_on_failure, start_from_stage
            );
        } else {
            info!(
                "=== MULTI-STAGE WORKFLOW: {} stages, stop_on_failure={} ===",
                total_stages, config.stop_on_failure
            );
        }

        let mut any_stage_passed = false;
        let mut last_loop_result: Option<LoopResult> = None;
        let mut total_stage_failures: u32 = 0;
        let mut total_iterations_across_stages: u32 = 0;

        for stage_idx in 0..config.stages.len() {
            let stage = &config.stages[stage_idx];
            // Skip stages that have already completed (resume support)
            if stage_idx < start_from_stage {
                info!(
                    "  Skipping stage {}/{} (already completed before resume)",
                    stage_idx + 1,
                    total_stages
                );
                continue;
            }
            let stage_num = stage_idx + 1;

            // Evaluate conditional stage execution
            if let Some(ref condition) = stage.condition {
                let previous_passed = last_loop_result
                    .as_ref()
                    .map(|r| r.verification_passed)
                    .unwrap_or(true); // No previous stage = treat as passed

                let should_skip = evaluate_stage_condition(
                    condition,
                    previous_passed,
                    total_iterations_across_stages,
                    total_stage_failures,
                );

                if should_skip {
                    info!(
                        "STAGE-SKIP: stage {}/{} '{}' skipped (condition not met: {:?})",
                        stage_num, total_stages, stage.name, condition
                    );
                    continue;
                }
            }

            // Notify canvas of stage start
            self.canvas_manager
                .lock()
                .await
                .on_stage_start(stage_idx as u32, &stage.name, total_stages)
                .await;

            // Wait if paused before starting a new stage
            self.wait_while_paused(&config.execution_id).await;

            // Check for external stop
            if self.is_task_stopped(&config.execution_id) {
                warn!("Task stopped before stage {}/{}", stage_num, total_stages);
                self.persist_workflow_state(
                    &config.execution_id,
                    &UnifiedWorkflowState::stopped_in_phase("stage", Some(stage_idx as u32)),
                );
                self.canvas_manager
                    .lock()
                    .await
                    .on_workflow_failed(&format!(
                        "Stopped before stage {}/{}",
                        stage_num, total_stages
                    ))
                    .await;
                return WorkflowResult {
                    success: false,
                    verification_passed: false,
                    step_results: all_step_results,
                    duration_ms: start.elapsed().as_millis() as u64,
                    loop_result: last_loop_result,
                    worktree_path: config.worktree_path.clone(),
                    worktree_branch: config.worktree_branch.clone(),
                    workflow_architecture: config.workflow_architecture.clone(),
                    agentic_verification_config: config.agentic_verification_config.clone(),
                    multi_agent_pipeline_config: config.multi_agent_pipeline_config.clone(),
                };
            }

            // Check global max_sessions budget before starting a new stage
            if let Some(max) = config.max_sessions {
                if let Ok(Some(task_run)) = self.checkpoint_db.get_task_run(&config.execution_id) {
                    if task_run.sessions_count >= max {
                        warn!(
                            "Global max_sessions ({}) reached before stage {}/{} (sessions_count={}) - stopping workflow",
                            max, stage_num, total_stages, task_run.sessions_count
                        );
                        self.persist_workflow_state(
                            &config.execution_id,
                            &UnifiedWorkflowState::failed_in_phase(
                                format!(
                                    "Max sessions ({}) reached before stage {}",
                                    max, stage_num
                                ),
                                "stage",
                                None,
                            ),
                        );
                        self.mark_task_failed(
                            &config.execution_id,
                            &format!(
                                "Max sessions ({}) exhausted after {} sessions (before stage {}/{})",
                                max, task_run.sessions_count, stage_num, total_stages
                            ),
                            Some(&config.workflow_id),
                        )
                        .await;
                        self.canvas_manager
                            .lock()
                            .await
                            .on_workflow_failed(&format!(
                                "Max sessions ({}) exhausted before stage {}/{}",
                                max, stage_num, total_stages
                            ))
                            .await;
                        return WorkflowResult {
                            success: false,
                            verification_passed: false,
                            step_results: all_step_results,
                            duration_ms: start.elapsed().as_millis() as u64,
                            loop_result: last_loop_result,
                            worktree_path: config.worktree_path.clone(),
                            worktree_branch: config.worktree_branch.clone(),
                            workflow_architecture: config.workflow_architecture.clone(),
                            agentic_verification_config: config.agentic_verification_config.clone(),
                            multi_agent_pipeline_config: config.multi_agent_pipeline_config.clone(),
                        };
                    }
                }
            }

            // Append stage header to output
            let separator = "=".repeat(60);
            let stage_header = format!(
                "\n\n{}\n=== STAGE {}/{}: \"{}\" ===\n{}\n",
                separator, stage_num, total_stages, stage.name, separator
            );
            let _ = self.checkpoint_db.append_task_output_ex(
                &config.execution_id,
                &stage_header,
                false,
                false,
            );

            info!(
                "=== STAGE {}/{}: {} (id: {}) ===",
                stage_num, total_stages, stage.name, stage.id
            );

            // ─── Stage Setup ───
            if !stage.setup_automation_steps.is_empty() || !stage.setup_prompt_steps.is_empty() {
                info!("  Stage {}: Running setup", stage_num);
                self.persist_workflow_state(
                    &config.execution_id,
                    &UnifiedWorkflowState::setup_running(),
                );
                self.record_stage_transition(
                    &config.execution_id,
                    &mut transitions,
                    &mut current_stage,
                    &format!("stage_{}_setup", stage_num),
                    0,
                );

                // Resolve setup phase model override: stage model_overrides → stage model → workflow model
                let setup_model = stage
                    .model_overrides
                    .get("setup")
                    .and_then(|c| c.model.clone())
                    .or_else(|| stage.model.clone())
                    .or_else(|| config.model_override.clone());
                let setup_provider = stage
                    .model_overrides
                    .get("setup")
                    .and_then(|c| c.provider.clone())
                    .or_else(|| stage.provider.clone())
                    .or_else(|| config.provider_override.clone());

                let (setup_ok, setup_results) = self
                    .setup_executor
                    .run_setup(
                        &stage.setup_automation_steps,
                        &stage.setup_prompt_steps,
                        &config.execution_id,
                        &format!(
                            "{} > Stage {}: {}",
                            config.workflow_name, stage_num, stage.name
                        ),
                        logger,
                        Some(stage_idx as u32),
                        setup_model,
                        setup_provider,
                    )
                    .await;

                // Emit canvas panel for setup completion
                self.canvas_manager
                    .lock()
                    .await
                    .on_setup_complete(setup_ok, &setup_results)
                    .await;

                // Log setup output
                {
                    let mut output = format!(
                        "\n--- Stage {} Setup ---\nSteps: {}\nSuccess: {}\n",
                        stage_num,
                        setup_results.len(),
                        setup_ok,
                    );
                    for sr in &setup_results {
                        output.push_str(&format!(
                            "  [{}] {} - {} ({}ms)\n",
                            if sr.success { "OK" } else { "FAIL" },
                            sr.step_type,
                            sr.step_name,
                            sr.duration_ms,
                        ));
                    }
                    let _ = self.checkpoint_db.append_task_output_ex(
                        &config.execution_id,
                        &output,
                        false,
                        false,
                    );
                }

                all_step_results.extend(setup_results);

                if !setup_ok {
                    warn!("Stage {} setup failed", stage_num);
                    if config.stop_on_failure {
                        error!("stop_on_failure=true, aborting workflow");
                        self.persist_workflow_state(
                            &config.execution_id,
                            &UnifiedWorkflowState::failed_in_phase(
                                format!("Stage {} setup failed", stage_num),
                                "setup",
                                None,
                            ),
                        );
                        self.mark_task_failed(
                            &config.execution_id,
                            &format!("Stage {} setup failed", stage_num),
                            Some(&config.workflow_id),
                        )
                        .await;
                        self.canvas_manager
                            .lock()
                            .await
                            .on_workflow_failed(&format!("Stage {} setup failed", stage_num))
                            .await;
                        return WorkflowResult {
                            success: false,
                            verification_passed: false,
                            step_results: all_step_results,
                            duration_ms: start.elapsed().as_millis() as u64,
                            loop_result: last_loop_result,
                            worktree_path: config.worktree_path.clone(),
                            worktree_branch: config.worktree_branch.clone(),
                            workflow_architecture: config.workflow_architecture.clone(),
                            agentic_verification_config: config.agentic_verification_config.clone(),
                            multi_agent_pipeline_config: config.multi_agent_pipeline_config.clone(),
                        };
                    }
                    // stop_on_failure=false: skip to next stage
                    info!("stop_on_failure=false, continuing to next stage");
                    self.persist_workflow_state(
                        &config.execution_id,
                        &UnifiedWorkflowState::stage_complete(stage_idx as u32),
                    );
                    continue;
                }

                // Expand runtime variables from setup phase outputs into prompts
                let shared_vars = self.setup_executor.shared_variables().get_all();
                if !shared_vars.is_empty() {
                    info!(
                        "  Stage {}: Expanding {} runtime variables from setup into prompts",
                        stage_num,
                        shared_vars.len()
                    );
                    for (name, value) in &shared_vars {
                        let pattern = format!("{{{{{}}}}}", name);
                        if config.base_prompt.contains(&pattern) {
                            info!(
                                "    Substituting {{{{{}}}}} ({} chars) into base_prompt",
                                name,
                                value.len()
                            );
                            config.base_prompt = config.base_prompt.replace(&pattern, value);
                        }
                    }

                    // Also substitute runtime variables into steps within current and remaining stages
                    for si in stage_idx..config.stages.len() {
                        let s = &mut config.stages[si];
                        for step in s
                            .setup_automation_steps
                            .iter_mut()
                            .chain(s.setup_prompt_steps.iter_mut())
                            .chain(s.verification_steps.iter_mut())
                            .chain(s.agentic_steps.iter_mut())
                            .chain(s.completion_automation_steps.iter_mut())
                            .chain(s.completion_prompt_steps.iter_mut())
                        {
                            for (name, value) in &shared_vars {
                                let pattern = format!("{{{{{}}}}}", name);
                                if let Some(ref mut content) = step.prompt_content {
                                    if content.contains(&pattern) {
                                        *content = content.replace(&pattern, value);
                                    }
                                }
                                if let Some(ref mut cmd) = step.shell_command {
                                    if cmd.contains(&pattern) {
                                        *cmd = cmd.replace(&pattern, value);
                                    }
                                }
                                if let Some(ref mut cmd) = step.check_command {
                                    if cmd.contains(&pattern) {
                                        *cmd = cmd.replace(&pattern, value);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Enrich reflection context with pre-loaded files and project structure
            if config.reflection_mode || config.base_prompt.contains("{{referenced_files}}") {
                crate::reflection::workflow::enrich_reflection_context(
                    self.setup_executor.shared_variables(),
                    config.project_path.as_deref(),
                );
                // Re-run substitution for the newly added variables
                let enriched_vars = self.setup_executor.shared_variables().get_all();
                for (name, value) in &enriched_vars {
                    let pattern = format!("{{{{{}}}}}", name);
                    if config.base_prompt.contains(&pattern) {
                        config.base_prompt = config.base_prompt.replace(&pattern, value);
                    }
                }
                // Also substitute enriched variables into stage step contents
                for si in stage_idx..config.stages.len() {
                    let s = &mut config.stages[si];
                    for step in s
                        .setup_automation_steps
                        .iter_mut()
                        .chain(s.setup_prompt_steps.iter_mut())
                        .chain(s.verification_steps.iter_mut())
                        .chain(s.agentic_steps.iter_mut())
                        .chain(s.completion_automation_steps.iter_mut())
                        .chain(s.completion_prompt_steps.iter_mut())
                    {
                        for (name, value) in &enriched_vars {
                            let pattern = format!("{{{{{}}}}}", name);
                            if let Some(ref mut content) = step.prompt_content {
                                if content.contains(&pattern) {
                                    *content = content.replace(&pattern, value);
                                }
                            }
                            if let Some(ref mut cmd) = step.shell_command {
                                if cmd.contains(&pattern) {
                                    *cmd = cmd.replace(&pattern, value);
                                }
                            }
                            if let Some(ref mut cmd) = step.check_command {
                                if cmd.contains(&pattern) {
                                    *cmd = cmd.replace(&pattern, value);
                                }
                            }
                        }
                    }
                }
            }

            // Always provide project context (tree + root) for all workflows
            if self
                .setup_executor
                .shared_variables()
                .get("project_root")
                .is_none()
            {
                crate::reflection::workflow::enrich_project_context(
                    self.setup_executor.shared_variables(),
                    config.project_path.as_deref(),
                );
                // Substitute the new variables into base_prompt
                let project_vars = self.setup_executor.shared_variables().get_all();
                for (name, value) in &project_vars {
                    if name == "project_root" || name == "project_structure" {
                        let pattern = format!("{{{{{}}}}}", name);
                        if config.base_prompt.contains(&pattern) {
                            config.base_prompt = config.base_prompt.replace(&pattern, value);
                        }
                    }
                }
            }

            // ─── Validate critical shared variables ───
            // After all setup steps and enrichment, check that critical template
            // variables were actually populated. If a setup API call failed (e.g.,
            // curl returned an error), the variable remains unset and the literal
            // {{variable_name}} text ends up in the agentic prompt.
            {
                let base_url = crate::mcp::types::get_self_base_url_from_env();
                let critical_vars = [
                    "source_findings",
                    "source_ai_output",
                    "source_workflow_state",
                ];
                for var_name in &critical_vars {
                    let pattern = format!("{{{{{}}}}}", var_name);
                    if config.base_prompt.contains(&pattern) {
                        let fallback = format!(
                            "Data loading failed for '{}'. Use the runner API directly: GET {}/task-runs/{{execution_id}}/output",
                            var_name, base_url
                        );
                        warn!(
                            "Stage {}: Critical variable '{}' was not populated by setup — injecting fallback message",
                            stage_num, var_name
                        );
                        config.base_prompt = config.base_prompt.replace(&pattern, &fallback);
                    }
                }
                // Also check for any remaining unresolved {{...}} markers in the prompt
                // and log warnings (but don't replace them — they may be intentional templates)
                let remaining_markers: Vec<&str> = config
                    .base_prompt
                    .match_indices("{{")
                    .filter_map(|(start, _)| {
                        config.base_prompt[start..]
                            .find("}}")
                            .map(|end| &config.base_prompt[start..start + end + 2])
                    })
                    .collect();
                if !remaining_markers.is_empty() {
                    warn!(
                        "Stage {}: {} unresolved template marker(s) remain in base_prompt: {:?}",
                        stage_num,
                        remaining_markers.len(),
                        &remaining_markers[..remaining_markers.len().min(5)]
                    );
                }
            }

            // Reborrow stage after mutable substitution
            let stage = &config.stages[stage_idx];

            // ─── Stage Verification-Agentic Loop ───
            let has_agentic = !stage.agentic_steps.is_empty();
            if !stage.verification_steps.is_empty() || has_agentic {
                info!(
                    "  Stage {}: Running verification-agentic loop (max_iterations={})",
                    stage_num, stage.max_iterations
                );

                // Build a per-stage LoopConfig with provider/model overrides from stage config.
                // If resuming into this specific stage, set starting_iteration from the resume point.
                // Also check config.run_agentic_first for the first stage on fresh starts
                // (e.g. error-fix workflows that need AI analysis before verification).
                let (stage_starting_iter, stage_agentic_first) = if stage_idx == start_from_stage {
                    match resume_point {
                        ResumePoint::VerificationPhase { iteration, .. } => {
                            (iteration.saturating_sub(1), false)
                        }
                        ResumePoint::AgenticPhase { iteration, .. } => {
                            (iteration.saturating_sub(1), true)
                        }
                        _ => (0, config.run_agentic_first && stage_idx == 0),
                    }
                } else {
                    (0, false)
                };
                let mut stage_loop_config = LoopConfig {
                    max_iterations: stage.max_iterations,
                    base_prompt: config.base_prompt.clone(),
                    workflow_name: format!(
                        "{} > Stage {}: {}",
                        config.workflow_name, stage_num, stage.name
                    ),
                    workflow_id: config.workflow_id.clone(),
                    execution_id: config.execution_id.clone(),
                    targeted_error_ids: config.targeted_error_ids.clone(),
                    starting_iteration: stage_starting_iter,
                    run_agentic_first: stage_agentic_first,
                    artifact_dir: config.artifact_dir.clone(),
                    is_dev_mode: config.is_dev_mode,
                    enable_sweep: false, // Sweep only at workflow level
                    max_sweep_iterations: 0,
                    stages: Vec::new(), // No nested stages
                    stop_on_failure: config.stop_on_failure,
                    constraint_overrides: config.constraint_overrides.clone(),
                    reflection_mode: config.reflection_mode,
                    provider_override: stage.provider.clone(),
                    model_override: stage.model.clone(),
                    model_overrides: stage.model_overrides.clone(),
                    stage_index: Some(stage_idx as u32),
                    max_sessions: config.max_sessions,
                    auto_run_generated: false,
                    approval_gate: config.approval_gate,
                    max_context_tokens: config.max_context_tokens,
                    cross_workflow_learning: config.cross_workflow_learning,
                    verification_history: std::collections::HashMap::new(),
                    routing_context: Default::default(),
                    project_path: config.project_path.clone(),
                    acceptance_criteria: config.acceptance_criteria.clone(),
                    multi_agent_mode: config.multi_agent_mode,
                    use_worktree: false, // Worktree is handled at workflow level, not per-stage
                    worktree_path: config.worktree_path.clone(),
                    worktree_branch: config.worktree_branch.clone(),
                    workflow_architecture: config.workflow_architecture.clone(),
                    agentic_verification_config: config.agentic_verification_config.clone(),
                    multi_agent_pipeline_config: config.multi_agent_pipeline_config.clone(),
                };

                // Handle agentic-first: run the agentic phase before the verification loop.
                // This is needed for error-fix workflows (run AI analysis before verification)
                // and for resume scenarios where execution was interrupted during agentic phase.
                let initial_dynamic_steps = if stage_agentic_first {
                    let agentic_iteration = stage_starting_iter + 1;
                    info!(
                        "  Stage {}: Running agentic-first (iteration {})",
                        stage_num, agentic_iteration
                    );

                    self.persist_workflow_state(
                        &config.execution_id,
                        &UnifiedWorkflowState::agentic_running(agentic_iteration),
                    );
                    self.record_stage_transition(
                        &config.execution_id,
                        &mut transitions,
                        &mut current_stage,
                        &format!("stage_{}_agentic", stage_num),
                        agentic_iteration,
                    );

                    let agentic_context = build_resume_agentic_context(
                        &self.checkpoint_db,
                        &config.execution_id,
                        agentic_iteration,
                    );

                    let (outcome, injected_steps) = self
                        .agentic_executor
                        .run_agentic(
                            &stage_loop_config,
                            agentic_iteration,
                            &agentic_context,
                            has_agentic,
                            &stage.agentic_steps,
                            logger,
                        )
                        .await;

                    // Log agentic output
                    let output_text = match &outcome {
                        AgenticOutcome::Success { output, .. } => {
                            format!(
                                "\n=== Agentic Phase (iteration {}) ===\n{}",
                                agentic_iteration, output
                            )
                        }
                        AgenticOutcome::Failed { output, error, .. } => {
                            warn!(
                                "Stage {} agentic-first failed: {}, continuing with verification",
                                stage_num, error
                            );
                            format!(
                                "\n=== Agentic Phase (iteration {}, FAILED: {}) ===\n{}",
                                agentic_iteration, error, output
                            )
                        }
                        AgenticOutcome::Error { error } => {
                            warn!(
                                "Stage {} agentic-first errored: {}, continuing with verification",
                                stage_num, error
                            );
                            format!(
                                "\n=== Agentic Phase (iteration {}, ERROR: {}) ===\n",
                                agentic_iteration, error
                            )
                        }
                        AgenticOutcome::Skipped => String::new(),
                    };
                    if !output_text.is_empty() {
                        let _ = self.checkpoint_db.append_task_output_ex(
                            &config.execution_id,
                            &output_text,
                            true,
                            false,
                        );
                    }

                    self.persist_workflow_state(
                        &config.execution_id,
                        &UnifiedWorkflowState::agentic_complete(agentic_iteration),
                    );

                    // Notify canvas of agentic-first completion
                    self.canvas_manager
                        .lock()
                        .await
                        .on_agentic_complete(
                            agentic_iteration,
                            &outcome,
                            &[],
                            injected_steps.len(),
                            0,
                        )
                        .await;

                    // Adjust starting_iteration so the loop continues after this agentic phase,
                    // and reduce max_iterations by 1 to account for the session already consumed.
                    // Without this, the total sessions = 1 (agentic-first) + max_iterations - 1
                    // (loop iterations), which exceeds the intended budget by 1.
                    stage_loop_config.starting_iteration = agentic_iteration;
                    stage_loop_config.max_iterations =
                        stage_loop_config.max_iterations.saturating_sub(1);

                    if !injected_steps.is_empty() {
                        info!(
                            "  Stage {}: Agentic-first injected {} dynamic verification step(s)",
                            stage_num,
                            injected_steps.len()
                        );
                    }

                    injected_steps
                } else {
                    Vec::new()
                };

                self.record_stage_transition(
                    &config.execution_id,
                    &mut transitions,
                    &mut current_stage,
                    &format!("stage_{}_verification", stage_num),
                    0,
                );

                let loop_result = if matches!(
                    stage_loop_config.workflow_architecture,
                    Some(crate::autoresearch::agentic_verification::WorkflowArchitecture::AgenticVerification)
                ) {
                    info!(
                        "  Stage {}: Using AGENTIC VERIFICATION architecture",
                        stage_num
                    );
                    self.run_agentic_verification_loop(
                        &mut stage_loop_config,
                        has_agentic,
                        &stage.agentic_steps,
                        &mut all_step_results,
                        logger,
                    )
                    .await
                } else if matches!(
                    stage_loop_config.workflow_architecture,
                    Some(crate::autoresearch::agentic_verification::WorkflowArchitecture::MultiAgentPipeline)
                ) {
                    info!(
                        "  Stage {}: Using MULTI-AGENT PIPELINE architecture",
                        stage_num
                    );
                    self.run_multi_agent_pipeline_loop(
                        &mut stage_loop_config,
                        &stage.verification_steps,
                        has_agentic,
                        &stage.agentic_steps,
                        &mut all_step_results,
                        logger,
                    )
                    .await
                } else {
                    self.run_verification_agentic_loop(
                        &mut stage_loop_config,
                        &stage.verification_steps,
                        has_agentic,
                        &stage.agentic_steps,
                        &mut all_step_results,
                        &mut transitions,
                        &mut current_stage,
                        logger,
                        initial_dynamic_steps,
                    )
                    .await
                };

                info!(
                    "  Stage {}: Loop result: {}",
                    stage_num,
                    loop_result.summary()
                );

                if loop_result.verification_passed {
                    any_stage_passed = true;
                }

                // Check if completion has critical artifact steps that must always run
                let has_artifact_save = stage
                    .completion_automation_steps
                    .iter()
                    .any(|s| s.step_type == "save_workflow_artifact");
                let force_completion =
                    has_artifact_save && !loop_result.critical_failure && !loop_result.was_stopped;

                // Run per-stage completion steps if verification passed or forced for artifact saves
                if (loop_result.should_run_completion() || force_completion)
                    && (!stage.completion_automation_steps.is_empty()
                        || !stage.completion_prompt_steps.is_empty())
                {
                    info!("  Stage {}: Running completion", stage_num);
                    // Resolve completion phase model override
                    let comp_model = stage
                        .model_overrides
                        .get("completion")
                        .and_then(|c| c.model.clone())
                        .or_else(|| stage.model.clone())
                        .or_else(|| config.model_override.clone());
                    let comp_provider = stage
                        .model_overrides
                        .get("completion")
                        .and_then(|c| c.provider.clone())
                        .or_else(|| stage.provider.clone())
                        .or_else(|| config.provider_override.clone());

                    let (_, completion_results) = self
                        .completion_executor
                        .run_completion(
                            &stage.completion_automation_steps,
                            &stage.completion_prompt_steps,
                            &config.execution_id,
                            &format!(
                                "{} > Stage {}: {}",
                                config.workflow_name, stage_num, stage.name
                            ),
                            loop_result.iterations_run,
                            logger,
                            Some(stage_idx as u32),
                            comp_model,
                            comp_provider,
                            stage.completion_prompts_first,
                        )
                        .await;
                    all_step_results.extend(completion_results);
                }

                if !loop_result.verification_passed && config.stop_on_failure {
                    warn!(
                        "Stage {} verification failed and stop_on_failure=true",
                        stage_num
                    );
                    self.persist_workflow_state(
                        &config.execution_id,
                        &UnifiedWorkflowState::failed_in_phase(
                            format!("Stage {} verification failed", stage_num),
                            "verification",
                            Some(loop_result.iterations_run),
                        ),
                    );
                    self.mark_task_failed(
                        &config.execution_id,
                        &format!(
                            "Stage {} verification failed after {} iterations",
                            stage_num, loop_result.iterations_run
                        ),
                        Some(&config.workflow_id),
                    )
                    .await;
                    {
                        let sessions = self.canvas_manager.lock().await.get_sessions_count().await;
                        self.canvas_manager
                            .lock()
                            .await
                            .on_workflow_complete(&loop_result, start.elapsed(), sessions)
                            .await;
                    }
                    return WorkflowResult {
                        success: false,
                        verification_passed: false,
                        step_results: all_step_results,
                        duration_ms: start.elapsed().as_millis() as u64,
                        loop_result: Some(loop_result),
                        worktree_path: config.worktree_path.clone(),
                        worktree_branch: config.worktree_branch.clone(),
                        workflow_architecture: config.workflow_architecture.clone(),
                        agentic_verification_config: config.agentic_verification_config.clone(),
                        multi_agent_pipeline_config: config.multi_agent_pipeline_config.clone(),
                    };
                }

                // Update tracking counters for conditional stage evaluation
                total_iterations_across_stages += loop_result.iterations_run;
                if !loop_result.verification_passed {
                    total_stage_failures += 1;
                }

                last_loop_result = Some(loop_result);
            }

            // Mark stage as complete
            self.persist_workflow_state(
                &config.execution_id,
                &UnifiedWorkflowState::stage_complete(stage_idx as u32),
            );

            info!("  Stage {}/{} complete", stage_num, total_stages);
        }

        // ─── Workflow-level Completion ───
        // A workflow passes only if at least one stage's verification actually passed.
        // Previously, stop_on_failure=false + any loop having run was enough to mark
        // overall_passed=true, which caused tasks to be marked "completed" even when
        // all stages failed verification (zombie "running" tasks if completion errored).
        let overall_passed = any_stage_passed;
        let total_iterations = last_loop_result
            .as_ref()
            .map(|r| r.iterations_run)
            .unwrap_or(0);

        // ─── Completion Sweep (optional, runs after verification passes) ───
        if config.enable_sweep && overall_passed && !self.is_task_stopped(&config.execution_id) {
            info!(
                "=== COMPLETION SWEEP (max {} iterations) ===",
                config.max_sweep_iterations
            );

            let sweep_result = self
                .run_sweep_loop(&config, &mut all_step_results, logger, total_iterations)
                .await;

            info!(
                "Sweep completed: {} iteration(s), no_more_steps={}",
                sweep_result.iterations_run, sweep_result.no_more_steps
            );
        }

        if overall_passed {
            info!("=== WORKFLOW COMPLETED: All stages processed ===");
            self.persist_workflow_state(
                &config.execution_id,
                &UnifiedWorkflowState::completion_running(),
            );
            self.record_stage_transition(
                &config.execution_id,
                &mut transitions,
                &mut current_stage,
                "completion",
                0,
            );

            self.persist_workflow_state(
                &config.execution_id,
                &UnifiedWorkflowState::completion_complete(),
            );
            self.mark_task_completed(&config.execution_id, Some(&config.workflow_id))
                .await;

            // Emit canvas outcome panel for successful completion
            if let Some(ref lr) = last_loop_result {
                let cm = &self.canvas_manager;
                let sessions = cm.lock().await.get_sessions_count().await;
                cm.lock()
                    .await
                    .on_workflow_complete(lr, start.elapsed(), sessions)
                    .await;
            } else {
                self.canvas_manager
                    .lock()
                    .await
                    .on_workflow_failed("Workflow completed (no verification loop)")
                    .await;
            }

            // Resolve targeted errors on successful completion
            if any_stage_passed && !config.targeted_error_ids.is_empty() {
                self.resolve_targeted_errors(&config.execution_id, &config.targeted_error_ids)
                    .await;
            }

            // Auto-resolve errors captured during this workflow run
            if any_stage_passed {
                self.resolve_workflow_scoped_errors(&config.execution_id)
                    .await;
            }

            // Fire-and-forget summary generation with per-phase model override
            let db = self.checkpoint_db.clone();
            let exec_id = config.execution_id.clone();
            let doctor_handle = self.doctor_handle.clone();
            let summary_model = config.resolve_model_for_phase("summary");
            let summary_provider = config.resolve_provider_for_phase("summary");
            tokio::spawn(async move {
                match generate_task_summary_async(
                    db,
                    exec_id.clone(),
                    doctor_handle,
                    summary_model,
                    summary_provider,
                )
                .await
                {
                    Ok(_) => info!("Generated summary for completed task {}", exec_id),
                    Err(e) => warn!("Failed to generate summary for task {}: {}", exec_id, e),
                }
            });

            info!("=== WORKFLOW COMPLETED SUCCESSFULLY ===");
        } else {
            let fail_reason = if let Some(ref lr) = last_loop_result {
                if lr.max_iterations_reached {
                    format!(
                        "Verification failed after {} iterations (max_iterations={} exhausted)",
                        lr.iterations_run, config.max_iterations
                    )
                } else if lr.was_stopped {
                    format!("Workflow stopped after {} iterations", lr.iterations_run)
                } else if lr.unfixable_errors {
                    format!(
                        "Unfixable errors detected after {} iterations",
                        lr.iterations_run
                    )
                } else if lr.critical_failure {
                    format!(
                        "Critical failure during verification after {} iterations",
                        lr.iterations_run
                    )
                } else {
                    format!(
                        "No stages passed verification after {} iterations",
                        lr.iterations_run
                    )
                }
            } else {
                "No stages passed verification (no iterations ran)".to_string()
            };
            info!("=== WORKFLOW FAILED: {} ===", fail_reason);
            self.persist_workflow_state(
                &config.execution_id,
                &UnifiedWorkflowState::failed_in_phase(
                    &fail_reason,
                    "verification",
                    last_loop_result.as_ref().map(|r| r.iterations_run),
                ),
            );
            self.mark_task_failed(
                &config.execution_id,
                &fail_reason,
                Some(&config.workflow_id),
            )
            .await;

            // Emit canvas outcome panel for failed completion
            if let Some(ref lr) = last_loop_result {
                let cm = &self.canvas_manager;
                let sessions = cm.lock().await.get_sessions_count().await;
                cm.lock()
                    .await
                    .on_workflow_complete(lr, start.elapsed(), sessions)
                    .await;
            } else {
                self.canvas_manager
                    .lock()
                    .await
                    .on_workflow_failed("No stages passed verification")
                    .await;
            }

            // Fire-and-forget summary generation for failed task with per-phase model override
            let db = self.checkpoint_db.clone();
            let exec_id = config.execution_id.clone();
            let doctor_handle = self.doctor_handle.clone();
            let summary_model = config.resolve_model_for_phase("summary");
            let summary_provider = config.resolve_provider_for_phase("summary");
            tokio::spawn(async move {
                match generate_task_summary_async(
                    db,
                    exec_id.clone(),
                    doctor_handle,
                    summary_model,
                    summary_provider,
                )
                .await
                {
                    Ok(_) => info!("Generated summary for failed task {}", exec_id),
                    Err(e) => warn!(
                        "Failed to generate summary for failed task {}: {}",
                        exec_id, e
                    ),
                }
            });

            info!("=== WORKFLOW FAILED ===");
        }

        // Record learning outcome for all workflows (fire-and-forget)
        {
            let db = self.checkpoint_db.clone();
            let category = if config.workflow_name.starts_with("AI Generate:") {
                "meta"
            } else if config.is_dev_mode {
                "dev"
            } else {
                "production"
            };
            // Infer architecture: use explicit config, infer from sub-configs, or default to "traditional"
            let architecture = config
                .workflow_architecture
                .as_ref()
                .map(|a| {
                    serde_json::to_value(a)
                        .ok()
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or_else(|| format!("{:?}", a).to_lowercase())
                })
                .unwrap_or_else(|| {
                    if config.multi_agent_pipeline_config.is_some() {
                        "multi_agent_pipeline".to_string()
                    } else if config.agentic_verification_config.is_some() {
                        "agentic_verification".to_string()
                    } else {
                        "traditional".to_string()
                    }
                });
            let outcome = crate::orchestrator::learning_recorder::WorkflowOutcome {
                task_run_id: config.execution_id.clone(),
                workflow_name: config.workflow_name.clone(),
                category: category.to_string(),
                status: if overall_passed {
                    "complete".to_string()
                } else {
                    "failed".to_string()
                },
                duration_secs: start.elapsed().as_secs_f64(),
                iterations: total_iterations,
                verification_passed: overall_passed,
                max_iterations_reached: last_loop_result
                    .as_ref()
                    .map(|r| r.max_iterations_reached)
                    .unwrap_or(false),
                was_stopped: last_loop_result
                    .as_ref()
                    .map(|r| r.was_stopped)
                    .unwrap_or(false),
                tools_used: Vec::new(),
                files_modified: Vec::new(),
                error_type: None,
                error_message: None,
                workflow_architecture: Some(architecture),
            };
            tokio::spawn(async move {
                if let Err(e) = db.with_conn(|conn| {
                    crate::orchestrator::learning_recorder::record_workflow_learning(conn, &outcome)
                }) {
                    warn!("Failed to record learning outcome: {}", e);
                }
            });
        }

        // Trigger reflection workflows
        // - Project reflection: runs in BOTH dev and production modes (non-generation workflows)
        // - Workflow reflection: runs in dev mode only (non-generation workflows)
        // - Generation reflection: runs in dev mode only (generation workflows)
        // - UI Bridge reflection: runs on NON-dev-mode workflows (real workflows that exercise UI Bridge)
        {
            let session_manager: Option<Arc<crate::claude_session::SessionManager>> = self
                .app_handle
                .try_state::<Arc<crate::claude_session::SessionManager>>()
                .map(|s| s.inner().clone());

            let is_generation = config.workflow_name.starts_with("AI Generate:");

            // Project reflection (both dev and production, non-generation only)
            if !is_generation {
                let project_deps = crate::reflection::trigger::ReflectionDeps {
                    app_state: self.app_state.clone(),
                    config_storage: self.config_storage.clone(),
                    app_handle: self.app_handle.clone(),
                    pid_tracker: self.pid_tracker.clone(),
                    session_manager: session_manager.clone(),
                };
                let project_source_id = config.execution_id.clone();
                tokio::spawn(async move {
                    // Delay to allow summary generation to finish
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    match crate::reflection::trigger::launch_project_reflection(
                        project_deps,
                        project_source_id.clone(),
                    ) {
                        Ok(id) if id == "skipped" => {
                            debug!("Project reflection skipped for {}", project_source_id);
                        }
                        Ok(id) => {
                            info!(
                                "Launched project reflection {} for completed run {}",
                                id, project_source_id
                            );
                        }
                        Err(e) => {
                            warn!(
                                "Failed to launch project reflection for {}: {}",
                                project_source_id, e
                            );
                        }
                    }
                });
            }

            // Workflow/generation reflection (dev mode only)
            if config.is_dev_mode {
                let deps = crate::reflection::trigger::ReflectionDeps {
                    app_state: self.app_state.clone(),
                    config_storage: self.config_storage.clone(),
                    app_handle: self.app_handle.clone(),
                    pid_tracker: self.pid_tracker.clone(),
                    session_manager: session_manager.clone(),
                };
                let source_task_run_id = config.execution_id.clone();
                tokio::spawn(async move {
                    // Delay: 10s for workflow reflection (after project reflection's 5s)
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
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

            // UI Bridge reflection (non-dev-mode workflows only, 15s delay)
            // This is the inverse of other reflections: it runs on real workflows
            // (user-triggered and auto-run generated), NOT on dev-mode workflows
            // (reflections, follow-ups). Dev-mode workflows don't exercise UI Bridge
            // at runtime — the real workflows do. The guard function additionally
            // checks for actual UI Bridge activity before launching.
            if !config.is_dev_mode {
                let ub_deps = crate::reflection::trigger::ReflectionDeps {
                    app_state: self.app_state.clone(),
                    config_storage: self.config_storage.clone(),
                    app_handle: self.app_handle.clone(),
                    pid_tracker: self.pid_tracker.clone(),
                    session_manager: session_manager.clone(),
                };
                let ub_source_id = config.execution_id.clone();
                tokio::spawn(async move {
                    // Delay: 15s (after workflow reflection's 10s, before follow-up's 20s)
                    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                    match crate::reflection::trigger::launch_ui_bridge_reflection(
                        ub_deps,
                        ub_source_id.clone(),
                    ) {
                        Ok(id) if id == "skipped" => {
                            debug!("UI Bridge reflection skipped for {}", ub_source_id);
                        }
                        Ok(id) => {
                            info!(
                                "Launched UI Bridge reflection {} for completed run {}",
                                id, ub_source_id
                            );
                        }
                        Err(e) => {
                            warn!(
                                "Failed to launch UI Bridge reflection for {}: {}",
                                ub_source_id, e
                            );
                        }
                    }
                });
            }

            // Trigger follow-up workflow (20s delay, after reflections)
            if config.is_dev_mode {
                let follow_up_deps = crate::follow_up::trigger::FollowUpDeps {
                    app_state: self.app_state.clone(),
                    config_storage: self.config_storage.clone(),
                    app_handle: self.app_handle.clone(),
                    pid_tracker: self.pid_tracker.clone(),
                    session_manager: session_manager.clone(),
                };
                let follow_up_source_id = config.execution_id.clone();
                tokio::spawn(async move {
                    // Longer delay to allow reflections to start first
                    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                    match crate::follow_up::trigger::launch_follow_up(
                        follow_up_deps,
                        follow_up_source_id.clone(),
                    ) {
                        Ok(id) if id == "skipped" => {
                            debug!("Follow-up skipped for {}", follow_up_source_id);
                        }
                        Ok(id) => {
                            info!(
                                "Launched follow-up {} for completed run {}",
                                id, follow_up_source_id
                            );
                        }
                        Err(e) => {
                            warn!(
                                "Failed to launch follow-up for {}: {}",
                                follow_up_source_id, e
                            );
                        }
                    }
                });
            }

            // Trigger fixer workflow (30s delay, waits for all children to complete)
            if config.is_dev_mode {
                let fixer_deps = crate::fixer::trigger::FixerDeps {
                    app_state: self.app_state.clone(),
                    config_storage: self.config_storage.clone(),
                    app_handle: self.app_handle.clone(),
                    pid_tracker: self.pid_tracker.clone(),
                    session_manager: session_manager.clone(),
                };
                let fixer_source_id = config.execution_id.clone();
                // launch_fixer is sync — it spawns its own async task internally
                // that waits for children before running the fixer workflow
                match crate::fixer::trigger::launch_fixer(fixer_deps, fixer_source_id.clone()) {
                    Ok(id) if id == "skipped" => {
                        debug!("Fixer skipped for {}", fixer_source_id);
                    }
                    Ok(id) => {
                        info!(
                            "Launched fixer {} for completed run {}",
                            id, fixer_source_id
                        );
                    }
                    Err(e) => {
                        warn!("Failed to launch fixer for {}: {}", fixer_source_id, e);
                    }
                }

                // Trigger meta-optimizer (threshold-based, most runs it's a fast no-op)
                let meta_deps = crate::meta_optimizer::types::MetaOptimizerDeps {
                    app_state: self.app_state.clone(),
                    config_storage: self.config_storage.clone(),
                    app_handle: self.app_handle.clone(),
                    pid_tracker: self.pid_tracker.clone(),
                    session_manager: session_manager.clone(),
                };
                match crate::meta_optimizer::trigger::check_and_launch_optimizers(
                    meta_deps,
                    config.execution_id.clone(),
                ) {
                    Ok(ids) => {
                        for id in &ids {
                            info!("Launched meta-optimizer {}", id);
                        }
                    }
                    Err(e) => {
                        warn!("Meta-optimizer trigger failed: {}", e);
                    }
                }
            }
        }

        // Auto-run generated workflow (for "Generate & Run" flow)
        // Always attempt if auto_run_generated is set — don't gate on overall_passed.
        // The save_workflow_artifact step already gates on stage completion, so if
        // result_data has a generated_workflow_id, the workflow was saved successfully.
        // launch_generated_workflow will fail gracefully if no workflow was produced.
        if config.auto_run_generated {
            let deps = super::auto_run::AutoRunDeps {
                app_state: self.app_state.clone(),
                config_storage: self.config_storage.clone(),
                app_handle: self.app_handle.clone(),
                pid_tracker: self.pid_tracker.clone(),
            };
            let meta_task_run_id = config.execution_id.clone();

            tokio::spawn(async move {
                // Delay to allow result_data to be fully written
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;

                match super::auto_run::launch_generated_workflow(deps, &meta_task_run_id) {
                    Ok(task_run_id) => {
                        info!(
                            "Auto-run launched generated workflow {} for {}",
                            task_run_id, meta_task_run_id
                        );
                    }
                    Err(e) => {
                        warn!(
                            "Failed to auto-run generated workflow for {}: {}",
                            meta_task_run_id, e
                        );
                    }
                }
            });
        }

        // Aggregate phase token usage totals for the task run
        if let Err(e) = self
            .checkpoint_db
            .update_task_run_token_totals(&config.execution_id)
        {
            warn!(
                "Failed to aggregate token totals for {}: {}",
                config.execution_id, e
            );
        }

        WorkflowResult {
            success: overall_passed,
            verification_passed: overall_passed,
            step_results: all_step_results,
            duration_ms: start.elapsed().as_millis() as u64,
            loop_result: last_loop_result,
            worktree_path: config.worktree_path.clone(),
            worktree_branch: config.worktree_branch.clone(),
            workflow_architecture: config.workflow_architecture.clone(),
            agentic_verification_config: config.agentic_verification_config.clone(),
            multi_agent_pipeline_config: config.multi_agent_pipeline_config.clone(),
        }
    }

    /// The core verification-agentic loop.
    ///
    /// CRITICAL: This loop is the ONLY authority on whether verification passes.
    /// The AI cannot bypass this by outputting [TASK_COMPLETE] or similar.
    #[instrument(
        name = "qontinui.workflow.verification_loop",
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
        config: &mut LoopConfig,
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

        // Enforce a floor of 1 on max_iterations to prevent zero-iteration failures
        // where workflows crash at startup without ever running.
        config.max_iterations = config.max_iterations.max(1);

        // Convergence detector: replaces inline stall/flaky heuristics with
        // pattern detection that returns actionable feedback for the AI.
        let mut convergence_detector = super::convergence::ConvergenceDetector::new(
            super::convergence::ConvergenceConfig::default(),
        );

        // Load constraint config from project directory (constraints.toml).
        // This provides builtin overrides, custom constraints, and resource limits.
        let constraint_config = config
            .project_path
            .as_ref()
            .and_then(|p| crate::constraint_engine::config::load_config(std::path::Path::new(p)));

        // Resource tracker: monitors wall time, file change blast radius, and
        // agentic phase cumulative time. Use limits from config if available.
        let resource_limits = constraint_config
            .as_ref()
            .and_then(|c| c.resource_limits.clone())
            .unwrap_or_default();
        let mut resource_tracker =
            super::convergence::ResourceTracker::new(resource_limits, std::time::Instant::now());

        // Constraint engine: evaluates declarative constraints against modified files
        // after each agentic phase. Violations become context injections for the AI.
        let mut constraint_engine = if let Some(ref loaded) = constraint_config {
            crate::constraint_engine::ConstraintEngine::from_config(loaded)
        } else {
            crate::constraint_engine::ConstraintEngine::new()
        };
        // Set project root for file resolution if available
        if let Some(ref p) = config.project_path {
            constraint_engine = constraint_engine.with_project_root(std::path::PathBuf::from(p));
        }

        // Apply per-workflow constraint overrides (enable/disable specific constraints)
        for (constraint_id, enabled) in &config.constraint_overrides {
            if *enabled {
                constraint_engine.enable_constraint(constraint_id);
            } else {
                constraint_engine.disable_constraint(constraint_id);
            }
        }

        // Pending constraint violations from the previous agentic phase, to be
        // injected into the next iteration's failure context.
        let mut pending_constraint_context: Option<String> = None;

        // Pre-build the proactive constraints prompt so the AI knows about
        // active constraints upfront (before it violates them). Only injected
        // once — on the first agentic iteration — to avoid wasting tokens.
        let proactive_constraints_prompt = {
            let prompt = constraint_engine.constraints_prompt();
            if !prompt.is_empty() {
                info!(
                    "CONSTRAINT-ENGINE: Generated proactive constraints prompt ({} chars)",
                    prompt.len(),
                );
                Some(prompt)
            } else {
                None
            }
        };
        let mut constraints_prompt_injected = false;

        // Track cumulative verification failures for conditional routing context
        let mut verification_failures: u32 = 0;

        // Dynamic verification steps accumulated from agentic phase outputs.
        // These are merged with the static verification steps for subsequent iterations.
        let mut dynamic_steps: Vec<ExecutionStepConfig> = initial_dynamic_steps;
        if !dynamic_steps.is_empty() {
            info!(
                "Starting verification-agentic loop with {} pre-injected dynamic step(s)",
                dynamic_steps.len()
            );
        }

        // Health regression warning from previous iteration's agentic phase.
        // Injected into the current iteration's failure context so the AI knows
        // its changes degraded the app.
        let mut pending_health_regression: Option<String> = None;

        let loop_result = loop {
            iteration += 1;

            // Update routing context for conditional model routing
            config.set_routing_context(iteration, verification_failures);

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

            self.record_activity(
                &config.execution_id,
                &format!("loop_iteration_{}_start", iteration),
            );

            // Check if the task has been stopped externally (e.g., user clicked Stop button)
            if self.is_task_stopped(&config.execution_id) {
                warn!("Task was stopped externally - exiting loop");
                break LoopResult {
                    iterations_run: iteration - 1,
                    verification_passed: false,
                    max_iterations_reached: false,
                    critical_failure: false,
                    was_stopped: true,
                    unfixable_errors: false,
                    iteration_results,
                };
            }

            // Wait if paused (user clicked Pause in the dashboard)
            self.wait_while_paused(&config.execution_id).await;

            // Re-check stop after unpause (user may have stopped while paused)
            if self.is_task_stopped(&config.execution_id) {
                warn!("Task was stopped while paused - exiting loop");
                break LoopResult {
                    iterations_run: iteration - 1,
                    verification_passed: false,
                    max_iterations_reached: false,
                    critical_failure: false,
                    was_stopped: true,
                    unfixable_errors: false,
                    iteration_results,
                };
            }

            // Check global max_sessions budget (across all stages).
            // sessions_count is incremented in the database on each agentic session,
            // so this enforces the overall session budget even in multi-stage workflows.
            if let Some(max) = config.max_sessions {
                if let Ok(Some(task_run)) = self.checkpoint_db.get_task_run(&config.execution_id) {
                    if task_run.sessions_count >= max {
                        warn!(
                            "Global max_sessions ({}) reached (sessions_count={}) - exiting loop",
                            max, task_run.sessions_count
                        );
                        break LoopResult {
                            iterations_run: iteration - 1,
                            verification_passed: false,
                            max_iterations_reached: true,
                            critical_failure: false,
                            was_stopped: false,
                            unfixable_errors: true,
                            iteration_results,
                        };
                    }
                }
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
                break LoopResult {
                    iterations_run: iteration - 1, // Don't count this iteration
                    verification_passed: false,
                    max_iterations_reached: true,
                    critical_failure: false,
                    was_stopped: false,
                    unfixable_errors: true,
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
            // AUTO-CONNECT SDK FOR UI-FOCUSED WORKFLOWS (first iteration only)
            // -----------------------------------------------------------------
            if iteration == 1 {
                super::phases::try_auto_connect_sdk_for_ui_workflow(&config.workflow_name).await;
            }

            // -----------------------------------------------------------------
            // ENVIRONMENT READINESS CHECK (before verification)
            // -----------------------------------------------------------------
            // Check that the runtime environment (runner API, SDK connection, app health)
            // is ready before running verification assertions. If issues are found, attempt
            // automated recovery (reconnect SDK, refresh page). This prevents wasting
            // agentic iterations on environment problems vs actual code issues.
            {
                let env_result =
                    super::phases::check_environment_readiness(iteration, &config.workflow_name)
                        .await;

                if !env_result.ready {
                    // Log environment issue to task output
                    let _ = self.checkpoint_db.append_task_output_ex(
                        &config.execution_id,
                        &format!(
                            "\n--- Environment Check (Iteration {}): NOT READY ---\n{}\n",
                            iteration, env_result.summary
                        ),
                        false,
                        false,
                    );

                    // Merge env failure context with any existing health regression from the
                    // previous iteration (don't clobber — both are valuable diagnostic context).
                    if let Some(env_ctx) = env_result.env_failure_context {
                        pending_health_regression = Some(match pending_health_regression.take() {
                            Some(existing) => format!("{}\n\n{}", env_ctx, existing),
                            None => env_ctx,
                        });
                    }

                    warn!(
                        "ENV-DOCTOR: Environment not ready for iteration {} — \
                         verification will likely fail due to environment, not code issues",
                        iteration
                    );
                } else {
                    if env_result.recovery_attempted {
                        let _ = self.checkpoint_db.append_task_output_ex(
                            &config.execution_id,
                            &format!(
                                "\n--- Environment Check (Iteration {}): RECOVERED ---\n{}\n",
                                iteration, env_result.summary
                            ),
                            false,
                            false,
                        );
                    }
                    debug!("ENV-DOCTOR: {}", env_result.summary);
                }
            }

            // -----------------------------------------------------------------
            // VERIFICATION PHASE
            // -----------------------------------------------------------------
            info!("Running verification phase (iteration {})", iteration);

            // Persist workflow state: VerificationRunning
            self.persist_workflow_state(
                &config.execution_id,
                &UnifiedWorkflowState::verification_running(iteration),
            );

            self.record_activity(
                &config.execution_id,
                &format!("verification_start_iter_{}", iteration),
            );

            self.record_stage_transition(
                &config.execution_id,
                transitions,
                current_stage,
                "verification",
                iteration,
            );

            // Build effective verification steps: static steps + any dynamically injected steps
            let all_verification_steps: Vec<ExecutionStepConfig> = if dynamic_steps.is_empty() {
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

            // Improvement C: Intelligent test selection — skip consistently passing steps
            let mut skipped_consistent_pass: Vec<String> = Vec::new();
            let effective_verification_steps: Vec<ExecutionStepConfig> = if iteration > 3 {
                let mut filtered = Vec::new();
                for step in &all_verification_steps {
                    let step_name = step.name.as_deref().unwrap_or(&step.step_type);
                    let history = config.verification_history.get(step_name);
                    let should_skip = if let Some(results) = history {
                        // Skip if last 3 consecutive iterations all passed
                        let len = results.len();
                        len >= 3 && results[len - 1] && results[len - 2] && results[len - 3]
                    } else {
                        false
                    };

                    if should_skip {
                        let consecutive_passes = history
                            .unwrap()
                            .iter()
                            .rev()
                            .take_while(|&&passed| passed)
                            .count();
                        info!(
                            "VERIFICATION-SKIP: step '{}' skipped (passed {} consecutive times)",
                            step_name, consecutive_passes
                        );
                        skipped_consistent_pass.push(step_name.to_string());
                    } else {
                        filtered.push(step.clone());
                    }
                }
                filtered
            } else {
                all_verification_steps.clone()
            };

            if !skipped_consistent_pass.is_empty() {
                info!(
                    "VERIFICATION-SELECTION: Running {}/{} steps ({} skipped - consistently passing)",
                    effective_verification_steps.len(),
                    all_verification_steps.len(),
                    skipped_consistent_pass.len()
                );
            }

            let (mut verification_result, step_results) = self
                .verification_executor
                .run_verification(
                    &effective_verification_steps,
                    &config.execution_id,
                    iteration,
                    &config.workflow_name,
                    logger,
                    config.stage_index,
                )
                .await;

            // Update skipped_steps count to include consistently-passing skips
            verification_result.skipped_steps += skipped_consistent_pass.len();

            // Update verification history with this iteration's results
            for step_result in &verification_result.step_results {
                let entry = config
                    .verification_history
                    .entry(step_result.step_name.clone())
                    .or_default();
                entry.push(step_result.success);
            }
            // Record skipped-as-passed entries too (they're still passing)
            for name in &skipped_consistent_pass {
                let entry = config.verification_history.entry(name.clone()).or_default();
                entry.push(true);
            }

            // Track regression issue step results
            {
                let db = &self.checkpoint_db;
                for step_result in &verification_result.step_results {
                    if let Some(ref step_id) = step_result.step_id {
                        if let Some(issue_id) = step_id.strip_prefix("regression-") {
                            let issue_id = issue_id.to_string();
                            let success = step_result.success;
                            if let Err(e) = db.with_conn(|conn| {
                                crate::known_issues::storage::increment_checked(conn, &issue_id)?;
                                if !success {
                                    crate::known_issues::storage::increment_detected(
                                        conn, &issue_id,
                                    )?;
                                } else {
                                    // Decay confidence when regression check passes
                                    if let Err(e) =
                                        crate::known_issues::storage::decay_confidence_on_pass(
                                            conn, &issue_id,
                                        )
                                    {
                                        debug!(
                                            "Failed to decay confidence for {}: {}",
                                            issue_id, e
                                        );
                                    }
                                }
                                Ok::<(), String>(())
                            }) {
                                debug!(
                                    "Failed to update known issue tracking for {}: {}",
                                    issue_id, e
                                );
                            }
                        }
                    }
                }
            }

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
                        let truncated = truncate_str(err, 200);
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

            // Emit canvas panel for verification completion
            self.canvas_manager
                .lock()
                .await
                .on_verification_complete(
                    iteration,
                    &verification_result,
                    &config.verification_history,
                )
                .await;

            // Persist workflow state: VerificationComplete
            self.persist_workflow_state(
                &config.execution_id,
                &UnifiedWorkflowState::verification_complete(
                    iteration,
                    verification_result.all_passed,
                ),
            );

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
                let report = convergence_detector.analyze(
                    iteration,
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

                let broadcaster = EventBroadcaster::new(self.app_handle.clone());
                broadcaster.iteration_metrics(
                    &config.execution_id,
                    iteration,
                    failed_count,
                    passed_count,
                    skipped_count,
                    new_failures,
                    repeated_failures,
                    is_stalled,
                );

                // Log convergence status
                if report.has_concerns() {
                    let pattern_names: Vec<&str> = report
                        .patterns
                        .iter()
                        .map(|p| match p {
                            super::convergence::ConvergencePattern::Stuck { .. } => "stuck",
                            super::convergence::ConvergencePattern::Diverging { .. } => "diverging",
                            super::convergence::ConvergencePattern::Oscillating { .. } => {
                                "oscillating"
                            }
                            super::convergence::ConvergencePattern::Plateau { .. } => "plateau",
                            super::convergence::ConvergencePattern::Converging => "converging",
                        })
                        .collect();
                    warn!(
                        "CONVERGENCE: iteration={}, failed={}, passed={}, patterns={:?}, actions={}",
                        iteration,
                        failed_count,
                        passed_count,
                        pattern_names,
                        report.actions.len(),
                    );
                } else {
                    info!(
                        "CONVERGENCE: iteration={}, failed={}, passed={}, healthy=true",
                        iteration, failed_count, passed_count,
                    );
                }

                // Increment cumulative verification failures for routing context
                if !verification_result.all_passed {
                    verification_failures += 1;
                }

                // If escalation was recommended, bump verification_failures to trigger
                // routing rules that use the verification_failures threshold
                if report.should_escalate_model() {
                    info!(
                        "CONVERGENCE-ESCALATE: Bumping verification_failures for routing (was {})",
                        verification_failures
                    );
                    // Ensure it's at least 3 so routing rules like
                    // "verification_failures >= 3 → use opus" fire
                    if verification_failures < 3 {
                        verification_failures = 3;
                    }
                }

                // Check resource limits and merge any actions into the report.
                // Resource actions use the same ConvergenceAction type so the loop
                // controller handles them uniformly with convergence pattern actions.
                let resource_actions = resource_tracker.check_limits(iteration);
                if !resource_actions.is_empty() {
                    // Merge resource actions into the report
                    let mut merged = report;
                    merged.is_healthy = false;
                    for action in &resource_actions {
                        if let super::convergence::ConvergenceAction::InjectContext {
                            context,
                            ..
                        } = action
                        {
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

                break LoopResult {
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

                break LoopResult {
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

            // Inject convergence analysis into the failure context.
            // The detector's context_injection contains formatted warnings about
            // stuck, diverging, oscillating, or plateau patterns — giving the AI
            // actionable feedback to change its approach.
            let failure_context = if !convergence_report.context_injection.is_empty() {
                format!(
                    "{}\n{}",
                    convergence_report.context_injection, failure_context
                )
            } else {
                failure_context
            };

            // Add skip summary to failure context
            let failure_context = if !skipped_consistent_pass.is_empty() {
                format!(
                    "{}\n_Ran {}/{} steps ({} skipped - consistently passing)_\n",
                    failure_context,
                    effective_verification_steps.len(),
                    all_verification_steps.len(),
                    skipped_consistent_pass.len()
                )
            } else {
                failure_context
            };

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
                    &get_parent_task_id(&config.execution_id),
                    iteration,
                    &verification_result,
                ) {
                    Some(warning) => format!("{}\n\n{}", warning, failure_context),
                    None => failure_context,
                }
            } else {
                failure_context
            };

            // Inject environment/health context BEFORE verification failures so the AI
            // sees the environmental framing first. This includes:
            // - Environment doctor results (SDK disconnected, app crashed, etc.)
            // - Health regression warnings from previous iteration's agentic phase
            // By placing this first, the AI knows to fix environment issues before
            // attempting code changes when verification failures are due to env problems.
            let failure_context = if let Some(warning) = pending_health_regression.take() {
                format!("{}\n\n{}", warning, failure_context)
            } else {
                failure_context
            };

            // Inject constraint violations from the previous agentic phase.
            // These are evaluated after the AI applies changes and stored for the
            // next iteration, so the AI gets feedback about constraint issues
            // alongside verification failures.
            let failure_context = if let Some(ctx) = pending_constraint_context.take() {
                format!("{}\n{}", failure_context, ctx)
            } else {
                failure_context
            };

            // Inject proactive constraints prompt on the first agentic iteration only.
            // This tells the AI about active constraints upfront so it can avoid
            // violations rather than learning about them reactively.
            let failure_context = if !constraints_prompt_injected {
                if let Some(ref prompt) = proactive_constraints_prompt {
                    constraints_prompt_injected = true;
                    info!(
                            "CONSTRAINT-ENGINE: Injecting proactive constraints prompt into iteration {}",
                            iteration,
                        );
                    if failure_context.is_empty() {
                        prompt.clone()
                    } else {
                        format!("{}\n\n{}", failure_context, prompt)
                    }
                } else {
                    failure_context
                }
            } else {
                failure_context
            };

            // Enrich failure context with structured build errors from managed processes.
            // Parses stderr from dev servers to extract actionable errors (file, line, message)
            // instead of dumping raw stderr output.
            let failure_context = {
                let mut enriched = failure_context;
                let mgr_lock = self.app_state.process_capture_manager.lock().await;
                if let Some(ref mgr) = *mgr_lock {
                    let statuses = mgr.get_all_status().await;
                    let active_processes: Vec<&crate::process_capture::types::ProcessStatus> =
                        statuses
                            .iter()
                            .filter(|s| {
                                s.state != crate::process_capture::types::ProcessState::Stopped
                            })
                            .collect();

                    let mut analyses = Vec::new();
                    for status in &active_processes {
                        if let Ok(lines) = mgr.get_output(&status.id, 100).await {
                            let analysis =
                                crate::process_capture::build_errors::analyze_process_output(
                                    &status.name,
                                    &lines,
                                    status,
                                );
                            analyses.push(analysis);
                        }
                    }

                    if let Some(build_section) =
                        crate::process_capture::build_errors::format_build_analysis(&analyses)
                    {
                        enriched.push_str("\n\n");
                        enriched.push_str(&build_section);
                    }
                }
                enriched
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

            // Apply convergence detector actions: force reflection mode if recommended.
            // This temporarily enables reflection_mode for this iteration even if the
            // workflow didn't configure it, giving the AI a structured investigation
            // protocol when it's clearly stuck.
            let reflection_was_forced =
                if convergence_report.should_force_reflection() && !config.reflection_mode {
                    info!(
                    "CONVERGENCE-ACTION: Forcing reflection mode for iteration {} (was disabled)",
                    iteration
                );
                    config.reflection_mode = true;
                    true
                } else {
                    false
                };

            // Inject reflection investigation hint into the failure context if present.
            // This gives the AI specific guidance about what to investigate.
            let failure_context = if let Some(hint) = convergence_report.reflection_hint() {
                format!("{}\n\n{}", failure_context, hint)
            } else {
                failure_context
            };

            // Inject unfixable suggestion into the failure context if recommended.
            // This doesn't force unfixable — it asks the AI to seriously consider it.
            let failure_context = {
                let mut ctx = failure_context;
                for action in &convergence_report.actions {
                    if let super::convergence::ConvergenceAction::SuggestUnfixable { reason } =
                        action
                    {
                        ctx.push_str(&format!(
                            "\n\n### Consider Declaring Unfixable\n\n\
                            {}\n\n\
                            If after thorough investigation you determine these errors truly cannot \
                            be fixed through code changes, output `[UNFIXABLE_ERRORS]` with an \
                            explanation. Only do this if you've exhausted all approaches.\n",
                            reason
                        ));
                    }
                }
                ctx
            };

            // Capture error baseline before agentic phase for regression detection.
            // After the AI makes changes, we compare to identify newly introduced errors.
            let pre_agentic_health = fetch_pre_agentic_health_baseline().await;

            let agentic_phase_start = std::time::Instant::now();

            // Multi-agent mode: triage failures and spawn specialized fix agents
            // instead of one monolithic AI session.
            let (agentic_outcome, new_injected_steps) =
                if config.multi_agent_mode && verification_result.failed_steps > 0 {
                    info!(
                        "MULTI-AGENT: Engaging multi-agent fixer (iteration {}, {} failed steps)",
                        iteration, verification_result.failed_steps
                    );

                    let ma_result = self
                        .run_multi_agent_fix(
                            config,
                            iteration,
                            &failure_context,
                            &verification_result,
                            &all_verification_steps,
                            logger,
                        )
                        .await;

                    match ma_result {
                        Some((outcome, steps)) => (outcome, steps),
                        None => {
                            // Multi-agent triage/fix failed, fall back to standard session
                            warn!("MULTI-AGENT: Falling back to standard agentic session");
                            self.agentic_executor
                                .run_agentic(
                                    config,
                                    iteration,
                                    &failure_context,
                                    has_agentic_steps,
                                    agentic_steps,
                                    logger,
                                )
                                .await
                        }
                    }
                } else {
                    self.agentic_executor
                        .run_agentic(
                            config,
                            iteration,
                            &failure_context,
                            has_agentic_steps,
                            agentic_steps,
                            logger,
                        )
                        .await
                };

            // Restore reflection_mode if we forced it for this iteration only
            if reflection_was_forced {
                config.reflection_mode = false;
                debug!(
                    "CONVERGENCE-ACTION: Restored reflection_mode to false after forced iteration"
                );
            }
            let agentic_duration_ms = agentic_phase_start.elapsed().as_millis() as u64;

            // Feed the resource tracker with this iteration's data.
            // Extract files_modified from the parsed agentic output (if available).
            let iteration_files: Vec<String> = agentic_outcome
                .parsed()
                .map(|p| p.files_modified.iter().map(|f| f.path.clone()).collect())
                .unwrap_or_default();
            resource_tracker.record_iteration(agentic_duration_ms, &iteration_files);

            // Run constraint engine against modified files.
            // Violations are stored and injected into the NEXT iteration's failure
            // context (alongside verification results). This catches issues like
            // secrets, debug statements, or scope violations early.
            {
                if !iteration_files.is_empty() {
                    let constraint_results = constraint_engine.evaluate(&iteration_files);
                    let all_passed = constraint_results.iter().all(|r| r.passed);
                    let has_blocking = constraint_results.iter().any(|r| {
                        !r.passed
                            && r.severity == crate::constraint_engine::ConstraintSeverity::Block
                    });
                    let summary = if all_passed {
                        format!("all {} constraints passed", constraint_results.len())
                    } else {
                        crate::constraint_engine::ConstraintEngine::summarize_results(
                            &constraint_results,
                        )
                    };

                    // Persist constraint results to database for post-run review
                    let parent_id = get_parent_task_id(&config.execution_id);
                    if let Err(e) = self.checkpoint_db.store_constraint_results(
                        &parent_id,
                        iteration,
                        &constraint_results,
                    ) {
                        warn!(
                            "Failed to store constraint results: {} - continuing anyway",
                            e
                        );
                    }

                    // Emit constraint results to the frontend
                    let broadcaster = EventBroadcaster::new(self.app_handle.clone());
                    let serialized_results = serde_json::to_value(&constraint_results)
                        .unwrap_or_else(|_| serde_json::json!([]));
                    broadcaster.constraint_results(
                        &config.execution_id,
                        iteration,
                        &summary,
                        has_blocking,
                        serialized_results,
                    );

                    if !all_passed {
                        info!("CONSTRAINT-ENGINE: iteration {} — {}", iteration, summary);
                        let actions =
                            crate::constraint_engine::ConstraintEngine::results_to_actions(
                                &constraint_results,
                            );
                        // Build context injection from constraint actions
                        let mut ctx = String::new();
                        for action in &actions {
                            if let super::convergence::ConvergenceAction::InjectContext {
                                context,
                                ..
                            } = action
                            {
                                ctx.push_str(context);
                                ctx.push('\n');
                            }
                        }
                        if !ctx.is_empty() {
                            pending_constraint_context =
                                Some(format!("\n## Constraint Violations\n\n{}", ctx));
                        }
                    } else {
                        debug!(
                            "CONSTRAINT-ENGINE: iteration {} — all constraints passed",
                            iteration
                        );
                    }
                }
            }

            // Compare post-agentic health with baseline to detect regressions.
            // Store for the NEXT iteration's failure context (since this iteration's
            // failure context was already built from verification results).
            if agentic_outcome.is_success() {
                pending_health_regression = detect_health_regression(&pre_agentic_health).await;
                if pending_health_regression.is_some() {
                    warn!(
                        "HEALTH-REGRESSION: Detected after agentic phase (iteration {})",
                        iteration
                    );
                }
            }

            // Accumulate any newly injected steps for future verification iterations
            let new_injected_count = new_injected_steps.len();
            if !new_injected_steps.is_empty() {
                info!(
                    "Injected {} dynamic verification step(s) from agentic phase (iteration {})",
                    new_injected_count, iteration
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

            // Sync session to web backend (best-effort, non-blocking).
            // The frontend-driven sync only fires for UI-initiated tasks; workflow-
            // executor sessions must sync themselves to keep the backend up to date.
            {
                let exec_id = config.execution_id.clone();
                let session_num = iteration;
                let duration_secs = (agentic_duration_ms / 1000) as i64;
                let output_summary = agentic_outcome
                    .output()
                    .map(|o| truncate_str(o, 5000).to_string());
                tokio::spawn(async move {
                    let sync_service = crate::commands::task_sync::AITaskSyncService::new();
                    // Start
                    if let Err(e) = sync_service
                        .sync_session_started(&exec_id, session_num)
                        .await
                    {
                        debug!("Failed to sync session start to backend: {}", e);
                    }
                    // End
                    if let Err(e) = sync_service
                        .sync_session_ended(
                            &exec_id,
                            session_num,
                            duration_secs,
                            output_summary.as_deref(),
                        )
                        .await
                    {
                        debug!("Failed to sync session end to backend: {}", e);
                    }
                });
            }

            // Record findings from AI output as knowledge entries
            let parsed_findings = if let Some(output) = agentic_outcome.output() {
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

                findings
            } else {
                Vec::new()
            };

            // Parse AI-discovered constraint proposals from the agentic output.
            // Proposals are applied to the constraint engine for subsequent iterations
            // and logged as TOML for the user to add to constraints.toml.
            if let Some(output) = agentic_outcome.output() {
                let proposals = crate::constraint_engine::proposals::parse_proposals(output);
                if !proposals.is_empty() {
                    let applied = constraint_engine.apply_proposals(&proposals);
                    info!(
                        "CONSTRAINT-PROPOSAL: Applied {}/{} proposal(s) from iteration {}",
                        applied,
                        proposals.len(),
                        iteration,
                    );
                    // Log the TOML snippet for user reference
                    let toml_snippet =
                        crate::constraint_engine::proposals::proposals_to_toml(&proposals);
                    if !toml_snippet.is_empty() {
                        info!(
                            "CONSTRAINT-PROPOSAL: Suggested constraints.toml additions:\n{}",
                            toml_snippet,
                        );
                    }
                }
            }

            // Emit canvas panel for agentic phase completion
            self.canvas_manager
                .lock()
                .await
                .on_agentic_complete(
                    iteration,
                    &agentic_outcome,
                    &parsed_findings,
                    new_injected_count,
                    agentic_duration_ms,
                )
                .await;

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

            // Clear failure_context from previous iterations to prevent unbounded
            // memory growth. The context has already been persisted to the knowledge
            // base (via record_verification_feedback) and used for the agentic prompt.
            // Downstream consumers only use passed_checks/failed_checks/verification_passed.
            let results_len = iteration_results.len();
            if results_len > 1 {
                for old in &mut iteration_results[..results_len - 1] {
                    if !old.failure_context.is_empty() {
                        old.failure_context = String::new();
                    }
                }
            }

            // Log agentic outcome for debugging (including parsed confidence)
            info!(
                "AGENTIC-OUTCOME: iteration={}, outcome={}, confidence={}",
                iteration,
                match &agentic_outcome {
                    AgenticOutcome::Success { .. } => "Success",
                    AgenticOutcome::Failed { .. } => "Failed",
                    AgenticOutcome::Error { error } => {
                        error!("AGENTIC-OUTCOME: Error details: {}", error);
                        "Error"
                    }
                    AgenticOutcome::Skipped => "Skipped",
                },
                agentic_outcome
                    .parsed()
                    .and_then(|p| p.confidence)
                    .map(|c| format!("{:.0}%", c * 100.0))
                    .unwrap_or_else(|| "N/A".to_string()),
            );

            // Log and store findings from the parsed output, if any
            if let Some(parsed) = agentic_outcome.parsed() {
                if !parsed.findings.is_empty() {
                    info!(
                        "AGENTIC-FINDINGS: iteration={}, count={}, titles=[{}]",
                        iteration,
                        parsed.findings.len(),
                        parsed
                            .findings
                            .iter()
                            .map(|f| format!("{}({})", f.title, f.severity))
                            .collect::<Vec<_>>()
                            .join(", "),
                    );

                    // Store findings as a task_run_event for later retrieval
                    let findings_data = serde_json::json!({
                        "phase": "agentic",
                        "iteration": iteration,
                        "findings": parsed.findings,
                    });
                    let findings_event = CreateTaskRunEventInput {
                        task_run_id: config.execution_id.clone(),
                        event_type: "agentic_findings".to_string(),
                        event_subtype: None,
                        message: format!(
                            "Agentic phase reported {} finding(s) on iteration {}",
                            parsed.findings.len(),
                            iteration,
                        ),
                        data: Some(serde_json::to_string(&findings_data).unwrap_or_default()),
                        workflow_name: None,
                        state_name: None,
                        action_id: None,
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        duration_ms: None,
                    };
                    if let Err(e) = self.checkpoint_db.create_task_run_event(&findings_event) {
                        warn!("Failed to store agentic findings event: {}", e);
                    }
                }
            }

            // Check if the AI signaled unfixable errors.
            // Prefer the structured `parsed` output, fall back to raw marker check.
            let is_unfixable = if let Some(parsed) = agentic_outcome.parsed() {
                parsed.unfixable
            } else if let Some(output) = agentic_outcome.output() {
                output.contains("[UNFIXABLE_ERRORS]") || output.contains("[UNFIXABLE_ERROR]")
            } else {
                false
            };
            if is_unfixable {
                let reason = agentic_outcome
                    .parsed()
                    .and_then(|p| p.unfixable_reason.as_deref())
                    .unwrap_or("(no reason provided)");
                warn!(
                    "AI signaled unfixable errors on iteration {} - exiting loop gracefully. Reason: {}",
                    iteration, reason
                );

                // Log the unfixable signal to the task output
                let unfixable_msg = format!(
                    "\n=== AI SIGNALED UNFIXABLE ERRORS ===\nThe AI has determined that some errors cannot be fixed automatically.\nReason: {}\nProceeding to completion phase.\n",
                    reason
                );
                let _ = self.checkpoint_db.append_task_output_ex(
                    &config.execution_id,
                    &unfixable_msg,
                    false,
                    false,
                );

                break LoopResult {
                    iterations_run: iteration,
                    verification_passed: false,
                    max_iterations_reached: false,
                    critical_failure: false,
                    was_stopped: false,
                    unfixable_errors: true,
                    iteration_results,
                };
            }

            // Check if the task was stopped during the agentic phase
            if self.is_task_stopped(&config.execution_id) {
                warn!("Task was stopped during agentic phase - exiting loop");
                break LoopResult {
                    iterations_run: iteration,
                    verification_passed: false,
                    max_iterations_reached: false,
                    critical_failure: false,
                    was_stopped: true,
                    unfixable_errors: false,
                    iteration_results,
                };
            }

            // Wait if paused after agentic phase
            self.wait_while_paused(&config.execution_id).await;

            // Re-check stop after unpause
            if self.is_task_stopped(&config.execution_id) {
                warn!("Task was stopped while paused (post-agentic) - exiting loop");
                break LoopResult {
                    iterations_run: iteration,
                    verification_passed: false,
                    max_iterations_reached: false,
                    critical_failure: false,
                    was_stopped: true,
                    unfixable_errors: false,
                    iteration_results,
                };
            }

            // -----------------------------------------------------------------
            // APPROVAL GATE (optional human-in-the-loop pause)
            // -----------------------------------------------------------------
            // If the workflow has approval_gate enabled, or if the AI output
            // contains the [APPROVAL_GATE] marker, pause for human review.
            let needs_approval = config.approval_gate
                || agentic_outcome
                    .output()
                    .map(|o| o.contains("[APPROVAL_GATE]"))
                    .unwrap_or(false);

            if needs_approval && agentic_outcome.is_success() {
                info!(
                    "Approval gate triggered on iteration {} - pausing for human review",
                    iteration
                );

                // Collect context for the reviewer
                let diff_stat_for_approval = match crate::process_helpers::tokio_no_window("git")
                    .args(["diff", "--stat"])
                    .output()
                    .await
                {
                    Ok(o) if o.status.success() => {
                        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                        if s.is_empty() {
                            None
                        } else {
                            Some(s)
                        }
                    }
                    _ => None,
                };

                let diff_for_approval = match crate::process_helpers::tokio_no_window("git")
                    .args(["diff"])
                    .output()
                    .await
                {
                    Ok(o) if o.status.success() => {
                        let raw = String::from_utf8_lossy(&o.stdout).to_string();
                        if raw.is_empty() {
                            None
                        } else if raw.len() > 8000 {
                            let t = truncate_str(&raw, 8000);
                            Some(format!(
                                "{}...\n[truncated, {} more chars]",
                                t,
                                raw.len() - t.len()
                            ))
                        } else {
                            Some(raw)
                        }
                    }
                    _ => None,
                };

                // Build the approval request
                let approval_id = format!("approval-{}-iter-{}", config.execution_id, iteration);
                let summary = agentic_outcome
                    .parsed()
                    .map(|p| p.summary.clone())
                    .unwrap_or_else(|| {
                        format!("Agentic phase completed (iteration {})", iteration)
                    });
                let files_modified = agentic_outcome
                    .parsed()
                    .map(|p| {
                        p.files_modified
                            .iter()
                            .map(|f| f.path.clone())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

                let context = super::approval::ApprovalContext {
                    summary: summary.clone(),
                    files_modified: files_modified.clone(),
                    git_diff_stat: diff_stat_for_approval.clone(),
                    git_diff: diff_for_approval.clone(),
                };

                let request = super::approval::ApprovalRequest {
                    id: approval_id.clone(),
                    execution_id: config.execution_id.clone(),
                    iteration,
                    prompt: format!(
                        "The AI has completed iteration {}. Review the changes and approve to continue.",
                        iteration
                    ),
                    context: context.clone(),
                    options: vec![
                        "Approve".to_string(),
                        "Reject".to_string(),
                        "Abort Workflow".to_string(),
                    ],
                    created_at: chrono::Utc::now().to_rfc3339(),
                };

                // Record to database for audit trail
                let context_json =
                    serde_json::to_string(&context).unwrap_or_else(|_| "{}".to_string());
                let _ = self.checkpoint_db.insert_approval_gate(
                    &approval_id,
                    &config.execution_id,
                    iteration,
                    &request.prompt,
                    &context_json,
                );

                // Persist workflow state: ApprovalPending
                self.persist_workflow_state(
                    &config.execution_id,
                    &UnifiedWorkflowState::approval_pending(
                        iteration,
                        config.stage_index,
                        approval_id.clone(),
                        request.prompt.clone(),
                    ),
                );

                // Register with the in-memory approval registry
                let registry = super::approval::get_approval_registry();
                let receiver = registry.register(request).await;

                // Emit event to notify the frontend
                let broadcaster = EventBroadcaster::new(self.app_handle.clone());
                broadcaster.approval_required(
                    &config.execution_id,
                    &approval_id,
                    iteration,
                    &format!("Review changes from iteration {}", iteration),
                );

                // Log the pause
                let _ = self.checkpoint_db.append_task_output_ex(
                    &config.execution_id,
                    &format!(
                        "\n=== APPROVAL GATE (Iteration {}) ===\nWaiting for human review...\n",
                        iteration
                    ),
                    false,
                    false,
                );

                // Wait for the human response (or stop signal)
                let approval_response = tokio::select! {
                    resp = receiver => {
                        match resp {
                            Ok(r) => r,
                            Err(_) => {
                                warn!("Approval receiver dropped - treating as abort");
                                super::approval::ApprovalResponse {
                                    approved: false,
                                    action: "abort".to_string(),
                                    comment: Some("Approval channel closed unexpectedly".to_string()),
                                }
                            }
                        }
                    }
                    _ = async {
                        // Poll for stop signal while waiting for approval
                        loop {
                            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                            if self.is_task_stopped(&config.execution_id) {
                                return;
                            }
                        }
                    } => {
                        // Task was stopped while waiting for approval
                        warn!("Task stopped while waiting for approval - aborting");
                        // Cancel the pending approval
                        registry.cancel_all_for_execution(&config.execution_id).await;
                        super::approval::ApprovalResponse {
                            approved: false,
                            action: "abort".to_string(),
                            comment: Some("Task was stopped".to_string()),
                        }
                    }
                };

                // Record the response to the database
                let _ = self.checkpoint_db.resolve_approval_gate(
                    &approval_id,
                    &approval_response.action,
                    approval_response.comment.as_deref(),
                );

                // Emit resolved event
                broadcaster.approval_resolved(
                    &config.execution_id,
                    &approval_id,
                    approval_response.approved,
                    &approval_response.action,
                );

                // Log the decision
                let _ = self.checkpoint_db.append_task_output_ex(
                    &config.execution_id,
                    &format!(
                        "Approval decision: {} (comment: {})\n",
                        approval_response.action,
                        approval_response.comment.as_deref().unwrap_or("none")
                    ),
                    false,
                    false,
                );

                // Handle the response
                if approval_response.action == "abort" {
                    warn!(
                        "Workflow aborted via approval gate on iteration {}",
                        iteration
                    );
                    break LoopResult {
                        iterations_run: iteration,
                        verification_passed: false,
                        max_iterations_reached: false,
                        critical_failure: false,
                        was_stopped: true,
                        unfixable_errors: false,
                        iteration_results,
                    };
                }
                if !approval_response.approved {
                    info!(
                        "Changes rejected on iteration {} - AI will retry",
                        iteration
                    );
                    // Continue to verification, which will likely fail,
                    // prompting the AI to try a different approach
                }

                // Restore workflow state to agentic_complete so normal flow continues
                self.persist_workflow_state(
                    &config.execution_id,
                    &UnifiedWorkflowState::agentic_complete(iteration),
                );
            }

            // Capture git diff after agentic phase for cross-iteration context.
            // This helps the AI understand what it changed in the previous iteration.
            {
                let parent_id = get_parent_task_id(&config.execution_id);
                match crate::process_helpers::tokio_no_window("git")
                    .args(["diff", "--stat"])
                    .output()
                    .await
                {
                    Ok(output) if output.status.success() => {
                        let diff_stat = String::from_utf8_lossy(&output.stdout).trim().to_string();
                        if !diff_stat.is_empty() {
                            // Also capture the actual diff (truncated)
                            let full_diff = match crate::process_helpers::tokio_no_window("git")
                                .args(["diff"])
                                .output()
                                .await
                            {
                                Ok(d) if d.status.success() => {
                                    let raw = String::from_utf8_lossy(&d.stdout).to_string();
                                    if raw.len() > 8000 {
                                        let t = truncate_str(&raw, 8000);
                                        format!(
                                            "{}...\n[truncated, {} more chars]",
                                            t,
                                            raw.len() - t.len()
                                        )
                                    } else {
                                        raw
                                    }
                                }
                                _ => String::new(),
                            };

                            let observation = format!(
                                "Git changes after iteration {}:\n{}\n\n{}",
                                iteration, diff_stat, full_diff
                            );
                            if let Err(e) = self.knowledge_base.record_observation(
                                &parent_id,
                                AgentType::Worker,
                                iteration,
                                &observation,
                                &[],
                            ) {
                                warn!(
                                    "Failed to record git diff observation (iteration {}): {}",
                                    iteration, e
                                );
                            } else {
                                debug!(
                                    "Recorded git diff as knowledge observation (iteration {})",
                                    iteration
                                );
                            }
                        }
                    }
                    _ => {
                        debug!("Git diff capture skipped (git not available or not in repo)");
                    }
                }
            }

            info!(
                "LOOP-CONTINUE: Iteration {} complete - looping back to verification (next iteration: {})",
                iteration, iteration + 1
            );
            // The loop continues here naturally - no return statement
            // Control flows back to the top of the loop for the next iteration
        };

        // Auto-detect recurring findings → known issues
        let auto_detected_ids: Vec<String> = self
            .checkpoint_db
            .with_conn(|conn| {
                match crate::known_issues::auto_detect::check_and_promote_recurring_findings(
                    conn,
                    &config.execution_id,
                ) {
                    Ok(new_ids) => {
                        if !new_ids.is_empty() {
                            info!(
                                "Auto-detected {} new known issue(s) from recurring findings",
                                new_ids.len()
                            );
                        }
                        Ok(new_ids)
                    }
                    Err(e) => {
                        warn!("Failed to check for recurring findings: {}", e);
                        Ok(vec![])
                    }
                }
            })
            .unwrap_or_default();

        // Notify frontend about auto-detected known issues
        if !auto_detected_ids.is_empty() {
            let _ = self.app_handle.emit(
                "known-issues-auto-detected",
                serde_json::json!({
                    "count": auto_detected_ids.len(),
                    "issue_ids": auto_detected_ids,
                }),
            );
        }

        loop_result
    }

    // =========================================================================
    // Agentic Verification Loop — alternative architecture
    // =========================================================================

    /// Run the agentic verification loop: Verification Agent → Worker Agent → repeat.
    ///
    /// Unlike the traditional loop that uses pre-defined deterministic verification steps,
    /// this architecture uses a verification *agent* that reasons about whether the goal
    /// has been achieved, and a worker agent that takes actions based on the verifier's
    /// feedback.
    ///
    /// Returns a standard LoopResult for compatibility with the existing result pipeline.
    async fn run_agentic_verification_loop(
        &self,
        config: &mut LoopConfig,
        has_agentic_steps: bool,
        agentic_steps: &[ExecutionStepConfig],
        _all_step_results: &mut Vec<crate::step_executor::StepExecutionResult>,
        logger: &StepEventLogger,
    ) -> LoopResult {
        use crate::autoresearch::agentic_verification::*;

        let av_config = config
            .agentic_verification_config
            .clone()
            .unwrap_or_default();

        let goal = if av_config.goal.is_empty() {
            config.base_prompt.clone()
        } else {
            av_config.goal.clone()
        };

        let max_iterations = if av_config.max_iterations > 0 {
            av_config.max_iterations
        } else {
            config.max_iterations
        };

        info!(
            "AGENTIC-VERIFICATION: Starting loop (max_iterations={}, confidence_threshold={}, consecutive_passes_required={})",
            max_iterations, av_config.confidence_threshold, av_config.required_consecutive_passes
        );

        let mut iteration_results: Vec<AgenticVerificationIterationResult> = Vec::new();
        let mut consecutive_passes: u32 = 0;
        let mut iteration: u32 = 0;

        // Save original model/provider overrides so we can restore after each iteration
        let original_model_override = config.model_override.clone();
        let original_provider_override = config.provider_override.clone();

        loop {
            iteration += 1;

            // Check stop signal
            if self.is_task_stopped(&config.execution_id) {
                info!(
                    "AGENTIC-VERIFICATION: Stopped by user at iteration {}",
                    iteration
                );
                let av_result = AgenticVerificationResult {
                    iterations_run: iteration,
                    goal_achieved: false,
                    unreachable: false,
                    was_stopped: true,
                    max_iterations_reached: false,
                    iteration_results,
                    final_verdict: None,
                };
                self.canvas_manager
                    .lock()
                    .await
                    .on_agentic_verification_exit(&av_result)
                    .await;
                return av_result.to_loop_result();
            }

            // Check max iterations
            if iteration > max_iterations {
                info!(
                    "AGENTIC-VERIFICATION: Max iterations ({}) reached",
                    max_iterations
                );
                let av_result = AgenticVerificationResult {
                    iterations_run: iteration - 1,
                    goal_achieved: false,
                    unreachable: false,
                    was_stopped: false,
                    max_iterations_reached: true,
                    iteration_results: iteration_results.clone(),
                    final_verdict: iteration_results.last().map(|r| r.verdict.clone()),
                };
                self.canvas_manager
                    .lock()
                    .await
                    .on_agentic_verification_exit(&av_result)
                    .await;
                return av_result.to_loop_result();
            }

            // Update routing context
            config.set_routing_context(iteration, 0);

            // Record activity
            self.record_activity(
                &config.execution_id,
                &format!("agentic_verification_iteration_{}", iteration),
            );

            // ── STEP 1: Verification Agent ──────────────────────────────
            // The verification agent assesses the current state against the goal.
            // On the first iteration (if verify_first is false), we skip directly
            // to the worker agent.

            let should_verify = iteration > 1 || av_config.verify_first;

            let verdict = if should_verify {
                info!(
                    "AGENTIC-VERIFICATION: Running verification agent (iteration {})",
                    iteration
                );

                let verifier_start = std::time::Instant::now();

                let verifier_system = AgenticVerificationPrompts::verifier_system_prompt(
                    &goal,
                    av_config.verifier.system_preamble.as_deref(),
                );

                // Build context for the verifier: include previous iteration results
                let mut verifier_context = verifier_system;
                if let Some(last_result) = iteration_results.last() {
                    verifier_context.push_str(&format!(
                        "\n\n## Previous Iteration ({}) Summary\nWorker action: {}\n",
                        last_result.iteration,
                        last_result.worker_summary.as_deref().unwrap_or("(none)"),
                    ));
                }

                // Fetch live UI Bridge data for the verifier
                let ui_context = fetch_verifier_ui_context(
                    av_config.verifier.use_screenshots,
                    av_config.verifier.include_console_errors,
                    av_config.verifier.include_app_health,
                )
                .await;
                if !ui_context.is_empty() {
                    verifier_context.push_str(&ui_context);
                }

                // Apply verifier model/provider overrides
                config.model_override = av_config
                    .verifier
                    .model
                    .clone()
                    .or_else(|| original_model_override.clone());
                config.provider_override = av_config
                    .verifier
                    .provider
                    .clone()
                    .or_else(|| original_provider_override.clone());

                // Run verifier through the agentic executor with a verification prompt
                let verifier_prompt = ExecutionStepConfig {
                    step_type: "prompt".to_string(),
                    name: Some(format!("Verification Agent (iteration {})", iteration)),
                    prompt_content: Some(verifier_context),
                    ..ExecutionStepConfig::default()
                };

                let (outcome, _injected) = self
                    .agentic_executor
                    .run_agentic(
                        config,
                        iteration,
                        "", // No failure context — the verifier generates its own
                        true,
                        &[verifier_prompt],
                        logger,
                    )
                    .await;

                let verifier_duration = verifier_start.elapsed().as_millis() as u64;

                // Parse the verdict from the AI output
                let verdict = match &outcome {
                    AgenticOutcome::Success { output, .. }
                    | AgenticOutcome::Failed { output, .. } => {
                        parse_verification_verdict(output)
                            .unwrap_or_else(|| {
                                info!("AGENTIC-VERIFICATION: Failed to parse structured verdict, using heuristic");
                                heuristic_verdict(output)
                            })
                    }
                    AgenticOutcome::Error { error } => {
                        warn!("AGENTIC-VERIFICATION: Verifier error: {}", error);
                        VerificationVerdict {
                            status: VerificationStatus::Fail,
                            confidence: 0.0,
                            observations: format!("Verification agent error: {}", error),
                            next_priority: Some("Retry verification".to_string()),
                            issues: vec![VerificationIssue {
                                description: error.clone(),
                                severity: "critical".to_string(),
                                suggestion: None,
                            }],
                            unreachable: false,
                            unreachable_reason: None,
                        }
                    }
                    AgenticOutcome::Skipped => {
                        VerificationVerdict {
                            status: VerificationStatus::Fail,
                            confidence: 0.0,
                            observations: "Verification agent was skipped".to_string(),
                            next_priority: Some("Configure verification agent".to_string()),
                            issues: vec![],
                            unreachable: false,
                            unreachable_reason: None,
                        }
                    }
                };

                info!(
                    "AGENTIC-VERIFICATION: Verdict: status={}, confidence={:.0}%, issues={}  ({}ms)",
                    verdict.status,
                    verdict.confidence * 100.0,
                    verdict.issues.len(),
                    verifier_duration,
                );

                // Emit verdict to task output
                if let Err(e) = self.checkpoint_db.append_task_output_ex(
                    &config.execution_id,
                    &format!(
                        "\n--- Verification Agent (iteration {}) ---\nStatus: {}\nConfidence: {:.0}%\nObservations: {}\n",
                        iteration,
                        verdict.status,
                        verdict.confidence * 100.0,
                        verdict.observations,
                    ),
                    false,
                    false,
                ) {
                    warn!("Failed to append verifier output: {}", e);
                }

                Some((verdict, verifier_duration))
            } else {
                None
            };

            // ── STEP 2: Check if goal achieved ──────────────────────────
            if let Some((ref v, _)) = verdict {
                if v.status == VerificationStatus::Pass
                    && v.confidence >= av_config.confidence_threshold
                {
                    consecutive_passes += 1;
                    if consecutive_passes >= av_config.required_consecutive_passes {
                        info!(
                            "AGENTIC-VERIFICATION: Goal achieved after {} iterations ({} consecutive passes)",
                            iteration, consecutive_passes
                        );
                        iteration_results.push(AgenticVerificationIterationResult {
                            iteration,
                            verdict: v.clone(),
                            worker_ran: false,
                            worker_summary: None,
                            verifier_duration_ms: verdict.as_ref().map(|(_, d)| *d).unwrap_or(0),
                            worker_duration_ms: 0,
                        });
                        // Canvas: emit iteration panel and exit summary
                        {
                            let verifier_ms = verdict.as_ref().map(|(_, d)| *d).unwrap_or(0);
                            self.canvas_manager
                                .lock()
                                .await
                                .on_agentic_verification_iteration(
                                    iteration,
                                    v,
                                    None,
                                    verifier_ms,
                                    0,
                                )
                                .await;
                        }
                        let av_result = AgenticVerificationResult {
                            iterations_run: iteration,
                            goal_achieved: true,
                            unreachable: false,
                            was_stopped: false,
                            max_iterations_reached: false,
                            iteration_results,
                            final_verdict: Some(v.clone()),
                        };
                        self.canvas_manager
                            .lock()
                            .await
                            .on_agentic_verification_exit(&av_result)
                            .await;
                        return av_result.to_loop_result();
                    }
                } else {
                    consecutive_passes = 0;
                }

                // Check unreachable
                if v.status == VerificationStatus::Unreachable || v.unreachable {
                    info!(
                        "AGENTIC-VERIFICATION: Goal deemed unreachable: {}",
                        v.unreachable_reason
                            .as_deref()
                            .unwrap_or("(no reason given)")
                    );
                    // Canvas: emit iteration panel for unreachable verdict
                    {
                        let verifier_ms = verdict.as_ref().map(|(_, d)| *d).unwrap_or(0);
                        self.canvas_manager
                            .lock()
                            .await
                            .on_agentic_verification_iteration(iteration, v, None, verifier_ms, 0)
                            .await;
                    }
                    iteration_results.push(AgenticVerificationIterationResult {
                        iteration,
                        verdict: v.clone(),
                        worker_ran: false,
                        worker_summary: None,
                        verifier_duration_ms: verdict.as_ref().map(|(_, d)| *d).unwrap_or(0),
                        worker_duration_ms: 0,
                    });
                    let av_result = AgenticVerificationResult {
                        iterations_run: iteration,
                        goal_achieved: false,
                        unreachable: true,
                        was_stopped: false,
                        max_iterations_reached: false,
                        iteration_results,
                        final_verdict: Some(v.clone()),
                    };
                    self.canvas_manager
                        .lock()
                        .await
                        .on_agentic_verification_exit(&av_result)
                        .await;
                    return av_result.to_loop_result();
                }
            }

            // ── STEP 3: Worker Agent ────────────────────────────────────
            // The worker agent receives the verifier's feedback and takes action.

            info!(
                "AGENTIC-VERIFICATION: Running worker agent (iteration {})",
                iteration
            );

            let worker_start = std::time::Instant::now();

            // Apply worker model/provider overrides
            config.model_override = av_config
                .worker
                .model
                .clone()
                .or_else(|| original_model_override.clone());
            config.provider_override = av_config
                .worker
                .provider
                .clone()
                .or_else(|| original_provider_override.clone());

            // Build worker context from the verification verdict
            let worker_failure_context = if let Some((ref v, _)) = verdict {
                AgenticVerificationPrompts::worker_context_from_verdict(
                    &goal,
                    v,
                    iteration,
                    max_iterations,
                )
            } else {
                // First iteration without verify_first — just provide the goal
                format!(
                    "## Goal\n{}\n\nThis is the first iteration. Assess the current state and take action toward the goal.\n",
                    goal
                )
            };

            let (worker_outcome, _injected_steps) = self
                .agentic_executor
                .run_agentic(
                    config,
                    iteration,
                    &worker_failure_context,
                    has_agentic_steps,
                    agentic_steps,
                    logger,
                )
                .await;

            let worker_duration = worker_start.elapsed().as_millis() as u64;

            let worker_summary = match &worker_outcome {
                AgenticOutcome::Success { output, .. } => {
                    // Extract first 500 chars as summary
                    let summary = if output.len() > 500 {
                        format!("{}...", &output[..500])
                    } else {
                        output.clone()
                    };
                    Some(summary)
                }
                AgenticOutcome::Failed { output, error, .. } => Some(format!(
                    "Failed: {} (output: {}...)",
                    error,
                    &output[..output.len().min(200)]
                )),
                AgenticOutcome::Error { error } => Some(format!("Error: {}", error)),
                AgenticOutcome::Skipped => None,
            };

            info!(
                "AGENTIC-VERIFICATION: Worker completed in {}ms",
                worker_duration
            );

            // ── Record iteration result ─────────────────────────────────
            let iter_result = AgenticVerificationIterationResult {
                iteration,
                verdict: verdict
                    .as_ref()
                    .map(|(v, _)| v.clone())
                    .unwrap_or(VerificationVerdict {
                        status: VerificationStatus::Partial,
                        confidence: 0.0,
                        observations: "No verification performed (first iteration)".to_string(),
                        next_priority: None,
                        issues: vec![],
                        unreachable: false,
                        unreachable_reason: None,
                    }),
                worker_ran: true,
                worker_summary,
                verifier_duration_ms: verdict.as_ref().map(|(_, d)| *d).unwrap_or(0),
                worker_duration_ms: worker_duration,
            };
            iteration_results.push(iter_result);

            // Canvas: emit iteration panel and update tracker
            {
                let last = iteration_results.last().expect("just pushed");
                let mut cm = self.canvas_manager.lock().await;
                cm.on_agentic_verification_iteration(
                    iteration,
                    &last.verdict,
                    last.worker_summary.as_deref(),
                    last.verifier_duration_ms,
                    last.worker_duration_ms,
                )
                .await;
                cm.on_agentic_verification_tracker(&iteration_results).await;
            }

            // Restore original model/provider overrides for next iteration
            config.model_override = original_model_override.clone();
            config.provider_override = original_provider_override.clone();

            // Increment session count in DB
            if let Err(e) = self.checkpoint_db.append_task_output_ex(
                &config.execution_id,
                "",
                true,  // increment_session
                false, // check_completion_marker
            ) {
                warn!("Failed to increment session count: {}", e);
            }
        }
    }

    /// Multi-Agent Pipeline architecture: specialized agents in a DAG-structured pipeline.
    ///
    /// Instead of a monolithic verify→fix loop, this architecture:
    /// 1. Analyzes specs into acceptance criteria with dependency ordering (Spec Analyst)
    /// 2. Builds an execution DAG from criteria dependencies (deterministic)
    /// 3. Captures UI state via UI Bridge (Snapshot Agent)
    /// 4. Maps criteria to code locations (Locator Agent)
    /// 5. Assigns independent DAG subtrees to parallel Implementer agents
    /// 6. Verifies each subtree with isolated Verifier agents
    /// 7. Runs integration verification to catch cross-subtree regressions
    ///
    /// Each agent produces a typed, serialized trace (PipelineAgentTrace) that
    /// enables per-agent autoresearch benchmarking and replay.
    async fn run_multi_agent_pipeline_loop(
        &self,
        config: &mut LoopConfig,
        verification_steps: &[ExecutionStepConfig],
        has_agentic_steps: bool,
        agentic_steps: &[ExecutionStepConfig],
        _all_step_results: &mut Vec<crate::step_executor::StepExecutionResult>,
        logger: &StepEventLogger,
    ) -> LoopResult {
        use crate::autoresearch::agentic_verification::*;

        let pipeline_config = config
            .multi_agent_pipeline_config
            .clone()
            .unwrap_or_default();

        // Load active prompt variants from the registry (populated by meta-optimizer).
        // If a variant exists for an agent type, it can be used to customize that agent's behavior.
        // Currently a no-op until the meta-optimizer populates the registry.
        let mut active_prompt_variants: std::collections::HashMap<String, String> = {
            let mut variants = std::collections::HashMap::new();
            for agent_type in &["spec_analyst", "locator", "implementer", "verifier"] {
                if let Ok(Some(variant)) = crate::meta_optimizer::prompt_registry::get_active_prompt(
                    &self.checkpoint_db,
                    agent_type,
                ) {
                    debug!(
                        "MULTI-AGENT-PIPELINE: Using prompt variant '{}' v{} for {}",
                        variant.variant_name, variant.version, agent_type
                    );
                    variants.insert(agent_type.to_string(), variant.prompt_content);
                }
            }
            variants
        };
        if !active_prompt_variants.is_empty() {
            info!(
                "MULTI-AGENT-PIPELINE: {} active prompt variant(s) loaded from registry",
                active_prompt_variants.len()
            );
        }

        // Check for active canary rollouts targeting pipeline agents.
        // If a canary is active, probabilistically decide whether this run uses the canary config.
        let active_canary: Option<(String, String)> = {
            // (canary_id, recommendation_id)
            match crate::meta_optimizer::canary::get_active_canaries(&self.checkpoint_db) {
                Ok(canaries) => canaries.into_iter().find_map(|c| {
                    if crate::meta_optimizer::canary::should_apply_canary(
                        &self.checkpoint_db,
                        &c.recommendation_id,
                    ) {
                        info!(
                            "MULTI-AGENT-PIPELINE: Canary rollout {} active for this run ({}%)",
                            c.id, c.percentage
                        );
                        Some((c.id, c.recommendation_id))
                    } else {
                        None
                    }
                }),
                Err(_) => None,
            }
        };
        let is_canary_run = active_canary.is_some();

        // For canary runs, load the recommendation's prompt overrides and inject them
        // into the active_prompt_variants map, replacing any existing variants for those agents.
        // For baseline runs (non-canary), the existing prompt variants remain as-is.
        if let Some((_, ref rec_id)) = active_canary {
            match crate::meta_optimizer::canary::get_canary_prompt_overrides(
                &self.checkpoint_db,
                rec_id,
            ) {
                Ok(overrides) => {
                    for (agent_type, prompt_content) in overrides {
                        info!(
                            "MULTI-AGENT-PIPELINE: Canary injecting prompt override for {}",
                            agent_type
                        );
                        active_prompt_variants.insert(agent_type, prompt_content);
                    }
                }
                Err(e) => {
                    warn!(
                        "MULTI-AGENT-PIPELINE: Failed to load canary prompt overrides: {}",
                        e
                    );
                }
            }
        }

        info!(
            "MULTI-AGENT-PIPELINE: Starting (max_parallel={}, max_retries={}, dag_strategy={}, level_strategy={}, max_total_iterations={})",
            pipeline_config.max_parallel_implementers,
            pipeline_config.max_retries_per_subtree,
            pipeline_config.dag_strategy,
            pipeline_config.level_strategy,
            pipeline_config.max_total_iterations,
        );

        let mut total_iterations: u32 = 0;
        let mut agent_traces: Vec<PipelineAgentTrace> = Vec::new();

        // ── Phase 1: Spec Analysis ──────────────────────────────────────
        // The Spec Analyst agent parses spec files into structured acceptance
        // criteria with dependency metadata. For now, we derive criteria from
        // the verification steps (which are already built from specs).
        info!("MULTI-AGENT-PIPELINE: Phase 1 — Spec Analysis");

        let analyst_start = std::time::Instant::now();
        let criteria: Vec<PipelineAcceptanceCriterion> = verification_steps
            .iter()
            .enumerate()
            .map(|(i, step)| {
                let step_name = step.name.clone().unwrap_or_else(|| format!("step_{}", i));
                PipelineAcceptanceCriterion {
                    id: format!("criterion_{}", i),
                    spec_assertion_id: step_name.clone(),
                    spec_group_id: step.id.clone().unwrap_or_default(),
                    description: step_name,
                    criterion_type: "deterministic".to_string(),
                    verification_method: step.step_type.clone(),
                    depends_on: vec![],
                    target_elements: vec![],
                    estimated_complexity: "simple".to_string(),
                    severity: "critical".to_string(),
                    enabled: true,
                }
            })
            .collect();
        let analyst_duration = analyst_start.elapsed().as_millis() as u64;

        agent_traces.push(PipelineAgentTrace {
            agent_type: "spec_analyst".to_string(),
            agent_id: "spec_analyst_0".to_string(),
            run_id: config.execution_id.clone(),
            input_snapshot: serde_json::json!({
                "verification_step_count": verification_steps.len(),
            }),
            output_snapshot: serde_json::json!({
                "criteria_count": criteria.len(),
            }),
            config: pipeline_config.spec_analyst.clone(),
            duration_ms: analyst_duration,
            tokens_in: 0,
            tokens_out: 0,
            cost_usd: 0.0,
            downstream_success: None,
            output_quality_score: None,
        });

        if criteria.is_empty() {
            info!("MULTI-AGENT-PIPELINE: No criteria derived — nothing to do");
            return LoopResult {
                iterations_run: 0,
                verification_passed: true,
                max_iterations_reached: false,
                critical_failure: false,
                was_stopped: false,
                unfixable_errors: false,
                iteration_results: vec![],
            };
        }

        info!(
            "MULTI-AGENT-PIPELINE: Spec Analyst produced {} criteria in {}ms",
            criteria.len(),
            analyst_duration
        );

        // ── Phase 2: DAG Construction (deterministic) ───────────────────
        info!(
            "MULTI-AGENT-PIPELINE: Phase 2 — DAG Construction (strategy={})",
            pipeline_config.dag_strategy
        );

        // Build a single subtree containing all criteria (flat DAG for initial impl).
        // The DAG builder will be enhanced to parse depends_on from analyst output.
        let dag = ExecutionDAG {
            nodes: criteria
                .iter()
                .map(|c| {
                    (
                        c.id.clone(),
                        DAGNode {
                            criterion_id: c.id.clone(),
                            dependencies: c.depends_on.clone(),
                            dependents: vec![],
                            level: 0,
                            subtree_id: "subtree_0".to_string(),
                        },
                    )
                })
                .collect(),
            roots: criteria.iter().map(|c| c.id.clone()).collect(),
            levels: vec![criteria.iter().map(|c| c.id.clone()).collect()],
            subtrees: vec![DAGSubtree {
                id: "subtree_0".to_string(),
                root_criteria: criteria.iter().map(|c| c.id.clone()).collect(),
                all_criteria: criteria.iter().map(|c| c.id.clone()).collect(),
                max_level: 0,
                estimated_complexity: "moderate".to_string(),
            }],
        };

        info!(
            "MULTI-AGENT-PIPELINE: DAG has {} subtree(s), {} level(s), {} total nodes",
            dag.subtrees.len(),
            dag.levels.len(),
            dag.nodes.len()
        );

        // ── Phase 3: Snapshot ───────────────────────────────────────────
        info!("MULTI-AGENT-PIPELINE: Phase 3 — UI Snapshot (delegated to verification steps)");

        // ── Phase 4: Code Location ─────────────────────────────────────
        info!("MULTI-AGENT-PIPELINE: Phase 4 — Code Location");

        let located_criteria: Vec<LocatedCriterion> = if pipeline_config
            .locator
            .max_tokens
            .unwrap_or(0)
            > 0
        {
            let locator_start = std::time::Instant::now();

            // Get the project file tree for the locator to analyze
            let project_path = config
                .project_path
                .clone()
                .unwrap_or_else(|| ".".to_string());
            let file_tree = get_file_tree(&project_path);

            // Build the locator prompt
            let criteria_text = criteria
                .iter()
                .map(|c| {
                    format!(
                        "- {} (id: {}): {}",
                        c.spec_assertion_id, c.id, c.description
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");

            let mut locator_prompt = format!(
                r#"You are a code locator agent. Given acceptance criteria and a project file tree, identify which files are most relevant to each criterion.

## Acceptance Criteria
{criteria_text}

## Project Files
{file_tree}

## Instructions
For each criterion, identify 1-5 files that are most likely to need changes or inspection.
Rate your confidence from 0.0 to 1.0 in each mapping.

Output JSON (and nothing else):

```json
[
  {{
    "criterion_id": "<the criterion id>",
    "spec_assertion_id": "<the spec_assertion_id>",
    "description": "<the criterion description>",
    "target_files": [
      {{"path": "src/components/MyComponent.tsx", "relevance": "primary"}}
    ],
    "related_files": [
      {{"path": "src/types/config.ts", "relevance": "type_definition"}}
    ],
    "confidence": 0.8
  }}
]
```

Only output the JSON array, nothing else."#,
                criteria_text = criteria_text,
                file_tree = file_tree,
            );

            // Inject active prompt variant for locator if available
            if let Some(variant_prompt) = active_prompt_variants.get("locator") {
                locator_prompt.push_str(&format!(
                    "\n\n## Additional Instructions\n{}",
                    variant_prompt
                ));
            }

            let locator_step = ExecutionStepConfig {
                step_type: "prompt".to_string(),
                name: Some("Locator: Map criteria to code locations".to_string()),
                prompt_content: Some(locator_prompt.clone()),
                ..Default::default()
            };

            // Run the locator through a single agentic iteration
            let (locator_outcome, _) = self
                .agentic_executor
                .run_agentic(
                    config,
                    0, // iteration
                    &locator_prompt,
                    true,
                    &[locator_step],
                    logger,
                )
                .await;

            // Parse the locator output into LocatedCriterion structs
            let parsed: Vec<LocatedCriterion> = if let Some(output) = locator_outcome.output() {
                // Try to find a JSON array in the output
                if let Some(start) = output.find('[') {
                    if let Some(end) = output.rfind(']') {
                        // Parse intermediate representation since the AI output schema
                        // differs from our LocatedCriterion struct (which embeds the full criterion)
                        #[derive(serde::Deserialize)]
                        struct LocatorOutputEntry {
                            criterion_id: String,
                            #[serde(default)]
                            target_files: Vec<CodeLocation>,
                            #[serde(default)]
                            related_files: Vec<CodeLocation>,
                            #[serde(default)]
                            confidence: f64,
                        }

                        match serde_json::from_str::<Vec<LocatorOutputEntry>>(&output[start..=end])
                        {
                            Ok(entries) => {
                                entries
                                    .into_iter()
                                    .filter_map(|entry| {
                                        // Find the matching criterion to embed in the LocatedCriterion
                                        criteria.iter().find(|c| c.id == entry.criterion_id).map(
                                            |c| LocatedCriterion {
                                                criterion: c.clone(),
                                                target_files: entry.target_files,
                                                related_files: entry.related_files,
                                                confidence: entry.confidence,
                                            },
                                        )
                                    })
                                    .collect()
                            }
                            Err(e) => {
                                warn!("MULTI-AGENT-PIPELINE: Failed to parse locator JSON: {}", e);
                                Vec::new()
                            }
                        }
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };

            info!(
                "MULTI-AGENT-PIPELINE: Locator identified {} criteria locations",
                parsed.len()
            );

            let locator_duration = locator_start.elapsed().as_millis() as u64;
            agent_traces.push(PipelineAgentTrace {
                agent_type: "locator".to_string(),
                agent_id: "locator_0".to_string(),
                run_id: config.execution_id.clone(),
                input_snapshot: serde_json::json!({
                    "criteria_count": criteria.len(),
                    "file_tree_lines": file_tree.lines().count(),
                }),
                output_snapshot: serde_json::json!({
                    "located_criteria_count": parsed.len(),
                }),
                config: pipeline_config.locator.clone(),
                duration_ms: locator_duration,
                tokens_in: 0,
                tokens_out: 0,
                cost_usd: 0.0,
                downstream_success: None,
                output_quality_score: None,
            });

            parsed
        } else {
            info!("MULTI-AGENT-PIPELINE: Phase 4 — Code Location (skipped, locator.max_tokens=0)");
            Vec::new()
        };

        // ── Phase 5: Implementation + Verification per subtree ──────────
        info!("MULTI-AGENT-PIPELINE: Phase 5 — Implementation + Verification");

        let mut subtree_results: Vec<SubtreeResult> = Vec::new();

        for subtree in &dag.subtrees {
            if self.is_task_stopped(&config.execution_id) {
                info!("MULTI-AGENT-PIPELINE: Stopped by user");
                let result = MultiAgentPipelineResult {
                    total_iterations,
                    goal_achieved: false,
                    was_stopped: true,
                    max_iterations_reached: false,
                    subtree_results,
                    integration_result: None,
                    agent_traces,
                    dag: dag.clone(),
                    total_criteria: criteria.len() as u32,
                    passed_criteria: 0,
                    total_tokens: 0,
                    total_cost_usd: 0.0,
                };
                return result.to_loop_result();
            }

            info!(
                "MULTI-AGENT-PIPELINE: Processing subtree '{}' ({} criteria)",
                subtree.id,
                subtree.all_criteria.len()
            );

            let mut subtree_level_results: Vec<SubtreeLevelResult> = Vec::new();
            let mut subtree_all_passed = true;
            let mut retries_used: u32 = 0;

            // Process levels within this subtree
            for (level_idx, level_criteria_ids) in dag.levels.iter().enumerate() {
                // Filter to criteria in this subtree
                let level_criteria: Vec<&str> = level_criteria_ids
                    .iter()
                    .filter(|id| subtree.all_criteria.contains(id))
                    .map(|s| s.as_str())
                    .collect();

                if level_criteria.is_empty() {
                    continue;
                }

                // Retry loop for this level: implementer + verifier, with retries on failure
                let mut level_attempt: u32 = 0;
                let mut level_passed = false;
                let mut level_criterion_results: Vec<PipelineCriterionResult> = Vec::new();
                let mut last_implementer_trace: Option<PipelineAgentTrace> = None;
                let mut last_verifier_trace: Option<PipelineAgentTrace> = None;
                let mut prior_failure_feedback: Option<String> = None;

                loop {
                    if total_iterations >= pipeline_config.max_total_iterations {
                        info!(
                            "MULTI-AGENT-PIPELINE: Total iteration budget ({}) exhausted",
                            pipeline_config.max_total_iterations
                        );
                        subtree_all_passed = false;
                        break;
                    }

                    total_iterations += 1;
                    level_attempt += 1;

                    // Build failure context, including feedback from prior attempt if retrying
                    let mut failure_context = format!(
                        "Multi-Agent Pipeline: Implement criteria at level {} for subtree '{}'. Criteria: {}",
                        level_idx,
                        subtree.id,
                        level_criteria.join(", ")
                    );
                    if let Some(ref feedback) = prior_failure_feedback {
                        failure_context.push_str(&format!(
                            "\n\n## Previous Attempt Failed (attempt {}/{})\n{}",
                            level_attempt - 1,
                            pipeline_config.max_retries_per_subtree + 1,
                            feedback
                        ));
                    }

                    // Inject active prompt variant for implementer if available
                    if let Some(variant_prompt) = active_prompt_variants.get("implementer") {
                        failure_context.push_str(&format!(
                            "\n\n## Agent Instructions (from optimized prompt)\n{}",
                            variant_prompt
                        ));
                    }

                    // Add location context from the Locator agent if available
                    if !located_criteria.is_empty() {
                        failure_context.push_str("\n\n## Code Locations (from Locator Agent)\n");
                        for lc in &located_criteria {
                            if level_criteria.contains(&lc.criterion.id.as_str()) {
                                failure_context.push_str(&format!(
                                    "### {} (confidence: {:.0}%)\n",
                                    lc.criterion.id,
                                    lc.confidence * 100.0
                                ));
                                if !lc.target_files.is_empty() {
                                    failure_context.push_str("Target files:\n");
                                    for f in &lc.target_files {
                                        failure_context.push_str(&format!(
                                            "- `{}` ({})\n",
                                            f.path, f.relevance
                                        ));
                                    }
                                }
                                if !lc.related_files.is_empty() {
                                    failure_context.push_str("Related files:\n");
                                    for f in &lc.related_files {
                                        failure_context.push_str(&format!(
                                            "- `{}` ({})\n",
                                            f.path, f.relevance
                                        ));
                                    }
                                }
                            }
                        }
                    }

                    // ── Implementer phase ────────────────────────────────────
                    let implementer_start = std::time::Instant::now();

                    let (agentic_outcome, _new_steps) = if has_agentic_steps {
                        self.agentic_executor
                            .run_agentic(
                                config,
                                total_iterations,
                                &failure_context,
                                has_agentic_steps,
                                agentic_steps,
                                logger,
                            )
                            .await
                    } else {
                        (
                            crate::unified_workflow_executor::AgenticOutcome::Skipped,
                            vec![],
                        )
                    };

                    let implementer_duration = implementer_start.elapsed().as_millis() as u64;

                    let implementer_trace = PipelineAgentTrace {
                        agent_type: "implementer".to_string(),
                        agent_id: format!("impl_{}_{}_{}", subtree.id, level_idx, level_attempt),
                        run_id: config.execution_id.clone(),
                        input_snapshot: serde_json::json!({
                            "subtree_id": subtree.id,
                            "level": level_idx,
                            "attempt": level_attempt,
                            "criteria": level_criteria,
                            "has_prior_feedback": prior_failure_feedback.is_some(),
                        }),
                        output_snapshot: serde_json::json!({
                            "outcome": format!("{:?}", agentic_outcome),
                        }),
                        config: pipeline_config.implementer.clone(),
                        duration_ms: implementer_duration,
                        tokens_in: 0,
                        tokens_out: 0,
                        cost_usd: 0.0,
                        downstream_success: None,
                        output_quality_score: None,
                    };
                    agent_traces.push(implementer_trace.clone());
                    last_implementer_trace = Some(implementer_trace);

                    // ── Verifier phase ───────────────────────────────────────
                    let verifier_start = std::time::Instant::now();

                    let level_verification_steps: Vec<ExecutionStepConfig> = verification_steps
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| {
                            level_criteria.contains(&format!("criterion_{}", i).as_str())
                        })
                        .map(|(_, step)| step.clone())
                        .collect();

                    let (verification_result, _step_results) = self
                        .verification_executor
                        .run_verification(
                            &level_verification_steps,
                            &config.execution_id,
                            total_iterations,
                            &config.workflow_name,
                            logger,
                            config.stage_index,
                        )
                        .await;

                    level_criterion_results = Vec::new();
                    level_passed = verification_result.all_passed;

                    for step_result in &verification_result.step_results {
                        let details = step_result
                            .verification_details
                            .as_ref()
                            .and_then(|vd| vd.stdout.as_ref())
                            .or(step_result.error.as_ref())
                            .map(|s| s.chars().take(500).collect::<String>())
                            .unwrap_or_default();

                        level_criterion_results.push(PipelineCriterionResult {
                            criterion_id: step_result.step_name.clone(),
                            passed: step_result.success,
                            method_used: step_result.step_type.clone(),
                            confidence: if step_result.success { 1.0 } else { 0.0 },
                            details,
                            duration_ms: step_result.duration_ms,
                        });
                    }

                    let verifier_duration = verifier_start.elapsed().as_millis() as u64;

                    let verifier_trace = PipelineAgentTrace {
                        agent_type: "verifier".to_string(),
                        agent_id: format!("verify_{}_{}_{}", subtree.id, level_idx, level_attempt),
                        run_id: config.execution_id.clone(),
                        input_snapshot: serde_json::json!({
                            "subtree_id": subtree.id,
                            "level": level_idx,
                            "attempt": level_attempt,
                            "criteria_count": level_criterion_results.len(),
                        }),
                        output_snapshot: serde_json::json!({
                            "passed": level_passed,
                            "results": level_criterion_results.len(),
                        }),
                        config: pipeline_config.verifier.clone(),
                        duration_ms: verifier_duration,
                        tokens_in: 0,
                        tokens_out: 0,
                        cost_usd: 0.0,
                        downstream_success: Some(level_passed),
                        output_quality_score: None,
                    };
                    agent_traces.push(verifier_trace.clone());
                    last_verifier_trace = Some(verifier_trace);

                    // Record canary run outcome if this is a canary run
                    if let Some((ref canary_id, _)) = active_canary {
                        let _ = crate::meta_optimizer::canary::record_canary_run(
                            &self.checkpoint_db,
                            canary_id,
                            is_canary_run,
                            level_passed,
                            0.0, // cost tracked separately via token usage
                            (implementer_duration + verifier_duration) as f64,
                        );
                    }

                    info!(
                        "MULTI-AGENT-PIPELINE: Subtree '{}' level {} attempt {} — {} (impl={}ms, verify={}ms)",
                        subtree.id,
                        level_idx,
                        level_attempt,
                        if level_passed { "PASSED" } else { "FAILED" },
                        implementer_duration,
                        verifier_duration,
                    );

                    if level_passed {
                        break; // Level succeeded, move to next level
                    }

                    // Check if we have retries remaining
                    retries_used += 1;
                    if retries_used >= pipeline_config.max_retries_per_subtree {
                        info!(
                            "MULTI-AGENT-PIPELINE: Level {} exhausted retries ({}/{})",
                            level_idx, retries_used, pipeline_config.max_retries_per_subtree
                        );
                        subtree_all_passed = false;
                        break; // No more retries, move on
                    }

                    // Build feedback from failed criteria for the next attempt
                    let failed_criteria: Vec<String> = level_criterion_results
                        .iter()
                        .filter(|c| !c.passed)
                        .map(|c| format!("- {} ({}): {}", c.criterion_id, c.method_used, c.details))
                        .collect();
                    prior_failure_feedback = Some(format!(
                        "The following criteria failed:\n{}",
                        failed_criteria.join("\n")
                    ));

                    info!(
                        "MULTI-AGENT-PIPELINE: Level {} failed, retrying ({}/{} retries used)",
                        level_idx, retries_used, pipeline_config.max_retries_per_subtree
                    );
                } // end retry loop

                if !level_passed {
                    subtree_all_passed = false;
                }

                subtree_level_results.push(SubtreeLevelResult {
                    level: level_idx as u32,
                    implementer_trace: last_implementer_trace.unwrap_or_else(|| {
                        PipelineAgentTrace {
                            agent_type: "implementer".to_string(),
                            agent_id: format!("impl_{}_{}", subtree.id, level_idx),
                            run_id: config.execution_id.clone(),
                            input_snapshot: serde_json::json!(null),
                            output_snapshot: serde_json::json!(null),
                            config: pipeline_config.implementer.clone(),
                            duration_ms: 0,
                            tokens_in: 0,
                            tokens_out: 0,
                            cost_usd: 0.0,
                            downstream_success: None,
                            output_quality_score: None,
                        }
                    }),
                    verifier_trace: last_verifier_trace.unwrap_or_else(|| PipelineAgentTrace {
                        agent_type: "verifier".to_string(),
                        agent_id: format!("verify_{}_{}", subtree.id, level_idx),
                        run_id: config.execution_id.clone(),
                        input_snapshot: serde_json::json!(null),
                        output_snapshot: serde_json::json!(null),
                        config: pipeline_config.verifier.clone(),
                        duration_ms: 0,
                        tokens_in: 0,
                        tokens_out: 0,
                        cost_usd: 0.0,
                        downstream_success: None,
                        output_quality_score: None,
                    }),
                    retries: level_attempt.saturating_sub(1),
                    passed: level_passed,
                    criterion_results: level_criterion_results,
                });
            }

            subtree_results.push(SubtreeResult {
                subtree_id: subtree.id.clone(),
                level_results: subtree_level_results,
                retries_used,
                all_passed: subtree_all_passed,
                regressions: vec![],
            });
        }

        // ── Phase 6: Integration Verification ───────────────────────────
        let integration_result = if pipeline_config.integration_verification {
            info!("MULTI-AGENT-PIPELINE: Phase 6 — Integration Verification (full spec check)");

            let (int_verification_result, _int_step_results) = self
                .verification_executor
                .run_verification(
                    verification_steps,
                    &config.execution_id,
                    total_iterations + 1, // integration is an extra verification pass
                    &config.workflow_name,
                    logger,
                    config.stage_index,
                )
                .await;

            let mut integration_criteria: Vec<PipelineCriterionResult> = Vec::new();
            for step_result in &int_verification_result.step_results {
                let details = step_result
                    .verification_details
                    .as_ref()
                    .and_then(|vd| vd.stdout.as_ref())
                    .or(step_result.error.as_ref())
                    .map(|s| s.chars().take(500).collect::<String>())
                    .unwrap_or_default();

                integration_criteria.push(PipelineCriterionResult {
                    criterion_id: step_result.step_name.clone(),
                    passed: step_result.success,
                    method_used: step_result.step_type.clone(),
                    confidence: if step_result.success { 1.0 } else { 0.0 },
                    details,
                    duration_ms: step_result.duration_ms,
                });
            }

            let int_passed = int_verification_result.all_passed;
            let int_total = integration_criteria.len();
            let int_ok = integration_criteria.iter().filter(|c| c.passed).count();
            info!(
                "MULTI-AGENT-PIPELINE: Integration verification — {}/{} passed ({})",
                int_ok,
                int_total,
                if int_passed { "ALL PASS" } else { "FAILURES" },
            );

            Some(integration_criteria)
        } else {
            None
        };

        // ── Build final result ──────────────────────────────────────────
        let goal_achieved = if let Some(ref int_results) = integration_result {
            int_results.iter().all(|c| c.passed)
        } else {
            subtree_results.iter().all(|s| s.all_passed)
        };

        let passed_criteria = if let Some(ref int_results) = integration_result {
            int_results.iter().filter(|c| c.passed).count() as u32
        } else {
            subtree_results
                .iter()
                .flat_map(|s| s.level_results.iter())
                .flat_map(|l| l.criterion_results.iter())
                .filter(|c| c.passed)
                .count() as u32
        };

        // Backfill downstream_success on all traces
        for trace in &mut agent_traces {
            trace.downstream_success = Some(goal_achieved);
        }

        let result = MultiAgentPipelineResult {
            total_iterations,
            goal_achieved,
            was_stopped: false,
            max_iterations_reached: total_iterations >= pipeline_config.max_total_iterations,
            subtree_results,
            integration_result,
            agent_traces,
            dag,
            total_criteria: criteria.len() as u32,
            passed_criteria,
            total_tokens: 0,
            total_cost_usd: 0.0,
        };

        info!("MULTI-AGENT-PIPELINE: {}", result.summary());

        // Persist agent traces for meta-optimizer analysis
        if let Err(e) = crate::database::pipeline_traces::save_pipeline_agent_traces(
            &self.checkpoint_db,
            &config.execution_id,
            &result.agent_traces,
        ) {
            warn!("Failed to persist pipeline agent traces: {}", e);
        }

        // Store the full pipeline result in task run result_data for autoresearch retrieval
        if let Ok(result_json) = serde_json::to_string(&result) {
            if let Err(e) = self
                .checkpoint_db
                .update_task_run_result_data(&config.execution_id, &result_json)
            {
                warn!("Failed to store pipeline result_data: {}", e);
            }
        }

        result.to_loop_result()
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

            // Auto-store convergence snapshot on completion
            let db2 = self.checkpoint_db.clone();
            let exec_id2 = execution_id.to_string();
            tokio::spawn(async move {
                let _ = db2.with_conn(|conn| {
                    let wf_name: Option<String> = conn
                        .query_row(
                            "SELECT workflow_name FROM task_runs WHERE id = ?1",
                            rusqlite::params![exec_id2],
                            |row| row.get(0),
                        )
                        .ok();
                    if let Some(wf_name) = wf_name {
                        if let Ok(metrics) =
                            crate::reflection::prediction::compute_convergence_score(
                                conn, &wf_name, "workflow",
                            )
                        {
                            let _ = crate::reflection::prediction::store_convergence_snapshot(
                                conn, &wf_name, None, "workflow", &metrics,
                            );
                        }
                    }
                    Ok(())
                });
            });

            // Parse meta-optimizer recommendations from completed output
            {
                let db = self.checkpoint_db.clone();
                let eid = execution_id.to_string();
                tokio::spawn(async move {
                    // Check if this task run is a meta-optimizer run
                    let task_run = match db.get_task_run(&eid) {
                        Ok(Some(tr)) => tr,
                        _ => return,
                    };
                    if !task_run.is_meta_optimizer {
                        return;
                    }

                    // Look up the optimizer_run record by task_run_id
                    let (optimizer_run_id, optimizer_type) = match db.with_conn({
                        let eid = eid.clone();
                        move |conn| {
                            conn.query_row(
                                "SELECT id, optimizer_type FROM meta_optimizer_runs WHERE task_run_id = ?1",
                                rusqlite::params![eid],
                                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                            )
                            .map_err(|e| format!("Failed to find optimizer run for task {}: {}", eid, e))
                        }
                    }) {
                        Ok((id, ot)) => (id, ot),
                        Err(e) => {
                            warn!("Could not find optimizer run for meta-optimizer task {}: {}", eid, e);
                            return;
                        }
                    };

                    // Parse recommendations from the output
                    let output = &task_run.output_log;
                    match crate::meta_optimizer::parser::save_parsed_recommendations(
                        &db,
                        &optimizer_type,
                        Some(&optimizer_run_id),
                        output,
                    ) {
                        Ok(count) => {
                            info!(
                                "Meta-optimizer {}: parsed {} recommendation(s) from task {}",
                                optimizer_type, count, eid
                            );
                            // Complete the optimizer run record
                            if let Err(e) =
                                crate::meta_optimizer::recommendations::complete_optimizer_run(
                                    &db,
                                    &optimizer_run_id,
                                    0, // runs_analyzed is not tracked here
                                    count as i64,
                                )
                            {
                                warn!(
                                    "Failed to complete optimizer run {}: {}",
                                    optimizer_run_id, e
                                );
                            }
                        }
                        Err(e) => {
                            warn!(
                                "Failed to parse meta-optimizer recommendations for task {}: {}",
                                eid, e
                            );
                        }
                    }
                });
            }

            // Check workflow chain triggers
            if let Some(wf_id) = workflow_id {
                self.check_chain_triggers(execution_id, wf_id, "completed")
                    .await;
            }
        }
    }

    async fn mark_task_failed(&self, execution_id: &str, reason: &str, workflow_id: Option<&str>) {
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

            // Check workflow chain triggers
            if let Some(wf_id) = workflow_id {
                self.check_chain_triggers(execution_id, wf_id, "failed")
                    .await;
            }
        }
    }

    /// Fire-and-forget: check if any workflow chain triggers match this completion.
    async fn check_chain_triggers(&self, execution_id: &str, workflow_id: &str, status: &str) {
        let service = crate::trigger_system::get_trigger_service().await;
        if let Some(service) = service {
            let tx = service.event_sender();
            let db = self.checkpoint_db.clone();
            let wf_id = workflow_id.to_string();
            let exec_id = execution_id.to_string();
            let status = status.to_string();
            tokio::spawn(async move {
                crate::trigger_system::watchers::workflow_chain::check_workflow_chains(
                    &db, &tx, &wf_id, &exec_id, &status, None,
                )
                .await;
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

    /// Resolve all errors captured during this workflow run on successful completion.
    ///
    /// This bulk-resolves errors scoped to the execution_id, reducing noise from
    /// errors that the workflow already handled. Placed after targeted resolution
    /// so those get their specific notes first; already-resolved errors won't be
    /// double-processed by the WHERE clause.
    async fn resolve_workflow_scoped_errors(&self, execution_id: &str) {
        match self.checkpoint_db.connection() {
            Ok(conn) => {
                match crate::error_monitor::ErrorEventStorage::resolve_errors_by_task_run(
                    &conn,
                    execution_id,
                    execution_id,
                ) {
                    Ok(count) if count > 0 => {
                        info!(
                            "Auto-resolved {} workflow-scoped errors for task {}",
                            count, execution_id
                        );
                    }
                    Ok(_) => {} // No errors to resolve
                    Err(e) => {
                        warn!(
                            "Failed to auto-resolve workflow-scoped errors for {}: {}",
                            execution_id, e
                        );
                    }
                }
            }
            Err(e) => {
                error!(
                    "Failed to get database connection for workflow-scoped error resolution: {}",
                    e
                );
            }
        }
    }

    /// Fallback: create a single-repo worktree (original behavior).
    /// Used when the monorepo root cannot be determined or multi-repo creation fails.
    fn create_single_repo_worktree(
        config: &mut LoopConfig,
        setup_automation_steps: &mut Vec<ExecutionStepConfig>,
        setup_prompt_steps: &mut Vec<ExecutionStepConfig>,
        verification_steps: &mut Vec<ExecutionStepConfig>,
        agentic_steps: &mut Vec<ExecutionStepConfig>,
        completion_automation_steps: &mut Vec<ExecutionStepConfig>,
        completion_prompt_steps: &mut Vec<ExecutionStepConfig>,
        checkpoint_db: &Arc<crate::database::CheckpointDb>,
    ) {
        if let Some(project_path) = config.project_path.clone() {
            let repo_path = std::path::Path::new(&project_path);
            match crate::worktree::create_worktree(
                repo_path,
                &config.execution_id,
                &config.workflow_name,
            ) {
                Ok(result) => {
                    info!(
                        "WORKTREE: Created single-repo worktree at {} (branch: {})",
                        result.worktree_path.display(),
                        result.branch_name
                    );
                    let wt_path = result.worktree_path.to_string_lossy().to_string();
                    config.project_path = Some(wt_path.clone());
                    config.worktree_path = Some(wt_path.clone());
                    config.worktree_branch = Some(result.branch_name.clone());

                    let now = chrono::Utc::now().to_rfc3339();
                    let record = crate::worktree::WorktreeRecord {
                        id: config.execution_id.clone(),
                        worktree_path: wt_path.clone(),
                        branch_name: result.branch_name.clone(),
                        source_branch: result.source_branch.clone(),
                        source_commit: result.source_commit.clone(),
                        repo_path: project_path.clone(),
                        task_run_id: Some(config.execution_id.clone()),
                        workflow_name: Some(config.workflow_name.clone()),
                        status: crate::worktree::WorktreeStatus::Active,
                        created_at: now.clone(),
                        updated_at: now,
                    };
                    if let Err(e) = checkpoint_db.insert_worktree(&record) {
                        warn!("WORKTREE: Failed to track worktree in database: {}", e);
                    }

                    let original_path = project_path;
                    let update_steps = |steps: &mut Vec<ExecutionStepConfig>| {
                        for step in steps.iter_mut() {
                            if let Some(ref wd) = step.shell_command_working_directory {
                                if wd.contains(&original_path) {
                                    step.shell_command_working_directory =
                                        Some(wd.replace(&original_path, &wt_path));
                                }
                            }
                            if let Some(ref wd) = step.check_working_directory {
                                if wd.contains(&original_path) {
                                    step.check_working_directory =
                                        Some(wd.replace(&original_path, &wt_path));
                                }
                            }
                        }
                    };
                    update_steps(setup_automation_steps);
                    update_steps(setup_prompt_steps);
                    update_steps(verification_steps);
                    update_steps(agentic_steps);
                    update_steps(completion_automation_steps);
                    update_steps(completion_prompt_steps);
                    for stage in &mut config.stages {
                        update_steps(&mut stage.setup_automation_steps);
                        update_steps(&mut stage.setup_prompt_steps);
                        update_steps(&mut stage.verification_steps);
                        update_steps(&mut stage.agentic_steps);
                        update_steps(&mut stage.completion_automation_steps);
                        update_steps(&mut stage.completion_prompt_steps);
                    }
                }
                Err(e) => {
                    warn!(
                        "WORKTREE: Failed to create worktree ({}). Running in main directory.",
                        e
                    );
                    config.use_worktree = false;
                }
            }
        } else {
            warn!("WORKTREE: No project_path set, cannot create worktree.");
            config.use_worktree = false;
        }
    }

    /// Check if the task has been stopped externally (via stop_ai_analysis endpoint).
    ///
    /// This allows the loop to gracefully abort when the user clicks the Stop button.
    fn is_task_stopped(&self, execution_id: &str) -> bool {
        // For composed run children (e.g., composed-run-X-workflow-N),
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

    /// Check if the task has been paused by the user.
    fn is_task_paused(&self, execution_id: &str) -> bool {
        let task_id_to_check = get_parent_task_id(execution_id);

        match self.checkpoint_db.get_task_run(&task_id_to_check) {
            Ok(Some(task)) => task.status == "paused",
            _ => false,
        }
    }

    /// Wait while the task is paused, polling every 500ms.
    /// Returns immediately if the task is not paused.
    /// Also returns if the task is stopped (so the caller can handle stop).
    async fn wait_while_paused(&self, execution_id: &str) {
        if !self.is_task_paused(execution_id) {
            return;
        }

        info!("Task {} is paused - waiting for resume", execution_id);

        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            // Check if stopped (takes priority over pause)
            if self.is_task_stopped(execution_id) {
                info!("Task {} was stopped while paused", execution_id);
                return;
            }

            // Check if still paused
            if !self.is_task_paused(execution_id) {
                info!("Task {} resumed - continuing execution", execution_id);
                return;
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
    /// For composed run children (e.g., composed-run-X-workflow-N),
    /// state is persisted under the parent composed run ID since children don't have
    /// their own task_run records.
    /// Record a workflow activity heartbeat for debugging stuck workflows.
    /// Updates runtime_context_json with the current phase and timestamp.
    fn record_activity(&self, execution_id: &str, activity: &str) {
        let persist_id = super::get_parent_task_id(execution_id);
        let now = chrono::Utc::now().to_rfc3339();

        // Merge into existing runtime context or create new
        let existing = self
            .checkpoint_db
            .get_task_run_runtime_context(&persist_id)
            .ok()
            .flatten()
            .unwrap_or_else(|| "{}".to_string());

        let mut ctx: serde_json::Value =
            serde_json::from_str(&existing).unwrap_or(serde_json::json!({}));
        ctx["last_activity"] = serde_json::json!(activity);
        ctx["last_activity_at"] = serde_json::json!(now);

        if let Ok(json) = serde_json::to_string(&ctx) {
            let _ = self
                .checkpoint_db
                .update_task_run_runtime_context(&persist_id, &json);
        }
    }

    fn persist_workflow_state(&self, execution_id: &str, state: &UnifiedWorkflowState) {
        // For composed run children (e.g., composed-run-X-workflow-N),
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
    ///
    /// Multi-agent fix: triage verification failures, then spawn specialized
    /// fix agents for each failure group with targeted verification between fixes.
    ///
    /// Returns Some((outcome, injected_steps)) on success, None if triage fails
    /// and we should fall back to the standard monolithic session.
    async fn run_multi_agent_fix(
        &self,
        config: &LoopConfig,
        iteration: u32,
        failure_context: &str,
        verification_result: &crate::step_executor::VerificationPhaseResult,
        all_verification_steps: &[crate::step_executor::ExecutionStepConfig],
        logger: &StepEventLogger,
    ) -> Option<(
        super::types::AgenticOutcome,
        Vec<crate::step_executor::ExecutionStepConfig>,
    )> {
        use super::multi_agent_fixer::*;

        // Step 1: Triage — classify failures
        let triage_prompt = build_triage_prompt(failure_context, &verification_result.step_results);

        let triage_result = match self
            .agentic_executor
            .run_triage_prompt(
                &triage_prompt,
                config.resolve_model_for_phase("verification"), // Use fast model for triage
            )
            .await
        {
            Ok(response) => match parse_triage_response(&response) {
                Ok(result) => {
                    info!(
                        "MULTI-AGENT: Triage classified {} failure group(s): {}",
                        result.groups.len(),
                        result.summary
                    );
                    result
                }
                Err(e) => {
                    warn!(
                        "MULTI-AGENT: Triage parse failed ({}), using deterministic fallback",
                        e
                    );
                    deterministic_triage(&verification_result.step_results)
                }
            },
            Err(e) => {
                warn!(
                    "MULTI-AGENT: Triage prompt failed ({}), using deterministic fallback",
                    e
                );
                deterministic_triage(&verification_result.step_results)
            }
        };

        if triage_result.groups.is_empty() {
            warn!("MULTI-AGENT: Triage produced no failure groups, falling back");
            return None;
        }

        // Step 2: Execute fix agents in dependency order
        let mut all_outputs = Vec::new();
        let mut fixed_groups: Vec<String> = Vec::new();

        for group_id in &triage_result.execution_order {
            let group = match triage_result.groups.iter().find(|g| &g.id == group_id) {
                Some(g) => g,
                None => continue,
            };

            // Skip cascading failures whose dependencies haven't been fixed
            if group.failure_type == FailureType::Cascading {
                let deps_fixed = group
                    .blocked_by
                    .iter()
                    .all(|dep| fixed_groups.contains(dep));
                if !deps_fixed {
                    info!(
                        "MULTI-AGENT: Skipping cascading group '{}' (dependencies not yet fixed)",
                        group.id
                    );
                    continue;
                }
            }

            // Build focused prompt based on failure type
            let group_failure_context =
                extract_group_failure_context(group, &verification_result.step_results);

            let working_dir = config.project_path.as_deref();

            let prompt = match group.failure_type {
                FailureType::LintFormat | FailureType::Compilation => {
                    build_quick_fix_prompt(group, &group_failure_context, working_dir)
                }
                _ => build_feature_fix_prompt(
                    group,
                    &group_failure_context,
                    &config.base_prompt,
                    working_dir,
                ),
            };

            // Run the focused fix agent
            let agent_label = format!("fix-{}", group.id);
            let (success, output, duration_ms) = self
                .agentic_executor
                .run_focused_session(
                    &config.execution_id,
                    &config.workflow_name,
                    iteration,
                    &agent_label,
                    &prompt,
                    config.resolve_model_for_phase("agentic"),
                    logger,
                )
                .await;

            info!(
                "MULTI-AGENT: Fix agent '{}' completed in {}ms (success={})",
                group.id, duration_ms, success
            );

            // Run targeted verification for this group's steps
            let targeted_result = self
                .verification_executor
                .run_targeted_verification(
                    all_verification_steps,
                    &group.step_indices,
                    &config.execution_id,
                    iteration,
                    &config.workflow_name,
                )
                .await;

            let group_passed = targeted_result.all_passed;
            if group_passed {
                info!(
                    "MULTI-AGENT: Group '{}' — targeted verification PASSED",
                    group.id
                );
                fixed_groups.push(group.id.clone());
            } else {
                info!(
                    "MULTI-AGENT: Group '{}' — targeted verification FAILED ({}/{} passed)",
                    group.id, targeted_result.passed_steps, targeted_result.total_steps
                );
            }

            all_outputs.push(format!(
                "## Fix Agent: {} ({})\nSuccess: {}\nVerification: {}\n\n{}",
                group.id,
                group.failure_type,
                success,
                if group_passed { "PASSED" } else { "FAILED" },
                &output[..output.len().min(2000)],
            ));
        }

        // Build aggregate outcome
        let combined_output = format!(
            "# Multi-Agent Fix Results (iteration {})\n\nGroups fixed: {}/{}\n\n{}",
            iteration,
            fixed_groups.len(),
            triage_result.groups.len(),
            all_outputs.join("\n---\n\n"),
        );

        let outcome = if !combined_output.is_empty() {
            super::types::AgenticOutcome::Success {
                output: combined_output,
                parsed: None,
            }
        } else {
            super::types::AgenticOutcome::Error {
                error: "Multi-agent fix produced no output".to_string(),
            }
        };

        Some((outcome, Vec::new()))
    }

    /// - The task is stopped externally
    /// - An AI session fails
    // Note: Sweep results are intentionally not added to all_step_results.
    // The sweep is a best-effort cleanup pass after the main workflow completes,
    // and its results should not affect the overall step counts or workflow outcome.
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
                AgenticOutcome::Success { output, .. } => {
                    format!(
                        "\n\n=== Completion Sweep (Iteration {}/{}) ===\n\n{}",
                        iteration + 1,
                        max_iterations,
                        output
                    )
                }
                AgenticOutcome::Failed { output, error, .. } => {
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
    /// Worktree info (if workflow ran in a worktree)
    pub worktree_path: Option<String>,
    /// Worktree branch name (if workflow ran in a worktree)
    pub worktree_branch: Option<String>,
    /// Workflow architecture used (traditional, agentic_verification, or multi_agent_pipeline).
    pub workflow_architecture:
        Option<crate::autoresearch::agentic_verification::WorkflowArchitecture>,
    /// Agentic verification config (if agentic verification was used).
    pub agentic_verification_config:
        Option<crate::autoresearch::agentic_verification::AgenticVerificationConfig>,
    /// Multi-agent pipeline config (if multi-agent pipeline was used).
    pub multi_agent_pipeline_config:
        Option<crate::autoresearch::agentic_verification::MultiAgentPipelineConfig>,
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
/// Evaluate whether a stage should be skipped based on its condition.
///
/// Returns `true` if the stage should be **skipped** (condition not met).
/// All condition fields combine with AND semantics: if any condition
/// is not satisfied, the stage is skipped.
///
/// Get the project's file tree for the locator agent to analyze.
///
/// Uses `git ls-files` to respect `.gitignore`, falling back to a placeholder
/// if git is not available or the directory is not a git repo.
/// Output is capped at 500 files to keep the prompt manageable.
fn get_file_tree(project_path: &str) -> String {
    let output = std::process::Command::new("git")
        .args(["ls-files", "--others", "--cached", "--exclude-standard"])
        .current_dir(project_path)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let files = String::from_utf8_lossy(&out.stdout);
            let lines: Vec<&str> = files.lines().take(500).collect();
            let total = files.lines().count();
            let mut result = lines.join("\n");
            if total > 500 {
                result.push_str(&format!("\n... and {} more files (truncated)", total - 500));
            }
            result
        }
        _ => {
            format!("(Could not list files in {})", project_path)
        }
    }
}

/// # Arguments
/// - `condition` - The stage condition to evaluate
/// - `previous_passed` - Whether the previous stage's verification passed
/// - `total_iterations` - Total iterations across all completed stages so far
/// - `total_failures` - Number of stages that failed verification so far
fn evaluate_stage_condition(
    condition: &crate::unified_workflows::StageCondition,
    previous_passed: bool,
    total_iterations: u32,
    total_failures: u32,
) -> bool {
    // Check if_previous condition
    if let Some(ref if_prev) = condition.if_previous {
        match if_prev.as_str() {
            "passed" => {
                if !previous_passed {
                    return true; // skip: previous did not pass
                }
            }
            "failed" => {
                if previous_passed {
                    return true; // skip: previous did not fail
                }
            }
            "any" => {} // always run
            other => {
                warn!("Unknown if_previous value '{}', treating as 'any'", other);
            }
        }
    }

    // Check min_iteration condition
    if let Some(min_iter) = condition.min_iteration {
        if total_iterations < min_iter {
            return true; // skip: not enough iterations yet
        }
    }

    // Check min_failures condition
    if let Some(min_fail) = condition.min_failures {
        if total_failures < min_fail {
            return true; // skip: not enough failures yet
        }
    }

    false // all conditions met, do NOT skip
}

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

/// Maximum time (in seconds) a task can remain in "running" status without a
/// matching workflow definition before it is automatically marked as failed.
/// Tasks whose `updated_at` timestamp is older than this threshold are
/// considered stale and will not be preserved indefinitely.
const STALE_RUNNING_TASK_TIMEOUT_SECS: i64 = 3600; // 1 hour

/// Check whether a task run has been stuck in "running" long enough to be
/// considered stale. Returns `true` when the task's `updated_at` timestamp
/// is more than [`STALE_RUNNING_TASK_TIMEOUT_SECS`] in the past.
fn is_task_stale(task_run: &crate::database::TaskRun) -> bool {
    let now = chrono::Utc::now();
    if let Ok(updated) = chrono::DateTime::parse_from_rfc3339(&task_run.updated_at) {
        let age = now.signed_duration_since(updated);
        age.num_seconds() > STALE_RUNNING_TASK_TIMEOUT_SECS
    } else {
        // If we cannot parse the timestamp treat the task as stale so it
        // does not stay in "running" forever.
        true
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
    let running_workflows = match db.get_running_unified_workflows(None) {
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
        // Check if the workflow state is actually complete but the task_run status
        // wasn't updated (e.g., runner crashed after workflow finished but before
        // the status was persisted).
        if let Ok(Some(wf_state)) = db.get_workflow_execution_state(&task_run.id) {
            if wf_state.state_name.contains("complete") || wf_state.state_name.contains("finished")
            {
                info!(
                    "Workflow '{}' (id: {}) state is '{}' — marking task_run as completed (stale running status)",
                    task_run.task_name, task_run.id, wf_state.state_name
                );
                if let Err(e) = db.complete_task_run(&task_run.id) {
                    error!(
                        "Failed to mark completed workflow {} as completed: {}",
                        task_run.id, e
                    );
                }
                processed_count += 1;
                continue;
            }
        }

        // Programmatic workflows (follow-up, reflection, fixer) are built in-memory
        // and never saved to the workflow library. They cannot be resumed from a DB
        // definition, so stop them cleanly instead of marking as failed — these
        // interruptions are not real failures and should not inflate failure metrics.
        if task_run.is_follow_up || task_run.is_reflection || task_run.is_fixer {
            let kind = if task_run.is_follow_up {
                "follow-up"
            } else if task_run.is_reflection {
                "reflection"
            } else {
                "fixer"
            };
            info!(
                "Marking interrupted {} workflow '{}' (id: {}) as stopped — programmatic workflows cannot be resumed",
                kind, task_run.task_name, task_run.id
            );
            if let Err(e) = db.stop_task_run_with_reason(
                &task_run.id,
                &format!(
                    "Interrupted by app restart ({} workflow — not resumable)",
                    kind
                ),
            ) {
                error!(
                    "Failed to mark {} workflow {} as stopped: {}",
                    kind, task_run.id, e
                );
            } else {
                processed_count += 1;
            }
            continue;
        }

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

                    let url_lock = Some(app_state.url_lock_manager.clone());
                    super::spawn_workflow_with_panic_guard(
                        checkpoint_db_for_guard,
                        task_id.clone(),
                        task_name.clone(),
                        url_lock,
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

                            // Normalize to stages — all workflows are now multi-stage
                            let normalized_stages = workflow.normalize_to_stages();
                            let total_stages = normalized_stages.len();

                            // Convert each stage to StageConfig
                            let stages: Vec<super::StageConfig> = normalized_stages
                                .iter()
                                .enumerate()
                                .map(|(idx, stage)| {
                                    crate::unified_workflows::stage_to_stage_config(
                                        stage,
                                        idx,
                                        total_stages,
                                        workflow.preflight_check_enabled,
                                        workflow.log_watch_enabled,
                                        workflow.health_check_enabled,
                                        &workflow.health_check_urls,
                                    )
                                })
                                .collect();

                            // Build combined prompt from all agentic steps across all stages
                            let combined_prompt = normalized_stages
                                .iter()
                                .flat_map(|stage| stage.agentic_steps.iter())
                                .filter_map(|step| step.get("content").and_then(|v| v.as_str()))
                                .collect::<Vec<_>>()
                                .join("\n\n---\n\n");

                            // For error-fix workflows, run agentic first (only if starting fresh)
                            let run_agentic_first =
                                !workflow.targeted_error_ids.is_empty() && starting_iteration == 0;

                            let loop_config = super::types::LoopConfig {
                                max_iterations: workflow.max_iterations,
                                base_prompt: combined_prompt,
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
                                stages,
                                stop_on_failure: workflow.stop_on_failure,
                                constraint_overrides: workflow.constraint_overrides.clone(),
                                reflection_mode: workflow.reflection_mode,
                                provider_override: None,
                                model_override: None,
                                model_overrides: workflow.model_overrides.clone(),
                                stage_index: None,
                                max_sessions: Some(workflow.max_iterations),
                                auto_run_generated: false,
                                approval_gate: workflow.approval_gate,
                                max_context_tokens: 100_000,
                                cross_workflow_learning: true,
                                verification_history: std::collections::HashMap::new(),
                                routing_context: Default::default(),
                                project_path: crate::mcp::shared::current_project_path(),
                                multi_agent_mode: false,
                                use_worktree: false,
                                worktree_path: None,
                                worktree_branch: None,
                                workflow_architecture: None,
                                agentic_verification_config: None,
                                multi_agent_pipeline_config: None,
                                acceptance_criteria: workflow.acceptance_criteria.clone(),
                            };

                            controller
                                .run(
                                    loop_config,
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                )
                                .await
                        },
                    );

                    processed_count += 1;
                }
                Ok(None) => {
                    if is_task_stale(task_run) {
                        warn!(
                            "Workflow definition {} not found for task {} and task has been running for over {} seconds - marking as failed",
                            wf_id, task_run.id, STALE_RUNNING_TASK_TIMEOUT_SECS
                        );
                        if let Err(e) = db.fail_task_run(
                            &task_run.id,
                            &format!(
                                "Workflow definition '{}' not found and task exceeded stale timeout ({}s)",
                                wf_id, STALE_RUNNING_TASK_TIMEOUT_SECS
                            ),
                        ) {
                            error!("Failed to mark stale task {} as failed: {}", task_run.id, e);
                        } else {
                            processed_count += 1;
                        }
                    } else {
                        warn!(
                            "Workflow definition {} not found for task {} - preserving 'running' status (will auto-fail after {}s)",
                            wf_id, task_run.id, STALE_RUNNING_TASK_TIMEOUT_SECS
                        );
                    }
                }
                Err(e) => {
                    error!("Failed to fetch workflow {} for resume: {}", wf_id, e);
                    if is_task_stale(task_run) {
                        warn!(
                            "Task {} has been running for over {} seconds and workflow fetch failed - marking as failed",
                            task_run.id, STALE_RUNNING_TASK_TIMEOUT_SECS
                        );
                        if let Err(e2) = db.fail_task_run(
                            &task_run.id,
                            &format!(
                                "Failed to fetch workflow definition and task exceeded stale timeout ({}s): {}",
                                STALE_RUNNING_TASK_TIMEOUT_SECS, e
                            ),
                        ) {
                            error!("Failed to mark stale task {} as failed: {}", task_run.id, e2);
                        } else {
                            processed_count += 1;
                        }
                    }
                }
            }
        } else if let Some(ref wf_name) = task_run.workflow_name {
            // Look up workflow by name — try exact match first, then strip
            // reflection prefixes ("Reflection: ", "Project Reflection: ") since
            // reflection task runs store a prefixed name while the workflow
            // definition keeps the original name.
            info!(
                "Looking up workflow by name '{}' for task_id: {}",
                wf_name, task_run.id
            );

            let workflow_result = db.get_unified_workflow_by_name(wf_name).and_then(|opt| {
                if opt.is_some() {
                    Ok(opt)
                } else {
                    // Try stripping reflection prefixes
                    let stripped = wf_name
                        .strip_prefix("Project Reflection: ")
                        .or_else(|| wf_name.strip_prefix("Reflection: "));
                    if let Some(name) = stripped {
                        info!("Retrying workflow lookup with stripped name: '{}'", name);
                        db.get_unified_workflow_by_name(name)
                    } else {
                        Ok(None)
                    }
                }
            });

            match workflow_result {
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

                    let url_lock2 = Some(app_state.url_lock_manager.clone());
                    super::spawn_workflow_with_panic_guard(
                        checkpoint_db_for_guard,
                        task_id.clone(),
                        task_name.clone(),
                        url_lock2,
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

                            // Normalize to stages — all workflows are now multi-stage
                            let normalized_stages = workflow.normalize_to_stages();
                            let total_stages = normalized_stages.len();

                            // Convert each stage to StageConfig
                            let stages: Vec<super::StageConfig> = normalized_stages
                                .iter()
                                .enumerate()
                                .map(|(idx, stage)| {
                                    crate::unified_workflows::stage_to_stage_config(
                                        stage,
                                        idx,
                                        total_stages,
                                        workflow.preflight_check_enabled,
                                        workflow.log_watch_enabled,
                                        workflow.health_check_enabled,
                                        &workflow.health_check_urls,
                                    )
                                })
                                .collect();

                            // Build combined prompt from all agentic steps across all stages
                            let combined_prompt = normalized_stages
                                .iter()
                                .flat_map(|stage| stage.agentic_steps.iter())
                                .filter_map(|step| step.get("content").and_then(|v| v.as_str()))
                                .collect::<Vec<_>>()
                                .join("\n\n---\n\n");

                            // For error-fix workflows, run agentic first (only if starting fresh)
                            let run_agentic_first =
                                !workflow.targeted_error_ids.is_empty() && starting_iteration == 0;

                            let loop_config = super::types::LoopConfig {
                                max_iterations: workflow.max_iterations,
                                base_prompt: combined_prompt,
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
                                stages,
                                stop_on_failure: workflow.stop_on_failure,
                                constraint_overrides: workflow.constraint_overrides.clone(),
                                reflection_mode: workflow.reflection_mode,
                                provider_override: None,
                                model_override: None,
                                model_overrides: workflow.model_overrides.clone(),
                                stage_index: None,
                                max_sessions: Some(workflow.max_iterations),
                                auto_run_generated: false,
                                approval_gate: workflow.approval_gate,
                                max_context_tokens: 100_000,
                                cross_workflow_learning: true,
                                verification_history: std::collections::HashMap::new(),
                                routing_context: Default::default(),
                                multi_agent_mode: false,
                                use_worktree: false,
                                worktree_path: None,
                                worktree_branch: None,
                                workflow_architecture: None,
                                agentic_verification_config: None,
                                multi_agent_pipeline_config: None,
                                project_path: crate::mcp::shared::current_project_path(),
                                acceptance_criteria: workflow.acceptance_criteria.clone(),
                            };

                            controller
                                .run(
                                    loop_config,
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                )
                                .await
                        },
                    );

                    processed_count += 1;
                }
                Ok(None) => {
                    if is_task_stale(task_run) {
                        warn!(
                            "Workflow definition not found by name '{}' for task {} and task has been running for over {} seconds - marking as failed",
                            wf_name, task_run.id, STALE_RUNNING_TASK_TIMEOUT_SECS
                        );
                        if let Err(e) = db.fail_task_run(
                            &task_run.id,
                            &format!(
                                "Workflow definition '{}' not found and task exceeded stale timeout ({}s)",
                                wf_name, STALE_RUNNING_TASK_TIMEOUT_SECS
                            ),
                        ) {
                            error!("Failed to mark stale task {} as failed: {}", task_run.id, e);
                        } else {
                            processed_count += 1;
                        }
                    } else {
                        warn!(
                            "Workflow definition not found by name '{}' for task {} - preserving 'running' status (will auto-fail after {}s)",
                            wf_name, task_run.id, STALE_RUNNING_TASK_TIMEOUT_SECS
                        );
                    }
                }
                Err(e) => {
                    error!(
                        "Failed to fetch workflow by name '{}' for resume: {}",
                        wf_name, e
                    );
                    if is_task_stale(task_run) {
                        warn!(
                            "Task {} has been running for over {} seconds and workflow name lookup failed - marking as failed",
                            task_run.id, STALE_RUNNING_TASK_TIMEOUT_SECS
                        );
                        if let Err(e2) = db.fail_task_run(
                            &task_run.id,
                            &format!(
                                "Failed to look up workflow '{}' and task exceeded stale timeout ({}s): {}",
                                wf_name, STALE_RUNNING_TASK_TIMEOUT_SECS, e
                            ),
                        ) {
                            error!("Failed to mark stale task {} as failed: {}", task_run.id, e2);
                        } else {
                            processed_count += 1;
                        }
                    }
                }
            }
        } else if is_task_stale(task_run) {
            warn!(
                "Could not extract workflow ID from task_id '{}' and no workflow_name set - task has been running for over {} seconds, marking as failed",
                task_run.id, STALE_RUNNING_TASK_TIMEOUT_SECS
            );
            if let Err(e) = db.fail_task_run(
                &task_run.id,
                &format!(
                    "No workflow definition could be resolved and task exceeded stale timeout ({}s)",
                    STALE_RUNNING_TASK_TIMEOUT_SECS
                ),
            ) {
                error!("Failed to mark stale task {} as failed: {}", task_run.id, e);
            } else {
                processed_count += 1;
            }
        } else {
            warn!(
                "Could not extract workflow ID from task_id '{}' and no workflow_name set - preserving 'running' status (will auto-fail after {}s)",
                task_run.id, STALE_RUNNING_TASK_TIMEOUT_SECS
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

// Step conversion utilities extracted to step_conversion module
pub use super::step_conversion::{
    convert_all_json_steps_with_phase, convert_json_steps_with_phase,
    extract_prompt_steps_with_phase, substitute_step_vars,
};

// Health monitoring utilities extracted to health_monitor module
pub(super) use super::health_monitor::{
    build_resume_agentic_context, detect_health_regression, detect_regression,
    fetch_pre_agentic_health_baseline, fetch_verifier_ui_context,
};
