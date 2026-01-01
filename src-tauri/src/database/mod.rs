//! SQLite database for qontinui-runner persistence.
//!
//! Provides transaction-safe storage for sessions, checkpoints, settings,
//! prompts, workflows, and scheduler state.

use chrono::Utc;
use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, Connection, Result as SqliteResult};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{info, warn};

/// Database handle for checkpoint and session persistence.
pub struct CheckpointDb {
    pool: Pool<SqliteConnectionManager>,
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

/// Task run data structure for the simplified task model.
/// Every task runs until [TASK_COMPLETE] is found in output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRun {
    pub id: String,
    pub task_name: String,
    pub prompt: String,
    pub status: String, // 'running', 'complete', 'failed', 'stopped'
    pub sessions_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_sessions: Option<u32>,
    pub output_log: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Per-run auto-continue setting (defaults to true)
    #[serde(default = "default_auto_continue")]
    pub auto_continue: bool,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

fn default_auto_continue() -> bool {
    true
}

/// Stored config entry metadata (without the full JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigStorageEntry {
    pub id: String,
    pub name: String,
    pub source_type: String, // 'web' or 'file'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Completion marker that AI uses to signal task is done
pub const TASK_COMPLETE_MARKER: &str = "[TASK_COMPLETE]";

/// Session start marker format
pub fn session_start_marker(session_num: u32) -> String {
    format!("[SESSION_START:{}]", session_num)
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
        // Create connection manager with PRAGMA initialization
        // Each connection from the pool will have these settings applied
        let manager = SqliteConnectionManager::file(&db_path).with_init(|conn| {
            // Enable WAL mode and configure for better concurrency
            // - journal_mode=WAL: Write-Ahead Logging for concurrent readers
            // - foreign_keys=ON: Enforce referential integrity
            // - busy_timeout=5000: Wait up to 5 seconds on lock contention (instead of immediate failure)
            // - synchronous=NORMAL: Safe for WAL mode, better performance than FULL
            // - temp_store=MEMORY: Keep temp tables in memory
            // - cache_size=-32000: 32MB page cache for better performance
            conn.execute_batch(
                r#"
                PRAGMA journal_mode=WAL;
                PRAGMA foreign_keys=ON;
                PRAGMA busy_timeout=5000;
                PRAGMA synchronous=NORMAL;
                PRAGMA temp_store=MEMORY;
                PRAGMA cache_size=-32000;
                "#,
            )?;
            Ok(())
        });

        // Build the connection pool with max 10 connections
        let pool = Pool::builder()
            .max_size(10)
            .build(manager)
            .map_err(|e| format!("Failed to create connection pool: {}", e))?;

        // Run migrations using a connection from the pool
        {
            let conn = pool
                .get()
                .map_err(|e| format!("Failed to get connection from pool: {}", e))?;
            Self::run_migrations_on_conn(&conn)?;
        }

        info!(
            "Checkpoint database initialized with connection pool at {:?}",
            db_path
        );

        Ok(Self {
            pool,
            db_path: db_path.clone(),
        })
    }

    /// Get the database path.
    pub fn path(&self) -> &PathBuf {
        &self.db_path
    }

    /// Get a connection from the pool.
    fn get_conn(&self) -> Result<PooledConnection<SqliteConnectionManager>, String> {
        self.pool
            .get()
            .map_err(|e| format!("Failed to get connection from pool: {}", e))
    }

    /// Run database migrations on a specific connection.
    /// This is a static method to allow calling during pool initialization.
    fn run_migrations_on_conn(conn: &Connection) -> Result<(), String> {
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
            info!("Creating database schema (version 5)");
            conn.execute_batch(include_str!("schema.sql"))
                .map_err(|e| format!("Failed to create schema: {}", e))?;
        }

