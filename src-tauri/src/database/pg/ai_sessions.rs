//! PostgreSQL workflow AI session operations.

use super::PgDb;
use tracing::info;

impl PgDb {
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
