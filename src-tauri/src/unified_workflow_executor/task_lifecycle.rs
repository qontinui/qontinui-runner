//! Task lifecycle operations for the loop controller.
//!
//! Handles task completion, failure, error resolution, chain triggers, and worktree creation.

use std::sync::Arc;
use tracing::{error, info, warn};

use crate::event_system::EventBroadcaster;
use crate::step_executor::ExecutionStepConfig;
use crate::workflow_event_bus::{events, get_workflow_event_bus};

use super::loop_controller::LoopController;
use super::types::LoopConfig;

impl LoopController {
    /// Mark a task as completed, sync to backend, promote workflow, store convergence snapshot,
    /// parse meta-optimizer recommendations, and check chain triggers.
    pub(crate) async fn mark_task_completed(&self, execution_id: &str, workflow_id: Option<&str>) {
        // Check for blocking incomplete children before completing
        match self
            .app_state
            .pg_db
            .has_blocking_incomplete_children(execution_id)
            .await
        {
            Ok(true) => {
                info!(
                    "Task {} has blocking incomplete children, deferring completion",
                    execution_id
                );
                // Don't complete yet -- the child completion handler will advance the parent
                return;
            }
            Ok(false) => {} // No blocking children, proceed with completion
            Err(e) => {
                warn!(
                    "Failed to check blocking children for task {}: {}, proceeding with completion",
                    execution_id, e
                );
            }
        }

        // If this task is a review subtask, process its outcome before writing status.
        // The outcome may redirect to mark_task_failed instead of completing.
        if let Ok(Some(tr)) = self.app_state.pg_db.get_task_run(execution_id).await {
            if tr.is_review {
                let parent_id = tr.parent_task_run_id.as_deref().unwrap_or("");
                if !parent_id.is_empty() {
                    let should_complete = super::review_subtask::process_review_outcome(
                        &self.app_state.pg_db,
                        execution_id,
                        parent_id,
                    )
                    .await;
                    if !should_complete {
                        info!(
                            "Review {} requested changes, marking as failed instead of completed",
                            execution_id
                        );
                        self.mark_task_failed(execution_id, "changes_requested", workflow_id)
                            .await;
                        return;
                    }
                }
            }
        }

        // Release advisory file registry entries on completion
        self.app_state
            .file_registry_manager
            .release_all(execution_id)
            .await;

        // Evict cached working representation for this task run
        self.app_state
            .working_representation_cache
            .evict(execution_id)
            .await;

        if let Err(e) = self.app_state.pg_db.complete_task_run(execution_id).await {
            error!(
                "Failed to mark task {} as completed (PG): {}",
                execution_id, e
            );
        } else {
            info!("Marked task {} as COMPLETED", execution_id);
            // Broadcast task-run-update to both Tauri + WebSocket
            let broadcaster = EventBroadcaster::new(self.app_handle.clone());
            broadcaster.task_run_update(execution_id, "completed", None, None);

            // Sync completion to web backend (best-effort, non-blocking)
            let pg = self.app_state.pg_db.clone();
            let eid = execution_id.to_string();
            tokio::spawn(async move {
                let sync_service = crate::commands::task_sync::AITaskSyncService::new();
                let task = pg.get_task_run(&eid).await.ok().flatten();
                if let Some(task) = task {
                    if let Err(e) = sync_service.sync_task_completed(&task).await {
                        warn!("Failed to sync task completion to backend: {}", e);
                    }
                }
                // Also sync deferred questions at completion
                if let Ok(questions) = pg.get_deferred_questions_for_task_run(&eid).await {
                    if !questions.is_empty() {
                        let sync_questions: Vec<_> = questions
                            .iter()
                            .map(crate::commands::task_sync::json_to_deferred_question_sync)
                            .collect();
                        if let Err(e) = sync_service
                            .sync_deferred_questions(&eid, sync_questions)
                            .await
                        {
                            warn!("Failed to sync deferred questions at completion: {}", e);
                        }
                    }
                }
            });

            // Fire-and-forget: try to promote workflow to example library
            if let Some(wf_id) = workflow_id {
                let wf_id = wf_id.to_string();
                // PG-primary: async context, fire-and-forget
                let pg = self.app_state.pg_db.clone();
                tokio::spawn(async move {
                    if let Err(e) = pg.try_promote_on_success(&wf_id).await {
                        tracing::warn!("Failed to promote workflow to examples: {}", e);
                    }
                });
            }

            // Auto-store convergence snapshot on completion via PG
            let pg2 = self.app_state.pg_db.clone();
            let exec_id2 = execution_id.to_string();
            tokio::spawn(async move {
                // PG-primary: async context
                let wf_name = pg2.get_workflow_name_for_task_run(&exec_id2).await.ok();
                if let Some(wf_name) = wf_name {
                    if let Ok(metrics) = pg2.compute_convergence_score(&wf_name, "workflow").await {
                        let _ = pg2
                            .store_convergence_snapshot(&wf_name, None, "workflow", &metrics)
                            .await;
                    }
                }
            });

            // Auto-capture task knowledge as observations (Engram-style cross-session memory)
            {
                let pg = self.app_state.pg_db.clone();
                let exec_id = execution_id.to_string();
                tokio::spawn(async move {
                    auto_capture_observations(&pg, &exec_id).await;
                });
            }

            // Parse meta-optimizer recommendations from completed output
            {
                let pg = self.app_state.pg_db.clone();
                let eid = execution_id.to_string();
                tokio::spawn(async move {
                    // Check if this task run is a meta-optimizer run
                    let task_run = match pg.get_task_run(&eid).await {
                        Ok(Some(tr)) => tr,
                        _ => return,
                    };
                    if !task_run.is_meta_optimizer {
                        return;
                    }

                    // Look up the optimizer_run record by task_run_id (PG)
                    let (optimizer_run_id, optimizer_type) =
                        match pg.get_optimizer_run_by_task_run_id(&eid).await {
                            Ok(Some((id, ot))) => (id, ot),
                            Ok(None) => {
                                warn!(
                                    "Could not find optimizer run for meta-optimizer task {}",
                                    eid
                                );
                                return;
                            }
                            Err(e) => {
                                warn!(
                                    "Could not find optimizer run for meta-optimizer task {}: {}",
                                    eid, e
                                );
                                return;
                            }
                        };

                    // Parse recommendations from the output
                    let output = &task_run.output_log;
                    match crate::meta_optimizer::parser::save_parsed_recommendations(
                        &pg,
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
                                    &pg,
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

            // Chain triggers are now handled reactively via the TriggerService's
            // event bus subscription (see trigger_system/service.rs start()).
            // The emit below fires the event that the bridge picks up.

            // Emit workflow.completed event on the event bus
            let bus = get_workflow_event_bus().clone();
            let eid = execution_id.to_string();
            let wf_id = workflow_id.map(|s| s.to_string());
            tokio::spawn(async move {
                bus.emit_workflow_event(
                    events::WORKFLOW_COMPLETED,
                    &eid,
                    wf_id.as_deref(),
                    None,
                    serde_json::json!({ "task_run_id": eid }),
                )
                .await;
            });

            // ── Online Learning: non-blocking post-run update ──
            // Spawns async pipeline: enrich from traces → drift detection →
            // bandit update → credit assignment → experience reflection → persist to PG.
            {
                let ol_pg = self.app_state.pg_db.clone();
                let ol_app_handle = self.app_handle.clone();
                let ol_task_run_id = execution_id.to_string();
                tokio::spawn(async move {
                    crate::online_learning::run_post_completion(
                        &ol_pg,
                        &ol_app_handle,
                        &ol_task_run_id,
                    )
                    .await;
                });
            }

            // ── Parent advancement: if this was a blocking child, check if parent can complete ──
            {
                let pg = self.app_state.pg_db.clone();
                let eid = execution_id.to_string();
                let app_handle = self.app_handle.clone();
                tokio::spawn(async move {
                    match pg.get_blocking_parent_info(&eid).await {
                        Ok(Some((parent_id, true))) => {
                            // This was a blocking child — check if parent can now complete
                            match pg.has_blocking_incomplete_children(&parent_id).await {
                                Ok(false) => {
                                    info!(
                                        "All blocking children of {} completed, advancing parent to complete",
                                        parent_id
                                    );
                                    if let Err(e) = pg.complete_task_run(&parent_id).await {
                                        warn!(
                                            "Failed to complete parent task {}: {}",
                                            parent_id, e
                                        );
                                    } else {
                                        // Emit task-run-update event for the parent
                                        let broadcaster = EventBroadcaster::new(app_handle);
                                        broadcaster.task_run_update(
                                            &parent_id,
                                            "completed",
                                            None,
                                            None,
                                        );
                                    }
                                }
                                Ok(true) => {
                                    tracing::debug!(
                                        "Parent {} still has other blocking children, not advancing",
                                        parent_id
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to check blocking children for parent {}: {}",
                                        parent_id, e
                                    );
                                }
                            }
                        }
                        Ok(Some((_, false))) | Ok(None) => {
                            // Not a blocking child, nothing to do
                        }
                        Err(e) => {
                            warn!("Failed to get blocking parent info for {}: {}", eid, e);
                        }
                    }
                });
            }

            // ── Ticket system bidirectional sync (Phase 5B) ──
            {
                let pg_clone = self.app_state.pg_db.clone();
                let task_id = execution_id.to_string();
                tokio::spawn(async move {
                    if let Err(e) =
                        crate::ticket_system::service::on_task_completed(&pg_clone, &task_id, true)
                            .await
                    {
                        tracing::debug!("Ticket sync on-completion hook failed: {}", e);
                    }
                });
            }
        }
    }

    /// Mark a task as failed, sync to backend, and check chain triggers.
    pub(crate) async fn mark_task_failed(
        &self,
        execution_id: &str,
        reason: &str,
        workflow_id: Option<&str>,
    ) {
        // Release advisory file registry entries on failure
        self.app_state
            .file_registry_manager
            .release_all(execution_id)
            .await;

        // Evict cached working representation for this task run
        self.app_state
            .working_representation_cache
            .evict(execution_id)
            .await;

        if let Err(e) = self
            .app_state
            .pg_db
            .fail_task_run(execution_id, reason)
            .await
        {
            error!("Failed to mark task {} as failed (PG): {}", execution_id, e);
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
            let pg = self.app_state.pg_db.clone();
            let eid = execution_id.to_string();
            tokio::spawn(async move {
                let sync_service = crate::commands::task_sync::AITaskSyncService::new();
                let task = pg.get_task_run(&eid).await.ok().flatten();
                if let Some(task) = task {
                    if let Err(e) = sync_service.sync_task_completed(&task).await {
                        warn!("Failed to sync task failure to backend: {}", e);
                    }
                }
            });

            // Chain triggers are now handled reactively via the TriggerService's
            // event bus subscription (see trigger_system/service.rs start()).
            // The emit below fires the event that the bridge picks up.

            // Emit workflow.failed event on the event bus
            let bus = get_workflow_event_bus().clone();
            let eid = execution_id.to_string();
            let wf_id = workflow_id.map(|s| s.to_string());
            let fail_reason = reason.to_string();
            tokio::spawn(async move {
                bus.emit_workflow_event(
                    events::WORKFLOW_FAILED,
                    &eid,
                    wf_id.as_deref(),
                    None,
                    serde_json::json!({ "task_run_id": eid, "reason": fail_reason }),
                )
                .await;
            });

            // ── Online Learning: also learn from failures ──
            {
                let ol_pg = self.app_state.pg_db.clone();
                let ol_app_handle = self.app_handle.clone();
                let ol_task_run_id = execution_id.to_string();
                tokio::spawn(async move {
                    crate::online_learning::run_post_completion(
                        &ol_pg,
                        &ol_app_handle,
                        &ol_task_run_id,
                    )
                    .await;
                });
            }

            // ── Ticket system bidirectional sync (Phase 5B) ──
            {
                let pg_clone = self.app_state.pg_db.clone();
                let task_id = execution_id.to_string();
                tokio::spawn(async move {
                    if let Err(e) =
                        crate::ticket_system::service::on_task_completed(&pg_clone, &task_id, false)
                            .await
                    {
                        tracing::debug!("Ticket sync on-failure hook failed: {}", e);
                    }
                });
            }

            // ── Parent advancement: failed blocking children also unblock parents ──
            // A failed review (or any blocking child) should let the parent proceed
            // rather than leaving it permanently blocked.
            {
                let pg = self.app_state.pg_db.clone();
                let eid = execution_id.to_string();
                let app_handle = self.app_handle.clone();
                tokio::spawn(async move {
                    match pg.get_blocking_parent_info(&eid).await {
                        Ok(Some((parent_id, true))) => {
                            match pg.has_blocking_incomplete_children(&parent_id).await {
                                Ok(false) => {
                                    info!(
                                        "All blocking children of {} resolved (including failures), advancing parent",
                                        parent_id
                                    );
                                    if let Err(e) = pg.complete_task_run(&parent_id).await {
                                        warn!(
                                            "Failed to complete parent task {}: {}",
                                            parent_id, e
                                        );
                                    } else {
                                        let broadcaster = EventBroadcaster::new(app_handle);
                                        broadcaster.task_run_update(
                                            &parent_id,
                                            "completed",
                                            None,
                                            None,
                                        );
                                    }
                                }
                                Ok(true) => {
                                    tracing::debug!(
                                        "Parent {} still has other blocking children after child {} failed",
                                        parent_id, eid
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to check blocking children for parent {}: {}",
                                        parent_id, e
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                });
            }
        }
    }

    /// Fire-and-forget: check if any workflow chain triggers match this completion.
    ///
    /// NOTE: Chain triggers are now handled reactively via the TriggerService's
    /// event bus subscription. This method is retained for manual/fallback use.
    #[allow(dead_code)]
    pub(crate) async fn check_chain_triggers(
        &self,
        execution_id: &str,
        workflow_id: &str,
        status: &str,
    ) {
        let service = crate::trigger_system::get_trigger_service().await;
        if let Some(service) = service {
            let tx = service.event_sender();
            let pg_db = self.app_state.pg_db.clone();
            let wf_id = workflow_id.to_string();
            let exec_id = execution_id.to_string();
            let status = status.to_string();
            tokio::spawn(async move {
                crate::trigger_system::watchers::workflow_chain::check_workflow_chains(
                    &pg_db, &tx, &wf_id, &exec_id, &status, None,
                )
                .await;
            });
        }
    }

    /// Resolve targeted errors after successful workflow completion.
    ///
    /// This marks all errors that were targeted by the workflow as resolved,
    /// recording the task_run_id (execution_id) that fixed them for traceability.
    pub(crate) async fn resolve_targeted_errors(&self, execution_id: &str, error_ids: &[i64]) {
        info!(
            "Resolving {} targeted errors for successful workflow {}",
            error_ids.len(),
            execution_id
        );

        // PG: mark_resolved_by_task
        let mut resolved_count = 0;
        let mut failed_count = 0;

        for error_id in error_ids {
            let resolution_note = format!(
                "Auto-resolved by successful completion of workflow task {}",
                execution_id
            );

            match self
                .app_state
                .pg_db
                .mark_resolved_by_task(*error_id, execution_id, Some(&resolution_note))
                .await
            {
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

    /// Resolve all errors captured during this workflow run on successful completion.
    ///
    /// This bulk-resolves errors scoped to the execution_id, reducing noise from
    /// errors that the workflow already handled. Placed after targeted resolution
    /// so those get their specific notes first; already-resolved errors won't be
    /// double-processed by the WHERE clause.
    pub(crate) async fn resolve_workflow_scoped_errors(&self, execution_id: &str) {
        // PG: resolve_errors_by_task_run
        match self
            .app_state
            .pg_db
            .resolve_errors_by_task_run(execution_id, execution_id)
            .await
        {
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

    /// Fallback: create a single-repo worktree (original behavior).
    /// Used when the monorepo root cannot be determined or multi-repo creation fails.
    pub(crate) fn create_single_repo_worktree(
        config: &mut LoopConfig,
        setup_automation_steps: &mut Vec<ExecutionStepConfig>,
        setup_prompt_steps: &mut Vec<ExecutionStepConfig>,
        verification_steps: &mut Vec<ExecutionStepConfig>,
        agentic_steps: &mut Vec<ExecutionStepConfig>,
        completion_automation_steps: &mut Vec<ExecutionStepConfig>,
        completion_prompt_steps: &mut Vec<ExecutionStepConfig>,
        pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
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
                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        let pg = pg_db.clone();
                        let record_clone = record.clone();
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            tokio::task::block_in_place(|| {
                                handle.block_on(async move {
                                    if let Err(e) = pg.insert_worktree(&record_clone).await {
                                        warn!("PG insert_worktree failed: {}", e);
                                    }
                                })
                            })
                        }));
                    }
                    // PG insert_worktree already done above; SQLite duplicate removed.

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

    /// Phase C: Auto-commit files touched by the sub-agent on successful
    /// terminal state.
    ///
    /// Reads the file-set from `session_touched_files` (populated by the
    /// dispatcher's auto-register on every Edit/Write tool call), filters by
    /// the workflow's `auto_commit_subagents` policy, and invokes the
    /// per-cwd-serialized [`auto_commit::commit_files`] helper against the
    /// worktree path.
    ///
    /// Policy semantics for `auto_commit_subagents`:
    /// - `Some(true)`  — commit after success regardless of worktree state.
    /// - `Some(false)` — never commit, even with a worktree.
    /// - `None`        — commit if and only if the workflow uses a worktree.
    ///
    /// This is **best-effort durability**: any failure is logged at `warn!`
    /// and swallowed. Workflow success > commit success. The tracker is
    /// cleared on `Committed` and `NothingToCommit` outcomes; on `Err` it is
    /// preserved so a future inspect/retry path can see the file-set.
    pub(crate) async fn auto_commit_on_success(&self, config: &LoopConfig) {
        // Resolve the policy:
        //   None      → commit iff use_worktree
        //   Some(true)→ always commit
        //   Some(false)→ never commit
        let should_commit =
            resolve_auto_commit_policy(config.auto_commit_subagents, config.use_worktree);

        if config.auto_commit_subagents == Some(false) {
            tracing::debug!(
                workflow_id = %config.workflow_id,
                execution_id = %config.execution_id,
                "auto_commit_on_success: explicit opt-out, skipping"
            );
            return;
        }

        if !should_commit {
            tracing::debug!(
                workflow_id = %config.workflow_id,
                execution_id = %config.execution_id,
                use_worktree = config.use_worktree,
                "auto_commit_on_success: not a worktree workflow and no explicit opt-in, skipping"
            );
            return;
        }

        // Determine the cwd for the commit. Inside a worktree, prefer
        // `worktree_path`; otherwise fall back to `project_path`. If neither is
        // set, we cannot proceed.
        let cwd_str = match config
            .worktree_path
            .as_deref()
            .or(config.project_path.as_deref())
        {
            Some(p) => p,
            None => {
                tracing::warn!(
                    execution_id = %config.execution_id,
                    "auto_commit_on_success: no worktree_path or project_path; skipping"
                );
                return;
            }
        };
        let cwd = std::path::PathBuf::from(cwd_str);

        // Pull the file-set the dispatcher recorded for this task_run_id.
        let pg = &self.app_state.pg_db;
        let files = match pg.get_files_touched(&config.execution_id).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    execution_id = %config.execution_id,
                    error = %e,
                    "auto_commit_on_success: get_files_touched failed; skipping"
                );
                return;
            }
        };

        if files.is_empty() {
            tracing::info!(
                execution_id = %config.execution_id,
                "auto_commit_on_success: no files touched, skipping commit"
            );
            return;
        }

        let short_id: String = config.execution_id.chars().take(8).collect();
        let message = format!(
            "auto(workflow:{}:{}): {} files",
            config.workflow_name,
            short_id,
            files.len()
        );

        match crate::auto_commit::commit_files(&cwd, &files, &message).await {
            Ok(crate::auto_commit::CommitOutcome::Committed { hash }) => {
                tracing::info!(
                    execution_id = %config.execution_id,
                    hash = %hash,
                    file_count = files.len(),
                    cwd = %cwd.display(),
                    "auto_commit_on_success: committed sub-agent file-set"
                );
                if let Err(e) = pg.clear_files_touched(&config.execution_id).await {
                    tracing::warn!(
                        execution_id = %config.execution_id,
                        error = %e,
                        "auto_commit_on_success: clear_files_touched failed after commit"
                    );
                }
            }
            Ok(crate::auto_commit::CommitOutcome::NothingToCommit) => {
                tracing::info!(
                    execution_id = %config.execution_id,
                    file_count = files.len(),
                    cwd = %cwd.display(),
                    "auto_commit_on_success: no diff, skipping commit (clearing tracker)"
                );
                // Files match HEAD already; the tracker would just stay stale.
                if let Err(e) = pg.clear_files_touched(&config.execution_id).await {
                    tracing::warn!(
                        execution_id = %config.execution_id,
                        error = %e,
                        "auto_commit_on_success: clear_files_touched failed after no-op"
                    );
                }
            }
            Err(e) => {
                // Never propagate — workflow success state must win.
                tracing::warn!(
                    execution_id = %config.execution_id,
                    file_count = files.len(),
                    cwd = %cwd.display(),
                    error = %e,
                    "auto_commit_on_success: auto-commit failed (workflow success preserved, tracker preserved for inspection)"
                );
            }
        }
    }
}

/// Auto-capture significant task findings as persistent observations.
///
/// Called on task completion to extract and persist cross-session knowledge:
/// - Task outcome (success/failure, duration, iteration count)
/// - Key findings from verification phases
/// - Error patterns encountered
async fn auto_capture_observations(pg: &crate::database::pg::PgDb, execution_id: &str) {
    let task_run = match pg.get_task_run(execution_id).await.ok().flatten() {
        Some(tr) => tr,
        None => return,
    };

    let workflow_name = task_run
        .workflow_name
        .as_deref()
        .unwrap_or(&task_run.task_name);
    let status = &task_run.status;
    let iterations = task_run.sessions_count;

    // Only capture observations for tasks that ran at least one iteration
    if iterations == 0 {
        return;
    }

    // Build a summary of the task outcome
    let outcome_type = if status == "completed" {
        "learning"
    } else {
        "bugfix"
    };

    let title = format!(
        "Task {}: {} ({} iterations)",
        if status == "completed" {
            "succeeded"
        } else {
            "failed"
        },
        workflow_name,
        iterations
    );

    let mut content_lines = vec![
        format!("## Task Run Outcome"),
        String::new(),
        format!("- **Workflow:** {}", workflow_name),
        format!("- **Status:** {}", status),
        format!("- **Iterations:** {}", iterations),
    ];

    // Compute duration from timestamps if available
    if let Some(ref completed) = task_run.completed_at {
        if let (Ok(start), Ok(end)) = (
            chrono::DateTime::parse_from_rfc3339(&task_run.created_at),
            chrono::DateTime::parse_from_rfc3339(completed),
        ) {
            let duration_secs = (end - start).num_seconds();
            if duration_secs > 0 {
                content_lines.push(format!("- **Duration:** {}s", duration_secs));
            }
        }
    }

    // Extract findings from task knowledge (PG)
    let findings = pg
        .get_task_knowledge_by_categories(
            execution_id,
            &["Finding", "RootCause", "Solution"],
            10,
            false,
        )
        .await
        .unwrap_or_default();

    if !findings.is_empty() {
        content_lines.push(String::new());
        content_lines.push("### Key Findings".to_string());
        content_lines.push(String::new());
        for (category, content) in &findings {
            content_lines.push(format!("- **[{}]** {}", category, content));
        }
    }

    let content = content_lines.join("\n");
    let topic_key = format!(
        "task-outcome/{}",
        workflow_name.to_lowercase().replace(' ', "-")
    );

    // Tenant-memory mirror (plan 2026-07-10-tenant-agentic-memory, Phase 2):
    // task-run outcomes are episodes the whole tenant can learn from.
    // Consent-gated (cloud_sync_enabled) + redacted inside the emitter;
    // best-effort — never affects the local capture below.
    crate::memory::tenant_sync::enqueue_memory_record(
        crate::memory::tenant_sync::TenantMemoryRecord::new(
            title.clone(),
            content.clone(),
            crate::memory::tenant_sync::MemoryRecordKind::Episode,
        )
        .with_importance(if status == "completed" { 0.5 } else { 0.7 })
        .with_source(serde_json::json!({
            "task_run_id": execution_id,
            "workflow": workflow_name,
            "status": status,
        })),
    );

    let input = crate::database::types::CreateObservationInput {
        title,
        content,
        observation_type: outcome_type.to_string(),
        scope: "project".to_string(),
        topic_key: Some(topic_key),
        project_id: None,
        workflow_id: task_run.workflow_id.clone(),
        task_run_id: Some(execution_id.to_string()),
        session_id: None,
    };

    match pg.save_observation(&input).await {
        Ok(id) => info!("Auto-captured task observation {} for {}", id, execution_id),
        Err(e) => warn!("Failed to auto-capture observation: {}", e),
    }
}

// =============================================================================
// Phase C: auto-commit policy resolver
// =============================================================================

/// Decide whether the workflow's terminal-state hook should auto-commit the
/// sub-agent's file-set.
///
/// Pure function so the policy can be unit-tested without mocking
/// `LoopController` (which is heavy with tauri/AppHandle dependencies).
///
/// - `Some(true)`  → always commit (caller still needs files non-empty).
/// - `Some(false)` → never commit, even with worktree.
/// - `None`        → commit iff the workflow uses a worktree.
fn resolve_auto_commit_policy(opt_in: Option<bool>, use_worktree: bool) -> bool {
    match opt_in {
        Some(v) => v,
        None => use_worktree,
    }
}

#[cfg(test)]
mod auto_commit_policy_tests {
    use super::*;

    #[test]
    fn explicit_true_always_commits() {
        assert!(resolve_auto_commit_policy(Some(true), false));
        assert!(resolve_auto_commit_policy(Some(true), true));
    }

    #[test]
    fn explicit_false_never_commits() {
        assert!(!resolve_auto_commit_policy(Some(false), false));
        assert!(!resolve_auto_commit_policy(Some(false), true));
    }

    #[test]
    fn none_defaults_to_worktree_state() {
        // No worktree → no commit by default.
        assert!(!resolve_auto_commit_policy(None, false));
        // With worktree → commit by default.
        assert!(resolve_auto_commit_policy(None, true));
    }
}
