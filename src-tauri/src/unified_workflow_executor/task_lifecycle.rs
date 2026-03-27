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
        if let Err(e) = self.app_state.pg_db.complete_task_run(execution_id).await {
            warn!("PG complete_task_run failed: {}", e);
        }
        if let Err(e) = self.checkpoint_db.complete_task_run(execution_id) {
            error!("Failed to mark task {} as completed: {}", execution_id, e);
        } else {
            info!("Marked task {} as COMPLETED", execution_id);
            // Broadcast task-run-update to both Tauri + WebSocket
            let broadcaster = EventBroadcaster::new(self.app_handle.clone());
            broadcaster.task_run_update(execution_id, "completed", None, None);

            // Sync completion to web backend (best-effort, non-blocking)
            let db = self.checkpoint_db.clone();
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

            // Auto-capture task knowledge as observations (Engram-style cross-session memory)
            {
                let pg = self.app_state.pg_db.clone();
                let db = self.checkpoint_db.clone();
                let exec_id = execution_id.to_string();
                tokio::spawn(async move {
                    auto_capture_observations(&db, &pg, &exec_id).await;
                });
            }

            // Parse meta-optimizer recommendations from completed output
            {
                let db = self.checkpoint_db.clone();
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
        }
    }

    /// Mark a task as failed, sync to backend, and check chain triggers.
    pub(crate) async fn mark_task_failed(
        &self,
        execution_id: &str,
        reason: &str,
        workflow_id: Option<&str>,
    ) {
        if let Err(e) = self.app_state.pg_db.fail_task_run(execution_id, reason).await {
            warn!("PG fail_task_run failed: {}", e);
        }
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
    pub(crate) async fn resolve_targeted_errors(&self, execution_id: &str, error_ids: &[i64]) {
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
    pub(crate) async fn resolve_workflow_scoped_errors(&self, execution_id: &str) {
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
    pub(crate) fn create_single_repo_worktree(
        config: &mut LoopConfig,
        setup_automation_steps: &mut Vec<ExecutionStepConfig>,
        setup_prompt_steps: &mut Vec<ExecutionStepConfig>,
        verification_steps: &mut Vec<ExecutionStepConfig>,
        agentic_steps: &mut Vec<ExecutionStepConfig>,
        completion_automation_steps: &mut Vec<ExecutionStepConfig>,
        completion_prompt_steps: &mut Vec<ExecutionStepConfig>,
        checkpoint_db: &Arc<crate::database::CheckpointDb>,
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
                            handle.block_on(async move {
                                if let Err(e) = pg_db.insert_worktree(&record_clone).await {
                                    warn!("PG insert_worktree failed: {}", e);
                                }
                            })
                        }));
                    }
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
}

/// Auto-capture significant task findings as persistent observations.
///
/// Called on task completion to extract and persist cross-session knowledge:
/// - Task outcome (success/failure, duration, iteration count)
/// - Key findings from verification phases
/// - Error patterns encountered
async fn auto_capture_observations(
    db: &crate::database::CheckpointDb,
    pg: &crate::database::pg::PgDb,
    execution_id: &str,
) {
    let task_run = match pg.get_task_run(execution_id).await.ok().flatten() {
        Some(tr) => tr,
        None => match db.get_task_run(execution_id) {
            Ok(Some(tr)) => tr,
            _ => return,
        },
    };

    let workflow_name = task_run.workflow_name.as_deref().unwrap_or(&task_run.task_name);
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
        if status == "completed" { "succeeded" } else { "failed" },
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

    // Extract findings from task knowledge (if available)
    let findings = db
        .with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT category, content FROM task_knowledge \
                     WHERE task_run_id = ?1 AND category IN ('Finding', 'RootCause', 'Solution') \
                     ORDER BY created_at DESC LIMIT 10",
                )
                .map_err(|e| format!("{}", e))?;
            let rows: Vec<(String, String)> = stmt
                .query_map(rusqlite::params![execution_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| format!("{}", e))?
                .filter_map(|r| r.ok())
                .collect();
            Ok(rows)
        })
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
    let topic_key = format!("task-outcome/{}", workflow_name.to_lowercase().replace(' ', "-"));

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
