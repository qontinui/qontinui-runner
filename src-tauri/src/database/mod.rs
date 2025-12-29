//! SQLite database for qontinui-runner persistence.
//!
//! Provides transaction-safe storage for sessions, checkpoints, settings,
//! prompts, workflows, and scheduler state.

use chrono::Utc;
use rusqlite::{params, Connection, Result as SqliteResult};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tracing::{info, warn};

/// Database handle for checkpoint and session persistence.
pub struct CheckpointDb {
    conn: Mutex<Connection>,
    db_path: PathBuf,
}

/// Checkpoint data structure for API compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointData {
    /// Session ID (for session-based checkpoints)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Workflow name (for workflow-based checkpoints like "improve-all")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,
    /// Current phase number
    pub current_phase: u32,
    /// Total phases (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_phases: Option<u32>,
    /// Whether the workflow/session is complete
    #[serde(default)]
    pub completed: bool,
    /// Whether restart is permitted
    #[serde(default)]
    pub restart_permitted: bool,
    /// Session status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Run ID for grouping sessions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Repos to process (for improve-all)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repos_to_process: Option<Vec<String>>,
    /// Work completed per phase
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_completed: Option<serde_json::Value>,
    /// Items needing user input
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items_needing_user_input: Option<Vec<String>>,
    /// Created timestamp
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Updated timestamp
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Error message if any
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Extra custom data
    #[serde(flatten)]
    pub extra: Option<serde_json::Value>,
}

/// Session event for history tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub id: i64,
    pub session_id: String,
    pub event_type: String,
    pub message: String,
    pub timestamp: String,
    pub data: Option<serde_json::Value>,
}

impl CheckpointDb {
    /// Create a new database connection at the default location.
    ///
    /// Database location: `~/.config/com.qontinui.runner/runner.db`
    pub fn new() -> Result<Self, String> {
        let config_dir = dirs::config_dir()
            .ok_or("Failed to get config directory")?
            .join("com.qontinui.runner");

        // Create directory if needed
        std::fs::create_dir_all(&config_dir)
            .map_err(|e| format!("Failed to create config directory: {}", e))?;

        let db_path = config_dir.join("runner.db");
        Self::new_at_path(db_path)
    }

    /// Create a new database connection at a specific path.
    pub fn new_at_path(db_path: PathBuf) -> Result<Self, String> {
        let conn = Connection::open(&db_path)
            .map_err(|e| format!("Failed to open database at {:?}: {}", db_path, e))?;

        // Enable WAL mode for better concurrency
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| format!("Failed to set PRAGMA options: {}", e))?;

        let db = Self {
            conn: Mutex::new(conn),
            db_path: db_path.clone(),
        };

        // Run schema migrations
        db.run_migrations()?;

        info!("Checkpoint database initialized at {:?}", db_path);

