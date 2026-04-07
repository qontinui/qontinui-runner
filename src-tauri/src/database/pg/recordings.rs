//! PostgreSQL recordings CRUD operations (raw SQL).
//!
//! Backs the MCP recording/playback HTTP API in `crate::mcp::recordings`.

use super::PgDb;
use crate::recording::types::{
    ActionTarget, ActionType, ExportFormat, ExportOptions, RecordedAction, Recording,
    RecordingExport, RecordingStatus,
};
use chrono::Utc;
use tokio_postgres::types::ToSql;

impl PgDb {
    // ========================================================================
    // Recordings
    // ========================================================================

    /// Create a new recording.
    pub async fn create_recording(
        &self,
        name: &str,
        description: Option<&str>,
        base_url: &str,
        browser_info_json: Option<&str>,
        tab_id: Option<i32>,
        tags: &[String],
    ) -> Result<Recording, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());

        conn.execute(
            r#"
            INSERT INTO recordings (
                id, name, description, base_url, action_count, status,
                started_at, browser_info, tab_id, tags, created_at, updated_at
            ) VALUES ($1, $2, $3, $4, 0, 'recording', NOW(), $5, $6, $7, NOW(), NOW())
            "#,
            &[
                &id as &(dyn ToSql + Sync),
                &name as &(dyn ToSql + Sync),
                &description as &(dyn ToSql + Sync),
                &base_url as &(dyn ToSql + Sync),
                &browser_info_json as &(dyn ToSql + Sync),
                &tab_id as &(dyn ToSql + Sync),
                &tags_json as &(dyn ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG create_recording: {}", e))?;

        // Silence unused-variable lint for `now` — kept in case of future
        // caller-supplied timestamps.
        let _ = now;

        self.get_recording(&id)
            .await?
            .ok_or_else(|| "Recording not found after insert".to_string())
    }

    /// Get a recording by ID.
    pub async fn get_recording(&self, id: &str) -> Result<Option<Recording>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT id, name, description, base_url, action_count, status,
                       started_at::TEXT, completed_at::TEXT, duration_ms, browser_info, tab_id,
                       tags, created_at::TEXT, updated_at::TEXT
                FROM recordings
                WHERE id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_recording: {}", e))?;

        Ok(row.map(|r| Self::recording_row_to_struct(&r)))
    }

    /// List all recordings, ordered by created_at DESC.
    pub async fn list_recordings(&self) -> Result<Vec<Recording>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, name, description, base_url, action_count, status,
                       started_at::TEXT, completed_at::TEXT, duration_ms, browser_info, tab_id,
                       tags, created_at::TEXT, updated_at::TEXT
                FROM recordings
                ORDER BY created_at DESC
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("PG list_recordings: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| Self::recording_row_to_struct(r))
            .collect())
    }

    /// Delete a recording by ID. ON DELETE CASCADE removes actions/exports.
    pub async fn delete_recording(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute("DELETE FROM recordings WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("PG delete_recording: {}", e))?;

        Ok(affected > 0)
    }

    /// Update a recording's status, and set `completed_at` when transitioning
    /// to a terminal status.
    pub async fn update_recording_status(
        &self,
        id: &str,
        status: &str,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let is_terminal = matches!(status, "completed" | "failed" | "cancelled");
        let sql = if is_terminal {
            r#"
            UPDATE recordings
            SET status = $1,
                completed_at = NOW(),
                duration_ms = CAST(
                    EXTRACT(EPOCH FROM (NOW() - started_at)) * 1000 AS INTEGER
                ),
                updated_at = NOW()
            WHERE id = $2
            "#
        } else {
            r#"
            UPDATE recordings
            SET status = $1,
                updated_at = NOW()
            WHERE id = $2
            "#
        };

        let affected = conn
            .execute(sql, &[&status, &id])
            .await
            .map_err(|e| format!("PG update_recording_status: {}", e))?;

        Ok(affected > 0)
    }

    // ========================================================================
    // Recorded Actions
    // ========================================================================

    /// Append a recorded action to a recording. Assigns the next sequence
    /// number, inserts the row, and bumps `recordings.action_count`.
    /// Returns the newly-inserted action's id.
    pub async fn add_recorded_action(
        &self,
        recording_id: &str,
        action: &RecordedAction,
    ) -> Result<String, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let txn = conn
            .transaction()
            .await
            .map_err(|e| format!("PG begin txn: {}", e))?;

        // Verify the recording exists.
        let exists = txn
            .query_opt("SELECT 1 FROM recordings WHERE id = $1", &[&recording_id])
            .await
            .map_err(|e| format!("PG add_recorded_action lookup: {}", e))?
            .is_some();
        if !exists {
            return Err(format!("Recording not found: {}", recording_id));
        }

        // Compute next sequence number.
        let seq_row = txn
            .query_one(
                r#"
                SELECT COALESCE(MAX(sequence_number), 0) + 1
                FROM recording_actions
                WHERE recording_id = $1
                "#,
                &[&recording_id],
            )
            .await
            .map_err(|e| format!("PG next sequence: {}", e))?;
        let sequence_number: i32 = seq_row.get(0);

        let id = if action.id.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            action.id.clone()
        };
        let action_type = action.action_type.to_string();
        let target_json = serde_json::to_string(&action.target)
            .map_err(|e| format!("Failed to serialize target: {}", e))?;
        let action_data_json: Option<String> = action
            .action_data
            .as_ref()
            .map(|d| serde_json::to_string(d).unwrap_or_else(|_| "null".to_string()));
        let duration_ms = action.duration_ms.map(|v| v as i32);

        txn.execute(
            r#"
            INSERT INTO recording_actions (
                id, recording_id, sequence_number, action_type, url, page_title,
                target_json, action_data_json, screenshot_path, timestamp,
                duration_ms, created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, NOW())
            "#,
            &[
                &id as &(dyn ToSql + Sync),
                &recording_id as &(dyn ToSql + Sync),
                &sequence_number as &(dyn ToSql + Sync),
                &action_type as &(dyn ToSql + Sync),
                &action.url as &(dyn ToSql + Sync),
                &action.page_title as &(dyn ToSql + Sync),
                &target_json as &(dyn ToSql + Sync),
                &action_data_json as &(dyn ToSql + Sync),
                &action.screenshot_path as &(dyn ToSql + Sync),
                &action.timestamp as &(dyn ToSql + Sync),
                &duration_ms as &(dyn ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG add_recorded_action insert: {}", e))?;

        txn.execute(
            r#"
            UPDATE recordings
            SET action_count = COALESCE(action_count, 0) + 1,
                updated_at = NOW()
            WHERE id = $1
            "#,
            &[&recording_id],
        )
        .await
        .map_err(|e| format!("PG bump action_count: {}", e))?;

        txn.commit()
            .await
            .map_err(|e| format!("PG commit add_recorded_action: {}", e))?;

        Ok(id)
    }

    /// Get all actions for a recording, ordered by sequence_number.
    pub async fn get_recorded_actions(
        &self,
        recording_id: &str,
    ) -> Result<Vec<RecordedAction>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, recording_id, sequence_number, action_type, url,
                       page_title, target_json, action_data_json, screenshot_path,
                       timestamp, duration_ms, created_at::TEXT
                FROM recording_actions
                WHERE recording_id = $1
                ORDER BY sequence_number ASC
                "#,
                &[&recording_id],
            )
            .await
            .map_err(|e| format!("PG get_recorded_actions: {}", e))?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows.iter() {
            out.push(Self::recorded_action_row_to_struct(r)?);
        }
        Ok(out)
    }

    // ========================================================================
    // Recording Exports
    // ========================================================================

    /// Persist a generated-script export for a recording.
    pub async fn create_recording_export(
        &self,
        recording_id: &str,
        format: &str,
        file_name: &str,
        script_content: &str,
        options: Option<&ExportOptions>,
    ) -> Result<RecordingExport, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();
        let options_json: Option<String> = options
            .map(|o| serde_json::to_string(o).unwrap_or_else(|_| "null".to_string()));

        conn.execute(
            r#"
            INSERT INTO recording_exports (
                id, recording_id, export_format, script_content, file_name,
                options_json, created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, NOW())
            "#,
            &[
                &id as &(dyn ToSql + Sync),
                &recording_id as &(dyn ToSql + Sync),
                &format as &(dyn ToSql + Sync),
                &script_content as &(dyn ToSql + Sync),
                &file_name as &(dyn ToSql + Sync),
                &options_json as &(dyn ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG create_recording_export: {}", e))?;

        // Re-read to pick up created_at.
        let row = conn
            .query_one(
                r#"
                SELECT id, recording_id, export_format, script_content, file_name,
                       options_json, created_at::TEXT
                FROM recording_exports
                WHERE id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG create_recording_export re-read: {}", e))?;

        Self::recording_export_row_to_struct(&row)
    }

    /// Get all exports for a recording, ordered by created_at DESC.
    pub async fn get_recording_exports(
        &self,
        recording_id: &str,
    ) -> Result<Vec<RecordingExport>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, recording_id, export_format, script_content, file_name,
                       options_json, created_at::TEXT
                FROM recording_exports
                WHERE recording_id = $1
                ORDER BY created_at DESC
                "#,
                &[&recording_id],
            )
            .await
            .map_err(|e| format!("PG get_recording_exports: {}", e))?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows.iter() {
            out.push(Self::recording_export_row_to_struct(r)?);
        }
        Ok(out)
    }

    // ========================================================================
    // Row -> struct helpers
    // ========================================================================

    fn recording_row_to_struct(row: &tokio_postgres::Row) -> Recording {
        let status_str: String = row
            .get::<_, Option<String>>(5)
            .unwrap_or_else(|| "recording".to_string());
        let browser_info_json: Option<String> = row.get(9);
        let tags_json: String = row
            .get::<_, Option<String>>(11)
            .unwrap_or_else(|| "[]".to_string());
        let action_count: Option<i32> = row.get(4);
        let duration_ms_raw: Option<i32> = row.get(8);

        Recording {
            id: row.get(0),
            name: row.get(1),
            description: row.get(2),
            base_url: row.get(3),
            action_count: action_count.unwrap_or(0),
            status: status_str.parse().unwrap_or(RecordingStatus::Recording),
            started_at: row
                .get::<_, Option<String>>(6)
                .unwrap_or_default(),
            completed_at: row.get(7),
            duration_ms: duration_ms_raw.map(|v| v as i64),
            browser_info: browser_info_json.and_then(|s| serde_json::from_str(&s).ok()),
            tab_id: row.get(10),
            tags: serde_json::from_str(&tags_json).unwrap_or_default(),
            created_at: row
                .get::<_, Option<String>>(12)
                .unwrap_or_default(),
            updated_at: row
                .get::<_, Option<String>>(13)
                .unwrap_or_default(),
        }
    }

    fn recorded_action_row_to_struct(
        row: &tokio_postgres::Row,
    ) -> Result<RecordedAction, String> {
        let action_type_str: String = row.get(3);
        let action_type: ActionType = action_type_str
            .parse()
            .map_err(|e| format!("Invalid action_type in row: {}", e))?;
        let target_json: String = row.get(6);
        let target: ActionTarget = serde_json::from_str(&target_json)
            .map_err(|e| format!("Invalid target_json: {}", e))?;
        let action_data_json: Option<String> = row.get(7);
        let action_data = action_data_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok());
        let duration_ms: Option<i32> = row.get(10);

        Ok(RecordedAction {
            id: row.get(0),
            recording_id: row.get(1),
            sequence_number: row.get(2),
            action_type,
            url: row.get(4),
            page_title: row.get(5),
            target,
            action_data,
            screenshot_path: row.get(8),
            timestamp: row.get(9),
            duration_ms: duration_ms.map(|v| v as i64),
            created_at: row
                .get::<_, Option<String>>(11)
                .unwrap_or_default(),
        })
    }

    fn recording_export_row_to_struct(
        row: &tokio_postgres::Row,
    ) -> Result<RecordingExport, String> {
        let format_str: String = row.get(2);
        let export_format: ExportFormat = format_str
            .parse()
            .map_err(|e| format!("Invalid export_format in row: {}", e))?;
        let options_json: Option<String> = row.get(5);
        let options = options_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok());

        Ok(RecordingExport {
            id: row.get(0),
            recording_id: row.get(1),
            export_format,
            script_content: row.get(3),
            file_name: row.get(4),
            options,
            created_at: row
                .get::<_, Option<String>>(6)
                .unwrap_or_default(),
        })
    }
}
