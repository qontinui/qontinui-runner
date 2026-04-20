//! Startup resume logic for interrupted unified workflows.
//!
//! When the runner starts, this module checks for any workflows that were
//! interrupted (e.g., by a crash or restart) and either resumes them or
//! marks them as failed, depending on the `ResumeConfig`.

use std::sync::Arc;
use tauri::Manager;
use tracing::{error, info, warn};

use crate::config_storage::ConfigStorage;
use crate::unified_workflows::UnifiedWorkflowExt;
use crate::AppState;

use super::compensation::CompensationManager;
use super::loop_controller::LoopController;
use super::types::{LoopResult, RollbackPolicy};

// ---------------------------------------------------------------------------
// WorkflowResult
// ---------------------------------------------------------------------------

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
    pub workflow_architecture: Option<crate::agentic_verification::WorkflowArchitecture>,
    /// Agentic verification config (if agentic verification was used).
    pub agentic_verification_config: Option<crate::agentic_verification::AgenticVerificationConfig>,
    /// Multi-agent pipeline config (if multi-agent pipeline was used).
    pub multi_agent_pipeline_config: Option<crate::agentic_verification::MultiAgentPipelineConfig>,
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

// get_file_tree lives in multi_agent_pipeline_loop.rs where it is used

// ---------------------------------------------------------------------------
// evaluate_stage_condition
// ---------------------------------------------------------------------------

