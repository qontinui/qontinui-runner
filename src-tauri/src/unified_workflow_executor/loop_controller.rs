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
    pub(super) setup_executor: SetupExecutor,
    pub(super) verification_executor: VerificationExecutor,
    pub(super) agentic_executor: AgenticExecutor,
    pub(super) completion_executor: CompletionExecutor,
    pub(super) checkpoint_db: Arc<crate::database::CheckpointDb>,
    knowledge_base: KnowledgeBase,
    app_state: Arc<AppState>,
    config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
    pub(super) app_handle: tauri::AppHandle,
    pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    doctor_handle: Option<DoctorHandle>,
    reflection_fix_ctx: Option<crate::mcp::shared::ReflectionFixContext>,
    step_injection_ctx: Option<crate::step_injection::types::StepInjectionContext>,
    pub(super) canvas_manager: tokio::sync::Mutex<CanvasPanelManager>,
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

                // Infer architecture if not explicitly set by user or autoresearch
                stage_loop_config.infer_architecture();

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
            // Compute complexity indicators from stages
            let total_step_count: usize = config
                .stages
                .iter()
                .map(|s| {
                    s.setup_automation_steps.len()
                        + s.setup_prompt_steps.len()
                        + s.verification_steps.len()
                        + s.agentic_steps.len()
                        + s.completion_automation_steps.len()
                        + s.completion_prompt_steps.len()
                })
                .sum();
            let total_verification_steps: usize = config
                .stages
                .iter()
                .map(|s| s.verification_steps.len())
                .sum();
            let total_agentic_steps: usize =
                config.stages.iter().map(|s| s.agentic_steps.len()).sum();
            let has_ui_bridge = config.stages.iter().any(|s| {
                s.verification_steps
                    .iter()
                    .chain(s.agentic_steps.iter())
                    .chain(s.setup_automation_steps.iter())
                    .chain(s.completion_automation_steps.iter())
                    .any(|step| step.step_type == "ui_bridge")
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
                step_count: Some(total_step_count as i64),
                verification_step_count: Some(total_verification_steps as i64),
                agentic_step_count: Some(total_agentic_steps as i64),
                has_ui_bridge,
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

    // Agentic Verification Loop is in agentic_verification_loop.rs
    // (run_agentic_verification_loop method)

    // Multi-Agent Pipeline Loop is in multi_agent_pipeline_loop.rs
    // (run_multi_agent_pipeline_loop method)

    // Task lifecycle methods (mark_task_completed, mark_task_failed, check_chain_triggers,
    // resolve_targeted_errors, resolve_workflow_scoped_errors, create_single_repo_worktree)
    // are in task_lifecycle.rs

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
                input_tokens: None,
                output_tokens: None,
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

// Re-export WorkflowResult which is referenced by external code via loop_controller::WorkflowResult
pub use super::startup_resume::WorkflowResult;
// Internal re-exports for use within this crate
pub(crate) use super::startup_resume::evaluate_stage_condition;

// Step conversion utilities extracted to step_conversion module
pub use super::step_conversion::{
    convert_all_json_steps_with_phase, convert_json_steps_with_phase,
    extract_prompt_steps_with_phase, substitute_step_vars,
};

// Health monitoring utilities extracted to health_monitor module
pub(super) use super::health_monitor::{
    build_resume_agentic_context, detect_health_regression, detect_regression,
    fetch_pre_agentic_health_baseline,
};
