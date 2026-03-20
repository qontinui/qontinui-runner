//! Process session persistence operations.
//!
//! Contains all CheckpointDb methods related to process session management and output logging.

use chrono::Utc;
use rusqlite::params;

use super::types::*;
use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Process Session Persistence
    // ========================================================================

    /// Create a new process session record.
    pub fn create_process_session(
        &self,
        id: &str,
        config_id: &str,
        name: &str,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "INSERT INTO process_sessions (id, process_config_id, process_name, started_at, state)
             VALUES (?1, ?2, ?3, ?4, 'running')",
            params![id, config_id, name, Utc::now().to_rfc3339()],
        )
        .map_err(|e| format!("Failed to create process session: {}", e))?;
        Ok(())
    }

    /// Update a process session (on stop/exit).
    pub fn update_process_session(
        &self,
        session_id: &str,
        state: &str,
        exit_code: Option<i32>,
        error_count: u32,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE process_sessions SET stopped_at = ?1, state = ?2, exit_code = ?3, error_count = ?4
             WHERE id = ?5",
            params![
                Utc::now().to_rfc3339(),
                state,
                exit_code,
                error_count,
                session_id,
            ],
        )
        .map_err(|e| format!("Failed to update process session: {}", e))?;
        Ok(())
    }

    /// Get process sessions, optionally filtered by config_id.
    pub fn get_process_sessions(
        &self,
        config_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<ProcessSession>, String> {
        let conn = self.get_conn()?;
        let mut sessions = Vec::new();

        if let Some(cid) = config_id {
            let mut stmt = conn
                .prepare(
                    "SELECT id, process_config_id, process_name, started_at, stopped_at, exit_code, state, error_count
                     FROM process_sessions
                     WHERE process_config_id = ?1
                     ORDER BY started_at DESC
                     LIMIT ?2",
                )
                .map_err(|e| format!("Failed to prepare query: {}", e))?;

            let rows = stmt
                .query_map(params![cid, limit], |row| {
                    Ok(ProcessSession {
                        id: row.get(0)?,
                        process_config_id: row.get(1)?,
                        process_name: row.get(2)?,
                        started_at: row.get(3)?,
                        stopped_at: row.get(4)?,
                        exit_code: row.get(5)?,
                        state: row.get(6)?,
                        error_count: row.get(7)?,
                    })
                })
                .map_err(|e| format!("Failed to query sessions: {}", e))?;

            for row in rows {
                sessions.push(row.map_err(|e| format!("Failed to read session row: {}", e))?);
            }
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT id, process_config_id, process_name, started_at, stopped_at, exit_code, state, error_count
                     FROM process_sessions
                     ORDER BY started_at DESC
                     LIMIT ?1",
                )
                .map_err(|e| format!("Failed to prepare query: {}", e))?;

            let rows = stmt
                .query_map(params![limit], |row| {
                    Ok(ProcessSession {
                        id: row.get(0)?,
                        process_config_id: row.get(1)?,
                        process_name: row.get(2)?,
                        started_at: row.get(3)?,
                        stopped_at: row.get(4)?,
                        exit_code: row.get(5)?,
                        state: row.get(6)?,
                        error_count: row.get(7)?,
                    })
                })
                .map_err(|e| format!("Failed to query sessions: {}", e))?;

            for row in rows {
                sessions.push(row.map_err(|e| format!("Failed to read session row: {}", e))?);
            }
        }

        Ok(sessions)
    }

    /// Batch insert process session output lines.
    pub fn insert_process_session_output(
        &self,
        session_id: &str,
        lines: &[(String, String, String)], // (timestamp, stream, line)
    ) -> Result<(), String> {
        if lines.is_empty() {
            return Ok(());
        }
        let conn = self.get_conn()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| format!("Failed to begin transaction: {}", e))?;

        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO process_session_output (session_id, timestamp, stream, line)
                     VALUES (?1, ?2, ?3, ?4)",
                )
                .map_err(|e| format!("Failed to prepare insert: {}", e))?;

            for (timestamp, stream, line) in lines {
                stmt.execute(params![session_id, timestamp, stream, line])
                    .map_err(|e| format!("Failed to insert output line: {}", e))?;
            }
        }

        tx.commit()
            .map_err(|e| format!("Failed to commit output lines: {}", e))?;
        Ok(())
    }

    /// Get process session output lines.
    pub fn get_process_session_output(
        &self,
        session_id: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<ProcessSessionOutputLine>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, timestamp, stream, line
                 FROM process_session_output
                 WHERE session_id = ?1
                 ORDER BY id ASC
                 LIMIT ?2 OFFSET ?3",
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let rows = stmt
            .query_map(params![session_id, limit, offset], |row| {
                Ok(ProcessSessionOutputLine {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    timestamp: row.get(2)?,
                    stream: row.get(3)?,
                    line: row.get(4)?,
                })
            })
            .map_err(|e| format!("Failed to query output: {}", e))?;

        let mut output = Vec::new();
        for row in rows {
            output.push(row.map_err(|e| format!("Failed to read output row: {}", e))?);
        }
        Ok(output)
    }

    /// Prune output lines for a session, keeping only the most recent `max_lines`.
    pub fn prune_session_output_lines(
        &self,
        session_id: &str,
        max_lines: u32,
    ) -> Result<u32, String> {
        let conn = self.get_conn()?;
        let deleted: usize = conn
            .execute(
                "DELETE FROM process_session_output
                 WHERE session_id = ?1
                 AND id NOT IN (
                     SELECT id FROM process_session_output
                     WHERE session_id = ?1
                     ORDER BY id DESC
                     LIMIT ?2
                 )",
                params![session_id, max_lines],
            )
            .map_err(|e| format!("Failed to prune session output: {}", e))?;
        Ok(deleted as u32)
    }

    /// Prune old sessions for a config, keeping the most recent `keep_count`.
    pub fn prune_old_process_sessions(
        &self,
        config_id: &str,
        keep_count: u32,
    ) -> Result<u32, String> {
        let conn = self.get_conn()?;
        let deleted: usize = conn
            .execute(
                "DELETE FROM process_sessions
                 WHERE process_config_id = ?1
                 AND id NOT IN (
                     SELECT id FROM process_sessions
                     WHERE process_config_id = ?1
                     ORDER BY started_at DESC
                     LIMIT ?2
                 )",
                params![config_id, keep_count],
            )
            .map_err(|e| format!("Failed to prune sessions: {}", e))?;
        Ok(deleted as u32)
    }
}