/// Evaluate whether a stage should be skipped based on its condition.
///
/// Returns `true` if the stage should be **skipped** (condition not met).
/// All condition fields combine with AND semantics: if any condition
/// is not satisfied, the stage is skipped.
///
/// # Arguments
/// - `condition` - The stage condition to evaluate
/// - `previous_passed` - Whether the previous stage's verification passed
/// - `total_iterations` - Total iterations across all completed stages so far
/// - `total_failures` - Number of stages that failed verification so far
pub(crate) fn evaluate_stage_condition(
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

// ---------------------------------------------------------------------------
// ResumeConfig
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Stale task detection
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// resume_interrupted_workflows
// ---------------------------------------------------------------------------

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
    app_state: Arc<AppState>,
    config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
    app_handle: tauri::AppHandle,
    pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    config: ResumeConfig,
) -> usize {
    // Each runner instance only resumes tasks tagged with its own runner_port.
    // Different runners bind different ports, so this is the natural per-runner
    // exclusion: two restarting runners can't double-claim the same task.
    let my_port = app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);

    let running_workflows: Vec<crate::database::types::TaskRun> = match app_state
        .pg_db
        .get_resumable_task_runs_for_runner(my_port)
        .await
    {
        Ok(rows) => rows
            .into_iter()
            // AI chat sessions are resumed by resume_ai_sessions, not here.
            .filter(|r| r.workflow_type.as_deref() != Some("chat"))
            .collect(),
        Err(e) => {
            error!("Failed to query resumable task runs: {}", e);
            Vec::new()
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
        // Skip Restate-managed workflows — Restate handles their resume automatically
        if crate::settings::load_settings().restate.enabled {
            if app_state
                .pg_db
                .is_restate_workflow(&task_run.id)
                .await
                .unwrap_or(false)
            {
                info!(
                    "Skipping Restate-managed workflow {} — Restate handles resume",
                    task_run.id
                );
                continue;
            }
        }

        // Check if the workflow state is actually complete but the task_run status
        // wasn't updated (e.g., runner crashed after workflow finished but before
        // the status was persisted).
        if let Ok(Some(wf_state)) = app_state
            .pg_db
            .get_workflow_execution_state(&task_run.id)
            .await
        {
            if wf_state.state_name.contains("complete") || wf_state.state_name.contains("finished")
            {
                info!(
                    "Workflow '{}' (id: {}) state is '{}' — marking task_run as completed (stale running status)",
                    task_run.task_name, task_run.id, wf_state.state_name
                );
                if let Err(e) = app_state.pg_db.complete_task_run(&task_run.id).await {
                    error!(
                        "Failed to mark task {} as completed in PG: {}",
                        task_run.id, e
                    );
                }
                processed_count += 1;
                continue;
            }
        }

        // ── Startup auto-compensation ────────────────────────────────
        //
        // Per plan §1.3 option-b: if the crashed run left any unexecuted
        // compensation rows in `runner.compensation_actions`, run them now —
        // regardless of `auto_continue`. Compensation is cleanup (undoing
        // side effects), not resumption, so it applies to ALL crashed runs.
        //
        // The presence of unexecuted rows is the signal that there was
        // pending work. If `load_pending_compensation_actions` returns an
        // empty Vec, the workflow was running cleanly when the runner
        // restarted — skip this block entirely, no log noise.
        let compensation_suffix: String = match app_state
            .pg_db
            .load_pending_compensation_actions(&task_run.id)
            .await
        {
            Ok(rows) if rows.is_empty() => String::new(),
            Ok(rows) => {
                let total = rows.len();
                info!(
                    "Startup compensation: workflow '{}' (id: {}) has {} pending action(s), executing LIFO before resume/fail decision",
                    task_run.task_name, task_run.id, total
                );
                let mgr = CompensationManager::new(app_state.pg_db.clone());
                match mgr.load_pending(&task_run.id).await {
                    Ok(()) => {
                        // RollbackPolicy::Clean runs the full LIFO without
                        // needing in-memory iteration context. We pass an
                        // empty IterationResult slice — this is startup
                        // recovery, the previous run's context is gone.
                        let results = mgr.execute_all(&RollbackPolicy::Clean, &[]).await;
                        let attempted = results.len();
                        let succeeded = results.iter().filter(|r| r.success).count();
                        let failed = attempted - succeeded;
                        info!(
                            "Startup compensation complete for '{}' (id: {}): {}/{} succeeded, {} failed (total pending: {})",
                            task_run.task_name, task_run.id, succeeded, attempted, failed, total
                        );
                        format!(" [auto-compensated: {}/{} succeeded]", succeeded, attempted)
                    }
                    Err(e) => {
                        error!(
                            "Startup compensation: load_pending failed for task {}: {} — falling through without compensation",
                            task_run.id, e
                        );
                        format!(" [auto-compensation failed: {}]", e)
                    }
                }
            }
            Err(e) => {
                error!(
                    "Startup compensation: could not query pending actions for task {}: {} — falling through without compensation",
                    task_run.id, e
                );
                String::new()
            }
        };

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
            let reason = format!(
                "Interrupted by app restart ({} workflow — not resumable){}",
                kind, compensation_suffix
            );
            if let Err(e) = app_state.pg_db.fail_task_run(&task_run.id, &reason).await {
                error!(
                    "Failed to mark task {} as stopped in PG: {}",
                    task_run.id, e
                );
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
            let reason_str = format!(
                "Interrupted workflow marked as failed ({}){}",
                reason, compensation_suffix
            );
            if let Err(e) = app_state
                .pg_db
                .fail_task_run(&task_run.id, &reason_str)
                .await
            {
                error!("Failed to mark workflow {} as failed: {}", task_run.id, e);
            } else {
                processed_count += 1;
            }
            continue;
        }

        // Resume path: compensation has already run (LIFO-undone the crashed
        // iteration's side effects). If any actions were pending and we
        // executed them, log that fact; the spawn below then resumes from
        // the iteration BEFORE those compensations.
        if !compensation_suffix.is_empty() {
            info!(
                "Workflow '{}' (id: {}) resuming after startup compensation:{}",
                task_run.task_name, task_run.id, compensation_suffix
            );
        }

        // Extract workflow ID from task_run.id (format: unified-workflow-{uuid}-{timestamp})
        let workflow_id = extract_workflow_id_from_task_id(&task_run.id);

        if let Some(wf_id) = workflow_id {
            info!(
                "Resuming interrupted workflow '{}' (task_id: {}, workflow_id: {})",
                task_run.task_name, task_run.id, wf_id
            );

            // Fetch the workflow definition
            match app_state.pg_db.get_unified_workflow(&wf_id).await {
                Ok(Some(workflow)) => {
                    // Spawn the workflow resume in a background task with panic protection
                    let task_id = task_run.id.clone();
                    let task_name = task_run.task_name.clone();
                    let starting_iteration = task_run.sessions_count; // Resume from after completed iterations
                                                                      // Capture values needed inside the async block
                    let app_state_for_spawn = app_state.clone();
                    let config_storage_for_spawn = config_storage.clone();
                    let app_handle_for_spawn = app_handle.clone();
                    let pid_tracker_for_spawn = pid_tracker.clone();
                    let wf_id_for_spawn = wf_id.clone();

                    let url_lock = Some(app_state.url_lock_manager.clone());
                    let file_registry = Some(app_state.file_registry_manager.clone());
                    let file_lock = Some(app_state.file_lock_manager.clone());
                    super::spawn_workflow_with_panic_guard(
                        task_id.clone(),
                        task_name.clone(),
                        url_lock,
                        file_registry,
                        file_lock,
                        app_state.pg_db.clone(),
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
                                max_iterations: workflow.iter_cap(),
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
                                max_sessions: workflow.max_iterations,
                                auto_run_generated: false,
                                approval_gate: workflow.approval_gate,
                                blocking_approval: false,
                                confidence_threshold: 0.85,
                                max_context_tokens: 100_000,
                                enforce_token_budget: workflow.enforce_token_budget,
                                cross_workflow_learning: true,
                                verification_history: std::collections::HashMap::new(),
                                routing_context: Default::default(),
                                project_path: crate::mcp::shared::current_project_path(),
                                multi_agent_mode: false,
                                strict_cwd: false,
                                tool_tags: Vec::new(),
                                use_worktree: false,
                                worktree_path: None,
                                worktree_branch: None,
                                workflow_architecture: None,
                                agentic_verification_config: None,
                                multi_agent_pipeline_config: None,

                                acceptance_criteria: workflow.acceptance_criteria.clone(),
                                rollback_policy:
                                    crate::unified_workflow_executor::RollbackPolicy::None,
                                escalation_policy: crate::unified_workflow_executor::blame::EscalationPolicy::default(),
                                iteration_diffs: Vec::new(),
                                active_canary: None,
                                is_canary_run: false,
                                phase_timeout_ms: None,
                                max_fix_attempts: 3,
                                max_ci_auto_resumes: 10,
                                ci_failure_context: None,
                                htn_config: crate::planning_bridge::HtnConfig::default(),
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
                        let reason = format!("Workflow definition '{}' not found and task exceeded stale timeout ({}s)", wf_id, STALE_RUNNING_TASK_TIMEOUT_SECS);
                        if let Err(e) = app_state.pg_db.fail_task_run(&task_run.id, &reason).await {
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
                        let reason2 = format!("Failed to fetch workflow definition and task exceeded stale timeout ({}s): {}", STALE_RUNNING_TASK_TIMEOUT_SECS, e);
                        if let Err(e2) = app_state.pg_db.fail_task_run(&task_run.id, &reason2).await
                        {
                            error!(
                                "Failed to mark stale task {} as failed: {}",
                                task_run.id, e2
                            );
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

            let workflow_result = async {
                match app_state.pg_db.get_unified_workflow_by_name(wf_name).await {
                    Ok(Some(wf)) => Ok(Some(wf)),
                    Ok(None) => {
                        // Try stripping reflection prefixes
                        let stripped = wf_name
                            .strip_prefix("Project Reflection: ")
                            .or_else(|| wf_name.strip_prefix("Reflection: "));
                        if let Some(name) = stripped {
                            info!("Retrying workflow lookup with stripped name: '{}'", name);
                            app_state.pg_db.get_unified_workflow_by_name(name).await
                        } else {
                            Ok(None)
                        }
                    }
                    Err(e) => Err(e),
                }
            }
            .await;

            match workflow_result {
                Ok(Some(workflow)) => {
                    // Spawn the workflow resume in a background task with panic protection
                    let task_id = task_run.id.clone();
                    let task_name = task_run.task_name.clone();
                    let wf_id = workflow.id.clone();
                    let starting_iteration = task_run.sessions_count;
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
                    let file_registry2 = Some(app_state.file_registry_manager.clone());
                    let file_lock2 = Some(app_state.file_lock_manager.clone());
                    let pg_db_for_spawn = app_state.pg_db.clone();
                    super::spawn_workflow_with_panic_guard(
                        task_id.clone(),
                        task_name.clone(),
                        url_lock2,
                        file_registry2,
                        file_lock2,
                        pg_db_for_spawn,
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
                                max_iterations: workflow.iter_cap(),
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
                                max_sessions: workflow.max_iterations,
                                auto_run_generated: false,
                                approval_gate: workflow.approval_gate,
                                blocking_approval: false,
                                confidence_threshold: 0.85,
                                max_context_tokens: 100_000,
                                enforce_token_budget: workflow.enforce_token_budget,
                                cross_workflow_learning: true,
                                verification_history: std::collections::HashMap::new(),
                                routing_context: Default::default(),
                                multi_agent_mode: false,
                                strict_cwd: false,
                                tool_tags: Vec::new(),
                                use_worktree: false,
                                worktree_path: None,
                                worktree_branch: None,
                                workflow_architecture: None,
                                agentic_verification_config: None,
                                multi_agent_pipeline_config: None,

                                project_path: crate::mcp::shared::current_project_path(),
                                acceptance_criteria: workflow.acceptance_criteria.clone(),
                                rollback_policy:
                                    crate::unified_workflow_executor::RollbackPolicy::None,
                                escalation_policy: crate::unified_workflow_executor::blame::EscalationPolicy::default(),
                                iteration_diffs: Vec::new(),
                                active_canary: None,
                                is_canary_run: false,
                                phase_timeout_ms: None,
                                max_fix_attempts: 3,
                                max_ci_auto_resumes: 10,
                                ci_failure_context: None,
                                htn_config: crate::planning_bridge::HtnConfig::default(),
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
                        let reason = format!("Workflow definition '{}' not found and task exceeded stale timeout ({}s)", wf_name, STALE_RUNNING_TASK_TIMEOUT_SECS);
                        if let Err(e) = app_state.pg_db.fail_task_run(&task_run.id, &reason).await {
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
                        let reason = format!("Failed to look up workflow '{}' and task exceeded stale timeout ({}s): {}", wf_name, STALE_RUNNING_TASK_TIMEOUT_SECS, e);
                        if let Err(e2) = app_state.pg_db.fail_task_run(&task_run.id, &reason).await
                        {
                            error!(
                                "Failed to mark stale task {} as failed: {}",
                                task_run.id, e2
                            );
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
            let reason = format!(
                "No workflow definition could be resolved and task exceeded stale timeout ({}s)",
                STALE_RUNNING_TASK_TIMEOUT_SECS
            );
            if let Err(e) = app_state.pg_db.fail_task_run(&task_run.id, &reason).await {
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

// ---------------------------------------------------------------------------
// extract_workflow_id_from_task_id
// ---------------------------------------------------------------------------

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
