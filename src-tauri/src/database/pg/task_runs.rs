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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

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
            now.signed_duration_since(start.with_timezone(&chrono::Utc)).num_milliseconds()
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

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
            now.signed_duration_since(start.with_timezone(&chrono::Utc)).num_milliseconds()
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

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

        Ok(crate::mcp_client::McpCallsResult {
            task_run_id: task_run_id.to_string(),
            calls: calls.clone(),
            count: calls.len(),
            success_count,
            failed_count,
        })
    }

    /// Pause a running task run, setting it to 'paused'.
    pub async fn pause_task_run(&self, id: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                "SELECT COALESCE(workflow_name, task_name) FROM task_runs WHERE id = $1",
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_source_workflow_name_for_task: {}", e))?;
        Ok(row.map(|r| r.get(0)))
    }
}
