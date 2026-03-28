//! PostgreSQL workflow AI session operations.

use super::PgDb;
use crate::database::types::AiSessionSummary;
use tracing::info;

impl PgDb {
    /// Get recent AI sessions (chat-type task runs) for the sidebar.
    pub async fn get_ai_sessions(
        &self,
        limit: u32,
        runner_port: Option<u16>,
    ) -> Result<Vec<AiSessionSummary>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let limit_i64 = limit as i64;
        let port_i64: Option<i64> = runner_port.map(|p| p as i64);

        let rows = conn
            .query(
                r#"
                SELECT id, task_name, status,
                       COALESCE(updated_at::TEXT, created_at::TEXT) as updated_at,
                       created_at::TEXT
                FROM task_runs
                WHERE workflow_type = 'chat'
                  AND ($1::BIGINT IS NULL OR runner_port IS NULL OR runner_port = $1)
                ORDER BY COALESCE(updated_at, created_at) DESC
                LIMIT $2
                "#,
                &[
                    &port_i64 as &(dyn tokio_postgres::types::ToSql + Sync),
                    &limit_i64,
                ],
            )
            .await
            .map_err(|e| format!("PG get_ai_sessions: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| AiSessionSummary {
                id: r.get(0),
                task_name: r.get(1),
                status: r.get(2),
                updated_at: r.get(3),
                created_at: r.get(4),
            })
            .collect())
    }

    /// Get running AI sessions (chat-type task runs that are still running).
    pub async fn get_running_ai_sessions(
        &self,
        runner_port: Option<u16>,
    ) -> Result<Vec<crate::database::types::TaskRun>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let port_i64: Option<i64> = runner_port.map(|p| p as i64);

        let rows = conn
            .query(
                r#"
                SELECT id, task_name, prompt, status, sessions_count, max_sessions,
                       error_message, auto_continue, execution_steps_json, log_sources_json,
                       COALESCE(summary, ai_summary) as summary, ai_summary,
                       goal_achieved, remaining_work, summary_generated_at::TEXT,
                       workspace_id, triggered_by,
                       created_at::TEXT, updated_at::TEXT, completed_at::TEXT,
                       task_type, config_id, workflow_name
                FROM task_runs
                WHERE status = 'running' AND workflow_type = 'chat'
                  AND ($1::BIGINT IS NULL OR runner_port IS NULL OR runner_port = $1)
                ORDER BY COALESCE(updated_at, created_at) DESC
                "#,
                &[&port_i64 as &(dyn tokio_postgres::types::ToSql + Sync)],
            )
            .await
            .map_err(|e| format!("PG get_running_ai_sessions: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| crate::database::types::TaskRun {
                id: r.get(0),
                task_name: r.get(1),
                prompt: r.get(2),
                status: r.get(3),
                sessions_count: r.get::<_, i32>(4) as u32,
                max_sessions: r.get::<_, Option<i32>>(5).map(|v| v as u32),
                output_log: String::new(),
                error_message: r.get(6),
                auto_continue: r.get::<_, bool>(7),
                execution_steps_json: r.get(8),
                log_sources_json: r.get(9),
                summary: r.get(10),
                ai_summary: r.get(11),
                goal_achieved: r.get(12),
                remaining_work: r.get(13),
                summary_generated_at: r.get(14),
                workspace_id: r.get(15),
                triggered_by: r.get(16),
                created_at: r.get::<_, Option<String>>(17).unwrap_or_default(),
                updated_at: r.get::<_, Option<String>>(18).unwrap_or_default(),
                completed_at: r.get(19),
                task_type: r.get::<_, Option<String>>(20).unwrap_or_else(|| "task".to_string()),
                config_id: r.get(21),
                workflow_name: r.get(22),
                workflow_id: None,
                transition_history_json: None,
                workflow_type: Some("chat".to_string()),
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
            })
            .collect())
    }

    /// Create (or upsert) a workflow AI session record.
    /// Returns the row ID of the inserted/updated session.
    pub async fn create_workflow_ai_session(
        &self,
        task_run_id: &str,
        iteration: i32,
        phase: &str,
        stage_index: Option<i32>,
        claude_cli_session_id: &str,
    ) -> Result<i64, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        // Use ON CONFLICT on the unique index (task_run_id, iteration, phase, COALESCE(stage_index, -1)).
        // PG doesn't allow COALESCE in ON CONFLICT target directly, so we do a manual check-then-upsert.
        let coalesced_stage = stage_index.unwrap_or(-1);

        let existing = conn
            .query_opt(
                r#"SELECT id FROM workflow_ai_sessions
                   WHERE task_run_id = $1 AND iteration = $2 AND phase = $3
                     AND COALESCE(stage_index, -1) = $4"#,
                &[&task_run_id, &iteration, &phase, &coalesced_stage],
            )
            .await
            .map_err(|e| format!("PG create_workflow_ai_session query: {}", e))?;

        let row_id: i64 = if let Some(row) = existing {
            let id: i64 = row.get(0);
            conn.execute(
                r#"UPDATE workflow_ai_sessions
                   SET claude_cli_session_id = $1, session_started_at = NOW(),
                       session_completed_at = NULL, output_length = 0, status = 'running'
                   WHERE id = $2"#,
                &[&claude_cli_session_id, &id],
            )
            .await
            .map_err(|e| format!("PG create_workflow_ai_session update: {}", e))?;
            id
        } else {
            let row = conn
                .query_one(
                    r#"INSERT INTO workflow_ai_sessions
                       (task_run_id, iteration, phase, stage_index, claude_cli_session_id, session_started_at, status)
                       VALUES ($1, $2, $3, $4, $5, NOW(), 'running')
                       RETURNING id"#,
                    &[&task_run_id, &iteration, &phase, &stage_index, &claude_cli_session_id],
                )
                .await
                .map_err(|e| format!("PG create_workflow_ai_session insert: {}", e))?;
            row.get(0)
        };

        info!(
            "Created workflow AI session: task={}, iter={}, phase={}, cli_session={}",
            task_run_id, iteration, phase, claude_cli_session_id
        );
        Ok(row_id)
    }

    /// Mark a workflow AI session as completed, failed, or interrupted.
    pub async fn complete_workflow_ai_session(
        &self,
        task_run_id: &str,
        iteration: i32,
        phase: &str,
        stage_index: Option<i32>,
        status: &str,
        output_length: i64,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let coalesced_stage = stage_index.unwrap_or(-1);
        let output_len_i32 = output_length as i32;

        conn.execute(
            r#"UPDATE workflow_ai_sessions
               SET status = $1, session_completed_at = NOW(), output_length = $2
               WHERE task_run_id = $3 AND iteration = $4 AND phase = $5
                 AND COALESCE(stage_index, -1) = $6"#,
            &[&status, &output_len_i32, &task_run_id, &iteration, &phase, &coalesced_stage],
        )
        .await
        .map_err(|e| format!("PG complete_workflow_ai_session: {}", e))?;

        Ok(())
    }

    /// Create a new session (orchestration session).
    pub async fn create_session(
        &self,
        id: &str,
        session_type: &str,
        name: &str,
        workflow_name: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            r#"INSERT INTO sessions (id, session_type, name, status, created_at, updated_at, workflow_name, run_id)
               VALUES ($1, $2, $3, 'starting', NOW(), NOW(), $4, $5)"#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &session_type as &(dyn tokio_postgres::types::ToSql + Sync),
                &name as &(dyn tokio_postgres::types::ToSql + Sync),
                &workflow_name as &(dyn tokio_postgres::types::ToSql + Sync),
                &run_id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG create_session: {}", e))?;

        // Record start event
        conn.execute(
            r#"INSERT INTO session_events (session_id, event_type, message, timestamp)
               VALUES ($1, 'started', 'Session started', NOW())"#,
            &[&id],
        )
        .await
        .map_err(|e| format!("PG create_session event: {}", e))?;

        Ok(())
    }

    /// Update session status.
    pub async fn update_session_status(
        &self,
        session_id: &str,
        status: &str,
        current_phase: Option<u32>,
        error_message: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let completed = status == "completed" || status == "failed";
        let phase: Option<i32> = current_phase.map(|p| p as i32);

        conn.execute(
            r#"UPDATE sessions SET
                status = $1,
                current_phase = COALESCE($2, current_phase),
                completed = $3,
                completed_at = CASE WHEN $3 THEN NOW() ELSE completed_at END,
                error_message = COALESCE($4, error_message),
                updated_at = NOW()
            WHERE id = $5"#,
            &[
                &status as &(dyn tokio_postgres::types::ToSql + Sync),
                &phase as &(dyn tokio_postgres::types::ToSql + Sync),
                &completed as &(dyn tokio_postgres::types::ToSql + Sync),
                &error_message as &(dyn tokio_postgres::types::ToSql + Sync),
                &session_id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG update_session_status: {}", e))?;

        // Record event
        let message = format!("Status changed to {}", status);
        conn.execute(
            r#"INSERT INTO session_events (session_id, event_type, message, timestamp)
               VALUES ($1, $2, $3, NOW())"#,
            &[
                &session_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &status as &(dyn tokio_postgres::types::ToSql + Sync),
                &message as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG update_session_status event: {}", e))?;

        Ok(())
    }

    /// Get the most recent AI session for a task run, filtered by phase and iteration.
    /// Returns (claude_cli_session_id, status) if found.
    pub async fn get_workflow_ai_session(
        &self,
        task_run_id: &str,
        iteration: i32,
        phase: &str,
    ) -> Result<Option<(String, String)>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"SELECT claude_cli_session_id, status
                   FROM workflow_ai_sessions
                   WHERE task_run_id = $1 AND iteration = $2 AND phase = $3
                   ORDER BY id DESC
                   LIMIT 1"#,
                &[&task_run_id, &iteration, &phase],
            )
            .await
            .map_err(|e| format!("PG get_workflow_ai_session: {}", e))?;

        Ok(row.map(|r| (r.get(0), r.get(1))))
    }
}
