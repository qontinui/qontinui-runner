//! PostgreSQL process_sessions and process_session_output operations (raw SQL).
//!
//! Mirrors the SQLite database/process_sessions.rs operations.

use super::PgDb;
use crate::database::types::{ProcessSession, ProcessSessionOutputLine};
use chrono::Utc;

impl PgDb {
    // ========================================================================
    // Process Sessions
    // ========================================================================

    /// Create a new process session.
    pub async fn create_process_session(
        &self,
        id: &str,
        config_id: &str,
        name: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO process_sessions (id, process_config_id, process_name, started_at, state)
            VALUES ($1, $2, $3, $4, 'running')
            "#,
            &[&id, &config_id, &name, &now],
        )
        .await
        .map_err(|e| format!("PG create_process_session: {}", e))?;

        Ok(())
    }

    /// Update a process session (on stop/exit).
    pub async fn update_process_session(
        &self,
        session_id: &str,
        state: &str,
        exit_code: Option<i32>,
        error_count: u32,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = Utc::now().to_rfc3339();
        let err_count = error_count as i32;

        conn.execute(
            r#"
            UPDATE process_sessions
            SET stopped_at = $1, state = $2, exit_code = $3, error_count = $4
            WHERE id = $5
            "#,
            &[
                &now as &(dyn tokio_postgres::types::ToSql + Sync),
                &state as &(dyn tokio_postgres::types::ToSql + Sync),
                &exit_code as &(dyn tokio_postgres::types::ToSql + Sync),
                &err_count as &(dyn tokio_postgres::types::ToSql + Sync),
                &session_id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG update_process_session: {}", e))?;

        Ok(())
    }

    /// Get process sessions, optionally filtered by config_id.
    pub async fn get_process_sessions(
        &self,
        config_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<ProcessSession>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let limit_i64 = limit as i64;

        let rows = if let Some(cid) = config_id {
            conn.query(
                r#"
                SELECT id, process_config_id, process_name, started_at, stopped_at, exit_code, state, error_count
                FROM process_sessions
                WHERE process_config_id = $1
                ORDER BY started_at DESC
                LIMIT $2
                "#,
                &[&cid, &limit_i64],
            )
            .await
        } else {
            conn.query(
                r#"
                SELECT id, process_config_id, process_name, started_at, stopped_at, exit_code, state, error_count
                FROM process_sessions
                ORDER BY started_at DESC
                LIMIT $1
                "#,
                &[&limit_i64],
            )
            .await
        }
        .map_err(|e| format!("PG get_process_sessions: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let error_count: i32 = r.get::<_, Option<i32>>(7).unwrap_or(0);
                ProcessSession {
                    id: r.get(0),
                    process_config_id: r.get(1),
                    process_name: r.get(2),
                    started_at: r.get(3),
                    stopped_at: r.get(4),
                    exit_code: r.get(5),
                    state: r.get(6),
                    error_count: error_count as u32,
                }
            })
            .collect())
    }

    // ========================================================================
    // Process Session Output
    // ========================================================================

    /// Batch insert output lines for a session.
    pub async fn insert_process_session_output(
        &self,
        session_id: &str,
        lines: &[(String, String, String)], // (timestamp, stream, line)
    ) -> Result<(), String> {
        if lines.is_empty() {
            return Ok(());
        }

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        for (timestamp, stream, line) in lines {
            conn.execute(
                "INSERT INTO process_session_output (session_id, timestamp, stream, line) VALUES ($1, $2, $3, $4)",
                &[&session_id, timestamp, stream, line],
            )
            .await
            .map_err(|e| format!("PG insert_process_session_output: {}", e))?;
        }

        Ok(())
    }

    /// Get output lines for a session.
    pub async fn get_process_session_output(
        &self,
        session_id: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<ProcessSessionOutputLine>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let limit_i64 = limit as i64;
        let offset_i64 = offset as i64;

        let rows = conn
            .query(
                r#"
                SELECT id, session_id, timestamp, stream, line
                FROM process_session_output
                WHERE session_id = $1
                ORDER BY id ASC
                LIMIT $2 OFFSET $3
                "#,
                &[&session_id, &limit_i64, &offset_i64],
            )
            .await
            .map_err(|e| format!("PG get_process_session_output: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| ProcessSessionOutputLine {
                id: r.get(0),
                session_id: r.get(1),
                timestamp: r.get(2),
                stream: r.get(3),
                line: r.get(4),
            })
            .collect())
    }

    /// Prune output lines for a session, keeping only the most recent `max_lines`.
    pub async fn prune_session_output_lines(
        &self,
        session_id: &str,
        max_lines: u32,
    ) -> Result<u32, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let max_i64 = max_lines as i64;

        let affected = conn
            .execute(
                r#"
                DELETE FROM process_session_output
                WHERE session_id = $1
                AND id NOT IN (
                    SELECT id FROM process_session_output
                    WHERE session_id = $1
                    ORDER BY id DESC
                    LIMIT $2
                )
                "#,
                &[&session_id, &max_i64],
            )
            .await
            .map_err(|e| format!("PG prune_session_output_lines: {}", e))?;

        Ok(affected as u32)
    }
}
