//! PostgreSQL process_sessions and process_session_output operations (raw SQL).

use super::PgDb;
use crate::database::types::{ProcessLogSearchHit, ProcessSession, ProcessSessionOutputLine};
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

    /// Batch insert output lines for a session using a single multi-row INSERT.
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

        // Build a single INSERT with N value tuples: ($1,$2,$3,$4),($5,$6,$7,$8),...
        // Each row has 4 params: session_id, timestamp, stream, line.
        let mut sql = String::from(
            "INSERT INTO process_session_output (session_id, timestamp, stream, line) VALUES ",
        );
        let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
        for (i, (timestamp, stream, line)) in lines.iter().enumerate() {
            if i > 0 {
                sql.push(',');
            }
            let base = i * 4;
            sql.push_str(&format!(
                "(${},${},${},${})",
                base + 1,
                base + 2,
                base + 3,
                base + 4,
            ));
            params.push(&session_id);
            params.push(timestamp);
            params.push(stream);
            params.push(line);
        }

        conn.execute(sql.as_str(), &params[..])
            .await
            .map_err(|e| format!("PG insert_process_session_output: {}", e))?;

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

    /// Get N lines of context (before and after) around a specific timestamp
    /// in the most recent session for a given process name. Useful for error
    /// monitor "show context" — given an error's log_source_name and
    /// log_timestamp, returns the surrounding output lines.
    pub async fn get_process_log_context(
        &self,
        process_name: &str,
        around_timestamp: &str,
        before: u32,
        after: u32,
    ) -> Result<Vec<ProcessSessionOutputLine>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Find the session containing the error: most recent session whose
        // started_at <= error timestamp and (stopped_at IS NULL OR stopped_at >= error timestamp).
        // Fall back to most recent session for the process name if no exact match.
        let session_row = conn
            .query_opt(
                r#"
                SELECT id FROM process_sessions
                WHERE process_name = $1
                  AND started_at <= $2::timestamptz
                  AND (stopped_at IS NULL OR stopped_at >= $2::timestamptz)
                ORDER BY started_at DESC
                LIMIT 1
                "#,
                &[&process_name, &around_timestamp],
            )
            .await
            .map_err(|e| format!("PG get_process_log_context (session lookup): {}", e))?;

        let session_id: String = match session_row {
            Some(r) => r.get(0),
            None => return Ok(Vec::new()),
        };

        let before_i64 = before as i64;
        let after_i64 = after as i64;

        // Fetch `before` lines at or before the timestamp, plus `after` lines after it.
        // Combine and sort by id ASC.
        let rows = conn
            .query(
                r#"
                (
                    SELECT id, session_id, timestamp, stream, line
                    FROM process_session_output
                    WHERE session_id = $1
                      AND timestamp <= $2
                    ORDER BY id DESC
                    LIMIT $3
                )
                UNION ALL
                (
                    SELECT id, session_id, timestamp, stream, line
                    FROM process_session_output
                    WHERE session_id = $1
                      AND timestamp > $2
                    ORDER BY id ASC
                    LIMIT $4
                )
                "#,
                &[&session_id, &around_timestamp, &before_i64, &after_i64],
            )
            .await
            .map_err(|e| format!("PG get_process_log_context (rows): {}", e))?;

        let mut lines: Vec<ProcessSessionOutputLine> = rows
            .iter()
            .map(|r| ProcessSessionOutputLine {
                id: r.get(0),
                session_id: r.get(1),
                timestamp: r.get(2),
                stream: r.get(3),
                line: r.get(4),
            })
            .collect();
        lines.sort_by_key(|l| l.id);
        Ok(lines)
    }

    /// Search across all process_session_output rows. Optionally filter by
    /// config_id and time range. Returns hits with the parent session's
    /// config_id and process_name for context.
    pub async fn search_process_logs(
        &self,
        query: &str,
        config_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<ProcessLogSearchHit>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        let limit_i64 = limit as i64;

        let rows = if let Some(cid) = config_id {
            conn.query(
                r#"
                SELECT pso.id, pso.session_id, pso.timestamp, pso.stream, pso.line,
                       ps.process_config_id, ps.process_name
                FROM process_session_output pso
                JOIN process_sessions ps ON pso.session_id = ps.id
                WHERE pso.line ILIKE $1
                  AND ps.process_config_id = $2
                ORDER BY pso.id DESC
                LIMIT $3
                "#,
                &[&pattern, &cid, &limit_i64],
            )
            .await
        } else {
            conn.query(
                r#"
                SELECT pso.id, pso.session_id, pso.timestamp, pso.stream, pso.line,
                       ps.process_config_id, ps.process_name
                FROM process_session_output pso
                JOIN process_sessions ps ON pso.session_id = ps.id
                WHERE pso.line ILIKE $1
                ORDER BY pso.id DESC
                LIMIT $2
                "#,
                &[&pattern, &limit_i64],
            )
            .await
        }
        .map_err(|e| format!("PG search_process_logs: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| ProcessLogSearchHit {
                id: r.get(0),
                session_id: r.get(1),
                timestamp: r.get(2),
                stream: r.get(3),
                line: r.get(4),
                process_config_id: r.get(5),
                process_name: r.get(6),
            })
            .collect())
    }

    /// Mark any sessions still in 'running' state as 'failed'. Called on
    /// runner startup — sessions in this state can only exist if a previous
    /// runner died without a clean shutdown (the event_loop normally updates
    /// state on natural exit). Without this, the UI shows zombie "running"
    /// sessions from previous runs forever.
    pub async fn mark_orphaned_running_sessions_as_failed(&self) -> Result<u32, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let now = Utc::now().to_rfc3339();
        let affected = conn
            .execute(
                "UPDATE process_sessions \
                 SET state = 'failed', \
                     stopped_at = COALESCE(stopped_at, $1::timestamptz) \
                 WHERE state = 'running'",
                &[&now],
            )
            .await
            .map_err(|e| format!("PG mark_orphaned_running_sessions_as_failed: {}", e))?;

        Ok(affected as u32)
    }

    /// Delete process sessions (and their output via CASCADE) older than the
    /// retention period. Returns the number of sessions deleted.
    pub async fn cleanup_old_process_sessions(
        &self,
        retention_days: u32,
    ) -> Result<u32, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let days = retention_days as f64;
        let affected = conn
            .execute(
                "DELETE FROM process_sessions \
                 WHERE started_at < NOW() - ($1 || ' days')::interval",
                &[&days.to_string()],
            )
            .await
            .map_err(|e| format!("PG cleanup_old_process_sessions: {}", e))?;

        Ok(affected as u32)
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
