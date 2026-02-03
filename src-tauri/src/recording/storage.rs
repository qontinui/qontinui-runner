//! Storage operations for recordings and actions

use crate::database::CheckpointDb;
use chrono::Utc;
use rusqlite::params;
use std::sync::Arc;

use super::types::*;

/// Storage layer for recording persistence
pub struct RecordingStorage {
    db: Arc<CheckpointDb>,
}

impl RecordingStorage {
    pub fn new(db: Arc<CheckpointDb>) -> Self {
        Self { db }
    }

    /// Create a new recording session
    pub fn create_recording(&self, input: CreateRecordingInput) -> Result<Recording, String> {
        let conn = self.db.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let browser_info_json = input
            .browser_info
            .as_ref()
            .map(|b| serde_json::to_string(b).unwrap_or_default());

        let tags_json = serde_json::to_string(&input.tags).unwrap_or_else(|_| "[]".to_string());

        conn.execute(
            r#"
            INSERT INTO recordings (
                id, name, description, base_url, action_count, status,
                started_at, browser_info, tab_id, tags, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, 0, 'recording', ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                id,
                input.name,
                input.description,
                input.base_url,
                now,
                browser_info_json,
                input.tab_id,
                tags_json,
                now,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create recording: {}", e))?;

        self.get_recording(&id)?
            .ok_or_else(|| "Failed to retrieve created recording".to_string())
    }

    /// Get a recording by ID
    pub fn get_recording(&self, id: &str) -> Result<Option<Recording>, String> {
        let conn = self.db.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, base_url, action_count, status,
                       started_at, completed_at, duration_ms, browser_info, tab_id,
                       tags, created_at, updated_at
                FROM recordings
                WHERE id = ?1
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let result = stmt
            .query_row(params![id], |row| {
                let status_str: String = row.get(5)?;
                let browser_info_json: Option<String> = row.get(9)?;
                let tags_json: String = row.get(11)?;

                Ok(Recording {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    base_url: row.get(3)?,
                    action_count: row.get(4)?,
                    status: status_str.parse().unwrap_or(RecordingStatus::Recording),
                    started_at: row.get(6)?,
                    completed_at: row.get(7)?,
                    duration_ms: row.get(8)?,
                    browser_info: browser_info_json.and_then(|s| serde_json::from_str(&s).ok()),
                    tab_id: row.get(10)?,
                    tags: serde_json::from_str(&tags_json).unwrap_or_default(),
                    created_at: row.get(12)?,
                    updated_at: row.get(13)?,
                })
            })
            .optional()
            .map_err(|e| format!("Failed to get recording: {}", e))?;

        Ok(result)
    }

    /// List all recordings
    pub fn list_recordings(
        &self,
        status: Option<RecordingStatus>,
        limit: Option<i32>,
    ) -> Result<Vec<Recording>, String> {
        let conn = self.db.get_conn()?;

        let query = match status {
            Some(s) => format!(
                r#"
                SELECT id, name, description, base_url, action_count, status,
                       started_at, completed_at, duration_ms, browser_info, tab_id,
                       tags, created_at, updated_at
                FROM recordings
                WHERE status = '{}'
                ORDER BY created_at DESC
                LIMIT {}
                "#,
                s,
                limit.unwrap_or(100)
            ),
            None => format!(
                r#"
                SELECT id, name, description, base_url, action_count, status,
                       started_at, completed_at, duration_ms, browser_info, tab_id,
                       tags, created_at, updated_at
                FROM recordings
                ORDER BY created_at DESC
                LIMIT {}
                "#,
                limit.unwrap_or(100)
            ),
        };

        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let status_str: String = row.get(5)?;
                let browser_info_json: Option<String> = row.get(9)?;
                let tags_json: String = row.get(11)?;

                Ok(Recording {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    base_url: row.get(3)?,
                    action_count: row.get(4)?,
                    status: status_str.parse().unwrap_or(RecordingStatus::Recording),
                    started_at: row.get(6)?,
                    completed_at: row.get(7)?,
                    duration_ms: row.get(8)?,
                    browser_info: browser_info_json.and_then(|s| serde_json::from_str(&s).ok()),
                    tab_id: row.get(10)?,
                    tags: serde_json::from_str(&tags_json).unwrap_or_default(),
                    created_at: row.get(12)?,
                    updated_at: row.get(13)?,
                })
            })
            .map_err(|e| format!("Failed to list recordings: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Update recording status
    pub fn update_recording_status(
        &self,
        id: &str,
        status: RecordingStatus,
    ) -> Result<Recording, String> {
        let conn = self.db.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Calculate duration if completing
        let duration_ms = if status == RecordingStatus::Completed {
            if let Some(rec) = self.get_recording(id)? {
                let started = chrono::DateTime::parse_from_rfc3339(&rec.started_at)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc));
                let completed = Utc::now();
                started.map(|s| (completed - s).num_milliseconds())
            } else {
                None
            }
        } else {
            None
        };

        conn.execute(
            r#"
            UPDATE recordings SET
                status = ?1,
                completed_at = CASE WHEN ?1 IN ('completed', 'failed', 'cancelled') THEN ?2 ELSE NULL END,
                duration_ms = ?3,
                updated_at = ?4
            WHERE id = ?5
            "#,
            params![status.to_string(), now, duration_ms, now, id],
        )
        .map_err(|e| format!("Failed to update recording status: {}", e))?;

        self.get_recording(id)?
            .ok_or_else(|| "Recording not found".to_string())
    }

    /// Delete a recording and all its actions
    pub fn delete_recording(&self, id: &str) -> Result<(), String> {
        let conn = self.db.get_conn()?;

        conn.execute("DELETE FROM recording_actions WHERE recording_id = ?1", params![id])
            .map_err(|e| format!("Failed to delete recording actions: {}", e))?;

        conn.execute("DELETE FROM recording_exports WHERE recording_id = ?1", params![id])
            .map_err(|e| format!("Failed to delete recording exports: {}", e))?;

        conn.execute("DELETE FROM recordings WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete recording: {}", e))?;

        Ok(())
    }

