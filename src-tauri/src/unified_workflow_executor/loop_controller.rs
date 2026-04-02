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
        todo!("SQLite removed")
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
        todo!("SQLite removed")
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
