//! PostgreSQL task run core CRUD operations via Clorinde-generated queries.
//!
//! Migrates the hot-path task run operations. Specialized operations
//! (AI session mgmt, iteration tracking, checkpoint ops) remain on SQLite.

use super::PgDb;
use crate::database::types::*;

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Map a Clorinde GetTaskRun row to TaskRun.
macro_rules! full_task_run {
    ($r:expr) => {{
        TaskRun {
            id: $r.id,
            task_name: $r.task_name,
            prompt: non_empty($r.prompt),
            task_type: if $r.task_type.is_empty() {
                "task".to_string()
            } else {
                $r.task_type
            },
            status: $r.status,
            sessions_count: $r.sessions_count as u32,
            max_sessions: if $r.max_sessions == 0 {
                None
            } else {
                Some($r.max_sessions as u32)
            },
            output_log: String::new(), // filled separately
            error_message: non_empty($r.error_message),
            auto_continue: $r.auto_continue,
            execution_steps_json: non_empty($r.execution_steps_json),
            log_sources_json: non_empty($r.log_sources_json),
            config_id: non_empty($r.config_id),
            workflow_name: non_empty($r.workflow_name),
            workflow_id: non_empty($r.workflow_id),
            summary: non_empty($r.summary),
            ai_summary: non_empty($r.ai_summary),
            goal_achieved: Some($r.goal_achieved),
            remaining_work: non_empty($r.remaining_work),
            summary_generated_at: non_empty($r.summary_generated_at),
            transition_history_json: non_empty($r.transition_history_json),
            workflow_type: non_empty($r.workflow_type),
            workspace_id: non_empty($r.workspace_id),
            triggered_by: non_empty($r.triggered_by),
            parent_task_run_id: non_empty($r.parent_task_run_id),
            root_task_run_id: non_empty($r.root_task_run_id),
            depth: $r.depth as u32,
            bridge_id: non_empty($r.bridge_id),
            result_data: non_empty($r.result_data),
            is_reflection: $r.is_reflection,
            reflection_source_task_run_id: non_empty($r.reflection_source_task_run_id),
            is_follow_up: $r.is_follow_up,
            follow_up_source_task_run_id: non_empty($r.follow_up_source_task_run_id),
            is_fixer: $r.is_fixer,
            fixer_source_task_run_id: non_empty($r.fixer_source_task_run_id),
            is_meta_optimizer: $r.is_meta_optimizer,
            is_review: $r.is_review,
            blocks_parent: $r.blocks_parent,
            created_at: $r.created_at.to_rfc3339(),
            updated_at: $r.updated_at.to_rfc3339(),
            completed_at: if $r.completed_at.timestamp() == 0 {
                None
            } else {
                Some($r.completed_at.to_rfc3339())
            },
        }
    }};
}

impl PgDb {
    /// Create a new task run.
    pub async fn create_task_run(&self, input: &CreateTaskRunInput) -> Result<TaskRun, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let task_type: Option<String> =
            Some(input.task_type.as_deref().unwrap_or("task").to_string());
        let auto_continue = input.auto_continue.unwrap_or(true);
        let effective_root: Option<String> = Some(
            input
                .root_task_run_id
                .clone()
                .unwrap_or_else(|| input.id.clone()),
        );
        let depth = input.depth as i32;
        let max_sessions: Option<i32> = input.max_sessions.map(|v| v as i32);
        let runner_port: Option<i32> = input.runner_port.map(|v| v as i32);
        let is_meta_optimizer = input.is_meta_optimizer;
        let is_reflection = Some(input.is_reflection);
        let is_follow_up = Some(input.is_follow_up);
        let is_fixer = Some(input.is_fixer);

        qontinui_db::queries::task_runs::create_task_run()
            .bind(
                &conn,
                &input.id.as_str(),
                &input.task_name.as_str(),
                &input.prompt.as_deref(),
                &task_type,
                &max_sessions,
                &auto_continue,
                &input.execution_steps_json.as_deref(),
                &input.log_sources_json.as_deref(),
                &input.config_id.as_deref(),
                &input.workflow_name.as_deref(),
                &input.workflow_id.as_deref(),
                &input.workflow_type.as_deref(),
                &input.parent_task_run_id.as_deref(),
                &effective_root,
                &depth,
                &input.workspace_id.as_deref(),
                &input.triggered_by.as_deref(),
                &input.bridge_id.as_deref(),
                &is_reflection,
                &input.reflection_source_task_run_id.as_deref(),
                &is_follow_up,
                &input.follow_up_source_task_run_id.as_deref(),
                &is_fixer,
                &input.fixer_source_task_run_id.as_deref(),
                &is_meta_optimizer,
                &runner_port,
            )
            .one()
            .await
            .map_err(|e| format!("PG create_task_run: {}", e))?;

