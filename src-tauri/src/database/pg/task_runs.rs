//! PostgreSQL task run core CRUD operations via Clorinde-generated queries.
//!
//! Migrates the hot-path task run operations. Specialized operations
//! (AI session mgmt, iteration tracking, checkpoint ops) remain on SQLite.

use super::PgDb;
use crate::database::types::*;

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

/// Map a Clorinde GetTaskRun row to TaskRun.
macro_rules! full_task_run {
    ($r:expr) => {{
        TaskRun {
            id: $r.id,
            task_name: $r.task_name,
            prompt: non_empty($r.prompt),
            task_type: if $r.task_type.is_empty() { "task".to_string() } else { $r.task_type },
            status: $r.status,
            sessions_count: $r.sessions_count as u32,
            max_sessions: if $r.max_sessions == 0 { None } else { Some($r.max_sessions as u32) },
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
            created_at: $r.created_at.to_rfc3339(),
            updated_at: $r.updated_at.to_rfc3339(),
            completed_at: if $r.completed_at.timestamp() == 0 { None } else { Some($r.completed_at.to_rfc3339()) },
        }
    }};
}

impl PgDb {
    /// Create a new task run.
    pub async fn create_task_run(&self, input: &CreateTaskRunInput) -> Result<TaskRun, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let task_type: Option<String> = Some(input.task_type.as_deref().unwrap_or("task").to_string());
        let auto_continue = input.auto_continue.unwrap_or(true);
        let effective_root: Option<String> = Some(input.root_task_run_id.clone().unwrap_or_else(|| input.id.clone()));
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