        Ok(db)
    }

    /// Get the database path.
    pub fn path(&self) -> &PathBuf {
        &self.db_path
    }

    /// Run database migrations.
    fn run_migrations(&self) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;

        // Check current schema version
        let current_version: i32 = conn
            .query_row(
                "SELECT version FROM schema_version ORDER BY version DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if current_version == 0 {
            // Fresh database - create schema
            info!("Creating database schema (version 1)");
            conn.execute_batch(include_str!("schema.sql"))
                .map_err(|e| format!("Failed to create schema: {}", e))?;
        }

        // Future migrations would go here:
        // if current_version < 2 { ... }

        Ok(())
    }

    // ========================================================================
    // Checkpoint/Workflow Operations
    // ========================================================================

    /// Get a checkpoint by workflow name.
    pub fn get_checkpoint(&self, workflow_name: &str) -> Result<Option<CheckpointData>, String> {
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;

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
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;

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
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;

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
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;

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
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;

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
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;

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
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;

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
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;

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

    // ========================================================================
    // Settings Operations
    // ========================================================================

    /// Get a setting value.
    pub fn get_setting(&self, key: &str) -> Result<Option<serde_json::Value>, String> {
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;

        let result: SqliteResult<String> = conn.query_row(
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
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;

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
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;

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

        // Rename original file to .bak
        let backup_path = path.with_extension("json.bak");
        if let Err(e) = std::fs::rename(path, &backup_path) {
            warn!("Failed to rename {:?} to backup: {}", path, e);
        }

        Ok(count)
    }

    fn migrate_prompts_file(&self, path: &PathBuf) -> Result<usize, String> {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;

        let prompts: Vec<serde_json::Value> =
            serde_json::from_str(&content).map_err(|e| format!("Failed to parse JSON: {}", e))?;

        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;
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

        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;
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

/// Result of JSON file migration.
#[derive(Debug, Default)]
pub struct MigrationResult {
    pub settings_migrated: usize,
    pub prompts_migrated: usize,
    pub scheduler_tasks_migrated: usize,
    pub workflows_migrated: usize,
    pub checkpoints_migrated: usize,
    pub errors: Vec<String>,
}

impl MigrationResult {
    pub fn is_success(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn total_migrated(&self) -> usize {
        self.settings_migrated
            + self.prompts_migrated
            + self.scheduler_tasks_migrated
            + self.workflows_migrated
            + self.checkpoints_migrated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_db() -> (CheckpointDb, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let db = CheckpointDb::new_at_path(db_path).unwrap();
        (db, temp_dir)
    }

    #[test]
    fn test_checkpoint_crud() {
        let (db, _temp) = create_test_db();

        // Create
        let checkpoint = CheckpointData {
            session_id: None,
            workflow_name: Some("test-workflow".to_string()),
            current_phase: 2,
            total_phases: Some(12),
            completed: false,
            restart_permitted: true,
            status: Some("running".to_string()),
            run_id: Some("test-run-123".to_string()),
            repos_to_process: Some(vec!["repo1".to_string(), "repo2".to_string()]),
            work_completed: None,
            items_needing_user_input: None,
            created_at: None,
            updated_at: None,
            error_message: None,
            extra: None,
        };

        db.save_checkpoint(&checkpoint).unwrap();

        // Read
        let loaded = db.get_checkpoint("test-workflow").unwrap().unwrap();
        assert_eq!(loaded.current_phase, 2);
        assert_eq!(loaded.completed, false);

        // Update
        let updated = CheckpointData {
            current_phase: 5,
            completed: true,
            ..checkpoint.clone()
        };
        db.save_checkpoint(&updated).unwrap();

        let reloaded = db.get_checkpoint("test-workflow").unwrap().unwrap();
        assert_eq!(reloaded.current_phase, 5);
        assert_eq!(reloaded.completed, true);

        // Delete
        let deleted = db.delete_checkpoint("test-workflow").unwrap();
        assert!(deleted);

        let not_found = db.get_checkpoint("test-workflow").unwrap();
        assert!(not_found.is_none());
    }

    #[test]
    fn test_check_checkpoint_status() {
        let (db, _temp) = create_test_db();

        let checkpoint = CheckpointData {
            session_id: None,
            workflow_name: Some("status-test".to_string()),
            current_phase: 5,
            total_phases: Some(12),
            completed: false,
            restart_permitted: true,
            status: None,
            run_id: None,
            repos_to_process: None,
            work_completed: None,
            items_needing_user_input: None,
            created_at: None,
            updated_at: None,
            error_message: None,
            extra: None,
        };

        db.save_checkpoint(&checkpoint).unwrap();

        // Not complete yet (5 < 12)
        let (is_complete, phase) = db
            .check_checkpoint_status("status-test", 12)
            .unwrap()
            .unwrap();
        assert_eq!(is_complete, false);
        assert_eq!(phase, 5);

        // Complete when threshold is 5
        let (is_complete, _) = db
            .check_checkpoint_status("status-test", 5)
            .unwrap()
            .unwrap();
        assert_eq!(is_complete, true);
    }

    #[test]
    fn test_settings() {
        let (db, _temp) = create_test_db();

        let value = serde_json::json!({"timeout": 300, "enabled": true});
        db.set_setting("ai_config", &value).unwrap();

        let loaded = db.get_setting("ai_config").unwrap().unwrap();
        assert_eq!(loaded["timeout"], 300);
        assert_eq!(loaded["enabled"], true);

        let all = db.get_all_settings().unwrap();
        assert!(all.get("ai_config").is_some());
    }
}