        // Migration to version 2: Add task_runs table
        if current_version == 1 {
            info!("Migrating database to version 2 (adding task_runs table)");
            conn.execute_batch(
                r#"
                -- Task Runs (simplified task execution model)
                CREATE TABLE IF NOT EXISTS task_runs (
                    id TEXT PRIMARY KEY,
                    task_name TEXT NOT NULL,
                    prompt TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'running',
                    sessions_count INTEGER NOT NULL DEFAULT 0,
                    max_sessions INTEGER,
                    output_log TEXT DEFAULT '',
                    error_message TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    completed_at TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_task_runs_status ON task_runs(status);
                CREATE INDEX IF NOT EXISTS idx_task_runs_created_at ON task_runs(created_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (2, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 2: {}", e))?;
        }

        // Migration to version 3: Add auto_continue column to task_runs
        if current_version == 1 || current_version == 2 {
            info!("Migrating database to version 3 (adding auto_continue to task_runs)");
            conn.execute_batch(
                r#"
                -- Add auto_continue column to task_runs (default true)
                ALTER TABLE task_runs ADD COLUMN auto_continue BOOLEAN NOT NULL DEFAULT 1;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (3, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 3: {}", e))?;
        }

        // Migration to version 4: Add task_run_output_chunks table for O(1) appending
        if current_version >= 1 && current_version < 4 {
            info!("Migrating database to version 4 (adding task_run_output_chunks table)");

            // Create the chunks table
            conn.execute_batch(
                r#"
                -- Task Run Output Chunks (for efficient O(1) appending)
                CREATE TABLE IF NOT EXISTS task_run_output_chunks (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_run_id TEXT NOT NULL,
                    chunk_sequence INTEGER NOT NULL,
                    content TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_chunks_task_run ON task_run_output_chunks(task_run_id, chunk_sequence);
                "#,
            )
            .map_err(|e| format!("Failed to create task_run_output_chunks table: {}", e))?;

            // Migrate existing output_log data to chunks (one-time migration)
            let now = Utc::now().to_rfc3339();

            // Get all task_runs with non-empty output_log
            let mut stmt = conn
                .prepare("SELECT id, output_log FROM task_runs WHERE output_log != '' AND output_log IS NOT NULL")
                .map_err(|e| format!("Failed to prepare migration query: {}", e))?;

            let tasks: Vec<(String, String)> = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|e| format!("Failed to query task_runs for migration: {}", e))?
                .filter_map(|r| r.ok())
                .collect();

            drop(stmt); // Release the statement before executing more queries

            for (task_id, output) in &tasks {
                conn.execute(
                    "INSERT INTO task_run_output_chunks (task_run_id, chunk_sequence, content, created_at) VALUES (?1, 1, ?2, ?3)",
                    params![task_id, output, now],
                )
                .map_err(|e| format!("Failed to migrate output for task {}: {}", task_id, e))?;
            }

            if !tasks.is_empty() {
                info!("Migrated {} task runs' output_log to chunks", tasks.len());
            }

            conn.execute_batch(
                r#"
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (4, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to update schema version to 4: {}", e))?;
        }

        // Migration to version 5: Add configs table for ConfigStorage
        if current_version >= 1 && current_version < 5 {
            info!("Migrating database to version 5 (adding configs table)");
            conn.execute_batch(
                r#"
                -- Configs storage (for auto-storing imported/loaded configs)
                CREATE TABLE IF NOT EXISTS configs (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    config_json TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    source_path TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_configs_name ON configs(name);
                CREATE INDEX IF NOT EXISTS idx_configs_updated_at ON configs(updated_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (5, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 5: {}", e))?;
        }

        Ok(())
    }

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

    // ========================================================================
    // Task Run Operations (Simplified Task Model)
    // ========================================================================

    /// Create a new task run.
    /// If `auto_continue` is None, defaults to true.
    pub fn create_task_run(
        &self,
        id: &str,
        task_name: &str,
        prompt: &str,
        max_sessions: Option<u32>,
        auto_continue: Option<bool>,
    ) -> Result<TaskRun, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();
        let auto_continue_val = auto_continue.unwrap_or(true);

        conn.execute(
            r#"
            INSERT INTO task_runs (id, task_name, prompt, status, sessions_count, max_sessions, output_log, auto_continue, created_at, updated_at)
            VALUES (?1, ?2, ?3, 'running', 0, ?4, '', ?5, ?6, ?6)
            "#,
            params![id, task_name, prompt, max_sessions.map(|v| v as i64), auto_continue_val as i32, now],
        )
        .map_err(|e| format!("Failed to create task run: {}", e))?;

        Ok(TaskRun {
            id: id.to_string(),
            task_name: task_name.to_string(),
            prompt: prompt.to_string(),
            status: "running".to_string(),
            sessions_count: 0,
            max_sessions,
            output_log: String::new(),
            error_message: None,
            auto_continue: auto_continue_val,
            created_at: now.clone(),
            updated_at: now,
            completed_at: None,
        })
    }

    /// Get a task run by ID.
    /// Note: output_log is reconstructed from chunks table for backward compatibility.
    pub fn get_task_run(&self, id: &str) -> Result<Option<TaskRun>, String> {
        let conn = self.get_conn()?;

        // First get the task_run metadata
        let result: SqliteResult<TaskRun> = conn.query_row(
            r#"
            SELECT id, task_name, prompt, status, sessions_count, max_sessions, error_message, auto_continue, created_at, updated_at, completed_at
            FROM task_runs
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    status: row.get(3)?,
                    sessions_count: row.get::<_, i64>(4)? as u32,
                    max_sessions: row.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                    output_log: String::new(), // Will be filled from chunks
                    error_message: row.get(6)?,
                    auto_continue: row.get::<_, i32>(7)? != 0,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                    completed_at: row.get(10)?,
                })
            },
        );

        match result {
            Ok(mut task_run) => {
                // Get output from chunks
                drop(conn); // Release connection before calling another method
                task_run.output_log = self.get_full_task_output(id).unwrap_or_default();
                Ok(Some(task_run))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get task run: {}", e)),
        }
    }

    /// Append output to a task run and increment session count.
    /// Returns true if [TASK_COMPLETE] marker was found in the appended text.
    ///
    /// Uses O(1) chunk insertion instead of O(n) string concatenation.
    /// Output is stored in the task_run_output_chunks table for efficient appending.
    ///
    /// NOTE: This method handles task completion inline to avoid multiple connection acquisitions.
    pub fn append_task_output(
        &self,
        id: &str,
        output: &str,
        increment_session: bool,
    ) -> Result<bool, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Get next chunk sequence number
        let next_seq: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(chunk_sequence), 0) + 1 FROM task_run_output_chunks WHERE task_run_id = ?",
                params![id],
                |row| row.get(0),
            )
            .unwrap_or(1);

        // Insert new chunk (O(1) operation)
        conn.execute(
            "INSERT INTO task_run_output_chunks (task_run_id, chunk_sequence, content, created_at) VALUES (?, ?, ?, ?)",
            params![id, next_seq, output, now],
        )
        .map_err(|e| format!("Failed to insert output chunk: {}", e))?;

        // Update task_run metadata only (no string concatenation)
        let session_increment = if increment_session { 1 } else { 0 };
        conn.execute(
            r#"
            UPDATE task_runs SET
                sessions_count = sessions_count + ?1,
                updated_at = ?2
            WHERE id = ?3
            "#,
            params![session_increment, now, id],
        )
        .map_err(|e| format!("Failed to update task run metadata: {}", e))?;

        // Check if task is complete
        let is_complete = output.contains(TASK_COMPLETE_MARKER);
        if is_complete {
            // IMPORTANT: Inline the completion logic here instead of calling complete_task_run()
            // to avoid nested lock acquisition (we already hold the conn lock above).
            conn.execute(
                r#"
                UPDATE task_runs SET
                    status = 'complete',
                    updated_at = ?1,
                    completed_at = ?1
                WHERE id = ?2 AND status = 'running'
                "#,
                params![now, id],
            )
            .map_err(|e| format!("Failed to complete task run: {}", e))?;

            info!("Task run {} marked complete via append_task_output", id);
        }

        Ok(is_complete)
    }

    /// Mark a task run as complete.
    pub fn complete_task_run(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                status = 'complete',
                updated_at = ?1,
                completed_at = ?1
            WHERE id = ?2
            "#,
            params![now, id],
        )
        .map_err(|e| format!("Failed to complete task run: {}", e))?;

        Ok(())
    }

    /// Mark a task run as failed.
    pub fn fail_task_run(&self, id: &str, error_message: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                status = 'failed',
                error_message = ?1,
                updated_at = ?2,
                completed_at = ?2
            WHERE id = ?3
            "#,
            params![error_message, now, id],
        )
        .map_err(|e| format!("Failed to fail task run: {}", e))?;

        Ok(())
    }

    /// Stop a task run.
    pub fn stop_task_run(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                status = 'stopped',
                updated_at = ?1,
                completed_at = ?1
            WHERE id = ?2
            "#,
            params![now, id],
        )
        .map_err(|e| format!("Failed to stop task run: {}", e))?;

        Ok(())
    }

    /// Get all running (incomplete) task runs.
    /// Note: output_log is empty for performance. Use get_full_task_output() to get output.
    pub fn get_running_task_runs(&self) -> Result<Vec<TaskRun>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_name, prompt, status, sessions_count, max_sessions, error_message, auto_continue, created_at, updated_at, completed_at
                FROM task_runs
                WHERE status = 'running'
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let task_runs = stmt
            .query_map([], |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    status: row.get(3)?,
                    sessions_count: row.get::<_, i64>(4)? as u32,
                    max_sessions: row.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                    output_log: String::new(), // Empty for performance - use get_full_task_output()
                    error_message: row.get(6)?,
                    auto_continue: row.get::<_, i32>(7)? != 0,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                    completed_at: row.get(10)?,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(task_runs)
    }

    /// Get recent task runs (for display in UI).
    /// Note: output_log is empty for performance. Use get_full_task_output() to get output.
    pub fn get_recent_task_runs(&self, limit: u32) -> Result<Vec<TaskRun>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_name, prompt, status, sessions_count, max_sessions, error_message, auto_continue, created_at, updated_at, completed_at
                FROM task_runs
                ORDER BY updated_at DESC
                LIMIT ?1
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let task_runs = stmt
            .query_map(params![limit], |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    status: row.get(3)?,
                    sessions_count: row.get::<_, i64>(4)? as u32,
                    max_sessions: row.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                    output_log: String::new(), // Empty for performance - use get_full_task_output()
                    error_message: row.get(6)?,
                    auto_continue: row.get::<_, i32>(7)? != 0,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                    completed_at: row.get(10)?,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(task_runs)
    }

    /// Get the last N characters of output for continuation context.
    pub fn get_task_output_tail(&self, id: &str, chars: usize) -> Result<String, String> {
        let task_run = self
            .get_task_run(id)?
            .ok_or_else(|| format!("Task run not found: {}", id))?;

        let output = &task_run.output_log;
        if output.len() <= chars {
            Ok(output.clone())
        } else {
            Ok(output[output.len() - chars..].to_string())
        }
    }

    /// Check if a task run should continue (not complete, not stopped, not at max sessions).
    pub fn should_continue_task(&self, id: &str) -> Result<bool, String> {
        let task_run = self
            .get_task_run(id)?
            .ok_or_else(|| format!("Task run not found: {}", id))?;

        // Already complete or stopped
        if task_run.status != "running" {
            return Ok(false);
        }

        // Check max sessions limit
        if let Some(max) = task_run.max_sessions {
            if task_run.sessions_count >= max {
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// Delete a task run by ID.
    /// Note: CASCADE DELETE will automatically remove associated chunks.
    pub fn delete_task_run(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows = conn
            .execute("DELETE FROM task_runs WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete task run: {}", e))?;

        Ok(rows > 0)
    }

    /// Get the full output log by joining all chunks.
    /// Use this when you need the complete output (e.g., for display or export).
    pub fn get_full_task_output(&self, id: &str) -> Result<String, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                "SELECT content FROM task_run_output_chunks WHERE task_run_id = ? ORDER BY chunk_sequence",
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let chunks: Vec<String> = stmt
            .query_map(params![id], |row| row.get(0))
            .map_err(|e| format!("Failed to query chunks: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(chunks.join(""))
    }

    /// Get the auto-continue setting for a specific task run.
    pub fn get_task_auto_continue(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<i32> = conn.query_row(
            "SELECT auto_continue FROM task_runs WHERE id = ?1",
            params![id],
            |row| row.get(0),
        );

        match result {
            Ok(value) => Ok(value != 0),
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(format!("Task run not found: {}", id)),
            Err(e) => Err(format!("Failed to get task auto_continue: {}", e)),
        }
    }

    /// Set the auto-continue setting for a specific task run.
    pub fn set_task_auto_continue(&self, id: &str, auto_continue: bool) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let rows = conn
            .execute(
                r#"
                UPDATE task_runs SET
                    auto_continue = ?1,
                    updated_at = ?2
                WHERE id = ?3
                "#,
                params![auto_continue as i32, now, id],
            )
            .map_err(|e| format!("Failed to set task auto_continue: {}", e))?;

        if rows == 0 {
            return Err(format!("Task run not found: {}", id));
        }

        Ok(())
    }

    // ========================================================================
    // Settings Operations
    // ========================================================================

    /// Get a setting value.
    pub fn get_setting(&self, key: &str) -> Result<Option<serde_json::Value>, String> {
        let conn = self.get_conn()?;

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

        let result: SqliteResult<String> = conn.query_row(
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

        // NOTE: Do NOT rename settings.json to .bak here!
        // settings.rs still reads from settings.json directly.
        // The database is used for settings that aren't in settings.rs yet.
        // Once settings.rs is fully migrated to database, we can rename.

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
    #[allow(dead_code)]
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

    #[test]
    fn test_task_run_auto_continue() {
        let (db, _temp) = create_test_db();

        // Create task run with default auto_continue (true)
        let task_run = db
            .create_task_run("test-task-1", "Test Task", "Do something", None, None)
            .unwrap();
        assert_eq!(task_run.auto_continue, true);

        // Create task run with explicit auto_continue = false
        let task_run_disabled = db
            .create_task_run(
                "test-task-2",
                "Test Task 2",
                "Do something else",
                None,
                Some(false),
            )
            .unwrap();
        assert_eq!(task_run_disabled.auto_continue, false);

        // Get auto_continue setting
        let auto_continue = db.get_task_auto_continue("test-task-1").unwrap();
        assert_eq!(auto_continue, true);

        let auto_continue_disabled = db.get_task_auto_continue("test-task-2").unwrap();
        assert_eq!(auto_continue_disabled, false);

        // Set auto_continue setting
        db.set_task_auto_continue("test-task-1", false).unwrap();
        let updated = db.get_task_auto_continue("test-task-1").unwrap();
        assert_eq!(updated, false);

        // Verify via get_task_run
        let loaded = db.get_task_run("test-task-1").unwrap().unwrap();
        assert_eq!(loaded.auto_continue, false);
    }
}
