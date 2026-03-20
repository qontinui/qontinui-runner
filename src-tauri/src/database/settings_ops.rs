//! Settings, config storage, and JSON file migration operations.
//!
//! Contains all CheckpointDb methods related to settings, config storage, and migration from JSON files.

use chrono::Utc;
use rusqlite::params;
use std::path::PathBuf;
use tracing::{info, warn};

use super::types::*;
use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Settings Operations
    // ========================================================================

    /// Get a setting value.
    pub fn get_setting(&self, key: &str) -> Result<Option<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let result: rusqlite::Result<String> = conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |row| row.get(0),
        );

        match result {
            Ok(value) => {
                let parsed = serde_json::from_str(&value)
                    .map_err(|e| format!("Failed to parse setting value: {}", e))?;
                Ok(Some(parsed))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get setting: {}", e)),
        }
    }

    /// Set a setting value.
    pub fn set_setting(&self, key: &str, value: &serde_json::Value) -> Result<(), String> {
        let conn = self.get_conn()?;

        let now = Utc::now().to_rfc3339();
        let value_str = value.to_string();

        conn.execute(
            r#"
            INSERT INTO settings (key, value, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3
            "#,
            params![key, value_str, now],
        )
        .map_err(|e| format!("Failed to set setting: {}", e))?;

        Ok(())
    }

    /// Get all settings as a JSON object.
    pub fn get_all_settings(&self) -> Result<serde_json::Value, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare("SELECT key, value FROM settings")
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let mut settings = serde_json::Map::new();

        let rows = stmt
            .query_map([], |row| {
                let key: String = row.get(0)?;
                let value: String = row.get(1)?;
                Ok((key, value))
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?;

        for row in rows.flatten() {
            if let Ok(value) = serde_json::from_str(&row.1) {
                settings.insert(row.0, value);
            }
        }

        Ok(serde_json::Value::Object(settings))
    }

    // ========================================================================
    // Config Storage Operations
    // ========================================================================

    /// Save a config with a specific ID.
    /// Used when importing from web where we have a project_id.
    pub fn save_config_with_id(
        &self,
        id: &str,
        config: serde_json::Value,
        name: &str,
        source_type: &str,
        source_path: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();
        let config_json = config.to_string();

        conn.execute(
            r#"
            INSERT INTO configs (id, name, config_json, source_type, source_path, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
            ON CONFLICT(id) DO UPDATE SET
                name = ?2,
                config_json = ?3,
                source_type = ?4,
                source_path = ?5,
                updated_at = ?6
            "#,
            params![id, name, config_json, source_type, source_path, now],
        )
        .map_err(|e| format!("Failed to save config: {}", e))?;

        info!("Saved config '{}' with id '{}'", name, id);
        Ok(())
    }

    /// Upsert a config (update if exists, insert if new).
    /// Used when loading from file.
    #[allow(dead_code)]
    pub fn upsert_config(
        &self,
        id: &str,
        config: serde_json::Value,
        name: &str,
        source_path: Option<&str>,
    ) -> Result<(), String> {
        self.save_config_with_id(id, config, name, "file", source_path)
    }

    /// Get a config by ID.
    #[allow(dead_code)]
    pub fn get_config(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let result: rusqlite::Result<String> = conn.query_row(
            "SELECT config_json FROM configs WHERE id = ?1",
            params![id],
            |row| row.get(0),
        );

        match result {
            Ok(json_str) => {
                let config = serde_json::from_str(&json_str)
                    .map_err(|e| format!("Failed to parse config JSON: {}", e))?;
                Ok(Some(config))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get config: {}", e)),
        }
    }

    /// List all stored configs.
    #[allow(dead_code)]
    pub fn list_configs(&self) -> Result<Vec<ConfigStorageEntry>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, source_type, source_path, created_at, updated_at
                FROM configs
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let configs = stmt
            .query_map([], |row| {
                Ok(ConfigStorageEntry {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    source_type: row.get(2)?,
                    source_path: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(configs)
    }

    /// Delete a config by ID.
    #[allow(dead_code)]
    pub fn delete_config(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows = conn
            .execute("DELETE FROM configs WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete config: {}", e))?;

        Ok(rows > 0)
    }

    // ========================================================================
    // Migration from JSON files
    // ========================================================================

    /// Migrate existing JSON files to the database.
    /// Called on first startup after database creation.
    pub fn migrate_from_json_files(&self) -> Result<MigrationResult, String> {
        let mut result = MigrationResult::default();

        // Get paths
        let config_dir = dirs::config_dir()
            .ok_or("Failed to get config directory")?
            .join("com.qontinui.runner");

        let data_dir = dirs::data_local_dir()
            .ok_or("Failed to get data directory")?
            .join("com.qontinui.runner");

        // Migrate settings.json
        let settings_path = config_dir.join("settings.json");
        if settings_path.exists() {
            match self.migrate_settings_file(&settings_path) {
                Ok(count) => {
                    result.settings_migrated = count;
                    info!("Migrated {} settings from {:?}", count, settings_path);
                }
                Err(e) => {
                    warn!("Failed to migrate settings: {}", e);
                    result.errors.push(format!("Settings: {}", e));
                }
            }
        }

        // Migrate prompts.json
        let prompts_path = data_dir.join("prompts.json");
        if prompts_path.exists() {
            match self.migrate_prompts_file(&prompts_path) {
                Ok(count) => {
                    result.prompts_migrated = count;
                    info!("Migrated {} prompts from {:?}", count, prompts_path);
                }
                Err(e) => {
                    warn!("Failed to migrate prompts: {}", e);
                    result.errors.push(format!("Prompts: {}", e));
                }
            }
        }

        // Migrate scheduler.json
        let scheduler_path = config_dir.join("scheduler.json");
        if scheduler_path.exists() {
            match self.migrate_scheduler_file(&scheduler_path) {
                Ok(count) => {
                    result.scheduler_tasks_migrated = count;
                    info!(
                        "Migrated {} scheduler tasks from {:?}",
                        count, scheduler_path
                    );
                }
                Err(e) => {
                    warn!("Failed to migrate scheduler: {}", e);
                    result.errors.push(format!("Scheduler: {}", e));
                }
            }
        }

        Ok(result)
    }

    fn migrate_settings_file(&self, path: &PathBuf) -> Result<usize, String> {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;

        let settings: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| format!("Failed to parse JSON: {}", e))?;

        let obj = settings
            .as_object()
            .ok_or("Settings file is not a JSON object")?;

        let mut count = 0;
        for (key, value) in obj {
            self.set_setting(key, value)?;
            count += 1;
        }

        Ok(count)
    }

    fn migrate_prompts_file(&self, path: &PathBuf) -> Result<usize, String> {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;

        let prompts: Vec<serde_json::Value> =
            serde_json::from_str(&content).map_err(|e| format!("Failed to parse JSON: {}", e))?;

        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let mut count = 0;
        for prompt in prompts {
            let id = prompt
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or(&uuid::Uuid::new_v4().to_string())
                .to_string();
            let name = prompt
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Untitled");
            let category = prompt.get("category").and_then(|v| v.as_str());
            let content_field = prompt.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let variables = prompt
                .get("variables")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "[]".to_string());

            conn.execute(
                r#"
                INSERT OR REPLACE INTO prompts (id, name, category, content, variables, created_at, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                "#,
                params![id, name, category, content_field, variables, now],
            )
            .map_err(|e| format!("Failed to insert prompt: {}", e))?;

            count += 1;
        }

        // Rename original file to .bak
        let backup_path = path.with_extension("json.bak");
        if let Err(e) = std::fs::rename(path, &backup_path) {
            warn!("Failed to rename {:?} to backup: {}", path, e);
        }

        Ok(count)
    }

    fn migrate_scheduler_file(&self, path: &PathBuf) -> Result<usize, String> {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;

        let scheduler: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| format!("Failed to parse JSON: {}", e))?;

        let tasks = scheduler
            .get("tasks")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let mut count = 0;
        for task in tasks {
            let id = task
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or(&uuid::Uuid::new_v4().to_string())
                .to_string();
            let name = task
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Untitled");
            let description = task.get("description").and_then(|v| v.as_str());
            let enabled = task
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let schedule = task
                .get("schedule")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            let schedule_type = schedule
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("once");
            let schedule_value = schedule
                .get("value")
                .map(|v| v.to_string())
                .unwrap_or_default();

            let task_config = task.get("task").map(|v| v.to_string()).unwrap_or_default();

            conn.execute(
                r#"
                INSERT OR REPLACE INTO scheduled_tasks
                (id, name, description, enabled, schedule_type, schedule_value, task_config, created_at, modified_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
                "#,
                params![
                    id,
                    name,
                    description,
                    enabled as i32,
                    schedule_type,
                    schedule_value,
                    task_config,
                    now
                ],
            )
            .map_err(|e| format!("Failed to insert scheduler task: {}", e))?;

            count += 1;
        }

        // Migrate settings
        if let Some(settings) = scheduler.get("settings") {
            let enabled = settings
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let max_concurrent = settings
                .get("max_concurrent")
                .and_then(|v| v.as_i64())
                .unwrap_or(1);

            conn.execute(
                r#"
                UPDATE scheduler_settings SET enabled = ?1, max_concurrent = ?2 WHERE id = 1
                "#,
                params![enabled as i32, max_concurrent],
            )
            .map_err(|e| format!("Failed to update scheduler settings: {}", e))?;
        }

        // Rename original file to .bak
        let backup_path = path.with_extension("json.bak");
        if let Err(e) = std::fs::rename(path, &backup_path) {
            warn!("Failed to rename {:?} to backup: {}", path, e);
        }

        Ok(count)
    }
}