        self.get_task_run(&input.id).await?
            .ok_or_else(|| "Failed to retrieve created task run".to_string())
    }

    /// Get a single task run by ID.
    pub async fn get_task_run(&self, id: &str) -> Result<Option<TaskRun>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let port: Option<i32> = runner_port.map(|p| p as i32);
        let max_results = limit as i64;

        let rows = qontinui_db::queries::task_runs::get_recent_task_runs()
            .bind(&conn, &port, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG get_recent_task_runs: {}", e))?;

        Ok(rows.into_iter().map(|r| TaskRun {
            id: r.id,
            task_name: r.task_name,
            prompt: non_empty(r.prompt),
            task_type: if r.task_type.is_empty() { "task".to_string() } else { r.task_type },
            status: r.status,
            sessions_count: r.sessions_count as u32,
            max_sessions: if r.max_sessions == 0 { None } else { Some(r.max_sessions as u32) },
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
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
            completed_at: if r.completed_at.timestamp() == 0 { None } else { Some(r.completed_at.to_rfc3339()) },
        }).collect())
    }

    /// Get recent task runs with optional workflow_type filter (lightweight).
    pub async fn get_recent_task_runs_filtered(
        &self,
        limit: u32,
        workflow_type: Option<&str>,
        runner_port: Option<u16>,
    ) -> Result<Vec<TaskRun>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let port: Option<i32> = runner_port.map(|p| p as i32);
        let max_results = limit as i64;
        let wt: Option<&str> = workflow_type;

        let rows = qontinui_db::queries::task_runs::get_recent_task_runs_filtered()
            .bind(&conn, &wt, &port, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG get_recent_task_runs_filtered: {}", e))?;

        Ok(rows.into_iter().map(|r| TaskRun {
            id: r.id,
            task_name: r.task_name,
            prompt: non_empty(r.prompt),
            task_type: if r.task_type.is_empty() { "task".to_string() } else { r.task_type },
            status: r.status,
            sessions_count: r.sessions_count as u32,
            max_sessions: if r.max_sessions == 0 { None } else { Some(r.max_sessions as u32) },
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
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
            completed_at: if r.completed_at.timestamp() == 0 { None } else { Some(r.completed_at.to_rfc3339()) },
        }).collect())
    }

    /// Update task run status.
    pub async fn update_task_run_status(&self, id: &str, status: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::task_runs::update_task_run_status()
            .bind(&conn, &status, &id)
            .opt()
            .await
            .map_err(|e| format!("PG update_task_run_status: {}", e))?;
        Ok(())
    }

    /// Complete a task run (status='complete', set completed_at).
    pub async fn complete_task_run(&self, id: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::task_runs::complete_task_run()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG complete_task_run: {}", e))?;
        Ok(())
    }

    /// Fail a task run (status='failed', set error_message and completed_at).
    pub async fn fail_task_run(&self, id: &str, error_message: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::task_runs::fail_task_run()
            .bind(&conn, &error_message, &id)
            .opt()
            .await
            .map_err(|e| format!("PG fail_task_run: {}", e))?;
        Ok(())
    }

    /// Stop a task run with a reason (status='stopped', set error_message and completed_at).
    pub async fn stop_task_run(&self, id: &str, reason: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::task_runs::stop_task_run()
            .bind(&conn, &reason, &id)
            .opt()
            .await
            .map_err(|e| format!("PG stop_task_run: {}", e))?;
        Ok(())
    }

    /// Delete a task run.
    pub async fn delete_task_run(&self, id: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::task_runs::update_task_summary()
            .bind(&conn, &summary, &goal_achieved, &remaining_work, &summary_generated_at, &id)
            .opt()
            .await
            .map_err(|e| format!("PG update_task_summary: {}", e))?;
        Ok(())
    }

    /// Append output to a task run.
    pub async fn append_task_output(&self, id: &str, output: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::task_runs::append_task_output()
            .bind(&conn, &output, &id)
            .await
            .map_err(|e| format!("PG append_task_output: {}", e))?;
        Ok(())
    }

    /// Get task run output.
    pub async fn get_task_output(&self, id: &str) -> Result<String, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let output = qontinui_db::queries::task_runs::get_task_output()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG get_task_output: {}", e))?;
        Ok(output.unwrap_or_default())
    }

    /// Update task run name.
    pub async fn update_task_name(&self, id: &str, name: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::task_runs::update_task_name()
            .bind(&conn, &name, &id)
            .opt()
            .await
            .map_err(|e| format!("PG update_task_name: {}", e))?;
        Ok(())
    }

    /// Update task run result data.
    pub async fn update_task_result_data(&self, id: &str, result_data: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE task_runs SET verification_passed = $1 WHERE id = $2",
            &[&passed, &id],
        )
        .await
        .map_err(|e| format!("PG set_verification_passed: {}", e))?;
        Ok(())
    }

    /// Get iteration commits for a task run (stored as JSON array in iteration_commits column).
    pub async fn get_iteration_commits(
        &self,
        id: &str,
    ) -> Result<Vec<crate::unified_workflow_executor::IterationCommit>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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

    /// Get currently running task runs.
    pub async fn get_running_task_runs(&self, runner_port: Option<u16>) -> Result<Vec<TaskRun>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let port: Option<i32> = runner_port.map(|p| p as i32);

        let rows = qontinui_db::queries::task_runs::get_running_task_runs()
            .bind(&conn, &port)
            .all()
            .await
            .map_err(|e| format!("PG get_running_task_runs: {}", e))?;

        Ok(rows.into_iter().map(|r| {
            TaskRun {
                id: r.id,
                task_name: r.task_name,
                prompt: non_empty(r.prompt),
                task_type: if r.task_type.is_empty() { "task".to_string() } else { r.task_type },
                status: r.status,
                sessions_count: r.sessions_count as u32,
                max_sessions: if r.max_sessions == 0 { None } else { Some(r.max_sessions as u32) },
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
                created_at: r.created_at.to_rfc3339(),
                updated_at: r.updated_at.to_rfc3339(),
                completed_at: if r.completed_at.timestamp() == 0 { None } else { Some(r.completed_at.to_rfc3339()) },
            }
        }).collect())
    }
}