        self.get_task_run(&input.id)
            .await?
            .ok_or_else(|| "Failed to retrieve created task run".to_string())
    }

    /// Get a single task run by ID.
    pub async fn get_task_run(&self, id: &str) -> Result<Option<TaskRun>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = qontinui_db::queries::task_runs::get_task_run()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG get_task_run: {}", e))?;

        match row {
            Some(r) => {
                let mut tr = full_task_run!(r);
                // Get output separately (stored in the same row for PG)
                tr.output_log = self.get_task_output(id).await.unwrap_or_default();
                Ok(Some(tr))
            }
            None => Ok(None),
        }
    }

    /// Get recent task runs (lightweight — no output_log, no hierarchy fields).
    pub async fn get_recent_task_runs(
        &self,
        limit: u32,
        runner_port: Option<u16>,
    ) -> Result<Vec<TaskRun>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let port: Option<i32> = runner_port.map(|p| p as i32);
        let max_results = limit as i64;

        let rows = qontinui_db::queries::task_runs::get_recent_task_runs()
            .bind(&conn, &port, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG get_recent_task_runs: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| TaskRun {
                id: r.id,
                task_name: r.task_name,
                prompt: non_empty(r.prompt),
                task_type: if r.task_type.is_empty() {
                    "task".to_string()
                } else {
                    r.task_type
                },
                status: r.status,
                sessions_count: r.sessions_count as u32,
                max_sessions: if r.max_sessions == 0 {
                    None
                } else {
                    Some(r.max_sessions as u32)
                },
                output_log: String::new(),
                error_message: non_empty(r.error_message),
                auto_continue: r.auto_continue,
                execution_steps_json: None,
                log_sources_json: None,
                config_id: non_empty(r.config_id),
                workflow_name: non_empty(r.workflow_name),
                workflow_id: non_empty(r.workflow_id),
                summary: non_empty(r.summary),
                ai_summary: non_empty(r.ai_summary),
                goal_achieved: Some(r.goal_achieved),
                remaining_work: non_empty(r.remaining_work),
                summary_generated_at: non_empty(r.summary_generated_at),
                transition_history_json: None,
                workflow_type: None,
                workspace_id: non_empty(r.workspace_id),
                triggered_by: non_empty(r.triggered_by),
                parent_task_run_id: None,
                root_task_run_id: None,
                depth: 0,
                bridge_id: None,
                result_data: None,
                is_reflection: false,
                reflection_source_task_run_id: None,
                is_follow_up: false,
                follow_up_source_task_run_id: None,
                is_fixer: false,
                fixer_source_task_run_id: None,
                is_meta_optimizer: false,
                is_review: false,
                blocks_parent: false,
                created_at: r.created_at.to_rfc3339(),
                updated_at: r.updated_at.to_rfc3339(),
                completed_at: if r.completed_at.timestamp() == 0 {
                    None
                } else {
                    Some(r.completed_at.to_rfc3339())
                },
            })
            .collect())
    }

    /// Get recent task runs with optional workflow_type filter (lightweight).
    pub async fn get_recent_task_runs_filtered(
        &self,
        limit: u32,
        workflow_type: Option<&str>,
        runner_port: Option<u16>,
    ) -> Result<Vec<TaskRun>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let port: Option<i32> = runner_port.map(|p| p as i32);
        let max_results = limit as i64;
        let wt: Option<&str> = workflow_type;

        let rows = qontinui_db::queries::task_runs::get_recent_task_runs_filtered()
            .bind(&conn, &wt, &port, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG get_recent_task_runs_filtered: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| TaskRun {
                id: r.id,
                task_name: r.task_name,
                prompt: non_empty(r.prompt),
                task_type: if r.task_type.is_empty() {
                    "task".to_string()
                } else {
                    r.task_type
                },
                status: r.status,
                sessions_count: r.sessions_count as u32,
                max_sessions: if r.max_sessions == 0 {
                    None
                } else {
                    Some(r.max_sessions as u32)
                },
                output_log: String::new(),
                error_message: non_empty(r.error_message),
                auto_continue: r.auto_continue,
                execution_steps_json: None,
                log_sources_json: None,
                config_id: non_empty(r.config_id),
                workflow_name: non_empty(r.workflow_name),
                workflow_id: non_empty(r.workflow_id),
                summary: non_empty(r.summary),
                ai_summary: non_empty(r.ai_summary),
                goal_achieved: Some(r.goal_achieved),
                remaining_work: non_empty(r.remaining_work),
                summary_generated_at: non_empty(r.summary_generated_at),
                transition_history_json: None,
                workflow_type: None,
                workspace_id: non_empty(r.workspace_id),
                triggered_by: non_empty(r.triggered_by),
                parent_task_run_id: None,
                root_task_run_id: None,
                depth: 0,
                bridge_id: None,
                result_data: None,
                is_reflection: false,
                reflection_source_task_run_id: None,
                is_follow_up: false,
                follow_up_source_task_run_id: None,
                is_fixer: false,
                fixer_source_task_run_id: None,
                is_meta_optimizer: false,
                is_review: false,
                blocks_parent: false,
                created_at: r.created_at.to_rfc3339(),
                updated_at: r.updated_at.to_rfc3339(),
                completed_at: if r.completed_at.timestamp() == 0 {
                    None
                } else {
                    Some(r.completed_at.to_rfc3339())
                },
            })
            .collect())
    }

    /// Update task run status.
    pub async fn update_task_run_status(&self, id: &str, status: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::task_runs::update_task_run_status()
            .bind(&conn, &status, &id)
            .opt()
            .await
            .map_err(|e| format!("PG update_task_run_status: {}", e))?;
        Ok(())
    }

    /// Complete a task run (status='complete', set completed_at).
    pub async fn complete_task_run(&self, id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::task_runs::complete_task_run()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG complete_task_run: {}", e))?;
        Ok(())
    }

    /// Fail a task run (status='failed', set error_message and completed_at).
    pub async fn fail_task_run(&self, id: &str, error_message: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::task_runs::fail_task_run()
            .bind(&conn, &error_message, &id)
            .opt()
            .await
            .map_err(|e| format!("PG fail_task_run: {}", e))?;
        Ok(())
    }

    /// Stop a task run with a reason (status='stopped', set error_message and completed_at).
    pub async fn stop_task_run(&self, id: &str, reason: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::task_runs::stop_task_run()
            .bind(&conn, &reason, &id)
            .opt()
            .await
            .map_err(|e| format!("PG stop_task_run: {}", e))?;
        Ok(())
    }

    /// Delete a task run.
    pub async fn delete_task_run(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let deleted = qontinui_db::queries::task_runs::delete_task_run()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG delete_task_run: {}", e))?;
        Ok(deleted.is_some())
    }

    /// Update task run summary.
    pub async fn update_task_summary(
        &self,
        id: &str,
        summary: Option<&str>,
        goal_achieved: Option<bool>,
        remaining_work: Option<&str>,
        summary_generated_at: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let parsed_at: chrono::DateTime<chrono::FixedOffset> =
            chrono::DateTime::parse_from_rfc3339(summary_generated_at).map_err(|e| {
                format!(
                    "PG update_task_summary: invalid timestamp '{}': {}",
                    summary_generated_at, e
                )
            })?;
        qontinui_db::queries::task_runs::update_task_summary()
            .bind(
                &conn,
                &summary,
                &goal_achieved,
                &remaining_work,
                &parsed_at,
                &id,
            )
            .opt()
            .await
            .map_err(|e| format!("PG update_task_summary: {}", e))?;
        Ok(())
    }

    /// Append output to a task run.
    pub async fn append_task_output(&self, id: &str, output: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::task_runs::append_task_output()
            .bind(&conn, &output, &id)
            .await
            .map_err(|e| format!("PG append_task_output: {}", e))?;
        Ok(())
    }

    /// Get task run output.
    pub async fn get_task_output(&self, id: &str) -> Result<String, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let output = qontinui_db::queries::task_runs::get_task_output()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG get_task_output: {}", e))?;
        Ok(output.unwrap_or_default())
    }

    /// Update task run name.
    pub async fn update_task_name(&self, id: &str, name: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::task_runs::update_task_name()
            .bind(&conn, &name, &id)
            .opt()
            .await
            .map_err(|e| format!("PG update_task_name: {}", e))?;
        Ok(())
    }

    /// Update task run result data.
    pub async fn update_task_result_data(&self, id: &str, result_data: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::task_runs::update_task_result_data()
            .bind(&conn, &result_data, &id)
            .opt()
            .await
            .map_err(|e| format!("PG update_task_result_data: {}", e))?;
        Ok(())
    }

    /// Extended output append with optional session increment and completion check.
    /// PG version uses simple string concatenation (no chunking) + session increment.
    pub async fn append_task_output_ex(
        &self,
        id: &str,
        output: &str,
        increment_session: bool,
        _check_completion_marker: bool,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Append output
        qontinui_db::queries::task_runs::append_task_output()
            .bind(&conn, &output, &id)
            .await
            .map_err(|e| format!("PG append_task_output: {}", e))?;

        // Increment session count if requested
        if increment_session {
            qontinui_db::queries::task_runs::increment_sessions_count()
                .bind(&conn, &id)
                .await
                .map_err(|e| format!("PG increment_sessions: {}", e))?;
        }

        // Completion marker check — for PG, return false (let unified workflow loop handle completion)
        Ok(false)
    }

    /// Set the verification_passed flag on a task run.
    pub async fn set_verification_passed(&self, id: &str, passed: bool) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE task_runs SET verification_passed = $1 WHERE id = $2",
            &[&passed, &id],
        )
        .await
        .map_err(|e| format!("PG set_verification_passed: {}", e))?;
        Ok(())
    }

    /// Update fix_attempts and ci_auto_resumes counters on a task run.
    /// Called when the workflow loop completes (success or failure) to persist
    /// the bounded-fix-loop metrics added in Phase 1.
    pub async fn update_task_run_fix_metrics(
        &self,
        id: &str,
        fix_attempts: i32,
        ci_auto_resumes: i32,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE task_runs SET fix_attempts = $1, ci_auto_resumes = $2 WHERE id = $3",
            &[&fix_attempts, &ci_auto_resumes, &id],
        )
        .await
        .map_err(|e| format!("PG update_task_run_fix_metrics: {}", e))?;
        Ok(())
    }

    /// Get iteration commits for a task run (stored as JSON array in iteration_commits column).
    pub async fn get_iteration_commits(
        &self,
        id: &str,
    ) -> Result<Vec<crate::unified_workflow_executor::IterationCommit>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                "SELECT iteration_commits FROM task_runs WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_iteration_commits: {}", e))?;

        match row.and_then(|r| r.get::<_, Option<String>>(0)) {
            Some(s) => serde_json::from_str(&s)
                .map_err(|e| format!("Failed to parse iteration commits: {}", e)),
            None => Ok(Vec::new()),
        }
    }

    /// Append a commit checkpoint for an iteration (read-modify-write on iteration_commits JSON column).
    pub async fn append_iteration_commit(
        &self,
        id: &str,
        commit: &crate::unified_workflow_executor::IterationCommit,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Read existing commits
        let row = conn
            .query_opt(
                "SELECT iteration_commits FROM task_runs WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(|e| format!("PG append_iteration_commit read: {}", e))?;

        let existing: Option<String> = row.and_then(|r| r.get(0));

        let mut commits: Vec<serde_json::Value> = existing
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();

        let commit_value = serde_json::to_value(commit)
            .map_err(|e| format!("Failed to serialize iteration commit: {}", e))?;
        commits.push(commit_value);

        let json = serde_json::to_string(&commits)
            .map_err(|e| format!("Failed to serialize iteration commits: {}", e))?;

        conn.execute(
            "UPDATE task_runs SET iteration_commits = $1, updated_at = NOW() WHERE id = $2",
            &[&json, &id],
        )
        .await
        .map_err(|e| format!("PG append_iteration_commit write: {}", e))?;

        Ok(())
    }

    /// Get the full output log (alias for get_task_output on PG where output is not chunked).
    pub async fn get_full_task_output(&self, id: &str) -> Result<String, String> {
        self.get_task_output(id).await
    }

    /// Update runtime context for a task run.
    pub async fn update_task_run_runtime_context(
        &self,
        id: &str,
        runtime_context_json: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE task_runs SET runtime_context_json = $1, updated_at = NOW() WHERE id = $2",
            &[&runtime_context_json, &id],
        )
        .await
        .map_err(|e| format!("PG update_task_run_runtime_context: {}", e))?;
        Ok(())
    }

    /// Get the most recent task run that has orchestrator checkpoints.
    pub async fn get_most_recent_task_with_checkpoints(&self) -> Result<Option<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                r#"SELECT DISTINCT t.id
                FROM task_runs t
                INNER JOIN orchestrator_checkpoints c ON t.id = c.task_id
                ORDER BY t.id DESC
                LIMIT 1"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_most_recent_task_with_checkpoints: {}", e))?;

        Ok(row.map(|r| r.get(0)))
    }

    /// Get all iteration diffs for a task run (JSON column).
    pub async fn get_iteration_diffs(
        &self,
        id: &str,
    ) -> Result<Vec<crate::unified_workflow_executor::IterationDiff>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                "SELECT iteration_diffs FROM task_runs WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_iteration_diffs: {}", e))?;

        match row.and_then(|r| r.get::<_, Option<String>>(0)) {
            Some(s) => serde_json::from_str(&s)
                .map_err(|e| format!("Failed to parse iteration diffs: {}", e)),
            None => Ok(Vec::new()),
        }
    }

    /// Clear iteration diffs from a given iteration onward (for replay).
    pub async fn clear_iteration_diffs_from(
        &self,
        id: &str,
        from_iteration: u32,
    ) -> Result<(), String> {
        let diffs = self.get_iteration_diffs(id).await?;
        let filtered: Vec<_> = diffs
            .into_iter()
            .filter(|d| d.iteration < from_iteration)
            .collect();
        let json = serde_json::to_string(&filtered)
            .map_err(|e| format!("Failed to serialize filtered diffs: {}", e))?;
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE task_runs SET iteration_diffs = $1, updated_at = NOW() WHERE id = $2",
            &[&json, &id],
        )
        .await
        .map_err(|e| format!("PG clear_iteration_diffs_from: {}", e))?;
        Ok(())
    }

    /// Clear iteration commits from a given iteration onward (for replay).
    pub async fn clear_iteration_commits_from(
        &self,
        id: &str,
        from_iteration: u32,
    ) -> Result<(), String> {
        let commits = self.get_iteration_commits(id).await?;
        let filtered: Vec<_> = commits
            .into_iter()
            .filter(|c| c.iteration < from_iteration)
            .collect();
        let json = serde_json::to_string(&filtered)
            .map_err(|e| format!("Failed to serialize filtered commits: {}", e))?;
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE task_runs SET iteration_commits = $1, updated_at = NOW() WHERE id = $2",
            &[&json, &id],
        )
        .await
        .map_err(|e| format!("PG clear_iteration_commits_from: {}", e))?;
        Ok(())
    }

    /// Clear step checkpoints from a given iteration onward (for replay).
    pub async fn clear_checkpoints_from_iteration(
        &self,
        execution_id: &str,
        from_iteration: u32,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let from_i32 = from_iteration as i32;
        conn.execute(
            "DELETE FROM workflow_step_checkpoints WHERE execution_id = $1 AND iteration >= $2",
            &[&execution_id, &from_i32],
        )
        .await
        .map_err(|e| format!("PG clear_checkpoints_from_iteration: {}", e))?;

        // Also clear verification phase results
        conn.execute(
            "DELETE FROM workflow_verification_phase_results WHERE task_run_id = $1 AND iteration >= $2",
            &[&execution_id, &from_i32],
        )
        .await
        .map_err(|e| format!("PG clear_verification_results: {}", e))?;

        Ok(())
    }

    /// Get verification phase results for replay point listing.
    pub async fn get_workflow_verification_results(
        &self,
        execution_id: &str,
    ) -> Result<Vec<crate::database::types::VerificationPhaseSummary>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT iteration, all_passed, passed_steps, failed_steps, created_at::TEXT
                   FROM workflow_verification_phase_results
                   WHERE task_run_id = $1
                   ORDER BY iteration ASC"#,
                &[&execution_id],
            )
            .await
            .map_err(|e| format!("PG get_workflow_verification_results: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| crate::database::types::VerificationPhaseSummary {
                iteration: r.get(0),
                all_passed: r.get(1),
                passed_steps: r.get(2),
                failed_steps: r.get(3),
                created_at: r.get(4),
            })
            .collect())
    }

    /// Get currently running task runs.
    pub async fn get_running_task_runs(
        &self,
        runner_port: Option<u16>,
    ) -> Result<Vec<TaskRun>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let port: Option<i32> = runner_port.map(|p| p as i32);

        let rows = qontinui_db::queries::task_runs::get_running_task_runs()
            .bind(&conn, &port)
            .all()
            .await
            .map_err(|e| format!("PG get_running_task_runs: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| TaskRun {
                id: r.id,
                task_name: r.task_name,
                prompt: non_empty(r.prompt),
                task_type: if r.task_type.is_empty() {
                    "task".to_string()
                } else {
                    r.task_type
                },
                status: r.status,
                sessions_count: r.sessions_count as u32,
                max_sessions: if r.max_sessions == 0 {
                    None
                } else {
                    Some(r.max_sessions as u32)
                },
                output_log: String::new(),
                error_message: non_empty(r.error_message),
                auto_continue: r.auto_continue,
                execution_steps_json: None,
                log_sources_json: None,
                config_id: non_empty(r.config_id),
                workflow_name: non_empty(r.workflow_name),
                workflow_id: non_empty(r.workflow_id),
                summary: None,
                ai_summary: None,
                goal_achieved: None,
                remaining_work: None,
                summary_generated_at: None,
                transition_history_json: None,
                workflow_type: non_empty(r.workflow_type),
                workspace_id: non_empty(r.workspace_id),
                triggered_by: non_empty(r.triggered_by),
                parent_task_run_id: non_empty(r.parent_task_run_id),
                root_task_run_id: non_empty(r.root_task_run_id),
                depth: r.depth as u32,
                bridge_id: non_empty(r.bridge_id),
                result_data: None,
                is_reflection: r.is_reflection,
                reflection_source_task_run_id: non_empty(r.reflection_source_task_run_id),
                is_follow_up: r.is_follow_up,
                follow_up_source_task_run_id: non_empty(r.follow_up_source_task_run_id),
                is_fixer: r.is_fixer,
                fixer_source_task_run_id: non_empty(r.fixer_source_task_run_id),
                is_meta_optimizer: r.is_meta_optimizer,
                is_review: r.is_review,
                blocks_parent: r.blocks_parent,
                created_at: r.created_at.to_rfc3339(),
                updated_at: r.updated_at.to_rfc3339(),
                completed_at: if r.completed_at.timestamp() == 0 {
                    None
                } else {
                    Some(r.completed_at.to_rfc3339())
                },
            })
            .collect())
    }

    /// Assign `runner_port` to every `running` task_run row that currently
    /// has `runner_port IS NULL`.
    ///
    /// NULL-port rows are created by scheduler-triggered workflow launches
    /// (which have no specific runner context) and by legacy code paths that
    /// predate the per-runner ownership tagging. Once a runner starts, it
    /// calls this to take ownership of all such orphans so the zombie sweep
    /// and startup-resume paths treat them uniformly.
    ///
    /// Returns the IDs of rows that were claimed.
    pub async fn claim_orphaned_task_runs(&self, runner_port: u16) -> Result<Vec<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let port = runner_port as i32;

        let rows = conn
            .query(
                r#"UPDATE task_runs
                   SET runner_port = $1, updated_at = NOW()
                   WHERE status = 'running' AND runner_port IS NULL
                   RETURNING id"#,
                &[&port],
            )
            .await
            .map_err(|e| format!("PG claim_orphaned_task_runs: {}", e))?;

        Ok(rows.into_iter().map(|r| r.get::<_, String>("id")).collect())
    }

    /// Stricter variant of `get_running_task_runs` used by the startup-resume
    /// path: returns only rows whose `runner_port` exactly matches the caller.
    /// Excludes NULL-port rows so multiple runners can't both claim the same
    /// orphaned task. NULL-port tasks are handled by `claim_orphaned_task_runs`.
    pub async fn get_resumable_task_runs_for_runner(
        &self,
        runner_port: u16,
    ) -> Result<Vec<TaskRun>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let port = runner_port as i32;

        let rows = conn
            .query(
                r#"SELECT id, task_name, COALESCE(prompt, '') as prompt,
                          COALESCE(task_type, 'task') as task_type,
                          COALESCE(status, 'running') as status,
                          COALESCE(sessions_count, 0) as sessions_count,
                          COALESCE(max_sessions, 0) as max_sessions,
                          COALESCE(error_message, '') as error_message,
                          COALESCE(auto_continue, true) as auto_continue,
                          COALESCE(config_id, '') as config_id,
                          COALESCE(workflow_name, '') as workflow_name,
                          COALESCE(workflow_id, '') as workflow_id,
                          COALESCE(workflow_type, 'task') as workflow_type,
                          COALESCE(workspace_id, '') as workspace_id,
                          COALESCE(triggered_by, '') as triggered_by,
                          COALESCE(parent_task_run_id, '') as parent_task_run_id,
                          COALESCE(root_task_run_id, '') as root_task_run_id,
                          COALESCE(depth, 0) as depth,
                          COALESCE(bridge_id, '') as bridge_id,
                          COALESCE(is_reflection, false) as is_reflection,
                          COALESCE(reflection_source_task_run_id, '') as reflection_source_task_run_id,
                          COALESCE(is_follow_up, false) as is_follow_up,
                          COALESCE(follow_up_source_task_run_id, '') as follow_up_source_task_run_id,
                          COALESCE(is_fixer, false) as is_fixer,
                          COALESCE(fixer_source_task_run_id, '') as fixer_source_task_run_id,
                          COALESCE(is_meta_optimizer, false) as is_meta_optimizer,
                          COALESCE(is_review, false) as is_review,
                          COALESCE(blocks_parent, false) as blocks_parent,
                          to_char(created_at, 'YYYY-MM-DD"T"HH24:MI:SS"Z"') as created_at,
                          to_char(updated_at, 'YYYY-MM-DD"T"HH24:MI:SS"Z"') as updated_at,
                          to_char(completed_at, 'YYYY-MM-DD"T"HH24:MI:SS"Z"') as completed_at
                   FROM task_runs
                   WHERE status = 'running' AND runner_port = $1
                   ORDER BY created_at DESC"#,
                &[&port],
            )
            .await
            .map_err(|e| format!("PG get_resumable_task_runs_for_runner: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| TaskRun {
                id: r.get("id"),
                task_name: r.get("task_name"),
                prompt: non_empty(r.get::<_, String>("prompt")),
                task_type: r.get("task_type"),
                status: r.get("status"),
                sessions_count: r.get::<_, i32>("sessions_count") as u32,
                max_sessions: {
                    let v: i32 = r.get("max_sessions");
                    if v == 0 {
                        None
                    } else {
                        Some(v as u32)
                    }
                },
                output_log: String::new(),
                error_message: non_empty(r.get::<_, String>("error_message")),
                auto_continue: r.get("auto_continue"),
                execution_steps_json: None,
                log_sources_json: None,
                config_id: non_empty(r.get::<_, String>("config_id")),
                workflow_name: non_empty(r.get::<_, String>("workflow_name")),
                workflow_id: non_empty(r.get::<_, String>("workflow_id")),
                summary: None,
                ai_summary: None,
                goal_achieved: None,
                remaining_work: None,
                summary_generated_at: None,
                transition_history_json: None,
                workflow_type: non_empty(r.get::<_, String>("workflow_type")),
                workspace_id: non_empty(r.get::<_, String>("workspace_id")),
                triggered_by: non_empty(r.get::<_, String>("triggered_by")),
                parent_task_run_id: non_empty(r.get::<_, String>("parent_task_run_id")),
                root_task_run_id: non_empty(r.get::<_, String>("root_task_run_id")),
                depth: r.get::<_, i32>("depth") as u32,
                bridge_id: non_empty(r.get::<_, String>("bridge_id")),
                result_data: None,
                is_reflection: r.get("is_reflection"),
                reflection_source_task_run_id: non_empty(
                    r.get::<_, String>("reflection_source_task_run_id"),
                ),
                is_follow_up: r.get("is_follow_up"),
                follow_up_source_task_run_id: non_empty(
                    r.get::<_, String>("follow_up_source_task_run_id"),
                ),
                is_fixer: r.get("is_fixer"),
                fixer_source_task_run_id: non_empty(r.get::<_, String>("fixer_source_task_run_id")),
                is_meta_optimizer: r.get("is_meta_optimizer"),
                is_review: r.get("is_review"),
                blocks_parent: r.get("blocks_parent"),
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
                completed_at: r.get::<_, Option<String>>("completed_at"),
            })
            .collect())
    }

    // ========================================================================
    // Task Run Automation (raw SQL)
    // ========================================================================

    /// Create a new task run automation record.
    pub async fn create_task_run_automation(
        &self,
        task_run_id: &str,
        workflow_name: Option<&str>,
        iteration_number: u32,
    ) -> Result<String, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let iter_num = iteration_number as i64;

        conn.execute(
            r#"INSERT INTO task_run_automation (id, task_run_id, workflow_name, started_at, automation_status, iteration_number)
               VALUES ($1, $2, $3, $4, 'running', $5)"#,
            &[&id, &task_run_id, &workflow_name, &now, &iter_num],
        )
        .await
        .map_err(|e| format!("PG create_task_run_automation: {}", e))?;

        Ok(id)
    }

    /// Complete a task run automation record with success.
    pub async fn complete_task_run_automation(
        &self,
        id: &str,
        actions_summary: Option<&str>,
        states_visited: Option<&str>,
        transitions_executed: Option<&str>,
        template_matches: Option<&str>,
        anomalies: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Get start time to calculate duration
        let row = conn
            .query_one(
                "SELECT started_at FROM task_run_automation WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get automation start time: {}", e))?;

        let started_at: String = row.get(0);
        let now = chrono::Utc::now();
        let duration_ms = if let Ok(start) = chrono::DateTime::parse_from_rfc3339(&started_at) {
            now.signed_duration_since(start.with_timezone(&chrono::Utc))
                .num_milliseconds()
        } else {
            0i64
        };
        let now_str = now.to_rfc3339();

        conn.execute(
            r#"UPDATE task_run_automation SET
                automation_status = 'success',
                success = true,
                ended_at = $1,
                duration_ms = $2,
                actions_summary = $3,
                states_visited = $4,
                transitions_executed = $5,
                template_matches = $6,
                anomalies = $7
            WHERE id = $8"#,
            &[
                &now_str,
                &duration_ms,
                &actions_summary,
                &states_visited,
                &transitions_executed,
                &template_matches,
                &anomalies,
                &id,
            ],
        )
        .await
        .map_err(|e| format!("PG complete_task_run_automation: {}", e))?;

        Ok(())
    }

    /// Fail a task run automation record.
    pub async fn fail_task_run_automation(
        &self,
        id: &str,
        error_type: Option<&str>,
        error_message: &str,
        actions_summary: Option<&str>,
        states_visited: Option<&str>,
        transitions_executed: Option<&str>,
        template_matches: Option<&str>,
        anomalies: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Get start time to calculate duration
        let row = conn
            .query_one(
                "SELECT started_at FROM task_run_automation WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get automation start time: {}", e))?;

        let started_at: String = row.get(0);
        let now = chrono::Utc::now();
        let duration_ms = if let Ok(start) = chrono::DateTime::parse_from_rfc3339(&started_at) {
            now.signed_duration_since(start.with_timezone(&chrono::Utc))
                .num_milliseconds()
        } else {
            0i64
        };
        let now_str = now.to_rfc3339();
        let err_msg: Option<&str> = Some(error_message);

        conn.execute(
            r#"UPDATE task_run_automation SET
                automation_status = 'failed',
                success = false,
                ended_at = $1,
                duration_ms = $2,
                error_type = $3,
                error_message = $4,
                actions_summary = $5,
                states_visited = $6,
                transitions_executed = $7,
                template_matches = $8,
                anomalies = $9
            WHERE id = $10"#,
            &[
                &now_str,
                &duration_ms,
                &error_type,
                &err_msg,
                &actions_summary,
                &states_visited,
                &transitions_executed,
                &template_matches,
                &anomalies,
                &id,
            ],
        )
        .await
        .map_err(|e| format!("PG fail_task_run_automation: {}", e))?;

        Ok(())
    }

    /// Get automation records for a task run.
    pub async fn get_task_run_automations(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<crate::database::types::TaskRunAutomation>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, task_run_id, workflow_name, started_at, ended_at, duration_ms,
                       automation_status, success, error_type, error_message,
                       actions_summary, states_visited, transitions_executed,
                       template_matches, anomalies, iteration_number
                FROM task_run_automation
                WHERE task_run_id = $1
                ORDER BY iteration_number ASC"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_task_run_automations: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| crate::database::types::TaskRunAutomation {
                id: r.get(0),
                task_run_id: r.get(1),
                workflow_name: r.get(2),
                started_at: r.get(3),
                ended_at: r.get(4),
                duration_ms: r.get(5),
                automation_status: r.get(6),
                success: r.get(7),
                error_type: r.get(8),
                error_message: r.get(9),
                actions_summary: r.get(10),
                states_visited: r.get(11),
                transitions_executed: r.get(12),
                template_matches: r.get(13),
                anomalies: r.get(14),
                iteration_number: r.get::<_, Option<i64>>(15).unwrap_or(1) as u32,
            })
            .collect())
    }

    /// Get a single automation record by its own ID.
    pub async fn get_task_run_automation_by_id(
        &self,
        automation_id: &str,
    ) -> Result<Option<crate::database::types::TaskRunAutomation>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"SELECT id, task_run_id, workflow_name, started_at, ended_at, duration_ms,
                       automation_status, success, error_type, error_message,
                       actions_summary, states_visited, transitions_executed,
                       template_matches, anomalies, iteration_number
                FROM task_run_automation
                WHERE id = $1"#,
                &[&automation_id],
            )
            .await
            .map_err(|e| format!("PG get_task_run_automation_by_id: {}", e))?;

        Ok(row.map(|r| crate::database::types::TaskRunAutomation {
            id: r.get(0),
            task_run_id: r.get(1),
            workflow_name: r.get(2),
            started_at: r.get(3),
            ended_at: r.get(4),
            duration_ms: r.get(5),
            automation_status: r.get(6),
            success: r.get(7),
            error_type: r.get(8),
            error_message: r.get(9),
            actions_summary: r.get(10),
            states_visited: r.get(11),
            transitions_executed: r.get(12),
            template_matches: r.get(13),
            anomalies: r.get(14),
            iteration_number: r.get::<_, Option<i64>>(15).unwrap_or(1) as u32,
        }))
    }

    // ========================================================================
    // Task Run MCP Calls (raw SQL)
    // ========================================================================

    /// Create a task run MCP call record.
    pub async fn create_task_run_mcp_call(
        &self,
        input: &crate::mcp_client::CreateMcpCallInput,
    ) -> Result<String, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            r#"INSERT INTO task_run_mcp_calls (
                id, task_run_id, step_id, step_name,
                server_id, server_name, tool_name,
                arguments, resolved_arguments,
                response, response_type, duration_ms,
                extractions, assertions,
                success, error_message, created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)"#,
            &[
                &id,
                &input.task_run_id.as_str(),
                &input.step_id.as_str(),
                &input.step_name.as_deref(),
                &input.server_id.as_str(),
                &input.server_name.as_deref(),
                &input.tool_name.as_str(),
                &input.arguments.as_deref(),
                &input.resolved_arguments.as_deref(),
                &input.response.as_deref(),
                &input.response_type.as_str(),
                &input.duration_ms,
                &input.extractions.as_deref(),
                &input.assertions.as_deref(),
                &input.success,
                &input.error_message.as_deref(),
                &now,
            ],
        )
        .await
        .map_err(|e| format!("PG create_task_run_mcp_call: {}", e))?;

        Ok(id)
    }

    /// Get all MCP calls for a task run, optionally filtered by success.
    pub async fn get_task_run_mcp_calls(
        &self,
        task_run_id: &str,
        success_filter: Option<bool>,
    ) -> Result<crate::mcp_client::McpCallsResult, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = if let Some(success) = success_filter {
            conn.query(
                r#"SELECT id, task_run_id, step_id, step_name,
                       server_id, server_name, tool_name,
                       arguments, resolved_arguments,
                       response, response_type, duration_ms,
                       extractions, assertions,
                       success, error_message, created_at
                FROM task_run_mcp_calls
                WHERE task_run_id = $1 AND success = $2
                ORDER BY created_at ASC"#,
                &[&task_run_id, &success],
            )
            .await
            .map_err(|e| format!("PG get_task_run_mcp_calls: {}", e))?
        } else {
            conn.query(
                r#"SELECT id, task_run_id, step_id, step_name,
                       server_id, server_name, tool_name,
                       arguments, resolved_arguments,
                       response, response_type, duration_ms,
                       extractions, assertions,
                       success, error_message, created_at
                FROM task_run_mcp_calls
                WHERE task_run_id = $1
                ORDER BY created_at ASC"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_task_run_mcp_calls: {}", e))?
        };

        let calls: Vec<crate::mcp_client::McpCallRecord> = rows
            .iter()
            .map(|r| crate::mcp_client::McpCallRecord {
                id: r.get(0),
                task_run_id: r.get(1),
                step_id: r.get(2),
                step_name: r.get(3),
                server_id: r.get(4),
                server_name: r.get(5),
                tool_name: r.get(6),
                arguments: r.get(7),
                resolved_arguments: r.get(8),
                response: r.get(9),
                response_type: r.get(10),
                duration_ms: r.get(11),
                extractions: r.get(12),
                assertions: r.get(13),
                success: r.get(14),
                error_message: r.get(15),
                created_at: r.get(16),
            })
            .collect();

        let success_count = calls.iter().filter(|c| c.success).count();
        let failed_count = calls.iter().filter(|c| !c.success).count();

        let total = calls.len();
        Ok(crate::mcp_client::McpCallsResult {
            task_run_id: task_run_id.to_string(),
            calls,
            count: total,
            total_count: total,
            success_count,
            failed_count,
            has_more: false,
        })
    }

    /// Pause a running task run, setting it to 'paused'.
    pub async fn pause_task_run(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .execute(
                "UPDATE task_runs SET status = 'paused', updated_at = NOW() WHERE id = $1 AND status = 'running'",
                &[&id],
            )
            .await
            .map_err(|e| format!("PG pause_task_run: {}", e))?;
        Ok(rows > 0)
    }

    /// Unpause a paused task run, setting it back to 'running'.
    pub async fn unpause_task_run(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .execute(
                "UPDATE task_runs SET status = 'running', updated_at = NOW() WHERE id = $1 AND status = 'paused'",
                &[&id],
            )
            .await
            .map_err(|e| format!("PG unpause_task_run: {}", e))?;
        Ok(rows > 0)
    }

    /// Get recent task runs with their learning outcomes joined.
    pub async fn get_recent_task_runs_with_outcomes(
        &self,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let limit_i64 = limit as i64;

        let rows = conn
            .query(
                r#"SELECT
                    t.id, t.task_name, t.prompt, t.task_type, t.status,
                    t.sessions_count, t.max_sessions, t.error_message,
                    COALESCE(t.summary, t.ai_summary) as summary,
                    t.goal_achieved, t.remaining_work,
                    t.created_at, t.updated_at, t.completed_at,
                    l.id as outcome_id, l.status as outcome_status,
                    l.duration_secs, l.iterations, l.strategy,
                    l.tools_used, l.files_modified, l.error_type,
                    l.error_message as outcome_error,
                    l.feedback, l.created_at as outcome_created_at
                FROM task_runs t
                LEFT JOIN learning_outcomes l ON t.id = l.task_id
                ORDER BY t.updated_at DESC
                LIMIT $1"#,
                &[&limit_i64],
            )
            .await
            .map_err(|e| format!("PG get_recent_task_runs_with_outcomes: {}", e))?;

        let results = rows
            .iter()
            .map(|r| {
                let created: chrono::DateTime<chrono::Utc> = r.get(11);
                let updated: chrono::DateTime<chrono::Utc> = r.get(12);
                let completed: Option<chrono::DateTime<chrono::Utc>> = r.get(13);
                let outcome_created: Option<chrono::DateTime<chrono::Utc>> = r.get(24);
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "task_name": r.get::<_, String>(1),
                    "prompt": r.get::<_, Option<String>>(2),
                    "task_type": r.get::<_, String>(3),
                    "status": r.get::<_, String>(4),
                    "sessions_count": r.get::<_, i32>(5),
                    "max_sessions": r.get::<_, i32>(6),
                    "error_message": r.get::<_, Option<String>>(7),
                    "summary": r.get::<_, Option<String>>(8),
                    "goal_achieved": r.get::<_, Option<bool>>(9),
                    "remaining_work": r.get::<_, Option<String>>(10),
                    "created_at": created.to_rfc3339(),
                    "updated_at": updated.to_rfc3339(),
                    "completed_at": completed.map(|dt| dt.to_rfc3339()),
                    "outcome_id": r.get::<_, Option<String>>(14),
                    "outcome_status": r.get::<_, Option<String>>(15),
                    "duration_secs": r.get::<_, Option<f64>>(16),
                    "iterations": r.get::<_, Option<i32>>(17),
                    "strategy": r.get::<_, Option<String>>(18),
                    "tools_used": r.get::<_, Option<String>>(19),
                    "files_modified": r.get::<_, Option<String>>(20),
                    "error_type": r.get::<_, Option<String>>(21),
                    "outcome_error": r.get::<_, Option<String>>(22),
                    "feedback": r.get::<_, Option<String>>(23),
                    "outcome_created_at": outcome_created.map(|dt| dt.to_rfc3339()),
                })
            })
            .collect();

        Ok(results)
    }

    /// Get workflow_name (or task_name as fallback) for a task run.
    pub async fn get_source_workflow_name_for_task(
        &self,
        task_run_id: &str,
    ) -> Result<Option<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                "SELECT COALESCE(workflow_name, task_name) FROM task_runs WHERE id = $1",
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_source_workflow_name_for_task: {}", e))?;
        Ok(row.map(|r| r.get(0)))
    }

    /// Reopen a completed/failed task run for additional iterations.
    pub async fn reopen_task_run(
        &self,
        id: &str,
        additional_sessions: u32,
    ) -> Result<TaskRun, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let add_i32 = additional_sessions as i32;
        conn.execute(
            r#"UPDATE task_runs SET
                status = 'running',
                max_sessions = COALESCE(max_sessions, 0) + $2,
                error_message = NULL,
                updated_at = NOW()
               WHERE id = $1"#,
            &[&id, &add_i32],
        )
        .await
        .map_err(|e| format!("PG reopen_task_run: {}", e))?;

        self.get_task_run(id)
            .await?
            .ok_or_else(|| format!("Task run not found after reopen: {}", id))
    }

    /// Get child task runs for a parent task run.
    pub async fn get_child_task_runs(&self, parent_id: &str) -> Result<Vec<TaskRun>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT id FROM task_runs WHERE parent_task_run_id = $1 ORDER BY created_at ASC",
                &[&parent_id],
            )
            .await
            .map_err(|e| format!("PG get_child_task_runs: {}", e))?;

        let mut results = Vec::new();
        for row in &rows {
            let child_id: String = row.get(0);
            if let Some(tr) = self.get_task_run(&child_id).await? {
                results.push(tr);
            }
        }
        Ok(results)
    }

    /// Get result_data for a task run.
    pub async fn get_task_run_result_data(&self, id: &str) -> Result<Option<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt("SELECT result_data FROM task_runs WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("PG get_task_run_result_data: {}", e))?;
        Ok(row.and_then(|r| r.get::<_, Option<String>>(0)))
    }

    /// Get task output tail (last N characters).
    pub async fn get_task_output_tail(
        &self,
        id: &str,
        tail_chars: usize,
    ) -> Result<String, String> {
        let output = self.get_task_output(id).await?;
        if output.len() <= tail_chars {
            Ok(output)
        } else {
            Ok(output[output.len() - tail_chars..].to_string())
        }
    }

    /// Get the latest mobile state for a task run.
    pub async fn get_latest_mobile_state(
        &self,
        task_run_id: &str,
    ) -> Result<Option<crate::database::types::MobileState>, String> {
        let states =
            crate::database::pg::PgDb::get_mobile_states(self, task_run_id, Some(1)).await?;
        Ok(states.into_iter().next())
    }

    /// Delete mobile data for a task run.
    pub async fn delete_mobile_data_for_task(&self, task_run_id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "DELETE FROM task_run_mobile_logs WHERE task_run_id = $1",
            &[&task_run_id],
        )
        .await
        .map_err(|e| format!("PG delete_mobile_logs: {}", e))?;
        conn.execute(
            "DELETE FROM task_run_mobile_state WHERE task_run_id = $1",
            &[&task_run_id],
        )
        .await
        .map_err(|e| format!("PG delete_mobile_state: {}", e))?;
        Ok(())
    }

    /// Set auto_continue for a task run.
    pub async fn set_task_auto_continue(
        &self,
        id: &str,
        auto_continue: bool,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE task_runs SET auto_continue = $2, updated_at = NOW() WHERE id = $1",
            &[&id, &auto_continue],
        )
        .await
        .map_err(|e| format!("PG set_task_auto_continue: {}", e))?;
        Ok(())
    }

    /// Update execution steps JSON for a task run.
    pub async fn update_task_run_execution_steps(
        &self,
        id: &str,
        steps_json: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE task_runs SET execution_steps_json = $2, updated_at = NOW() WHERE id = $1",
            &[&id, &steps_json],
        )
        .await
        .map_err(|e| format!("PG update_task_run_execution_steps: {}", e))?;
        Ok(())
    }

    /// Get all verification phase results for a task run.
    pub async fn get_all_verification_phase_results(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn.query(
            "SELECT result_json FROM workflow_verification_phase_results WHERE task_run_id = $1 ORDER BY iteration ASC",
            &[&task_run_id],
        ).await.map_err(|e| format!("PG get_all_verification_phase_results: {}", e))?;

        let mut results = Vec::new();
        for row in &rows {
            let json_str: String = row.get(0);
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&json_str) {
                results.push(parsed);
            }
        }
        Ok(results)
    }

    /// Get latest verification results for a task run.
    pub async fn get_latest_verification_results(
        &self,
        task_run_id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_opt(
            "SELECT result_json FROM workflow_verification_phase_results WHERE task_run_id = $1 ORDER BY iteration DESC LIMIT 1",
            &[&task_run_id],
        ).await.map_err(|e| format!("PG get_latest_verification_results: {}", e))?;

        match row {
            Some(r) => {
                let json_str: String = r.get(0);
                Ok(serde_json::from_str(&json_str).ok())
            }
            None => Ok(None),
        }
    }

    /// Get verification results for a specific iteration.
    pub async fn get_iteration_verification_results(
        &self,
        task_run_id: &str,
        iteration: u32,
    ) -> Result<Option<serde_json::Value>, String> {
        self.get_verification_phase_result(task_run_id, iteration)
            .await
    }

    /// Get execution spans for a task run with optional server-side filtering.
    pub async fn get_execution_spans(
        &self,
        execution_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        self.get_execution_spans_filtered(execution_id, None, None, None)
            .await
    }

    /// Get execution spans with optional filtering pushed down to SQL.
    ///
    /// - `name_pattern`: ILIKE pattern for span_type (e.g. "workflow.%")
    /// - `min_duration_ms`: only spans with duration_ms >= this value
    /// - `limit`: max rows returned (default unlimited)
    pub async fn get_execution_spans_filtered(
        &self,
        execution_id: &str,
        name_pattern: Option<&str>,
        min_duration_ms: Option<i64>,
        limit: Option<u32>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Build dynamic query with optional WHERE clauses
        let mut query = String::from(
            r#"SELECT id, execution_id, name, phase, iteration,
                      start_ts, end_ts, duration_ms, attributes
               FROM execution_spans
               WHERE execution_id = $1"#,
        );

        let mut param_idx = 2u32;
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> =
            vec![Box::new(execution_id.to_string())];

        if let Some(pattern) = name_pattern {
            query.push_str(&format!(" AND name ILIKE ${}", param_idx));
            params.push(Box::new(pattern.to_string()));
            param_idx += 1;
        }

        if let Some(min_dur) = min_duration_ms {
            query.push_str(&format!(" AND duration_ms >= ${}", param_idx));
            params.push(Box::new(min_dur));
            param_idx += 1;
        }

        query.push_str(" ORDER BY start_ts ASC");

        if let Some(lim) = limit {
            query.push_str(&format!(" LIMIT ${}", param_idx));
            params.push(Box::new(lim as i64));
        }

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();

        let rows = conn
            .query(&query, &param_refs)
            .await
            .map_err(|e| format!("PG get_execution_spans_filtered: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.get::<_, i64>(0),
                    "execution_id": r.get::<_, Option<String>>(1),
                    "span_type": r.get::<_, String>(2),
                    "phase": r.get::<_, Option<String>>(3),
                    "iteration": r.get::<_, Option<i32>>(4),
                    "started_at": r.get::<_, String>(5),
                    "completed_at": r.get::<_, Option<String>>(6),
                    "duration_ms": r.get::<_, Option<i64>>(7),
                    "metadata": r.get::<_, Option<String>>(8),
                })
            })
            .collect())
    }

    /// Get step progress markers for a checkpoint.
    pub async fn get_step_progress_markers(
        &self,
        checkpoint_id: &str,
    ) -> Result<Vec<crate::workflow_state::StepProgressMarker>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT id, checkpoint_id, marker_type, current_value, total_value,
                      description, data_json, created_at
               FROM step_progress_markers
               WHERE checkpoint_id = $1
               ORDER BY created_at DESC"#,
                &[&checkpoint_id],
            )
            .await
            .map_err(|e| format!("PG get_step_progress_markers: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let created: chrono::DateTime<chrono::Utc> = r.get(7);
                crate::workflow_state::StepProgressMarker {
                    id: r.get(0),
                    checkpoint_id: r.get(1),
                    marker_type: r.get(2),
                    current_value: r.get::<_, i64>(3) as u64,
                    total_value: r.get::<_, Option<i64>>(4).map(|v| v as u64),
                    description: r.get(5),
                    data_json: r.get(6),
                    created_at: created.to_rfc3339(),
                }
            })
            .collect())
    }

    /// Get latest step progress marker for a checkpoint.
    pub async fn get_latest_step_progress_marker(
        &self,
        checkpoint_id: &str,
    ) -> Result<Option<crate::workflow_state::StepProgressMarker>, String> {
        let markers = self.get_step_progress_markers(checkpoint_id).await?;
        Ok(markers.into_iter().next())
    }

    /// Get latest verification plan for a task run (stub — returns None until PG table exists).
    pub async fn get_latest_verification_plan(
        &self,
        _task_run_id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        // Verification plans table may not exist in PG yet. Return None.
        Ok(None)
    }

    // ========================================================================
    // Review Subtask Operations (raw SQL — columns not yet in Clorinde)
    // ========================================================================

    /// Set the is_review and blocks_parent flags on a task run.
    /// Used after Clorinde-based create_task_run to set review-specific columns.
    pub async fn set_review_flags(
        &self,
        task_run_id: &str,
        is_review: bool,
        blocks_parent: bool,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE task_runs SET is_review = $1, blocks_parent = $2 WHERE id = $3",
            &[
                &is_review as &(dyn tokio_postgres::types::ToSql + Sync),
                &blocks_parent as &(dyn tokio_postgres::types::ToSql + Sync),
                &task_run_id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG set_review_flags: {}", e))?;
        Ok(())
    }

    /// Check if a task has any incomplete blocking children.
    pub async fn has_blocking_incomplete_children(
        &self,
        task_run_id: &str,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_one(
                "SELECT COUNT(*) FROM task_runs \
                 WHERE parent_task_run_id = $1 \
                   AND blocks_parent = true \
                   AND status NOT IN ('complete', 'failed', 'stopped')",
                &[&task_run_id as &(dyn tokio_postgres::types::ToSql + Sync)],
            )
            .await
            .map_err(|e| format!("PG has_blocking_incomplete_children: {}", e))?;
        let count: i64 = row.get(0);
        Ok(count > 0)
    }

    /// Check if a review subtask already exists for a given parent + PR number.
    pub async fn has_review_subtask_for_pr(
        &self,
        parent_task_run_id: &str,
        pr_number: u64,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        // Use "PR #N (" pattern to avoid false matches (e.g. PR #12 matching PR #123)
        let pr_pattern = format!("%PR #{} (%", pr_number);
        let row = conn
            .query_one(
                "SELECT EXISTS(\
                    SELECT 1 FROM task_runs \
                    WHERE parent_task_run_id = $1 \
                      AND is_review = true \
                      AND task_name LIKE $2 \
                      AND status NOT IN ('failed', 'stopped')\
                 )",
                &[
                    &parent_task_run_id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &pr_pattern as &(dyn tokio_postgres::types::ToSql + Sync),
                ],
            )
            .await
            .map_err(|e| format!("PG has_review_subtask_for_pr: {}", e))?;
        let exists: bool = row.get(0);
        Ok(exists)
    }

    /// Set the result_data JSON column on a task run.
    pub async fn set_task_run_result_data(
        &self,
        task_run_id: &str,
        result_data: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE task_runs SET result_data = $1, updated_at = NOW() WHERE id = $2",
            &[
                &result_data as &(dyn tokio_postgres::types::ToSql + Sync),
                &task_run_id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG set_task_run_result_data: {}", e))?;
        Ok(())
    }

    /// Merge JSON into result_data without overwriting existing keys.
    /// If result_data is NULL or empty, sets it to the provided JSON.
    /// Otherwise merges via jsonb `||` (new keys win on conflict).
    pub async fn merge_task_run_result_data(
        &self,
        task_run_id: &str,
        json_str: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            r#"UPDATE task_runs
               SET result_data = CASE
                   WHEN result_data IS NULL OR result_data = '' THEN $1
                   ELSE (result_data::jsonb || $1::jsonb)::text
               END,
               updated_at = NOW()
               WHERE id = $2"#,
            &[
                &json_str as &(dyn tokio_postgres::types::ToSql + Sync),
                &task_run_id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG merge_task_run_result_data: {}", e))?;
        Ok(())
    }

    /// Get the parent_task_run_id and blocks_parent flag for a task.
    /// Returns Some((parent_id, blocks_parent)) if the task has a parent, None otherwise.
    /// Uses raw SQL since blocks_parent is not in the Clorinde-generated struct.
    pub async fn get_blocking_parent_info(
        &self,
        task_run_id: &str,
    ) -> Result<Option<(String, bool)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT parent_task_run_id, COALESCE(blocks_parent, false) \
                 FROM task_runs WHERE id = $1 AND parent_task_run_id IS NOT NULL",
                &[&task_run_id as &(dyn tokio_postgres::types::ToSql + Sync)],
            )
            .await
            .map_err(|e| format!("PG get_blocking_parent_info: {}", e))?;
        if let Some(row) = rows.first() {
            let parent_id: String = row.get(0);
            let blocks_parent: bool = row.get(1);
            if parent_id.is_empty() {
                Ok(None)
            } else {
                Ok(Some((parent_id, blocks_parent)))
            }
        } else {
            Ok(None)
        }
    }

    /// Save a complete task run automation row in a single INSERT.
    ///
    /// Used by `RunRecorder::finish_*_with_db` to persist a fully-built
    /// run recording (including states, transitions, anomalies, etc.) at the
    /// end of the workflow. Unlike the create+update pattern, this writes
    /// everything in one statement.
    ///
    /// Note: the `task_run_automation` schema has no `screenshots` column;
    /// screenshot records are not persisted by this path.
    #[allow(clippy::too_many_arguments)]
    pub async fn save_task_run_automation(
        &self,
        id: &str,
        task_run_id: &str,
        workflow_name: Option<&str>,
        started_at: &str,
        ended_at: &str,
        duration_ms: i32,
        automation_status: &str,
        success: Option<bool>,
        error_type: Option<&str>,
        error_message: Option<&str>,
        actions_summary: Option<&str>,
        states_visited: Option<&str>,
        transitions_executed: Option<&str>,
        template_matches: Option<&str>,
        anomalies: Option<&str>,
        iteration_number: u32,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let iter_num = iteration_number as i32;
        conn.execute(
            r#"INSERT INTO task_run_automation (
                id, task_run_id, workflow_name, started_at, ended_at, duration_ms,
                automation_status, success, error_type, error_message,
                actions_summary, states_visited, transitions_executed,
                template_matches, anomalies, iteration_number
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)"#,
            &[
                &id,
                &task_run_id,
                &workflow_name,
                &started_at,
                &ended_at,
                &duration_ms,
                &automation_status,
                &success,
                &error_type,
                &error_message,
                &actions_summary,
                &states_visited,
                &transitions_executed,
                &template_matches,
                &anomalies,
                &iter_num,
            ],
        )
        .await
        .map_err(|e| format!("PG save_task_run_automation: {}", e))?;
        Ok(())
    }
}
