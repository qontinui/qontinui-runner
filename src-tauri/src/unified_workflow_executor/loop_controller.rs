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
use crate::doctor::DoctorHandle;
use crate::orchestrator::integration::StageTransition;
use crate::orchestrator::knowledge::KnowledgeBase;
use crate::step_executor::ExecutionStepConfig;
use crate::step_registry::StepEventLogger;
use crate::summary_generator::generate_task_summary_async;
use crate::AppState;

use super::canvas_panels::CanvasPanelManager;
use super::phases::{AgenticExecutor, CompletionExecutor, SetupExecutor, VerificationExecutor};
use super::resume::{ResumeManager, ResumePoint};
use super::states::UnifiedWorkflowState;
use super::types::{AgenticOutcome, LoopConfig, LoopResult, SweepResult};

/// The main loop controller for unified workflows.
///
/// This controller manages the verification-agentic loop with strict guarantees:
/// - Verification is the ONLY authority on whether work is complete
/// - The AI cannot self-declare completion
/// - Completion phase only runs when verification passes
/// - Step events are logged through a centralized registry to prevent duplicates
pub struct LoopController {
    pub(super) setup_executor: SetupExecutor,
    pub(super) verification_executor: Arc<VerificationExecutor>,
    pub(super) agentic_executor: Arc<AgenticExecutor>,
    pub(super) completion_executor: CompletionExecutor,
    pub(super) knowledge_base: KnowledgeBase,
    pub(super) app_state: Arc<AppState>,
    pub(super) config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
    pub(super) app_handle: tauri::AppHandle,
    pub(super) pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    pub(super) doctor_handle: Option<DoctorHandle>,
    pub(super) reflection_fix_ctx: Option<crate::mcp::shared::ReflectionFixContext>,
    pub(super) step_injection_ctx: Option<crate::step_injection::types::StepInjectionContext>,
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
            verification_executor: Arc::new(VerificationExecutor::new(
                app_state.clone(),
                config_storage.clone(),
                app_handle.clone(),
            )),
            agentic_executor: Arc::new(AgenticExecutor::new(
                app_state.clone(),
                app_handle.clone(),
                pid_tracker.clone(),
            )),
            completion_executor: CompletionExecutor::new(
                app_state.clone(),
                config_storage.clone(),
                app_handle.clone(),
                pid_tracker.clone(),
            ),
            knowledge_base: KnowledgeBase::new(crate::database::CheckpointDb::global()),
            canvas_manager: tokio::sync::Mutex::new(CanvasPanelManager::new(
                app_state.canvas_state.clone(),
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
        Arc::get_mut(&mut self.agentic_executor)
            .expect("agentic_executor Arc has multiple owners during builder phase")
            .set_session_manager(sm.clone());
        self.completion_executor.set_session_manager(sm);
        self
    }

    /// Set the reflection fix context for parsing [REFLECTION_FIX:...] markers during agentic phase.
    pub fn with_reflection_fix_ctx(
        mut self,
        ctx: crate::mcp::shared::ReflectionFixContext,
    ) -> Self {
        Arc::get_mut(&mut self.agentic_executor)
            .expect("agentic_executor Arc has multiple owners during builder phase")
            .set_reflection_fix_ctx(ctx.clone());
        self.reflection_fix_ctx = Some(ctx);
        self
    }

    /// Set the step injection context for parsing [INJECT_STEP]...[/INJECT_STEP] markers during agentic phase.
    pub fn with_step_injection_ctx(
        mut self,
        ctx: crate::step_injection::types::StepInjectionContext,
    ) -> Self {
        Arc::get_mut(&mut self.agentic_executor)
            .expect("agentic_executor Arc has multiple owners during builder phase")
            .set_step_injection_ctx(ctx.clone());
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
        if let Some(ve) = Arc::get_mut(&mut self.verification_executor) {
            ve.set_task_run_id(config.execution_id.clone());
        }
        self.completion_executor
            .set_task_run_id(config.execution_id.clone());

        // =====================================================================
        // PROPAGATE PATH SCOPE POLICY to phase executors
        // =====================================================================
        // When strict_cwd is enabled (per-workflow or global setting), enforce
        // workspace-scoped working directory resolution on all step handlers.
        if config.strict_cwd {
            let policy = crate::paths::PathScopePolicy::WorkspaceScoped;
            self.setup_executor.set_path_scope_policy(policy.clone());
            if let Some(ve) = Arc::get_mut(&mut self.verification_executor) {
                ve.set_path_scope_policy(policy.clone());
            }
            self.completion_executor.set_path_scope_policy(policy);
        }

        // =====================================================================
        // PROPAGATE MIDDLEWARE CHAIN to phase executors
        // =====================================================================
        // Apply the hardener sanitizers (CommandSanitizer + SdkUrlSanitizer) as
        // middleware on all AI sessions, not just the hardener's own AI call.
        // This ensures AI-generated commands are sanitized across all phases.
        {
            use crate::workflow_generation::hardener::build_hardener_middleware;
            self.setup_executor
                .set_middleware_chain(build_hardener_middleware());
            if let Some(ae) = Arc::get_mut(&mut self.agentic_executor) {
                ae.set_middleware_chain(build_hardener_middleware());
            }
            self.completion_executor
                .set_middleware_chain(build_hardener_middleware());
        }

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
                            let wt_result = self.app_state.pg_db.insert_worktree(&record).await;
                            if let Err(e) = wt_result {
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
                            &self.app_state.pg_db,
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
                    &self.app_state.pg_db,
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
        let resume_manager = ResumeManager::new(self.app_state.pg_db.clone());
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
            let task_run_opt = self.app_state.pg_db.get_task_run(&config.execution_id).await;
            if let Ok(Some(task_run)) = task_run_opt {
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
            self.app_state.pg_db.clone(),
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

            // Clearing of transition history, verification phase results,
            // constraint results, and workflow execution state removed — all persistence now via PgDb.
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
        // Load and enforce flow control from the workflow's flow_control_json
        let flow_enforcer = crate::flow_control::get_flow_control_enforcer();
        let flow_control: crate::flow_control::FlowControl = crate::flow_control::FlowControl::default();

        match flow_enforcer.check(&config.workflow_id, &flow_control).await {
            crate::flow_control::FlowControlDecision::Allow => {}
            crate::flow_control::FlowControlDecision::Skip { reason } => {
                warn!("Workflow {} skipped by flow control: {}", config.workflow_id, reason);
                return WorkflowResult {
                    success: false,
                    verification_passed: false,
                    step_results: all_step_results,
                    duration_ms: start.elapsed().as_millis() as u64,
                    loop_result: None,
                    worktree_path: config.worktree_path.clone(),
                    worktree_branch: config.worktree_branch.clone(),
                    workflow_architecture: config.workflow_architecture.clone(),
                    agentic_verification_config: config.agentic_verification_config.clone(),
                    multi_agent_pipeline_config: config.multi_agent_pipeline_config.clone(),
                };
            }
            crate::flow_control::FlowControlDecision::Wait { wait_ms, reason } => {
                info!(
                    "Workflow {} throttled: {} (waiting {}ms)",
                    config.workflow_id, reason, wait_ms
                );
                tokio::time::sleep(tokio::time::Duration::from_millis(wait_ms)).await;
            }
        }

        // Record flow control start for concurrency tracking (after admission)
        flow_enforcer.record_start(&config.workflow_id, None).await;

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

        // =====================================================================
        // CANARY ROLLOUT: Detection + Config Injection
        // =====================================================================
        // Check for active canary rollouts and probabilistically assign this run
        // as canary or baseline. Config overrides are applied here (shared across
        // all architectures); prompt overrides are applied per-architecture
        // (currently only multi-agent pipeline has named agent roles).
        let mut canary_config_originals: Vec<(String, Option<serde_json::Value>)> = Vec::new();
        {
            let active_canary: Option<(String, String)> =
                match crate::meta_optimizer::canary::get_active_canaries(&self.app_state.pg_db) {
                    Ok(canaries) => canaries.into_iter().find_map(|c| {
                        if crate::meta_optimizer::canary::should_apply_canary(
                            &self.app_state.pg_db,
                            &c.recommendation_id,
                        ) {
                            info!(
                                "CANARY: Rollout {} active for this run ({}%)",
                                c.id, c.percentage
                            );
                            Some((c.id, c.recommendation_id))
                        } else {
                            None
                        }
                    }),
                    Err(_) => None,
                };
            let is_canary_run = active_canary.is_some();

            // Apply config overrides for canary runs
            if let Some((_, ref rec_id)) = active_canary {
                match crate::meta_optimizer::canary::get_canary_config_overrides(
                    &self.app_state.pg_db,
                    rec_id,
                ) {
                    Ok(overrides) => {
                        for (key, value) in overrides {
                            let original = self.app_state.pg_db.get_setting(&key).await.ok().flatten();
                            canary_config_originals.push((key.clone(), original));
                            let set_result = self.app_state.pg_db.set_setting(&key, &value).await;
                            if let Err(e) = set_result {
                                warn!("CANARY: Failed to set config {}={}: {}", key, value, e);
                            } else {
                                info!("CANARY: Injecting config override {}={}", key, value);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("CANARY: Failed to load config overrides: {}", e);
                    }
                }
            }

            // Store canary state on config so loop paths can access it
            config.active_canary = active_canary;
            config.is_canary_run = is_canary_run;
        }

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
                Self::restore_canary_config(&canary_config_originals);
                flow_enforcer.record_end(&config.workflow_id, None).await;
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
                let sessions_task_run = self.app_state.pg_db.get_task_run(&config.execution_id).await;
                if let Ok(Some(task_run)) = sessions_task_run {
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
                        Self::restore_canary_config(&canary_config_originals);
                        flow_enforcer.record_end(&config.workflow_id, None).await;
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
            let _ = self.app_state.pg_db.append_task_output_ex(
                &config.execution_id,
                &stage_header,
                false,
                false,
            ).await;

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
                    let _ = self.app_state.pg_db.append_task_output_ex(
                        &config.execution_id,
                        &output,
                        false,
                        false,
                    ).await;
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
                        Self::restore_canary_config(&canary_config_originals);
                        flow_enforcer.record_end(&config.workflow_id, None).await;
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
                    enforce_token_budget: config.enforce_token_budget,
                    cross_workflow_learning: config.cross_workflow_learning,
                    verification_history: std::collections::HashMap::new(),
                    routing_context: Default::default(),
                    project_path: config.project_path.clone(),
                    acceptance_criteria: config.acceptance_criteria.clone(),
                    multi_agent_mode: config.multi_agent_mode,
                    strict_cwd: config.strict_cwd,
                    tool_tags: config.tool_tags.clone(),
                    use_worktree: false, // Worktree is handled at workflow level, not per-stage
                    worktree_path: config.worktree_path.clone(),
                    worktree_branch: config.worktree_branch.clone(),
                    workflow_architecture: config.workflow_architecture.clone(),
                    agentic_verification_config: config.agentic_verification_config.clone(),
                    multi_agent_pipeline_config: config.multi_agent_pipeline_config.clone(),
                    active_canary: config.active_canary.clone(),
                    is_canary_run: config.is_canary_run,
                    rollback_policy: config.rollback_policy.clone(),
                    escalation_policy: crate::unified_workflow_executor::blame::EscalationPolicy::default(),
                    iteration_diffs: Vec::new(),
                    phase_timeout_ms: config.phase_timeout_ms,
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
                        &config.execution_id,
                        agentic_iteration,
                        &self.app_state.pg_db,
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
                        let _ = self.app_state.pg_db.append_task_output_ex(
                            &config.execution_id,
                            &output_text,
                            true,
                            false,
                        ).await;
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
                        stage_loop_config.max_iterations.saturating_sub(1).max(1);

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
                    // State machine implementation — replaces the monolithic
                    // run_verification_agentic_loop (deleted in this commit).
                    self.run_loop_state_machine(
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

                // Record canary run outcome (all architectures)
                if let Some((ref canary_id, _)) = config.active_canary {
                    let stage_duration_ms = start.elapsed().as_millis() as f64;
                    let _ = crate::meta_optimizer::canary::record_canary_run(
                        &self.app_state.pg_db,
                        canary_id,
                        config.is_canary_run,
                        loop_result.verification_passed,
                        loop_result.total_cost_usd.unwrap_or(0.0),
                        stage_duration_ms,
                    );
                }

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
                    Self::restore_canary_config(&canary_config_originals);
                    flow_enforcer.record_end(&config.workflow_id, None).await;
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

            // Record verification result before completing (used by fixer trigger Guard 7)
            if let Some(ref lr) = last_loop_result {
                let vp_result = self.app_state.pg_db.set_verification_passed(
                    &config.execution_id,
                    lr.verification_passed,
                ).await;
                if let Err(e) = vp_result {
                    error!("Failed to set verification_passed for {}: {}", config.execution_id, e);
                }
            }

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
            let exec_id = config.execution_id.clone();
            let doctor_handle = self.doctor_handle.clone();
            let summary_model = config.resolve_model_for_phase("summary");
            let summary_provider = config.resolve_provider_for_phase("summary");
            tokio::spawn(async move {
                match generate_task_summary_async(
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
            let exec_id = config.execution_id.clone();
            let doctor_handle = self.doctor_handle.clone();
            let summary_model = config.resolve_model_for_phase("summary");
            let summary_provider = config.resolve_provider_for_phase("summary");
            tokio::spawn(async move {
                match generate_task_summary_async(
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
                files_modified: last_loop_result
                    .as_ref()
                    .map(|r| r.files_modified.clone())
                    .unwrap_or_default(),
                error_type: None,
                error_message: None,
                workflow_architecture: Some(architecture),
                step_count: Some(total_step_count as i64),
                verification_step_count: Some(total_verification_steps as i64),
                agentic_step_count: Some(total_agentic_steps as i64),
                has_ui_bridge,
                total_tokens: last_loop_result.as_ref().and_then(|r| r.total_tokens),
                total_cost_usd: last_loop_result.as_ref().and_then(|r| r.total_cost_usd),
                model_used: None, // Backfilled from iteration_logs by PG layer
            };
            // Capture fields for async LLM judge before moving outcome into spawn
            let judge_task_run_id = outcome.task_run_id.clone();
            let judge_iterations = outcome.iterations;
            let judge_verification_passed = outcome.verification_passed;
            let judge_was_stopped = outcome.was_stopped;

            // Capture fields for online learning before outcome is moved
            let ol_task_run_id = outcome.task_run_id.clone();
            let ol_status = outcome.status.clone();
            let ol_cost_usd = outcome.total_cost_usd;
            let ol_duration_secs = outcome.duration_secs;
            let ol_model_used = outcome.model_used.clone();
            let ol_has_ui_bridge = outcome.has_ui_bridge;
            let ol_workflow_architecture = outcome.workflow_architecture.clone();
            let ol_iterations = outcome.iterations;
            let ol_files_modified = outcome.files_modified.clone();
            let ol_workflow_name = outcome.workflow_name.clone();
            let ol_category = outcome.category.clone();

            // Build step reflections from iteration results for online learning
            let ol_step_reflections: Vec<crate::online_learning::reflection::StepReflection> =
                last_loop_result
                    .as_ref()
                    .map(|lr| {
                        lr.iteration_results
                            .iter()
                            .enumerate()
                            .map(|(i, iter_result)| {
                                crate::online_learning::reflection::StepReflection {
                                    step_type: if iter_result.agentic_phase_ran {
                                        "agentic_verification".to_string()
                                    } else {
                                        "standard_iteration".to_string()
                                    },
                                    step_index: i,
                                    task_run_id: outcome.task_run_id.clone(),
                                    duration_ms: 0, // Not tracked per-iteration in LoopResult
                                    success: iter_result.verification_passed,
                                    tool_calls: Vec::new(),
                                    error_type: if iter_result.critical_failure {
                                        Some("critical_failure".to_string())
                                    } else if !iter_result.verification_passed
                                        && iter_result.failed_checks > 0
                                    {
                                        Some("verification_failure".to_string())
                                    } else {
                                        None
                                    },
                                    retry_count: if iter_result.verification_passed {
                                        0
                                    } else {
                                        i as u32
                                    },
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();

            let pg_db_for_q = self.app_state.pg_db.clone();
            tokio::spawn(async move {
                // Record learning outcome + agentic metrics via spawn_blocking
                // SQLite: spawn_blocking-wrapped, removed with SQLite
                // PG-primary: record learning outcome directly
                let pg_learning_ok = pg_db_for_q.record_workflow_learning_from_outcome(&outcome).await;

                if let Err(ref e) = pg_learning_ok {
                    warn!("Failed to record learning outcome: {}", e);
                }

                // PG Q-routing update — only if PG recording succeeded
                if pg_learning_ok.is_ok() {
                    // PG: get_composite_agentic_score
                    let composite_score = pg_db_for_q
                        .get_composite_agentic_score(&outcome.task_run_id)
                        .await
                        .unwrap_or(0.0);

                    if let Err(e) = crate::orchestrator::learning_recorder::update_q_routing_table_pg(
                        &pg_db_for_q, &outcome, composite_score,
                    ).await {
                        warn!("Failed PG Q-routing update: {}", e);
                    }
                }
            });

            // Async LLM-as-judge: scores plan_adherence, goal_accuracy, plan_quality
            // Fire-and-forget — checks eligibility internally (skips zero-iter, simple successes)
            crate::orchestrator::learning_recorder::spawn_llm_judge_if_eligible(
                crate::database::CheckpointDb::global(),
                judge_task_run_id.clone(),
                judge_iterations,
                judge_verification_passed,
                judge_was_stopped,
            );

            // Async RAG judge: scores retrieval quality if this run used hybrid search
            crate::orchestrator::learning_recorder::spawn_rag_judge_if_eligible(
                crate::database::CheckpointDb::global(),
                judge_task_run_id,
            );

            // ── Online Learning: non-blocking post-run update ──
            // Runs drift detection, model routing bandit update, credit
            // assignment, experience reflection, and strategy stats.
            {
                use crate::online_learning::coordinator::{self, OnlineLearningOutcome};
                use crate::orchestrator::learning_recorder::{
                    infer_technology_tags, infer_domain_tags, compute_complexity_tier,
                };

                let tech_tags = infer_technology_tags(&ol_files_modified);
                let domain_tags = infer_domain_tags(
                    &ol_files_modified,
                    &ol_workflow_name,
                    &ol_category,
                    ol_has_ui_bridge,
                );
                let primary_domain = domain_tags
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "backend".to_string());
                let complexity = compute_complexity_tier(
                    Some(ol_files_modified.len() as i64),
                    ol_iterations,
                    ol_duration_secs,
                    None,
                );

                let ol_outcome = OnlineLearningOutcome {
                    task_run_id: ol_task_run_id,
                    status: ol_status,
                    composite_score: None,
                    cost_usd: ol_cost_usd,
                    duration_secs: Some(ol_duration_secs),
                    model_used: ol_model_used,
                    domain: primary_domain,
                    complexity_tier: complexity,
                    has_ui: ol_has_ui_bridge,
                    prompt_length: config.base_prompt.len(),
                    file_count: ol_files_modified.len(),
                    workflow_architecture: ol_workflow_architecture,
                    strategy_id: None,
                    step_attributions: Vec::new(), // Populated from pipeline traces when available
                    step_reflections: ol_step_reflections,
                    total_iterations: ol_iterations,
                    technology_tags: tech_tags,
                };

                let ol_pg = self.app_state.pg_db.clone();
                let ol_app_handle = self.app_handle.clone();
                tokio::spawn(async move {
                    // Brief delay to let the learning outcome + deterministic metrics record
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

                    // Fetch composite score from PG (may be available from deterministic metrics)
                    let mut ol_outcome = ol_outcome;
                    if ol_outcome.composite_score.is_none() {
                        if let Ok(score) = ol_pg.get_composite_agentic_score(&ol_outcome.task_run_id).await {
                            if score > 0.0 {
                                ol_outcome.composite_score = Some(score);
                            }
                        }
                    }

                    // Phase 1: Sync computation (in a block so guards are dropped before await)
                    let snapshot = {
                        let router_lock = crate::online_learning::model_router();
                        let drift_lock = crate::online_learning::drift_monitor();
                        match (router_lock, drift_lock) {
                            (Some(router_mtx), Some(drift_mtx)) => {
                                let mut router =
                                    router_mtx.lock().unwrap_or_else(|e| e.into_inner());
                                let mut drift =
                                    drift_mtx.lock().unwrap_or_else(|e| e.into_inner());
                                coordinator::post_run_online_learning(
                                    &ol_outcome,
                                    &mut router,
                                    &mut drift,
                                );
                                // Snapshot data for persistence (guards dropped at block end)
                                Some((router.table_snapshot(), drift.serialize_state().ok(), drift.take_pending_signals()))
                            }
                            _ => {
                                tracing::debug!(
                                    "Online learning not initialized — skipping"
                                );
                                None
                            }
                        }
                    }; // MutexGuards dropped here

                    // Phase 2: Async persistence to PG (no guards held)
                    if let Some((routing_rows, drift_state, drift_signals)) = snapshot {
                        let mut persisted = 0usize;

                        // Persist model routing Q-table
                        for row in &routing_rows {
                            if let Err(e) = ol_pg.upsert_model_routing_entry(
                                &row.context_key, &row.arm,
                                row.q_value, row.visit_count as i32, row.sum_of_squares,
                            ).await {
                                tracing::warn!("Failed to persist routing entry: {}", e);
                            } else {
                                persisted += 1;
                            }
                        }

                        // Persist drift signals and emit Tauri events
                        for signal in &drift_signals {
                            let drift_level = match signal.drift_level {
                                crate::meta_optimizer::drift_detection::DriftLevel::Warning => "warning",
                                crate::meta_optimizer::drift_detection::DriftLevel::Drift => "drift",
                                _ => continue,
                            };
                            let id = format!("{}:{}:{}:{}", signal.detector_type, signal.metric_name, signal.context_key, chrono::Utc::now().timestamp_millis());
                            let _ = ol_pg.insert_drift_signal(&id, &signal.detector_type, &signal.metric_name, &signal.context_key, drift_level, signal.pre_drift_mean, signal.post_drift_mean, signal.window_size as i64).await;
                            persisted += 1;

                            // Emit Tauri event for UI notification
                            let _ = ol_app_handle.emit(
                                crate::orchestrator::realtime_events::EVENT_DRIFT_DETECTED,
                                &serde_json::json!({
                                    "detector_type": signal.detector_type,
                                    "metric_name": signal.metric_name,
                                    "context_key": signal.context_key,
                                    "drift_level": drift_level,
                                    "pre_drift_mean": signal.pre_drift_mean,
                                    "post_drift_mean": signal.post_drift_mean,
                                }),
                            );
                        }

                        // Persist drift monitor state
                        if let Some(ref state_json) = drift_state {
                            let _ = ol_pg.save_drift_detector_state("global_monitor", "composite", state_json).await;
                        }

                        // Persist step credits
                        let credits = crate::online_learning::credit_assignment::compute_step_credits(
                            &ol_outcome.step_attributions,
                            ol_outcome.composite_score.unwrap_or(0.0),
                            ol_outcome.status == "success",
                        );
                        if !credits.is_empty() {
                            if let Ok(n) = ol_pg.save_step_credits(&credits, &ol_outcome.task_run_id).await {
                                persisted += n;
                            }
                        }

                        // Persist experience summary
                        let summary = crate::online_learning::reflection::summarize_experience(
                            &crate::online_learning::reflection::RunReflectionInput {
                                task_run_id: ol_outcome.task_run_id.clone(),
                                domain: ol_outcome.domain.clone(),
                                complexity_tier: ol_outcome.complexity_tier.clone(),
                                outcome_status: ol_outcome.status.clone(),
                                composite_score: ol_outcome.composite_score,
                                steps: ol_outcome.step_reflections.clone(),
                                total_iterations: ol_outcome.total_iterations,
                                total_cost_usd: ol_outcome.cost_usd,
                            },
                        );
                        let summary_id = format!("exp-{}", ol_outcome.task_run_id);
                        let kd = serde_json::to_string(&summary.key_decisions).unwrap_or_default();
                        let fp = serde_json::to_string(&summary.failure_points).unwrap_or_default();
                        let ep = serde_json::to_string(&summary.effective_patterns).unwrap_or_default();
                        if ol_pg.insert_experience_summary(&summary_id, &ol_outcome.task_run_id, &ol_outcome.domain, &ol_outcome.complexity_tier, &ol_outcome.status, &kd, &fp, &ep).await.is_ok() {
                            persisted += 1;
                        }

                        // Backfill model_used
                        if let Some(ref model) = ol_outcome.model_used {
                            let _ = ol_pg.update_learning_outcome_model(&ol_outcome.task_run_id, model).await;
                        }

                        // Strategy extraction: if this run scored > P90 for its context, create a candidate strategy
                        if ol_outcome.status == "success" || ol_outcome.status == "complete" {
                            if let Some(score) = ol_outcome.composite_score {
                                let p90 = ol_pg.get_score_percentile(
                                    &format!("{}:{}:{}", ol_outcome.domain, ol_outcome.complexity_tier, if ol_outcome.has_ui { "ui" } else { "no_ui" }),
                                    90,
                                ).await.unwrap_or(0.95);

                                if score > p90 && score > 0.7 {
                                    let strategy = crate::online_learning::strategy_evolution::extract_strategy_from_run(
                                        &crate::online_learning::strategy_evolution::HighPerformingRun {
                                            task_run_id: ol_outcome.task_run_id.clone(),
                                            domain: ol_outcome.domain.clone(),
                                            complexity_tier: ol_outcome.complexity_tier.clone(),
                                            technology_tags: ol_outcome.technology_tags.clone(),
                                            workflow_architecture: ol_outcome.workflow_architecture.clone().unwrap_or_default(),
                                            model_used: ol_outcome.model_used.clone(),
                                            composite_score: score,
                                            cost_usd: ol_outcome.cost_usd.unwrap_or(0.0),
                                            duration_secs: ol_outcome.duration_secs.unwrap_or(0.0),
                                        },
                                    );
                                    let app_json = serde_json::to_string(&strategy.applicability).unwrap_or_default();
                                    let comp_json = serde_json::to_string(&strategy.components).unwrap_or_default();
                                    let prov_json = serde_json::to_string(&strategy.provenance).unwrap_or_default();
                                    if let Err(e) = ol_pg.insert_strategy(
                                        &strategy.id, &strategy.name, &strategy.description,
                                        &app_json, &comp_json, &prov_json,
                                        "candidate", strategy.parent_strategy_id.as_deref(),
                                    ).await {
                                        tracing::debug!("Failed to insert strategy: {}", e);
                                    } else {
                                        tracing::info!("Extracted candidate strategy '{}' from P90+ run {}", strategy.name, ol_outcome.task_run_id);
                                        persisted += 1;
                                    }
                                }
                            }
                        }

                        if persisted > 0 {
                            tracing::debug!("Online learning persisted {} items for {}", persisted, ol_outcome.task_run_id);
                        }
                    }
                });
            }
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
            // Skip if verification passed — the fixer is only useful for failed runs.
            // The fixer's own should_launch_fixer() has Guard 7 for this too (belt-and-suspenders).
            if config.is_dev_mode && !overall_passed {
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

                // (Online learning block moved inside outcome scope above)
            }
        }

        // Cross-run analysis after reflection completes
        // When a reflection workflow finishes, run post_run_analysis to detect
        // recurring patterns, disable ineffective rules, and auto-apply fixes.
        if config.reflection_mode {
            let cra_task_run_id = config.execution_id.clone();
            let cra_workflow_name = config.workflow_name.clone();
            let cra_pg_db = self.app_state.pg_db.clone();
            tokio::spawn(async move {
                // Look up source workflow name from PG
                let source_wf_name = cra_pg_db
                    .get_source_workflow_name(&cra_task_run_id, &cra_workflow_name)
                    .await
                    .unwrap_or_else(|_| cra_workflow_name.clone());

                // PG-primary: cross-run analysis
                let cra_result = {
                    let swf = source_wf_name.clone();
                    let ctri = cra_task_run_id.clone();
                    match cra_pg_db.post_run_analysis(&swf, &ctri).await {
                        Ok((patterns, rules, fixes)) => Ok((patterns, rules, fixes, Vec::<crate::reflection::cross_run_learning::ExtractedSkill>::new())),
                        Err(e) => Err(e),
                    }
                };
                match cra_result {
                    Ok((patterns, rules, fixes, extracted_skills)) => {
                        info!(
                            "Cross-run analysis after reflection: {} patterns, {} rules disabled, {} fixes applied, {} skills extracted",
                            patterns, rules, fixes, extracted_skills.len()
                        );

                        // Emit skill.created events and mirror to PG observations
                        if !extracted_skills.is_empty() {
                            let bus = crate::workflow_event_bus::get_workflow_event_bus().clone();
                            let source_tr = cra_task_run_id.clone();
                            let wf_name = source_wf_name.clone();
                            let pg_for_mirror = cra_pg_db.clone();
                            tokio::spawn(async move {
                                for skill in &extracted_skills {
                                    // 1. Emit event on workflow bus
                                    bus.emit_workflow_event(
                                        crate::workflow_event_bus::events::SKILL_CREATED,
                                        &source_tr,
                                        None,
                                        Some(&wf_name),
                                        serde_json::json!({
                                            "skill_id": skill.skill_id,
                                            "skill_slug": skill.skill_slug,
                                            "source_fix_id": skill.source_fix_id,
                                            "source_task_run_id": skill.source_task_run_id,
                                        }),
                                    ).await;

                                    // 2. Mirror to PG observations for cross-session FTS
                                    {
                                        let pg = &pg_for_mirror;
                                        let obs_input = crate::database::types::CreateObservationInput {
                                            title: format!("Skill: {}", skill.skill_slug),
                                            content: format!(
                                                "Auto-extracted procedural skill '{}' from workflow '{}' (task_run: {}, fix: {})",
                                                skill.skill_slug, skill.workflow_name,
                                                skill.source_task_run_id, skill.source_fix_id
                                            ),
                                            observation_type: "solution".to_string(),
                                            scope: "project".to_string(),
                                            topic_key: Some(format!("skill:{}", skill.skill_slug)),
                                            project_id: None,
                                            workflow_id: None,
                                            task_run_id: Some(skill.source_task_run_id.clone()),
                                            session_id: None,
                                        };
                                        if let Err(e) = pg.save_observation(&obs_input).await {
                                            tracing::warn!(
                                                "Failed to mirror skill '{}' to PG observation: {}",
                                                skill.skill_slug, e
                                            );
                                        }
                                    }
                                }
                            });
                        }

                        // 3. Save detected patterns as observations
                        if patterns > 0 {
                            {
                                let pg = cra_pg_db.clone();
                                let wf = source_wf_name.clone();
                                let tr = cra_task_run_id.clone();
                                tokio::spawn(async move {
                                    let obs = crate::database::types::CreateObservationInput {
                                        title: format!("Recurring patterns in {}", wf),
                                        content: format!(
                                            "Cross-run analysis detected {} recurring pattern(s) and {} fix oscillation(s) \
                                             for workflow '{}'. {} rules auto-disabled, {} fixes auto-applied.",
                                            patterns, 0u32, wf, rules, fixes
                                        ),
                                        observation_type: "pattern".to_string(),
                                        scope: "project".to_string(),
                                        topic_key: Some(format!("cross-run/{}", wf.to_lowercase().replace(' ', "-"))),
                                        project_id: None,
                                            workflow_id: None,
                                            task_run_id: Some(tr),
                                            session_id: None,
                                        };
                                        if let Err(e) = pg.save_observation(&obs).await {
                                            tracing::warn!("Failed to save cross-run pattern observation: {}", e);
                                        }
                                    });
                                }
                            }

                    }
                    Err(e) => {
                        warn!("Cross-run analysis DB error: {}", e);
                    }
                }
            });
        }

        // Memory consolidation trigger (after every N completed task runs)
        // Runs in background with cooldown enforcement to prevent excessive consolidation.
        {
            let consolidation_pg = self.app_state.pg_db.clone();
            tokio::spawn(async move {
                let settings = crate::settings::load_settings();
                let config: crate::memory::consolidation::ConsolidationConfig = (&settings.memory_consolidation).into();
                if !crate::memory::consolidation::can_run_consolidation(&consolidation_pg, config.cooldown_hours).await {
                    return;
                }
                // Check if we've accumulated enough runs since last consolidation
                // (lightweight check — just count recent completed task runs)
                let count = consolidation_pg
                    .get_observations_for_consolidation(1)
                    .await
                    .map(|obs| obs.len())
                    .unwrap_or(0);
                if count == 0 {
                    return;
                }
                tracing::info!("Triggering background memory consolidation");
                match crate::memory::consolidation::run_consolidation(&consolidation_pg, &config).await {
                    Ok(stats) => {
                        tracing::info!(
                            "Memory consolidation complete: {} models created, {} archived",
                            stats.models_created, stats.observations_archived
                        );
                    }
                    Err(e) => {
                        tracing::warn!("Memory consolidation failed: {}", e);
                    }
                }
            });
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

        // Token totals aggregation removed — all persistence now via PgDb.

        // Restore canary config overrides
        Self::restore_canary_config(&canary_config_originals);

        // Record flow control end for concurrency tracking
        flow_enforcer.record_end(&config.workflow_id, None).await;

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

    /// Restore canary config overrides to their original values.
    fn restore_canary_config(
        _originals: &[(String, Option<serde_json::Value>)],
    ) {
        // Canary config restore removed — settings persistence now via PgDb.
    }

    /// Multi-agent fix: triage failures into groups, run specialized fix agents
    /// in dependency order with targeted verification between each.
    ///
    /// Returns Some((outcome, injected_steps)) on success, None if triage fails
    /// and we should fall back to the standard monolithic session.
    pub(super) async fn run_multi_agent_fix(
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
                config.resolve_model_for_phase("verification"),
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
                let _ = self.app_state.pg_db.append_task_output_ex(
                    &config.execution_id,
                    &output_text,
                    true,
                    false,
                ).await;
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
pub(super) use super::health_monitor::build_resume_agentic_context;
