//! PostgreSQL recordings CRUD operations (raw SQL).
//!
//! Mirrors the SQLite recording/storage.rs operations.

use super::PgDb;
use crate::recording::types::{Recording, RecordingStatus};
use chrono::Utc;

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
            ) VALUES ($1, $2, $3, $4, 0, 'recording', $5, $6, $7, $8, $9::TIMESTAMPTZ, $9::TIMESTAMPTZ)
            "#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &name as &(dyn tokio_postgres::types::ToSql + Sync),
                &description as &(dyn tokio_postgres::types::ToSql + Sync),
                &base_url as &(dyn tokio_postgres::types::ToSql + Sync),
                &now as &(dyn tokio_postgres::types::ToSql + Sync),
                &browser_info_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &tab_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &tags_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &now as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG create_recording: {}", e))?;

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
                       started_at, completed_at, duration_ms, browser_info, tab_id,
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
                       started_at, completed_at, duration_ms, browser_info, tab_id,
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

    /// Delete a recording by ID.
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

    // -- helpers --

    fn recording_row_to_struct(row: &tokio_postgres::Row) -> Recording {
        let status_str: String = row.get(5);
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
            started_at: row.get(6),
            completed_at: row.get(7),
            duration_ms: duration_ms_raw.map(|v| v as i64),
            browser_info: browser_info_json.and_then(|s| serde_json::from_str(&s).ok()),
            tab_id: row.get(10),
            tags: serde_json::from_str(&tags_json).unwrap_or_default(),
            created_at: row.get(12),
            updated_at: row.get(13),
        }
    }
}
