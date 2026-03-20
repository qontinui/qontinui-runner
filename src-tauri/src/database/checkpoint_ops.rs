//! Checkpoint and session operations.
//!
//! Contains all CheckpointDb methods related to workflow checkpoints and session tracking.

use chrono::Utc;
use rusqlite::{params, Result as SqliteResult};

use super::types::*;
use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Checkpoint/Workflow Operations
    // ========================================================================

    /// Get a checkpoint by workflow name.
    pub fn get_checkpoint(&self, workflow_name: &str) -> Result<Option<CheckpointData>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<CheckpointData> = conn.query_row(
            r#"
            SELECT
                NULL as session_id,
                workflow_name,
                json_extract(checkpoint_data, '$.current_phase') as current_phase,
                json_extract(checkpoint_data, '$.total_phases') as total_phases,
                completed,
                json_extract(checkpoint_data, '$.restart_permitted') as restart_permitted,
                json_extract(checkpoint_data, '$.status') as status,
                run_id,
                json_extract(checkpoint_data, '$.repos_to_process') as repos_to_process,
                json_extract(checkpoint_data, '$.work_completed') as work_completed,
                json_extract(checkpoint_data, '$.items_needing_user_input') as items_needing_user_input,
                created_at,
                updated_at,
                json_extract(checkpoint_data, '$.error_message') as error_message,
                checkpoint_data as extra
            FROM active_workflows
            WHERE workflow_name = ?1
            "#,
            params![workflow_name],
            |row| {
                Ok(CheckpointData {
                    session_id: row.get(0).ok(),
                    workflow_name: row.get(1).ok(),
                    current_phase: row.get::<_, Option<i64>>(2)?.unwrap_or(0) as u32,
                    total_phases: row.get::<_, Option<i64>>(3)?.map(|v| v as u32),
                    completed: row.get::<_, i32>(4)? != 0,
                    restart_permitted: row.get::<_, Option<i32>>(5)?.unwrap_or(0) != 0,
                    status: row.get(6).ok(),
                    run_id: row.get(7).ok(),
                    repos_to_process: row
                        .get::<_, Option<String>>(8)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    work_completed: row
                        .get::<_, Option<String>>(9)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    items_needing_user_input: row
                        .get::<_, Option<String>>(10)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    created_at: row.get(11).ok(),
                    updated_at: row.get(12).ok(),
                    error_message: row.get(13).ok(),
                    extra: row
                        .get::<_, Option<String>>(14)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                })
            },
        );

        match result {
            Ok(checkpoint) => Ok(Some(checkpoint)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get checkpoint: {}", e)),
        }
    }

    /// Save or update a checkpoint.
    pub fn save_checkpoint(&self, data: &CheckpointData) -> Result<(), String> {
        let conn = self.get_conn()?;

        let workflow_name = data
            .workflow_name
            .as_ref()
            .ok_or("workflow_name is required")?;

        let now = Utc::now().to_rfc3339();
        let run_id = data
            .run_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // Build checkpoint_data JSON
        let checkpoint_data = serde_json::json!({
            "current_phase": data.current_phase,
            "total_phases": data.total_phases,
            "completed": data.completed,
            "restart_permitted": data.restart_permitted,
            "status": data.status,
            "repos_to_process": data.repos_to_process,
            "work_completed": data.work_completed,
            "items_needing_user_input": data.items_needing_user_input,
            "error_message": data.error_message,
        });

        // Upsert into active_workflows
        conn.execute(
            r#"
            INSERT INTO active_workflows (workflow_name, checkpoint_data, run_id, phase_field, completion_value, created_at, updated_at, completed)
            VALUES (?1, ?2, ?3, 'current_phase', 12, ?4, ?4, ?5)
            ON CONFLICT(workflow_name) DO UPDATE SET
                checkpoint_data = ?2,
                run_id = CASE WHEN ?3 != '' THEN ?3 ELSE run_id END,
                updated_at = ?4,
                completed = ?5
            "#,
            params![
                workflow_name,
                checkpoint_data.to_string(),
                run_id,
                now,
                data.completed as i32,
            ],
        )
        .map_err(|e| format!("Failed to save checkpoint: {}", e))?;

        // Also record in session_events for history
        if let Some(session_id) = &data.session_id {
            let _ = conn.execute(
                r#"
                INSERT INTO session_events (session_id, event_type, message, timestamp, data)
                VALUES (?1, 'checkpoint_updated', ?2, ?3, ?4)
                "#,
                params![
                    session_id,
                    format!("Phase {} updated", data.current_phase),
                    now,
                    checkpoint_data.to_string(),
                ],
            );
        }

        Ok(())
    }

    /// Delete a checkpoint by workflow name.
    pub fn delete_checkpoint(&self, workflow_name: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows = conn
            .execute(
                "DELETE FROM active_workflows WHERE workflow_name = ?1",
                params![workflow_name],
            )
            .map_err(|e| format!("Failed to delete checkpoint: {}", e))?;

        Ok(rows > 0)
    }

    /// List all active (non-completed) checkpoints.
    pub fn list_active_checkpoints(&self) -> Result<Vec<CheckpointData>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
            SELECT
                workflow_name,
                json_extract(checkpoint_data, '$.current_phase') as current_phase,
                completed,
                run_id,
                created_at,
                updated_at
            FROM active_workflows
            WHERE completed = 0
            ORDER BY updated_at DESC
            "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let checkpoints = stmt
            .query_map([], |row| {
                Ok(CheckpointData {
                    session_id: None,
                    workflow_name: row.get(0).ok(),
                    current_phase: row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u32,
                    total_phases: None,
                    completed: row.get::<_, i32>(2)? != 0,
                    restart_permitted: false,
                    status: None,
                    run_id: row.get(3).ok(),
                    repos_to_process: None,
                    work_completed: None,
                    items_needing_user_input: None,
                    created_at: row.get(4).ok(),
                    updated_at: row.get(5).ok(),
                    error_message: None,
                    extra: None,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(checkpoints)
    }

    /// Check external checkpoint status (for cross-session continuation).
    /// Returns (is_complete, current_phase) or None if not found.
    pub fn check_checkpoint_status(
        &self,
        workflow_name: &str,
        completion_value: u32,
    ) -> Result<Option<(bool, u32)>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<(i32, i64)> = conn.query_row(
            r#"
            SELECT
                completed,
                COALESCE(json_extract(checkpoint_data, '$.current_phase'), 0) as current_phase
            FROM active_workflows
            WHERE workflow_name = ?1
            "#,
            params![workflow_name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );

        match result {
            Ok((completed_flag, current_phase)) => {
                let current = current_phase as u32;
                let is_complete = completed_flag != 0 || current >= completion_value;
                Ok(Some((is_complete, current)))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to check checkpoint status: {}", e)),
        }
    }

    // ========================================================================
    // Session Operations
    // ========================================================================

    /// Create a new session.
    pub fn create_session(
        &self,
        id: &str,
        session_type: &str,
        name: &str,
        workflow_name: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;

        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO sessions (id, session_type, name, status, created_at, updated_at, workflow_name, run_id)
            VALUES (?1, ?2, ?3, 'starting', ?4, ?4, ?5, ?6)
            "#,
            params![id, session_type, name, now, workflow_name, run_id],
        )
        .map_err(|e| format!("Failed to create session: {}", e))?;

        // Record start event
        conn.execute(
            r#"
            INSERT INTO session_events (session_id, event_type, message, timestamp)
            VALUES (?1, 'started', 'Session started', ?2)
            "#,
            params![id, now],
        )
        .map_err(|e| format!("Failed to record session event: {}", e))?;

        Ok(())
    }

    /// Update session status.
    pub fn update_session_status(
        &self,
        session_id: &str,
        status: &str,
        current_phase: Option<u32>,
        error_message: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;

        let now = Utc::now().to_rfc3339();
        let completed = status == "completed" || status == "failed";
        let completed_at = if completed { Some(now.clone()) } else { None };

        conn.execute(
            r#"
            UPDATE sessions SET
                status = ?1,
                current_phase = COALESCE(?2, current_phase),
                completed = ?3,
                completed_at = COALESCE(?4, completed_at),
                error_message = COALESCE(?5, error_message),
                updated_at = ?6
            WHERE id = ?7
            "#,
            params![
                status,
                current_phase.map(|p| p as i64),
                completed as i32,
                completed_at,
                error_message,
                now,
                session_id,
            ],
        )
        .map_err(|e| format!("Failed to update session: {}", e))?;

        // Record event
        conn.execute(
            r#"
            INSERT INTO session_events (session_id, event_type, message, timestamp)
            VALUES (?1, 'status_changed', ?2, ?3)
            "#,
            params![session_id, format!("Status changed to {}", status), now],
        )
        .map_err(|e| format!("Failed to record session event: {}", e))?;

        Ok(())
    }

    /// Get session history/events.
    pub fn get_session_history(
        &self,
        workflow_name: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SessionEvent>, String> {
        let conn = self.get_conn()?;

        // Helper function to map a row to SessionEvent
        fn map_row(row: &rusqlite::Row) -> rusqlite::Result<SessionEvent> {
            Ok(SessionEvent {
                id: row.get(0)?,
                session_id: row.get(1)?,
                event_type: row.get(2)?,
                message: row.get(3)?,
                timestamp: row.get(4)?,
                data: row
                    .get::<_, Option<String>>(5)?
                    .and_then(|s| serde_json::from_str(&s).ok()),
            })
        }

        let events: Vec<SessionEvent> = if let Some(wf) = workflow_name {
            let mut stmt = conn
                .prepare(
                    r#"
                    SELECT e.id, e.session_id, e.event_type, e.message, e.timestamp, e.data
                    FROM session_events e
                    JOIN sessions s ON e.session_id = s.id
                    WHERE s.workflow_name = ?1
                    ORDER BY e.timestamp DESC
                    LIMIT ?2
                    "#,
                )
                .map_err(|e| format!("Failed to prepare query: {}", e))?;

            let result: Vec<SessionEvent> = stmt
                .query_map(params![wf, limit], map_row)
                .map_err(|e| format!("Failed to execute query: {}", e))?
                .filter_map(|r| r.ok())
                .collect();
            result
        } else {
            let mut stmt = conn
                .prepare(
                    r#"
                    SELECT id, session_id, event_type, message, timestamp, data
                    FROM session_events
                    ORDER BY timestamp DESC
                    LIMIT ?1
                    "#,
                )
                .map_err(|e| format!("Failed to prepare query: {}", e))?;

            let result: Vec<SessionEvent> = stmt
                .query_map(params![limit], map_row)
                .map_err(|e| format!("Failed to execute query: {}", e))?
                .filter_map(|r| r.ok())
                .collect();
            result
        };

        Ok(events)
    }
}