    /// Add an action to a recording
    pub fn add_action(
        &self,
        recording_id: &str,
        input: AddActionInput,
    ) -> Result<RecordedAction, String> {
        let conn = self.db.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        // Get next sequence number
        let sequence_number: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(sequence_number), 0) + 1 FROM recording_actions WHERE recording_id = ?1",
                params![recording_id],
                |row| row.get(0),
            )
            .unwrap_or(1);

        let target_json =
            serde_json::to_string(&input.target).map_err(|e| format!("Failed to serialize target: {}", e))?;

        let action_data_json = input
            .action_data
            .as_ref()
            .map(|d| serde_json::to_string(d).unwrap_or_default());

        conn.execute(
            r#"
            INSERT INTO recording_actions (
                id, recording_id, sequence_number, action_type, url, page_title,
                target_json, action_data_json, timestamp, duration_ms, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
            params![
                id,
                recording_id,
                sequence_number,
                input.action_type.to_string(),
                input.url,
                input.page_title,
                target_json,
                action_data_json,
                input.timestamp,
                input.duration_ms,
                now,
            ],
        )
        .map_err(|e| format!("Failed to add action: {}", e))?;

        // Update action count
        conn.execute(
            "UPDATE recordings SET action_count = action_count + 1, updated_at = ?1 WHERE id = ?2",
            params![now, recording_id],
        )
        .map_err(|e| format!("Failed to update action count: {}", e))?;

        self.get_action(&id)?
            .ok_or_else(|| "Failed to retrieve created action".to_string())
    }

    /// Get a single action by ID
    pub fn get_action(&self, id: &str) -> Result<Option<RecordedAction>, String> {
        let conn = self.db.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, recording_id, sequence_number, action_type, url, page_title,
                       target_json, action_data_json, screenshot_path, timestamp,
                       duration_ms, created_at
                FROM recording_actions
                WHERE id = ?1
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let result = stmt
            .query_row(params![id], |row| {
                let action_type_str: String = row.get(3)?;
                let target_json: String = row.get(6)?;
                let action_data_json: Option<String> = row.get(7)?;

                Ok(RecordedAction {
                    id: row.get(0)?,
                    recording_id: row.get(1)?,
                    sequence_number: row.get(2)?,
                    action_type: action_type_str.parse().unwrap_or(ActionType::Click),
                    url: row.get(4)?,
                    page_title: row.get(5)?,
                    target: serde_json::from_str(&target_json).unwrap_or_else(|_| ActionTarget {
                        ui_id: None,
                        tag_name: "unknown".to_string(),
                        selector: "".to_string(),
                        xpath: None,
                        text_content: None,
                        bbox: None,
                        attributes: None,
                    }),
                    action_data: action_data_json.and_then(|s| serde_json::from_str(&s).ok()),
                    screenshot_path: row.get(8)?,
                    timestamp: row.get(9)?,
                    duration_ms: row.get(10)?,
                    created_at: row.get(11)?,
                })
            })
            .optional()
            .map_err(|e| format!("Failed to get action: {}", e))?;

        Ok(result)
    }

    /// Get all actions for a recording
    pub fn get_recording_actions(&self, recording_id: &str) -> Result<Vec<RecordedAction>, String> {
        let conn = self.db.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, recording_id, sequence_number, action_type, url, page_title,
                       target_json, action_data_json, screenshot_path, timestamp,
                       duration_ms, created_at
                FROM recording_actions
                WHERE recording_id = ?1
                ORDER BY sequence_number ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(params![recording_id], |row| {
                let action_type_str: String = row.get(3)?;
                let target_json: String = row.get(6)?;
                let action_data_json: Option<String> = row.get(7)?;

                Ok(RecordedAction {
                    id: row.get(0)?,
                    recording_id: row.get(1)?,
                    sequence_number: row.get(2)?,
                    action_type: action_type_str.parse().unwrap_or(ActionType::Click),
                    url: row.get(4)?,
                    page_title: row.get(5)?,
                    target: serde_json::from_str(&target_json).unwrap_or_else(|_| ActionTarget {
                        ui_id: None,
                        tag_name: "unknown".to_string(),
                        selector: "".to_string(),
                        xpath: None,
                        text_content: None,
                        bbox: None,
                        attributes: None,
                    }),
                    action_data: action_data_json.and_then(|s| serde_json::from_str(&s).ok()),
                    screenshot_path: row.get(8)?,
                    timestamp: row.get(9)?,
                    duration_ms: row.get(10)?,
                    created_at: row.get(11)?,
                })
            })
            .map_err(|e| format!("Failed to get recording actions: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Save an export record
    pub fn save_export(
        &self,
        recording_id: &str,
        format: ExportFormat,
        script_content: &str,
        file_name: &str,
        options: Option<&ExportOptions>,
    ) -> Result<RecordingExport, String> {
        let conn = self.db.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let options_json = options.map(|o| serde_json::to_string(o).unwrap_or_default());

        conn.execute(
            r#"
            INSERT INTO recording_exports (
                id, recording_id, export_format, script_content, file_name,
                options_json, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![
                id,
                recording_id,
                format.to_string(),
                script_content,
                file_name,
                options_json,
                now,
            ],
        )
        .map_err(|e| format!("Failed to save export: {}", e))?;

        Ok(RecordingExport {
            id,
            recording_id: recording_id.to_string(),
            export_format: format,
            script_content: script_content.to_string(),
            file_name: file_name.to_string(),
            options: options.cloned(),
            created_at: now,
        })
    }

    /// Get exports for a recording
    pub fn get_recording_exports(&self, recording_id: &str) -> Result<Vec<RecordingExport>, String> {
        let conn = self.db.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, recording_id, export_format, script_content, file_name,
                       options_json, created_at
                FROM recording_exports
                WHERE recording_id = ?1
                ORDER BY created_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(params![recording_id], |row| {
                let format_str: String = row.get(2)?;
                let options_json: Option<String> = row.get(5)?;

                Ok(RecordingExport {
                    id: row.get(0)?,
                    recording_id: row.get(1)?,
                    export_format: format_str.parse().unwrap_or(ExportFormat::Python),
                    script_content: row.get(3)?,
                    file_name: row.get(4)?,
                    options: options_json.and_then(|s| serde_json::from_str(&s).ok()),
                    created_at: row.get(6)?,
                })
            })
            .map_err(|e| format!("Failed to get recording exports: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }
}

// Use rusqlite::OptionalExtension for query_row().optional()
use rusqlite::OptionalExtension;
