//! SQLite database for qontinui-runner persistence.
//!
//! Provides transaction-safe storage for sessions, checkpoints, settings,
//! prompts, workflows, and scheduler state.

#![allow(dead_code)]

pub mod embedding_client;
pub mod embedding_jobs;
pub mod embeddings;
pub mod export_import;
pub mod hybrid_search;
pub mod migrations;
pub mod orchestrator_ops;
pub mod pipeline_traces;
pub mod query_builder;
pub mod scheduler;
pub mod task_runs;
pub mod types;
pub mod verification_ops;
pub mod workflow_ops;

// Re-export all types for backward compatibility
pub use types::*;

use chrono::Utc;
use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, Connection, OptionalExtension, Result as SqliteResult};
use std::path::PathBuf;
use tracing::{info, warn};

/// Database handle for checkpoint and session persistence.
pub struct CheckpointDb {
    pool: Pool<SqliteConnectionManager>,
    db_path: PathBuf,
    /// The runner's API port, used to tag task_runs for instance-level filtering.
    /// Set after the HTTP server binds. Defaults to 0 (unset).
    runner_port: std::sync::atomic::AtomicU16,
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
            runner_port: std::sync::atomic::AtomicU16::new(0),
        })
    }

    /// Set the runner port for instance-level task_run filtering.
    /// Called after the HTTP server binds to its port.
    pub fn set_runner_port(&self, port: u16) {
        self.runner_port
            .store(port, std::sync::atomic::Ordering::Relaxed);
    }

    /// Get the runner port (0 means unset).
    pub fn get_runner_port(&self) -> Option<u16> {
        let port = self.runner_port.load(std::sync::atomic::Ordering::Relaxed);
        if port == 0 {
            None
        } else {
            Some(port)
        }
    }

    /// Create an in-memory database for testing or no-op logging.
    ///
    /// This creates a database that exists only in memory and is not persisted.
    /// Useful for testing or when you need a valid CheckpointDb instance but
    /// don't want to actually store anything.
    pub fn new_in_memory() -> Result<Self, String> {
        let manager = SqliteConnectionManager::memory().with_init(|conn| {
            conn.execute_batch(
                r#"
                PRAGMA foreign_keys=ON;
                PRAGMA synchronous=OFF;
                "#,
            )?;
            Ok(())
        });

        let pool = Pool::builder()
            .max_size(1)
            .build(manager)
            .map_err(|e| format!("Failed to create in-memory connection pool: {}", e))?;

        // Run migrations
        {
            let conn = pool
                .get()
                .map_err(|e| format!("Failed to get connection from pool: {}", e))?;
            Self::run_migrations_on_conn(&conn)?;
        }

        Ok(Self {
            pool,
            db_path: PathBuf::from(":memory:"),
            runner_port: std::sync::atomic::AtomicU16::new(0),
        })
    }

    /// Get the database path.
    pub fn path(&self) -> &PathBuf {
        &self.db_path
    }

    /// Get a clone of the underlying connection pool.
    ///
    /// This is primarily used for the tracing span layer which needs
    /// its own reference to the pool for writing span data.
    pub fn get_pool(&self) -> Pool<SqliteConnectionManager> {
        self.pool.clone()
    }

    /// Get a connection from the pool.
    ///
    /// # Errors
    /// Returns `AppError::PoolError` if unable to get a connection from the pool.
    pub fn get_conn(
        &self,
    ) -> Result<PooledConnection<SqliteConnectionManager>, crate::error::AppError> {
        Ok(self.pool.get()?)
    }

    /// Get a connection from the pool (legacy API returning String error).
    ///
    /// Deprecated: Use `get_conn()` instead which returns `AppError`.
    pub fn get_conn_string(&self) -> Result<PooledConnection<SqliteConnectionManager>, String> {
        self.pool
            .get()
            .map_err(|e| format!("Failed to get connection from pool: {}", e))
    }

    /// Get a reference to the underlying connection for direct rusqlite operations.
    /// This is useful for modules that need raw rusqlite::Connection access (e.g., findings storage).
    ///
    /// Note: The returned reference borrows the PooledConnection, so the caller must
    /// ensure the PooledConnection stays alive while using the Connection.
    ///
    /// # Errors
    /// Returns `AppError::PoolError` if unable to get a connection from the pool.
    pub fn connection(
        &self,
    ) -> Result<PooledConnection<SqliteConnectionManager>, crate::error::AppError> {
        self.get_conn()
    }

    /// Execute a closure with a database connection, returning the closure's result.
    ///
    /// This is a convenience method for modules that need to run operations on
    /// a raw `rusqlite::Connection` reference without managing the pooled connection lifetime.
    pub fn with_conn<F, T>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&rusqlite::Connection) -> Result<T, String>,
    {
        let conn = self.get_conn_string()?;
        f(&conn)
    }

    /// Execute a parameterized SQL statement (INSERT, UPDATE, DELETE).
    /// Returns the number of rows affected.
    pub fn execute_sql(
        &self,
        sql: &str,
        params: &[&dyn rusqlite::types::ToSql],
    ) -> Result<usize, String> {
        let conn = self.get_conn_string()?;
        conn.execute(sql, params)
            .map_err(|e| format!("SQL execution failed: {}", e))
    }

    /// Get a connection (legacy API returning String error).
    ///
    /// Deprecated: Use `connection()` instead which returns `AppError`.
    pub fn connection_string(&self) -> Result<PooledConnection<SqliteConnectionManager>, String> {
        self.get_conn_string()
    }

    /// Execute a function within a database transaction.
    ///
    /// The transaction is automatically committed if the function returns Ok,
    /// or rolled back if it returns Err or panics.
    ///
    /// This ensures atomic updates across multiple tables (e.g., checkpoint + state).
    ///
    /// # Example
    /// ```ignore
    /// db.transaction(|conn| {
    ///     conn.execute("UPDATE table1 SET ...", params![...])?;
    ///     conn.execute("UPDATE table2 SET ...", params![...])?;
    ///     Ok(())
    /// })?;
    /// ```
    pub fn transaction<F, T>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&Connection) -> Result<T, rusqlite::Error>,
    {
        let conn = self.get_conn_string()?;

        // Start transaction with IMMEDIATE mode for write isolation
        conn.execute("BEGIN IMMEDIATE", [])
            .map_err(|e| format!("Failed to begin transaction: {}", e))?;

        // Execute the user function
        match f(&conn) {
            Ok(result) => {
                // Commit on success
                conn.execute("COMMIT", [])
                    .map_err(|e| format!("Failed to commit transaction: {}", e))?;
                Ok(result)
            }
            Err(e) => {
                // Rollback on error
                let _ = conn.execute("ROLLBACK", []);
                Err(format!("Transaction failed: {}", e))
            }
        }
    }

    /// Optimize the database for better performance.
    ///
    /// This function performs:
    /// - VACUUM: Rebuilds the database file, reclaiming unused space
    /// - ANALYZE: Updates statistics for the query planner
    /// - Integrity check (optional)
    ///
    /// Returns optimization statistics including space reclaimed and time taken.
    pub fn optimize_database(
        &self,
        run_integrity_check: bool,
    ) -> Result<DatabaseOptimizeResult, String> {
        let conn = self.get_conn()?;
        let start_time = std::time::Instant::now();

        // Get database size before optimization
        let size_before: i64 = conn
            .query_row(
                "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        // Run integrity check if requested
        let integrity_ok = if run_integrity_check {
            let result: String = conn
                .query_row("PRAGMA integrity_check", [], |row| row.get(0))
                .unwrap_or_else(|_| "error".to_string());
            result == "ok"
        } else {
            true
        };

        if !integrity_ok {
            warn!("Database integrity check failed - skipping VACUUM");
            return Err("Database integrity check failed".to_string());
        }

        // Run ANALYZE to update statistics for query planner
        conn.execute_batch("ANALYZE")
            .map_err(|e| format!("Failed to run ANALYZE: {}", e))?;
        info!("ANALYZE completed successfully");

        // Run VACUUM to rebuild the database and reclaim space
        // Note: VACUUM requires exclusive access and cannot run in a transaction
        conn.execute_batch("VACUUM")
            .map_err(|e| format!("Failed to run VACUUM: {}", e))?;
        info!("VACUUM completed successfully");

        // Get database size after optimization
        let size_after: i64 = conn
            .query_row(
                "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let duration_ms = start_time.elapsed().as_millis() as u64;
        let space_reclaimed = if size_before > size_after {
            size_before - size_after
        } else {
            0
        };

        info!(
            "Database optimization completed in {}ms. Space reclaimed: {} bytes",
            duration_ms, space_reclaimed
        );

        Ok(DatabaseOptimizeResult {
            duration_ms,
            size_before_bytes: size_before,
            size_after_bytes: size_after,
            space_reclaimed_bytes: space_reclaimed,
            integrity_check_passed: integrity_ok,
        })
    }

    /// Get database statistics for monitoring and debugging.
    pub fn get_database_stats(&self) -> Result<DatabaseStats, String> {
        let conn = self.get_conn()?;

        // Get page count and size
        let (page_count, page_size): (i64, i64) = conn
            .query_row(
                "SELECT page_count, page_size FROM pragma_page_count(), pragma_page_size()",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or((0, 0));

        // Get freelist count (unused pages)
        let freelist_count: i64 = conn
            .query_row("PRAGMA freelist_count", [], |row| row.get(0))
            .unwrap_or(0);

        // Get WAL checkpoint info
        let (wal_pages, wal_frames): (i64, i64) = conn
            .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
                Ok((row.get(1).unwrap_or(0), row.get(2).unwrap_or(0)))
            })
            .unwrap_or((0, 0));

        // Get table counts
        let table_counts = self.get_table_row_counts()?;

        Ok(DatabaseStats {
            total_size_bytes: page_count * page_size,
            page_count,
            page_size,
            freelist_count,
            wal_pages,
            wal_frames,
            table_counts,
        })
    }

    /// Get row counts for all tables.
    fn get_table_row_counts(&self) -> Result<Vec<TableRowCount>, String> {
        let conn = self.get_conn()?;

        // Get list of tables
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .map_err(|e| format!("Failed to list tables: {}", e))?;

        let tables: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| format!("Failed to query tables: {}", e))?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);

        let mut counts = Vec::new();
        for table in tables {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM \"{}\"", table), [], |row| {
                    row.get(0)
                })
                .unwrap_or(0);
            counts.push(TableRowCount {
                table_name: table,
                row_count: count,
            });
        }

        Ok(counts)
    }

    /// Run EXPLAIN QUERY PLAN on a query for debugging.
    /// Returns the query plan as a formatted string.
    pub fn explain_query_plan(&self, query: &str) -> Result<String, String> {
        let conn = self.get_conn()?;

        let explain_query = format!("EXPLAIN QUERY PLAN {}", query);
        let mut stmt = conn
            .prepare(&explain_query)
            .map_err(|e| format!("Failed to prepare EXPLAIN QUERY PLAN: {}", e))?;

        let rows: Vec<String> = stmt
            .query_map([], |row| {
                let id: i32 = row.get(0)?;
                let parent: i32 = row.get(1)?;
                let notused: i32 = row.get(2)?;
                let detail: String = row.get(3)?;
                Ok(format!("{}|{}|{}|{}", id, parent, notused, detail))
            })
            .map_err(|e| format!("Failed to run EXPLAIN QUERY PLAN: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(rows.join("\n"))
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

    // ========================================================================
    // Findings Operations (wrapper methods for findings::storage)
    // ========================================================================

    /// Get a finding by ID.
    pub fn get_finding(&self, id: &str) -> Result<Option<crate::findings::Finding>, String> {
        let conn = self.get_conn()?;
        crate::findings::storage::get_finding(&conn, id)
    }

    /// Get all findings for a task run.
    pub fn get_findings_for_task(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<crate::findings::Finding>, String> {
        let conn = self.get_conn()?;
        crate::findings::storage::get_findings_for_task(&conn, task_run_id)
    }

    /// Get findings by status for a task run.
    pub fn get_findings_by_status(
        &self,
        task_run_id: &str,
        status: &crate::findings::FindingStatus,
    ) -> Result<Vec<crate::findings::Finding>, String> {
        let conn = self.get_conn()?;
        crate::findings::storage::get_findings_by_status(&conn, task_run_id, status)
    }

    /// Update finding status.
    pub fn update_finding_status(
        &self,
        id: &str,
        status: &crate::findings::FindingStatus,
        resolution: Option<&str>,
        session_num: Option<u32>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        crate::findings::storage::update_finding_status(&conn, id, status, resolution, session_num)
    }

    /// Set user response for a finding.
    pub fn set_finding_user_response(&self, id: &str, response: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        crate::findings::storage::set_user_response(&conn, id, response)
    }

    /// Get summary statistics for a task run.
    pub fn get_finding_summary(
        &self,
        task_run_id: &str,
    ) -> Result<crate::findings::FindingSummary, String> {
        let conn = self.get_conn()?;
        crate::findings::storage::get_finding_summary(&conn, task_run_id)
    }

    /// Format findings for inclusion in a continuation prompt.
    ///
    /// This creates a structured section showing resolved, outstanding,
    /// and needs_input findings to provide context for continuation sessions.
    pub fn format_findings_for_continuation_prompt(
        &self,
        task_run_id: &str,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        crate::findings::storage::format_findings_for_continuation_prompt(&conn, task_run_id)
    }

    // ========================================================================
    // Hybrid Logging Operations (Phase 10)
    // ========================================================================

    /// Create a task run event (batch insert for migration from JSONL).
    pub fn create_task_run_event(&self, input: &CreateTaskRunEventInput) -> Result<i64, String> {
        let conn = self.get_conn()?;

        conn.execute(
            r#"
            INSERT INTO task_run_events (
                task_run_id, event_type, event_subtype, message, data,
                workflow_name, state_name, action_id, timestamp, duration_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                input.task_run_id,
                input.event_type,
                input.event_subtype,
                input.message,
                input.data,
                input.workflow_name,
                input.state_name,
                input.action_id,
                input.timestamp,
                input.duration_ms,
            ],
        )
        .map_err(|e| format!("Failed to create task run event: {}", e))?;

        let id = conn.last_insert_rowid();
        Ok(id)
    }

    /// Batch insert task run events (efficient for JSONL migration).
    pub fn batch_create_task_run_events(
        &self,
        events: &[CreateTaskRunEventInput],
    ) -> Result<usize, String> {
        if events.is_empty() {
            return Ok(0);
        }

        let conn = self.get_conn()?;
        let mut count = 0;

        for event in events {
            conn.execute(
                r#"
                INSERT INTO task_run_events (
                    task_run_id, event_type, event_subtype, message, data,
                    workflow_name, state_name, action_id, timestamp, duration_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                "#,
                params![
                    event.task_run_id,
                    event.event_type,
                    event.event_subtype,
                    event.message,
                    event.data,
                    event.workflow_name,
                    event.state_name,
                    event.action_id,
                    event.timestamp,
                    event.duration_ms,
                ],
            )
            .map_err(|e| format!("Failed to create task run event: {}", e))?;
            count += 1;
        }

        Ok(count)
    }

    /// Get events for a task run with optional filtering.
    pub fn get_task_run_events(
        &self,
        task_run_id: &str,
        event_type: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<TaskRunEvent>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT id, task_run_id, event_type, event_subtype, message, data,
                   workflow_name, state_name, action_id, timestamp, duration_ms
            FROM task_run_events
            WHERE task_run_id = ?1
            "#,
        );

        let mut params_vec: Vec<String> = vec![task_run_id.to_string()];

        if let Some(et) = event_type {
            sql.push_str(" AND event_type = ?2");
            params_vec.push(et.to_string());
        }

        sql.push_str(" ORDER BY timestamp ASC");

        if let Some(lim) = limit {
            sql.push_str(&format!(" LIMIT {}", lim));
        }

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params: Vec<&dyn rusqlite::ToSql> = params_vec
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();

        let events = stmt
            .query_map(params.as_slice(), |row| {
                Ok(TaskRunEvent {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    event_type: row.get(2)?,
                    event_subtype: row.get(3)?,
                    message: row.get(4)?,
                    data: row.get(5)?,
                    workflow_name: row.get(6)?,
                    state_name: row.get(7)?,
                    action_id: row.get(8)?,
                    timestamp: row.get(9)?,
                    duration_ms: row.get(10)?,
                })
            })
            .map_err(|e| format!("Failed to get task run events: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(events)
    }

    /// Create a task run screenshot record.
    pub fn create_task_run_screenshot(
        &self,
        input: &CreateTaskRunScreenshotInput,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO task_run_screenshots (
                id, task_run_id, event_id, file_path, screenshot_type,
                template_name, confidence, match_location,
                width, height, file_size_bytes, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            "#,
            params![
                id,
                input.task_run_id,
                input.event_id,
                input.file_path,
                input.screenshot_type,
                input.template_name,
                input.confidence,
                input.match_location,
                input.width,
                input.height,
                input.file_size_bytes,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create task run screenshot: {}", e))?;

        Ok(id)
    }

    /// Get screenshots for a task run.
    pub fn get_task_run_screenshots(
        &self,
        task_run_id: &str,
        screenshot_type: Option<&str>,
    ) -> Result<Vec<TaskRunScreenshot>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT id, task_run_id, event_id, file_path, screenshot_type,
                   template_name, confidence, match_location,
                   width, height, file_size_bytes, created_at
            FROM task_run_screenshots
            WHERE task_run_id = ?1
            "#,
        );

        let mut params_vec: Vec<String> = vec![task_run_id.to_string()];

        if let Some(st) = screenshot_type {
            sql.push_str(" AND screenshot_type = ?2");
            params_vec.push(st.to_string());
        }

        sql.push_str(" ORDER BY created_at ASC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params: Vec<&dyn rusqlite::ToSql> = params_vec
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();

        let screenshots = stmt
            .query_map(params.as_slice(), |row| {
                Ok(TaskRunScreenshot {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    event_id: row.get(2)?,
                    file_path: row.get(3)?,
                    screenshot_type: row.get(4)?,
                    template_name: row.get(5)?,
                    confidence: row.get(6)?,
                    match_location: row.get(7)?,
                    width: row.get(8)?,
                    height: row.get(9)?,
                    file_size_bytes: row.get(10)?,
                    created_at: row.get(11)?,
                })
            })
            .map_err(|e| format!("Failed to get task run screenshots: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(screenshots)
    }

    /// Create a Playwright test result.
    pub fn create_task_run_playwright_result(
        &self,
        input: &CreateTaskRunPlaywrightResultInput,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO task_run_playwright_results (
                id, task_run_id, test_name, spec_file, status, duration_ms,
                stdout, stderr, console_output, page_snapshot,
                error_message, failure_screenshot_path,
                assertions_passed, assertions_failed, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            "#,
            params![
                id,
                input.task_run_id,
                input.test_name,
                input.spec_file,
                input.status,
                input.duration_ms,
                input.stdout,
                input.stderr,
                input.console_output,
                input.page_snapshot,
                input.error_message,
                input.failure_screenshot_path,
                input.assertions_passed,
                input.assertions_failed,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create Playwright result: {}", e))?;

        Ok(id)
    }

    /// Get Playwright results for a task run.
    pub fn get_task_run_playwright_results(
        &self,
        task_run_id: &str,
        status_filter: Option<&str>,
    ) -> Result<Vec<TaskRunPlaywrightResult>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT id, task_run_id, test_name, spec_file, status, duration_ms,
                   stdout, stderr, console_output, page_snapshot,
                   error_message, failure_screenshot_path,
                   assertions_passed, assertions_failed, created_at
            FROM task_run_playwright_results
            WHERE task_run_id = ?1
            "#,
        );

        let mut params_vec: Vec<String> = vec![task_run_id.to_string()];

        if let Some(status) = status_filter {
            sql.push_str(" AND status = ?2");
            params_vec.push(status.to_string());
        }

        sql.push_str(" ORDER BY created_at ASC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params: Vec<&dyn rusqlite::ToSql> = params_vec
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();

        let results = stmt
            .query_map(params.as_slice(), |row| {
                Ok(TaskRunPlaywrightResult {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    test_name: row.get(2)?,
                    spec_file: row.get(3)?,
                    status: row.get(4)?,
                    duration_ms: row.get(5)?,
                    stdout: row.get(6)?,
                    stderr: row.get(7)?,
                    console_output: row.get(8)?,
                    page_snapshot: row.get(9)?,
                    error_message: row.get(10)?,
                    failure_screenshot_path: row.get(11)?,
                    assertions_passed: row.get(12)?,
                    assertions_failed: row.get(13)?,
                    created_at: row.get(14)?,
                })
            })
            .map_err(|e| format!("Failed to get Playwright results: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    // ========================================================================
    // Execution Spans Operations
    // ========================================================================

    /// Get execution spans with optional filtering.
    ///
    /// Supports filtering by:
    /// - execution_id: Filter by task/execution ID
    /// - name_pattern: Filter span names using SQL LIKE pattern (e.g., "workflow.%")
    /// - min_duration_ms: Filter spans with duration >= this value
    /// - limit: Maximum number of spans to return (default: 100)
    pub fn get_execution_spans(
        &self,
        execution_id: Option<&str>,
        name_pattern: Option<&str>,
        min_duration_ms: Option<i64>,
        limit: Option<u32>,
    ) -> Result<Vec<ExecutionSpan>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT id, execution_id, trace_id, span_id, parent_span_id, name,
                   start_ts, end_ts, duration_ms, attributes, success, error, created_at
            FROM execution_spans
            WHERE 1=1
            "#,
        );

        let mut param_values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        let mut param_idx = 1;

        if let Some(exec_id) = execution_id {
            sql.push_str(&format!(" AND execution_id = ?{}", param_idx));
            param_values.push(Box::new(exec_id.to_string()));
            param_idx += 1;
        }

        if let Some(pattern) = name_pattern {
            sql.push_str(&format!(" AND name LIKE ?{}", param_idx));
            param_values.push(Box::new(pattern.to_string()));
            param_idx += 1;
        }

        if let Some(min_dur) = min_duration_ms {
            sql.push_str(&format!(" AND duration_ms >= ?{}", param_idx));
            param_values.push(Box::new(min_dur));
            // param_idx += 1; // Not needed as it's the last
        }

        sql.push_str(" ORDER BY start_ts DESC");

        let lim = limit.unwrap_or(100);
        sql.push_str(&format!(" LIMIT {}", lim));

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params: Vec<&dyn rusqlite::ToSql> = param_values
            .iter()
            .map(|p| p.as_ref() as &dyn rusqlite::ToSql)
            .collect();

        let spans = stmt
            .query_map(params.as_slice(), |row| {
                Ok(ExecutionSpan {
                    id: row.get(0)?,
                    execution_id: row.get(1)?,
                    trace_id: row.get(2)?,
                    span_id: row.get(3)?,
                    parent_span_id: row.get(4)?,
                    name: row.get(5)?,
                    start_ts: row.get(6)?,
                    end_ts: row.get(7)?,
                    duration_ms: row.get(8)?,
                    attributes: row.get(9)?,
                    success: row.get::<_, i32>(10)? == 1,
                    error: row.get(11)?,
                    created_at: row.get(12)?,
                })
            })
            .map_err(|e| format!("Failed to get execution spans: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(spans)
    }

    // ========================================================================
    // Task Run API Request Operations
    // ========================================================================

    /// Create a task run API request record.
    pub fn create_task_run_api_request(
        &self,
        input: &CreateTaskRunApiRequestInput,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();

        conn.execute(
            r#"
            INSERT INTO task_run_api_requests (
                id, task_run_id, step_id, step_name,
                method, url, resolved_url, request_headers, request_body,
                status_code, status_text, response_headers, response_time_ms,
                response_body_type, response_body, response_size_bytes,
                extractions, assertions, success, error_message, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
            "#,
            params![
                id,
                input.task_run_id,
                input.step_id,
                input.step_name,
                input.method,
                input.url,
                input.resolved_url,
                input.request_headers,
                input.request_body,
                input.status_code,
                input.status_text,
                input.response_headers,
                input.response_time_ms,
                input.response_body_type,
                input.response_body,
                input.response_size_bytes,
                input.extractions,
                input.assertions,
                input.success,
                input.error_message,
                input.timestamp,
            ],
        )
        .map_err(|e| format!("Failed to create task run API request: {}", e))?;

        Ok(id)
    }

    /// Batch insert task run API requests (efficient for JSONL migration).
    pub fn batch_create_task_run_api_requests(
        &self,
        requests: &[CreateTaskRunApiRequestInput],
    ) -> Result<usize, String> {
        if requests.is_empty() {
            return Ok(0);
        }

        let conn = self.get_conn()?;
        let mut count = 0;

        for input in requests {
            let id = uuid::Uuid::new_v4().to_string();

            conn.execute(
                r#"
                INSERT INTO task_run_api_requests (
                    id, task_run_id, step_id, step_name,
                    method, url, resolved_url, request_headers, request_body,
                    status_code, status_text, response_headers, response_time_ms,
                    response_body_type, response_body, response_size_bytes,
                    extractions, assertions, success, error_message, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
                "#,
                params![
                    id,
                    input.task_run_id,
                    input.step_id,
                    input.step_name,
                    input.method,
                    input.url,
                    input.resolved_url,
                    input.request_headers,
                    input.request_body,
                    input.status_code,
                    input.status_text,
                    input.response_headers,
                    input.response_time_ms,
                    input.response_body_type,
                    input.response_body,
                    input.response_size_bytes,
                    input.extractions,
                    input.assertions,
                    input.success,
                    input.error_message,
                    input.timestamp,
                ],
            )
            .map_err(|e| format!("Failed to create task run API request: {}", e))?;
            count += 1;
        }

        Ok(count)
    }

    /// Get API requests for a task run.
    pub fn get_task_run_api_requests(
        &self,
        task_run_id: &str,
        success_filter: Option<bool>,
    ) -> Result<Vec<TaskRunApiRequest>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT id, task_run_id, step_id, step_name,
                   method, url, resolved_url, request_headers, request_body,
                   status_code, status_text, response_headers, response_time_ms,
                   response_body_type, response_body, response_size_bytes,
                   extractions, assertions, success, error_message, created_at
            FROM task_run_api_requests
            WHERE task_run_id = ?1
            "#,
        );

        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(task_run_id.to_string())];

        if let Some(success) = success_filter {
            sql.push_str(" AND success = ?2");
            params_vec.push(Box::new(success));
        }

        sql.push_str(" ORDER BY created_at ASC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();

        let results = stmt
            .query_map(params.as_slice(), |row| {
                Ok(TaskRunApiRequest {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    step_id: row.get(2)?,
                    step_name: row.get(3)?,
                    method: row.get(4)?,
                    url: row.get(5)?,
                    resolved_url: row.get(6)?,
                    request_headers: row.get(7)?,
                    request_body: row.get(8)?,
                    status_code: row.get(9)?,
                    status_text: row.get(10)?,
                    response_headers: row.get(11)?,
                    response_time_ms: row.get(12)?,
                    response_body_type: row.get(13)?,
                    response_body: row.get(14)?,
                    response_size_bytes: row.get(15)?,
                    extractions: row.get(16)?,
                    assertions: row.get(17)?,
                    success: row.get(18)?,
                    error_message: row.get(19)?,
                    created_at: row.get(20)?,
                })
            })
            .map_err(|e| format!("Failed to get API requests: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Delete all events for a task run (used when clearing/re-importing).
    pub fn delete_task_run_events(&self, task_run_id: &str) -> Result<usize, String> {
        let conn = self.get_conn()?;

        let rows_affected = conn
            .execute(
                "DELETE FROM task_run_events WHERE task_run_id = ?1",
                params![task_run_id],
            )
            .map_err(|e| format!("Failed to delete task run events: {}", e))?;

        Ok(rows_affected)
    }

    // ========================================================================
    // Task Run AWAS Step Operations
    // ========================================================================

    /// Create a task run AWAS step record.
    pub fn create_task_run_awas_step(
        &self,
        input: &CreateTaskRunAwasStepInput,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();

        conn.execute(
            r#"
            INSERT INTO task_run_awas_steps (
                id, task_run_id, step_id, step_name, step_type,
                url, action_id, parameters, response_data,
                success, error_message, duration_ms, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            "#,
            params![
                id,
                input.task_run_id,
                input.step_id,
                input.step_name,
                input.step_type,
                input.url,
                input.action_id,
                input.parameters,
                input.response_data,
                input.success,
                input.error_message,
                input.duration_ms,
                input.timestamp,
            ],
        )
        .map_err(|e| format!("Failed to create task run AWAS step: {}", e))?;

        Ok(id)
    }

    /// Get AWAS steps for a task run.
    pub fn get_task_run_awas_steps(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<TaskRunAwasStep>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_run_id, step_id, step_name, step_type,
                       url, action_id, parameters, response_data,
                       success, error_message, duration_ms, created_at
                FROM task_run_awas_steps
                WHERE task_run_id = ?1
                ORDER BY created_at ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(params![task_run_id], |row| {
                Ok(TaskRunAwasStep {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    step_id: row.get(2)?,
                    step_name: row.get(3)?,
                    step_type: row.get(4)?,
                    url: row.get(5)?,
                    action_id: row.get(6)?,
                    parameters: row.get(7)?,
                    response_data: row.get(8)?,
                    success: row.get(9)?,
                    error_message: row.get(10)?,
                    duration_ms: row.get(11)?,
                    created_at: row.get(12)?,
                })
            })
            .map_err(|e| format!("Failed to get AWAS steps: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    // ========================================================================
    // Learning System Operations
    // ========================================================================

    /// Record a task outcome for learning
    pub fn record_learning_outcome(
        &self,
        task_id: &str,
        status: &str,
        duration_secs: Option<f64>,
        iterations: Option<u32>,
        strategy: Option<&str>,
        tools_used: Option<&[String]>,
        files_modified: Option<&[String]>,
        error_type: Option<&str>,
        error_message: Option<&str>,
        feedback: Option<&serde_json::Value>,
        workflow_architecture: Option<&str>,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = format!("lo-{}", uuid::Uuid::new_v4());
        let now = Utc::now().to_rfc3339();

        let tools_json = tools_used.map(|t| serde_json::to_string(t).unwrap_or_default());
        let files_json = files_modified.map(|f| serde_json::to_string(f).unwrap_or_default());
        let feedback_json = feedback.map(|f| f.to_string());

        conn.execute(
            r#"
            INSERT INTO learning_outcomes (
                id, task_id, status, duration_secs, iterations, strategy,
                tools_used, files_modified, error_type, error_message, feedback,
                workflow_architecture, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            "#,
            params![
                id,
                task_id,
                status,
                duration_secs,
                iterations,
                strategy,
                tools_json,
                files_json,
                error_type,
                error_message,
                feedback_json,
                workflow_architecture,
                now
            ],
        )
        .map_err(|e| format!("Failed to record learning outcome: {}", e))?;

        Ok(id)
    }

    /// Get learning outcomes for analysis
    pub fn get_learning_outcomes(
        &self,
        limit: Option<u32>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;
        let limit_val = limit.unwrap_or(100) as i64;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_id, status, duration_secs, iterations, strategy,
                       tools_used, files_modified, error_type, error_message, feedback, created_at,
                       workflow_architecture
                FROM learning_outcomes
                ORDER BY created_at DESC
                LIMIT ?1
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(params![limit_val], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "status": row.get::<_, String>(2)?,
                    "duration_secs": row.get::<_, Option<f64>>(3)?,
                    "iterations": row.get::<_, Option<i64>>(4)?,
                    "strategy": row.get::<_, Option<String>>(5)?,
                    "tools_used": row.get::<_, Option<String>>(6)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "files_modified": row.get::<_, Option<String>>(7)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "error_type": row.get::<_, Option<String>>(8)?,
                    "error_message": row.get::<_, Option<String>>(9)?,
                    "feedback": row.get::<_, Option<String>>(10)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "created_at": row.get::<_, String>(11)?,
                    "workflow_architecture": row.get::<_, Option<String>>(12)?,
                }))
            })
            .map_err(|e| format!("Failed to get learning outcomes: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Save or update a learning pattern
    pub fn save_learning_pattern(
        &self,
        id: &str,
        pattern_type: &str,
        description: &str,
        confidence: f64,
        occurrences: u32,
        context: Option<&serde_json::Value>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();
        let context_json = context.map(|c| c.to_string());

        conn.execute(
            r#"
            INSERT INTO learning_patterns (id, pattern_type, description, confidence, occurrences, context, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
            ON CONFLICT(id) DO UPDATE SET
                description = ?3,
                confidence = ?4,
                occurrences = ?5,
                context = ?6,
                updated_at = ?7
            "#,
            params![id, pattern_type, description, confidence, occurrences, context_json, now],
        )
        .map_err(|e| format!("Failed to save learning pattern: {}", e))?;

        Ok(())
    }

    /// Get all learning patterns
    pub fn get_learning_patterns(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, pattern_type, description, confidence, occurrences, context, created_at, updated_at
                FROM learning_patterns
                ORDER BY confidence DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "pattern_type": row.get::<_, String>(1)?,
                    "description": row.get::<_, String>(2)?,
                    "confidence": row.get::<_, f64>(3)?,
                    "occurrences": row.get::<_, i64>(4)?,
                    "context": row.get::<_, Option<String>>(5)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "created_at": row.get::<_, String>(6)?,
                    "updated_at": row.get::<_, String>(7)?,
                }))
            })
            .map_err(|e| format!("Failed to get learning patterns: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    // ========================================================================
    // Orchestrator Checkpoint Operations
    // ========================================================================

    /// Save an orchestrator checkpoint
    pub fn save_orchestrator_checkpoint(
        &self,
        id: &str,
        task_id: &str,
        iteration: u32,
        trigger: &str,
        state: &serde_json::Value,
        name: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();
        let state_json = state.to_string();

        conn.execute(
            r#"
            INSERT INTO orchestrator_checkpoints (id, task_id, iteration, trigger, state, name, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![id, task_id, iteration, trigger, state_json, name, now],
        )
        .map_err(|e| format!("Failed to save orchestrator checkpoint: {}", e))?;

        Ok(())
    }

    /// Get checkpoints for a task
    pub fn get_orchestrator_checkpoints(
        &self,
        task_id: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let query = if task_id.is_some() {
            r#"
            SELECT id, task_id, iteration, trigger, state, name, created_at
            FROM orchestrator_checkpoints
            WHERE task_id = ?1
            ORDER BY iteration ASC
            "#
        } else {
            r#"
            SELECT id, task_id, iteration, trigger, state, name, created_at
            FROM orchestrator_checkpoints
            ORDER BY created_at DESC
            LIMIT 100
            "#
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = if let Some(tid) = task_id {
            stmt.query_map(params![tid], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "trigger": row.get::<_, String>(3)?,
                    "state": row.get::<_, String>(4)?.parse::<serde_json::Value>().ok(),
                    "name": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            })
            .map_err(|e| format!("Failed to get checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        } else {
            stmt.query_map([], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "trigger": row.get::<_, String>(3)?,
                    "state": row.get::<_, String>(4)?.parse::<serde_json::Value>().ok(),
                    "name": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            })
            .map_err(|e| format!("Failed to get checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        };

        Ok(results)
    }

    /// Get a single checkpoint by ID
    pub fn get_orchestrator_checkpoint(
        &self,
        id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<serde_json::Value> = conn.query_row(
            r#"
            SELECT id, task_id, iteration, trigger, state, name, created_at
            FROM orchestrator_checkpoints
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "trigger": row.get::<_, String>(3)?,
                    "state": row.get::<_, String>(4)?.parse::<serde_json::Value>().ok(),
                    "name": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            },
        );

        match result {
            Ok(cp) => Ok(Some(cp)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get checkpoint: {}", e)),
        }
    }

    /// Delete a checkpoint
    pub fn delete_orchestrator_checkpoint(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let affected = conn
            .execute(
                "DELETE FROM orchestrator_checkpoints WHERE id = ?1",
                params![id],
            )
            .map_err(|e| format!("Failed to delete checkpoint: {}", e))?;

        Ok(affected > 0)
    }

    /// Get all unique task IDs that have checkpoints
    pub fn get_checkpoint_task_ids(&self) -> Result<Vec<String>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare("SELECT DISTINCT task_id FROM orchestrator_checkpoints ORDER BY task_id")
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| format!("Failed to get task IDs: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    // ========================================================================
    // Flow Designer Operations
    // ========================================================================

    /// Save a flow definition
    pub fn save_flow(&self, flow: &serde_json::Value) -> Result<String, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let id = flow["id"].as_str().ok_or("Flow must have an id")?;
        let name = flow["name"].as_str().ok_or("Flow must have a name")?;
        let description = flow["description"].as_str();
        let steps = serde_json::to_string(&flow["steps"]).map_err(|e| e.to_string())?;
        let start_step = flow["start_step"].as_str();
        let timeout_secs = flow["timeout_secs"].as_i64().map(|v| v as i32);
        let inputs = serde_json::to_string(&flow["inputs"]).ok();
        let outputs = serde_json::to_string(&flow["outputs"]).ok();
        let tags = serde_json::to_string(&flow["tags"]).ok();
        let version = flow["version"].as_str().unwrap_or("1.0.0");

        conn.execute(
            r#"
            INSERT INTO orchestrator_flows (
                id, name, description, steps, start_step, timeout_secs,
                inputs, outputs, tags, version, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
            ON CONFLICT(id) DO UPDATE SET
                name = ?2,
                description = ?3,
                steps = ?4,
                start_step = ?5,
                timeout_secs = ?6,
                inputs = ?7,
                outputs = ?8,
                tags = ?9,
                version = ?10,
                updated_at = ?11
            "#,
            params![
                id,
                name,
                description,
                steps,
                start_step,
                timeout_secs,
                inputs,
                outputs,
                tags,
                version,
                now
            ],
        )
        .map_err(|e| format!("Failed to save flow: {}", e))?;

        Ok(id.to_string())
    }

    /// Get a flow by ID
    pub fn get_flow(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<serde_json::Value> = conn.query_row(
            r#"
            SELECT id, name, description, steps, start_step, timeout_secs,
                   inputs, outputs, tags, version, created_at, updated_at
            FROM orchestrator_flows
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "steps": row.get::<_, String>(3)?.parse::<serde_json::Value>().ok(),
                    "start_step": row.get::<_, Option<String>>(4)?,
                    "timeout_secs": row.get::<_, Option<i32>>(5)?,
                    "inputs": row.get::<_, Option<String>>(6)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "outputs": row.get::<_, Option<String>>(7)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "tags": row.get::<_, Option<String>>(8)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "version": row.get::<_, String>(9)?,
                    "created_at": row.get::<_, String>(10)?,
                    "updated_at": row.get::<_, String>(11)?,
                }))
            },
        );

        match result {
            Ok(flow) => Ok(Some(flow)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get flow: {}", e)),
        }
    }

    /// List all flows (summaries)
    pub fn list_flows(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description,
                       json_array_length(json_extract(steps, '$')) as step_count,
                       tags, version
                FROM orchestrator_flows
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "step_count": row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    "tags": row.get::<_, Option<String>>(4)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "version": row.get::<_, String>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to list flows: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Delete a flow
    pub fn delete_flow(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let affected = conn
            .execute("DELETE FROM orchestrator_flows WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete flow: {}", e))?;

        Ok(affected > 0)
    }

    // ========================================================================
    // Flow Version History
    // ========================================================================

    /// Create a new version snapshot of a flow.
    /// Automatically increments version number.
    pub fn create_flow_version(
        &self,
        flow_id: &str,
        definition: &serde_json::Value,
        message: Option<&str>,
        created_by: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Get the next version number for this flow
        let next_version: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) + 1 FROM flow_versions WHERE flow_id = ?1",
                params![flow_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to get next version number: {}", e))?;

        let id = format!("{}_v{}", flow_id, next_version);
        let definition_json = serde_json::to_string(definition)
            .map_err(|e| format!("Failed to serialize flow definition: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO flow_versions (id, flow_id, version, definition, message, created_by, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![id, flow_id, next_version, definition_json, message, created_by, now],
        )
        .map_err(|e| format!("Failed to create flow version: {}", e))?;

        Ok(serde_json::json!({
            "id": id,
            "flow_id": flow_id,
            "version": next_version,
            "message": message,
            "created_by": created_by,
            "created_at": now
        }))
    }

    /// List all versions of a flow.
    pub fn list_flow_versions(&self, flow_id: &str) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, flow_id, version, message, created_by, created_at
                FROM flow_versions
                WHERE flow_id = ?1
                ORDER BY version DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(params![flow_id], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "version": row.get::<_, i32>(2)?,
                    "message": row.get::<_, Option<String>>(3)?,
                    "created_by": row.get::<_, Option<String>>(4)?,
                    "created_at": row.get::<_, String>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to list flow versions: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get a specific version of a flow (full definition).
    pub fn get_flow_version(
        &self,
        flow_id: &str,
        version: i32,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<serde_json::Value> = conn.query_row(
            r#"
            SELECT id, flow_id, version, definition, message, created_by, created_at
            FROM flow_versions
            WHERE flow_id = ?1 AND version = ?2
            "#,
            params![flow_id, version],
            |row| {
                let definition_str: String = row.get(3)?;
                let definition: serde_json::Value =
                    serde_json::from_str(&definition_str).unwrap_or(serde_json::Value::Null);

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "version": row.get::<_, i32>(2)?,
                    "definition": definition,
                    "message": row.get::<_, Option<String>>(4)?,
                    "created_by": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            },
        );

        match result {
            Ok(version_data) => Ok(Some(version_data)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get flow version: {}", e)),
        }
    }

    /// Restore a flow to a specific version.
    /// Creates a new version first as backup, then updates the flow.
    pub fn restore_flow_version(
        &self,
        flow_id: &str,
        version: i32,
        created_by: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        // Get the version to restore
        let version_data = self
            .get_flow_version(flow_id, version)?
            .ok_or_else(|| format!("Version {} not found for flow {}", version, flow_id))?;

        let definition = version_data["definition"].clone();

        // Get current flow for backup
        let current_flow = self.get_flow(flow_id)?;

        // Create backup version of current state before restoring
        if let Some(current) = current_flow {
            self.create_flow_version(
                flow_id,
                &current,
                Some(&format!("Backup before restoring to version {}", version)),
                created_by,
            )?;
        }

        // Update the flow with the restored definition
        self.save_flow(&definition)?;

        // Create a new version marking the restore
        let new_version = self.create_flow_version(
            flow_id,
            &definition,
            Some(&format!("Restored from version {}", version)),
            created_by,
        )?;

        Ok(serde_json::json!({
            "flow_id": flow_id,
            "restored_from_version": version,
            "new_version": new_version["version"],
            "definition": definition
        }))
    }

    /// Compare two versions of a flow.
    /// Returns the definitions of both versions for client-side diff.
    pub fn compare_flow_versions(
        &self,
        flow_id: &str,
        version1: i32,
        version2: i32,
    ) -> Result<serde_json::Value, String> {
        let v1 = self
            .get_flow_version(flow_id, version1)?
            .ok_or_else(|| format!("Version {} not found", version1))?;

        let v2 = self
            .get_flow_version(flow_id, version2)?
            .ok_or_else(|| format!("Version {} not found", version2))?;

        Ok(serde_json::json!({
            "flow_id": flow_id,
            "version1": {
                "version": version1,
                "definition": v1["definition"],
                "message": v1["message"],
                "created_at": v1["created_at"],
                "created_by": v1["created_by"]
            },
            "version2": {
                "version": version2,
                "definition": v2["definition"],
                "message": v2["message"],
                "created_at": v2["created_at"],
                "created_by": v2["created_by"]
            }
        }))
    }

    /// Delete a specific version of a flow.
    pub fn delete_flow_version(&self, flow_id: &str, version: i32) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let affected = conn
            .execute(
                "DELETE FROM flow_versions WHERE flow_id = ?1 AND version = ?2",
                params![flow_id, version],
            )
            .map_err(|e| format!("Failed to delete flow version: {}", e))?;

        Ok(affected > 0)
    }

    /// Get the latest version number of a flow.
    pub fn get_latest_flow_version(&self, flow_id: &str) -> Result<Option<i32>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<i32> = conn.query_row(
            "SELECT MAX(version) FROM flow_versions WHERE flow_id = ?1",
            params![flow_id],
            |row| row.get(0),
        );

        match result {
            Ok(version) => Ok(Some(version)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get latest version: {}", e)),
        }
    }

    /// Save flow execution state
    pub fn save_flow_execution(&self, execution: &serde_json::Value) -> Result<(), String> {
        let conn = self.get_conn()?;

        let instance_id = execution["instance_id"]
            .as_str()
            .ok_or("Execution must have instance_id")?;
        let flow_id = execution["flow_id"]
            .as_str()
            .ok_or("Execution must have flow_id")?;
        let current_step = execution["current_step"].as_str();
        let status = execution["status"].as_str().unwrap_or("pending");
        let context = serde_json::to_string(&execution["context"]).ok();
        let history = serde_json::to_string(&execution["history"]).ok();
        let error = execution["error"].as_str();
        let default_started_at = Utc::now().to_rfc3339();
        let started_at = execution["started_at"]
            .as_str()
            .unwrap_or(&default_started_at);
        let completed_at = execution["completed_at"].as_str();

        conn.execute(
            r#"
            INSERT INTO flow_executions (
                instance_id, flow_id, current_step, status, context, history, error, started_at, completed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(instance_id) DO UPDATE SET
                current_step = ?3,
                status = ?4,
                context = ?5,
                history = ?6,
                error = ?7,
                completed_at = ?9
            "#,
            params![instance_id, flow_id, current_step, status, context, history, error, started_at, completed_at],
        )
        .map_err(|e| format!("Failed to save flow execution: {}", e))?;

        Ok(())
    }

    /// Get flow execution by instance ID
    pub fn get_flow_execution(
        &self,
        instance_id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<serde_json::Value> = conn.query_row(
            r#"
            SELECT instance_id, flow_id, current_step, status, context, history, error, started_at, completed_at
            FROM flow_executions
            WHERE instance_id = ?1
            "#,
            params![instance_id],
            |row| {
                Ok(serde_json::json!({
                    "instance_id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "current_step": row.get::<_, Option<String>>(2)?,
                    "status": row.get::<_, String>(3)?,
                    "context": row.get::<_, Option<String>>(4)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "history": row.get::<_, Option<String>>(5)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "error": row.get::<_, Option<String>>(6)?,
                    "started_at": row.get::<_, String>(7)?,
                    "completed_at": row.get::<_, Option<String>>(8)?,
                }))
            },
        );

        match result {
            Ok(exec) => Ok(Some(exec)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get flow execution: {}", e)),
        }
    }

    /// List flow executions
    pub fn list_flow_executions(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT instance_id, flow_id, current_step, status, started_at, completed_at
                FROM flow_executions
                ORDER BY started_at DESC
                LIMIT 50
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                Ok(serde_json::json!({
                    "instance_id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "current_step": row.get::<_, Option<String>>(2)?,
                    "status": row.get::<_, String>(3)?,
                    "started_at": row.get::<_, String>(4)?,
                    "completed_at": row.get::<_, Option<String>>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to list flow executions: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    // ========================================================================
    // Enhanced Learning Queries (Filtering, Pagination, Date Ranges)
    // ========================================================================

    /// Get learning outcomes with optional filtering by status, strategy, and date.
    pub fn get_learning_outcomes_filtered(
        &self,
        status: Option<&str>,
        strategy: Option<&str>,
        since: Option<&str>,
        limit: Option<i64>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;
        let limit_val = limit.unwrap_or(100);

        // Build dynamic WHERE clause
        let mut conditions = Vec::new();
        if status.is_some() {
            conditions.push("status = ?1");
        }
        if strategy.is_some() {
            conditions.push("strategy = ?2");
        }
        if since.is_some() {
            conditions.push("created_at >= ?3");
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let query = format!(
            r#"
            SELECT id, task_id, status, duration_secs, iterations, strategy,
                   tools_used, files_modified, error_type, error_message, feedback, created_at,
                   workflow_architecture
            FROM learning_outcomes
            {}
            ORDER BY created_at DESC
            LIMIT ?4
            "#,
            where_clause
        );

        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(
                params![
                    status.unwrap_or(""),
                    strategy.unwrap_or(""),
                    since.unwrap_or(""),
                    limit_val
                ],
                |row| {
                    Ok(serde_json::json!({
                        "id": row.get::<_, String>(0)?,
                        "task_id": row.get::<_, String>(1)?,
                        "status": row.get::<_, String>(2)?,
                        "duration_secs": row.get::<_, Option<f64>>(3)?,
                        "iterations": row.get::<_, Option<i64>>(4)?,
                        "strategy": row.get::<_, Option<String>>(5)?,
                        "tools_used": row.get::<_, Option<String>>(6)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                        "files_modified": row.get::<_, Option<String>>(7)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                        "error_type": row.get::<_, Option<String>>(8)?,
                        "error_message": row.get::<_, Option<String>>(9)?,
                        "feedback": row.get::<_, Option<String>>(10)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                        "created_at": row.get::<_, String>(11)?,
                        "workflow_architecture": row.get::<_, Option<String>>(12)?,
                    }))
                },
            )
            .map_err(|e| format!("Failed to get learning outcomes: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get learning outcomes with pagination support.
    pub fn get_learning_outcomes_paginated(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_id, status, duration_secs, iterations, strategy,
                       tools_used, files_modified, error_type, error_message, feedback, created_at,
                       workflow_architecture
                FROM learning_outcomes
                ORDER BY created_at DESC
                LIMIT ?1 OFFSET ?2
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(params![limit, offset], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "status": row.get::<_, String>(2)?,
                    "duration_secs": row.get::<_, Option<f64>>(3)?,
                    "iterations": row.get::<_, Option<i64>>(4)?,
                    "strategy": row.get::<_, Option<String>>(5)?,
                    "tools_used": row.get::<_, Option<String>>(6)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "files_modified": row.get::<_, Option<String>>(7)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "error_type": row.get::<_, Option<String>>(8)?,
                    "error_message": row.get::<_, Option<String>>(9)?,
                    "feedback": row.get::<_, Option<String>>(10)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "created_at": row.get::<_, String>(11)?,
                    "workflow_architecture": row.get::<_, Option<String>>(12)?,
                }))
            })
            .map_err(|e| format!("Failed to get learning outcomes: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get learning statistics for a date range.
    pub fn get_learning_stats_by_date_range(
        &self,
        start: &str,
        end: &str,
    ) -> Result<serde_json::Value, String> {
        let conn = self.get_conn()?;

        // Get counts by status
        let mut status_stmt = conn
            .prepare(
                r#"
                SELECT status, COUNT(*) as count
                FROM learning_outcomes
                WHERE created_at >= ?1 AND created_at <= ?2
                GROUP BY status
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let status_counts: Vec<(String, i64)> = status_stmt
            .query_map(params![start, end], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| format!("Failed to get status counts: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Get counts by strategy
        let mut strategy_stmt = conn
            .prepare(
                r#"
                SELECT strategy, COUNT(*) as count
                FROM learning_outcomes
                WHERE created_at >= ?1 AND created_at <= ?2 AND strategy IS NOT NULL
                GROUP BY strategy
                ORDER BY count DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let strategy_counts: Vec<(String, i64)> = strategy_stmt
            .query_map(params![start, end], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| format!("Failed to get strategy counts: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Get average duration and iterations
        let avg_stats: (Option<f64>, Option<f64>) = conn
            .query_row(
                r#"
                SELECT AVG(duration_secs), AVG(iterations)
                FROM learning_outcomes
                WHERE created_at >= ?1 AND created_at <= ?2
                "#,
                params![start, end],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or((None, None));

        // Get total count
        let total: i64 = conn
            .query_row(
                r#"
                SELECT COUNT(*)
                FROM learning_outcomes
                WHERE created_at >= ?1 AND created_at <= ?2
                "#,
                params![start, end],
                |row| row.get(0),
            )
            .unwrap_or(0);

        // Convert to JSON
        let mut status_map = serde_json::Map::new();
        for (status, count) in status_counts {
            status_map.insert(status, serde_json::json!(count));
        }

        let mut strategy_map = serde_json::Map::new();
        for (strategy, count) in strategy_counts {
            strategy_map.insert(strategy, serde_json::json!(count));
        }

        Ok(serde_json::json!({
            "total": total,
            "by_status": status_map,
            "by_strategy": strategy_map,
            "avg_duration_secs": avg_stats.0,
            "avg_iterations": avg_stats.1,
            "date_range": {
                "start": start,
                "end": end
            }
        }))
    }

    /// Get total count of learning outcomes (for pagination).
    pub fn get_learning_outcomes_count(&self) -> Result<i64, String> {
        let conn = self.get_conn()?;
        conn.query_row("SELECT COUNT(*) FROM learning_outcomes", [], |row| {
            row.get(0)
        })
        .map_err(|e| format!("Failed to get count: {}", e))
    }

    // ========================================================================
    // Enhanced Checkpoint Queries (Filtering, Pagination)
    // ========================================================================

    /// Get checkpoints with optional filtering by task ID, trigger type, and date.
    pub fn get_checkpoints_filtered(
        &self,
        task_id: Option<&str>,
        trigger: Option<&str>,
        since: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        // Build dynamic WHERE clause
        let mut conditions = Vec::new();
        if task_id.is_some() {
            conditions.push("task_id = ?1");
        }
        if trigger.is_some() {
            conditions.push("trigger = ?2");
        }
        if since.is_some() {
            conditions.push("created_at >= ?3");
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let query = format!(
            r#"
            SELECT id, task_id, iteration, trigger, state, name, created_at
            FROM orchestrator_checkpoints
            {}
            ORDER BY created_at DESC
            LIMIT 100
            "#,
            where_clause
        );

        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(
                params![
                    task_id.unwrap_or(""),
                    trigger.unwrap_or(""),
                    since.unwrap_or("")
                ],
                |row| {
                    Ok(serde_json::json!({
                        "id": row.get::<_, String>(0)?,
                        "task_id": row.get::<_, String>(1)?,
                        "iteration": row.get::<_, i64>(2)?,
                        "trigger": row.get::<_, String>(3)?,
                        "state": row.get::<_, String>(4)?.parse::<serde_json::Value>().ok(),
                        "name": row.get::<_, Option<String>>(5)?,
                        "created_at": row.get::<_, String>(6)?,
                    }))
                },
            )
            .map_err(|e| format!("Failed to get checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get checkpoints with pagination support.
    pub fn get_checkpoints_paginated(
        &self,
        task_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let query = if task_id.is_some() {
            r#"
            SELECT id, task_id, iteration, trigger, state, name, created_at
            FROM orchestrator_checkpoints
            WHERE task_id = ?1
            ORDER BY iteration ASC
            LIMIT ?2 OFFSET ?3
            "#
        } else {
            r#"
            SELECT id, task_id, iteration, trigger, state, name, created_at
            FROM orchestrator_checkpoints
            ORDER BY created_at DESC
            LIMIT ?2 OFFSET ?3
            "#
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = if let Some(tid) = task_id {
            stmt.query_map(params![tid, limit, offset], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "trigger": row.get::<_, String>(3)?,
                    "state": row.get::<_, String>(4)?.parse::<serde_json::Value>().ok(),
                    "name": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            })
            .map_err(|e| format!("Failed to get checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        } else {
            stmt.query_map(params!["", limit, offset], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "trigger": row.get::<_, String>(3)?,
                    "state": row.get::<_, String>(4)?.parse::<serde_json::Value>().ok(),
                    "name": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            })
            .map_err(|e| format!("Failed to get checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        };

        Ok(results)
    }

    /// Get total count of checkpoints (for pagination).
    pub fn get_checkpoints_count(&self, task_id: Option<&str>) -> Result<i64, String> {
        let conn = self.get_conn()?;
        if let Some(tid) = task_id {
            conn.query_row(
                "SELECT COUNT(*) FROM orchestrator_checkpoints WHERE task_id = ?1",
                params![tid],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to get count: {}", e))
        } else {
            conn.query_row("SELECT COUNT(*) FROM orchestrator_checkpoints", [], |row| {
                row.get(0)
            })
            .map_err(|e| format!("Failed to get count: {}", e))
        }
    }

    // ========================================================================
    // Enhanced Flow Queries (Tag Filtering, Execution Filtering)
    // ========================================================================

    /// Get flows filtered by tag.
    pub fn get_flows_by_tag(&self, tag: &str) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        // Use JSON search to find flows with the specified tag
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description,
                       json_array_length(json_extract(steps, '$')) as step_count,
                       tags, version
                FROM orchestrator_flows
                WHERE json_extract(tags, '$') LIKE ?1
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let search_pattern = format!("%\"{}%", tag);
        let results = stmt
            .query_map(params![search_pattern], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "step_count": row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    "tags": row.get::<_, Option<String>>(4)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "version": row.get::<_, String>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to get flows by tag: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get flow executions filtered by flow ID and/or status.
    pub fn get_flow_executions_filtered(
        &self,
        flow_id: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        // Build dynamic WHERE clause
        let mut conditions = Vec::new();
        if flow_id.is_some() {
            conditions.push("flow_id = ?1");
        }
        if status.is_some() {
            conditions.push("status = ?2");
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let query = format!(
            r#"
            SELECT instance_id, flow_id, current_step, status, started_at, completed_at
            FROM flow_executions
            {}
            ORDER BY started_at DESC
            LIMIT 100
            "#,
            where_clause
        );

        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(
                params![flow_id.unwrap_or(""), status.unwrap_or("")],
                |row| {
                    Ok(serde_json::json!({
                        "instance_id": row.get::<_, String>(0)?,
                        "flow_id": row.get::<_, String>(1)?,
                        "current_step": row.get::<_, Option<String>>(2)?,
                        "status": row.get::<_, String>(3)?,
                        "started_at": row.get::<_, String>(4)?,
                        "completed_at": row.get::<_, Option<String>>(5)?,
                    }))
                },
            )
            .map_err(|e| format!("Failed to get flow executions: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get flow executions with pagination support.
    pub fn get_flow_executions_paginated(
        &self,
        flow_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let query = if flow_id.is_some() {
            r#"
            SELECT instance_id, flow_id, current_step, status, started_at, completed_at
            FROM flow_executions
            WHERE flow_id = ?1
            ORDER BY started_at DESC
            LIMIT ?2 OFFSET ?3
            "#
        } else {
            r#"
            SELECT instance_id, flow_id, current_step, status, started_at, completed_at
            FROM flow_executions
            ORDER BY started_at DESC
            LIMIT ?2 OFFSET ?3
            "#
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = if let Some(fid) = flow_id {
            stmt.query_map(params![fid, limit, offset], |row| {
                Ok(serde_json::json!({
                    "instance_id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "current_step": row.get::<_, Option<String>>(2)?,
                    "status": row.get::<_, String>(3)?,
                    "started_at": row.get::<_, String>(4)?,
                    "completed_at": row.get::<_, Option<String>>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to get flow executions: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        } else {
            stmt.query_map(params!["", limit, offset], |row| {
                Ok(serde_json::json!({
                    "instance_id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "current_step": row.get::<_, Option<String>>(2)?,
                    "status": row.get::<_, String>(3)?,
                    "started_at": row.get::<_, String>(4)?,
                    "completed_at": row.get::<_, Option<String>>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to get flow executions: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        };

        Ok(results)
    }

    /// Get total count of flow executions (for pagination).
    pub fn get_flow_executions_count(&self, flow_id: Option<&str>) -> Result<i64, String> {
        let conn = self.get_conn()?;
        if let Some(fid) = flow_id {
            conn.query_row(
                "SELECT COUNT(*) FROM flow_executions WHERE flow_id = ?1",
                params![fid],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to get count: {}", e))
        } else {
            conn.query_row("SELECT COUNT(*) FROM flow_executions", [], |row| row.get(0))
                .map_err(|e| format!("Failed to get count: {}", e))
        }
    }

    // ========================================================================
    // Task Run with Learning Outcome Queries (for Dashboard Integration)
    // ========================================================================

    /// Get recent task runs with their learning outcomes joined.
    /// Returns task runs along with any associated learning outcome data.
    pub fn get_recent_task_runs_with_outcomes(
        &self,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT
                    t.id, t.task_name, t.prompt, t.task_type, t.status,
                    t.sessions_count, t.max_sessions, t.error_message,
                    COALESCE(t.summary, t.ai_summary) as summary,
                    t.goal_achieved, t.remaining_work,
                    t.created_at, t.updated_at, t.completed_at,
                    l.id as outcome_id, l.status as outcome_status,
                    l.duration_secs, l.iterations, l.strategy,
                    l.tools_used, l.files_modified, l.error_type, l.error_message as outcome_error,
                    l.feedback, l.created_at as outcome_created_at
                FROM task_runs t
                LEFT JOIN learning_outcomes l ON t.id = l.task_id
                ORDER BY t.updated_at DESC
                LIMIT ?1
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results = stmt
            .query_map(params![limit], |row| {
                // Parse tools_used JSON if present
                let tools_used: Option<serde_json::Value> = row
                    .get::<_, Option<String>>(19)?
                    .and_then(|s| serde_json::from_str(&s).ok());

                // Parse files_modified JSON if present
                let files_modified: Option<serde_json::Value> = row
                    .get::<_, Option<String>>(20)?
                    .and_then(|s| serde_json::from_str(&s).ok());

                // Parse feedback JSON if present
                let feedback: Option<serde_json::Value> = row
                    .get::<_, Option<String>>(23)?
                    .and_then(|s| serde_json::from_str(&s).ok());

                Ok(serde_json::json!({
                    "task": {
                        "id": row.get::<_, String>(0)?,
                        "task_name": row.get::<_, String>(1)?,
                        "prompt": row.get::<_, Option<String>>(2)?,
                        "task_type": row.get::<_, Option<String>>(3)?,
                        "status": row.get::<_, String>(4)?,
                        "sessions_count": row.get::<_, i64>(5)?,
                        "max_sessions": row.get::<_, Option<i64>>(6)?,
                        "error_message": row.get::<_, Option<String>>(7)?,
                        "summary": row.get::<_, Option<String>>(8)?,
                        "goal_achieved": row.get::<_, Option<i32>>(9)?.map(|v| v != 0),
                        "remaining_work": row.get::<_, Option<String>>(10)?,
                        "created_at": row.get::<_, String>(11)?,
                        "updated_at": row.get::<_, String>(12)?,
                        "completed_at": row.get::<_, Option<String>>(13)?,
                    },
                    "learning_outcome": if row.get::<_, Option<String>>(14)?.is_some() {
                        Some(serde_json::json!({
                            "id": row.get::<_, Option<String>>(14)?,
                            "status": row.get::<_, Option<String>>(15)?,
                            "duration_secs": row.get::<_, Option<f64>>(16)?,
                            "iterations": row.get::<_, Option<i64>>(17)?,
                            "strategy": row.get::<_, Option<String>>(18)?,
                            "tools_used": tools_used,
                            "files_modified": files_modified,
                            "error_type": row.get::<_, Option<String>>(21)?,
                            "error_message": row.get::<_, Option<String>>(22)?,
                            "feedback": feedback,
                            "created_at": row.get::<_, Option<String>>(24)?,
                        }))
                    } else {
                        None
                    }
                }))
            })
            .map_err(|e| format!("Failed to query task runs with outcomes: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get the most recent task run with checkpoints (for auto-selection in checkpoint browser).
    pub fn get_most_recent_task_with_checkpoints(&self) -> Result<Option<String>, String> {
        let conn = self.get_conn()?;

        let result: Result<String, _> = conn.query_row(
            r#"
            SELECT DISTINCT t.id
            FROM task_runs t
            INNER JOIN orchestrator_checkpoints c ON t.id = c.task_id
            ORDER BY t.updated_at DESC
            LIMIT 1
            "#,
            [],
            |row| row.get(0),
        );

        match result {
            Ok(task_id) => Ok(Some(task_id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!(
                "Failed to get most recent task with checkpoints: {}",
                e
            )),
        }
    }

    /// Get learning statistics summary (for dashboard cards).
    pub fn get_learning_stats_summary(&self) -> Result<serde_json::Value, String> {
        let conn = self.get_conn()?;

        // Get counts by status
        let mut stmt = conn
            .prepare(
                r#"
                SELECT
                    COUNT(*) as total,
                    SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) as success_count,
                    SUM(CASE WHEN status = 'failure' THEN 1 ELSE 0 END) as failure_count,
                    SUM(CASE WHEN status = 'partial' THEN 1 ELSE 0 END) as partial_count,
                    AVG(duration_secs) as avg_duration,
                    AVG(iterations) as avg_iterations
                FROM learning_outcomes
                "#,
            )
            .map_err(|e| format!("Failed to prepare stats query: {}", e))?;

        let stats = stmt
            .query_row([], |row| {
                let total: i64 = row.get(0)?;
                let success: i64 = row.get(1)?;
                let failure: i64 = row.get(2)?;
                let partial: i64 = row.get(3)?;
                let avg_duration: Option<f64> = row.get(4)?;
                let avg_iterations: Option<f64> = row.get(5)?;

                let success_rate = if total > 0 {
                    (success as f64 / total as f64) * 100.0
                } else {
                    0.0
                };

                Ok(serde_json::json!({
                    "total_tasks": total,
                    "success_count": success,
                    "failure_count": failure,
                    "partial_count": partial,
                    "success_rate": success_rate,
                    "avg_duration_secs": avg_duration,
                    "avg_iterations": avg_iterations,
                }))
            })
            .map_err(|e| format!("Failed to get learning stats: {}", e))?;

        Ok(stats)
    }

    // ========================================================================
    // Comprehensive Data Export (for Backup)

    // ========================================================================
    // Workflow State Management Operations
    // ========================================================================

    /// Save or update workflow execution state.
    pub fn save_workflow_execution_state(
        &self,
        execution_id: &str,
        workflow_type: &str,
        state_name: &str,
        state_data: Option<&str>,
        phase: Option<&str>,
        iteration: Option<u32>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO workflow_execution_state (
                execution_id, workflow_type, state_name, state_data, phase, iteration, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(execution_id) DO UPDATE SET
                workflow_type = ?2,
                state_name = ?3,
                state_data = ?4,
                phase = ?5,
                iteration = ?6,
                updated_at = ?7
            "#,
            params![
                execution_id,
                workflow_type,
                state_name,
                state_data,
                phase,
                iteration.map(|i| i as i64),
                now,
            ],
        )
        .map_err(|e| format!("Failed to save workflow execution state: {}", e))?;

        Ok(())
    }

    /// Get workflow execution state by execution_id.
    pub fn get_workflow_execution_state(
        &self,
        execution_id: &str,
    ) -> Result<Option<crate::workflow_state::WorkflowExecutionStateRecord>, String> {
        let conn = self.get_conn()?;

        let result = conn
            .query_row(
                r#"
                SELECT execution_id, workflow_type, state_name, state_data, phase, iteration, updated_at
                FROM workflow_execution_state
                WHERE execution_id = ?1
                "#,
                params![execution_id],
                |row| {
                    Ok(crate::workflow_state::WorkflowExecutionStateRecord {
                        execution_id: row.get(0)?,
                        workflow_type: row.get(1)?,
                        state_name: row.get(2)?,
                        state_data: row.get(3)?,
                        phase: row.get(4)?,
                        iteration: row.get::<_, Option<i64>>(5)?.map(|i| i as u32),
                        updated_at: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(|e| format!("Failed to get workflow execution state: {}", e))?;

        Ok(result)
    }

    /// Delete workflow execution state.
    pub fn delete_workflow_execution_state(&self, execution_id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;

        conn.execute(
            "DELETE FROM workflow_execution_state WHERE execution_id = ?1",
            params![execution_id],
        )
        .map_err(|e| format!("Failed to delete workflow execution state: {}", e))?;

        Ok(())
    }

    // ========================================================================
    // Workflow Step Checkpoint Operations
    // ========================================================================

    /// Save or update a workflow step checkpoint.
    pub fn save_workflow_step_checkpoint(
        &self,
        checkpoint: &crate::workflow_state::StepCheckpoint,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;

        conn.execute(
            r#"
            INSERT INTO workflow_step_checkpoints (
                id, execution_id, workflow_type, phase, iteration, step_index,
                stage_index, step_type, step_name, status, result_json, step_config_json,
                started_at, completed_at, duration_ms, error
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, COALESCE(?7, 0), ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
            ON CONFLICT(execution_id, phase, iteration, step_index, stage_index) DO UPDATE SET
                status = ?10,
                result_json = ?11,
                step_config_json = COALESCE(?12, step_config_json),
                started_at = COALESCE(?13, started_at),
                completed_at = ?14,
                duration_ms = ?15,
                error = ?16
            "#,
            params![
                checkpoint.id,
                checkpoint.execution_id,
                checkpoint.workflow_type,
                checkpoint.phase,
                checkpoint.iteration.map(|i| i as i64),
                checkpoint.step_index as i64,
                checkpoint.stage_index.map(|i| i as i64),
                checkpoint.step_type,
                checkpoint.step_name,
                checkpoint.status.to_string(),
                checkpoint.result_json,
                checkpoint.step_config_json,
                checkpoint.started_at,
                checkpoint.completed_at,
                checkpoint.duration_ms,
                checkpoint.error,
            ],
        )
        .map_err(|e| format!("Failed to save workflow step checkpoint: {}", e))?;

        Ok(())
    }

    /// Atomically save both workflow execution state and a step checkpoint.
    ///
    /// This is critical for ensuring data consistency when a step completes and the
    /// workflow state advances. If either operation fails, both are rolled back.
    ///
    /// # Arguments
    /// * `execution_id` - The execution/task run ID
    /// * `workflow_type` - Type of workflow (e.g., "unified")
    /// * `state_name` - Name of the new workflow state
    /// * `state_data` - Serialized state data (JSON)
    /// * `phase` - Current phase name
    /// * `iteration` - Current iteration number
    /// * `checkpoint` - The step checkpoint to save
    pub fn save_state_and_checkpoint_atomic(
        &self,
        execution_id: &str,
        workflow_type: &str,
        state_name: &str,
        state_data: Option<&str>,
        phase: Option<&str>,
        iteration: Option<u32>,
        checkpoint: &crate::workflow_state::StepCheckpoint,
    ) -> Result<(), String> {
        self.transaction(|conn| {
            let now = chrono::Utc::now().to_rfc3339();

            // Save workflow execution state
            conn.execute(
                r#"
                INSERT INTO workflow_execution_state (
                    execution_id, workflow_type, state_name, state_data, phase, iteration, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(execution_id) DO UPDATE SET
                    workflow_type = ?2,
                    state_name = ?3,
                    state_data = ?4,
                    phase = ?5,
                    iteration = ?6,
                    updated_at = ?7
                "#,
                params![
                    execution_id,
                    workflow_type,
                    state_name,
                    state_data,
                    phase,
                    iteration.map(|i| i as i64),
                    now,
                ],
            )?;

            // Save step checkpoint
            conn.execute(
                r#"
                INSERT INTO workflow_step_checkpoints (
                    id, execution_id, workflow_type, phase, iteration, step_index,
                    stage_index, step_type, step_name, status, result_json, step_config_json,
                    started_at, completed_at, duration_ms, error
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, COALESCE(?7, 0), ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
                ON CONFLICT(execution_id, phase, iteration, step_index, stage_index) DO UPDATE SET
                    status = ?10,
                    result_json = ?11,
                    step_config_json = COALESCE(?12, step_config_json),
                    started_at = COALESCE(?13, started_at),
                    completed_at = ?14,
                    duration_ms = ?15,
                    error = ?16
                "#,
                params![
                    checkpoint.id,
                    checkpoint.execution_id,
                    checkpoint.workflow_type,
                    checkpoint.phase,
                    checkpoint.iteration.map(|i| i as i64),
                    checkpoint.step_index as i64,
                    checkpoint.stage_index.map(|i| i as i64),
                    checkpoint.step_type,
                    checkpoint.step_name,
                    checkpoint.status.to_string(),
                    checkpoint.result_json,
                    checkpoint.step_config_json,
                    checkpoint.started_at,
                    checkpoint.completed_at,
                    checkpoint.duration_ms,
                    checkpoint.error,
                ],
            )?;

            Ok(())
        })
    }

    /// Get workflow step checkpoints for a given execution, phase, and iteration.
    pub fn get_workflow_step_checkpoints(
        &self,
        execution_id: &str,
        phase: &str,
        iteration: Option<u32>,
    ) -> Result<Vec<crate::workflow_state::StepCheckpoint>, String> {
        let conn = self.get_conn()?;

        let query = if iteration.is_some() {
            r#"
            SELECT id, execution_id, workflow_type, phase, iteration, step_index,
                   stage_index, step_type, step_name, status, result_json, step_config_json,
                   started_at, completed_at, duration_ms, error
            FROM workflow_step_checkpoints
            WHERE execution_id = ?1 AND phase = ?2 AND iteration = ?3
            ORDER BY step_index ASC
            "#
        } else {
            r#"
            SELECT id, execution_id, workflow_type, phase, iteration, step_index,
                   stage_index, step_type, step_name, status, result_json, step_config_json,
                   started_at, completed_at, duration_ms, error
            FROM workflow_step_checkpoints
            WHERE execution_id = ?1 AND phase = ?2 AND iteration IS NULL
            ORDER BY step_index ASC
            "#
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let row_mapper =
            |row: &rusqlite::Row| -> rusqlite::Result<crate::workflow_state::StepCheckpoint> {
                let status_str: String = row.get(9)?;
                let status = status_str
                    .parse()
                    .unwrap_or(crate::workflow_state::StepCheckpointStatus::Pending);

                Ok(crate::workflow_state::StepCheckpoint {
                    id: row.get(0)?,
                    execution_id: row.get(1)?,
                    workflow_type: row.get(2)?,
                    phase: row.get(3)?,
                    iteration: row.get::<_, Option<i64>>(4)?.map(|i| i as u32),
                    step_index: row.get::<_, i64>(5)? as usize,
                    stage_index: row.get::<_, Option<i64>>(6)?.map(|i| i as u32),
                    step_type: row.get(7)?,
                    step_name: row.get(8)?,
                    status,
                    result_json: row.get(10)?,
                    step_config_json: row.get(11)?,
                    started_at: row.get(12)?,
                    completed_at: row.get(13)?,
                    duration_ms: row.get(14)?,
                    error: row.get(15)?,
                })
            };

        let checkpoints: Vec<crate::workflow_state::StepCheckpoint> = if let Some(iter) = iteration
        {
            stmt.query_map(params![execution_id, phase, iter as i64], row_mapper)
                .map_err(|e| format!("Failed to get step checkpoints: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            stmt.query_map(params![execution_id, phase], row_mapper)
                .map_err(|e| format!("Failed to get step checkpoints: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        };

        Ok(checkpoints)
    }

    /// Get workflow step checkpoints with cursor-based pagination.
    ///
    /// This is optimized for handling runs with 1000+ steps without loading all data at once.
    /// Uses step_index as the cursor for efficient pagination.
    ///
    /// # Arguments
    /// * `execution_id` - The execution/task run ID
    /// * `cursor` - Optional step_index to start from (exclusive). None means start from beginning.
    /// * `limit` - Maximum number of checkpoints to return
    ///
    /// # Returns
    /// A tuple of (checkpoints, next_cursor). If next_cursor is Some, there are more results.
    pub fn get_workflow_step_checkpoints_paginated(
        &self,
        execution_id: &str,
        cursor: Option<i64>,
        limit: usize,
    ) -> Result<(Vec<crate::workflow_state::StepCheckpoint>, Option<i64>), String> {
        let conn = self.get_conn()?;

        // Use cursor-based pagination for efficiency
        // The idx_step_checkpoints_cursor index on (execution_id, step_index) makes this fast
        let query = if cursor.is_some() {
            r#"
            SELECT id, execution_id, workflow_type, phase, iteration, step_index,
                   stage_index, step_type, step_name, status, result_json, step_config_json,
                   started_at, completed_at, duration_ms, error
            FROM workflow_step_checkpoints
            WHERE execution_id = ?1 AND step_index > ?2
            ORDER BY step_index ASC
            LIMIT ?3
            "#
        } else {
            r#"
            SELECT id, execution_id, workflow_type, phase, iteration, step_index,
                   stage_index, step_type, step_name, status, result_json, step_config_json,
                   started_at, completed_at, duration_ms, error
            FROM workflow_step_checkpoints
            WHERE execution_id = ?1
            ORDER BY step_index ASC
            LIMIT ?2
            "#
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let row_mapper =
            |row: &rusqlite::Row| -> rusqlite::Result<crate::workflow_state::StepCheckpoint> {
                let status_str: String = row.get(9)?;
                let status = status_str
                    .parse()
                    .unwrap_or(crate::workflow_state::StepCheckpointStatus::Pending);

                Ok(crate::workflow_state::StepCheckpoint {
                    id: row.get(0)?,
                    execution_id: row.get(1)?,
                    workflow_type: row.get(2)?,
                    phase: row.get(3)?,
                    iteration: row.get::<_, Option<i64>>(4)?.map(|i| i as u32),
                    step_index: row.get::<_, i64>(5)? as usize,
                    stage_index: row.get::<_, Option<i64>>(6)?.map(|i| i as u32),
                    step_type: row.get(7)?,
                    step_name: row.get(8)?,
                    status,
                    result_json: row.get(10)?,
                    step_config_json: row.get(11)?,
                    started_at: row.get(12)?,
                    completed_at: row.get(13)?,
                    duration_ms: row.get(14)?,
                    error: row.get(15)?,
                })
            };

        // Request one more than limit to check if there are more results
        let fetch_limit = (limit + 1) as i64;

        let checkpoints: Vec<crate::workflow_state::StepCheckpoint> =
            if let Some(cursor_val) = cursor {
                stmt.query_map(params![execution_id, cursor_val, fetch_limit], row_mapper)
                    .map_err(|e| format!("Failed to get step checkpoints: {}", e))?
                    .filter_map(|r| r.ok())
                    .collect()
            } else {
                stmt.query_map(params![execution_id, fetch_limit], row_mapper)
                    .map_err(|e| format!("Failed to get step checkpoints: {}", e))?
                    .filter_map(|r| r.ok())
                    .collect()
            };

        // Determine if there are more results and what the next cursor should be
        let (result_checkpoints, next_cursor) = if checkpoints.len() > limit {
            // There are more results; return only `limit` items
            let mut result = checkpoints;
            result.truncate(limit);
            let last_step_index = result.last().map(|cp| cp.step_index as i64);
            (result, last_step_index)
        } else {
            // No more results
            (checkpoints, None)
        };

        Ok((result_checkpoints, next_cursor))
    }

    /// Delete workflow step checkpoints.
    pub fn delete_workflow_step_checkpoints(
        &self,
        execution_id: &str,
        phase: Option<&str>,
        iteration: Option<u32>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;

        match (phase, iteration) {
            (Some(p), Some(i)) => {
                conn.execute(
                    "DELETE FROM workflow_step_checkpoints WHERE execution_id = ?1 AND phase = ?2 AND iteration = ?3",
                    params![execution_id, p, i as i64],
                )
            }
            (Some(p), None) => {
                conn.execute(
                    "DELETE FROM workflow_step_checkpoints WHERE execution_id = ?1 AND phase = ?2",
                    params![execution_id, p],
                )
            }
            (None, _) => {
                conn.execute(
                    "DELETE FROM workflow_step_checkpoints WHERE execution_id = ?1",
                    params![execution_id],
                )
            }
        }
        .map_err(|e| format!("Failed to delete step checkpoints: {}", e))?;

        Ok(())
    }

    // ========================================================================
    // Step Progress Marker Operations
    // ========================================================================

    /// Save a progress marker for a step checkpoint.
    ///
    /// Progress markers track intra-step progress, such as "analyzed 50/100 files".
    /// This is useful for long-running AI operations where you want to show progress
    /// and enable resume from the last known position.
    pub fn save_step_progress_marker(
        &self,
        checkpoint_id: &str,
        marker_type: &str,
        current_value: u64,
        total_value: Option<u64>,
        description: Option<&str>,
        data_json: Option<&str>,
    ) -> Result<i64, String> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO step_progress_markers (
                checkpoint_id, marker_type, current_value, total_value,
                description, data_json, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![
                checkpoint_id,
                marker_type,
                current_value as i64,
                total_value.map(|v| v as i64),
                description,
                data_json,
                now,
            ],
        )
        .map_err(|e| format!("Failed to save step progress marker: {}", e))?;

        Ok(conn.last_insert_rowid())
    }

    /// Get the latest progress marker for a step checkpoint.
    ///
    /// Returns the most recent progress marker for the given checkpoint_id,
    /// which can be used to resume from the last known position.
    pub fn get_latest_step_progress_marker(
        &self,
        checkpoint_id: &str,
    ) -> Result<Option<crate::workflow_state::StepProgressMarker>, String> {
        let conn = self.get_conn()?;

        let result = conn.query_row(
            r#"
                SELECT id, checkpoint_id, marker_type, current_value, total_value,
                       description, data_json, created_at
                FROM step_progress_markers
                WHERE checkpoint_id = ?1
                ORDER BY id DESC
                LIMIT 1
                "#,
            params![checkpoint_id],
            |row| {
                Ok(crate::workflow_state::StepProgressMarker {
                    id: row.get(0)?,
                    checkpoint_id: row.get(1)?,
                    marker_type: row.get(2)?,
                    current_value: row.get::<_, i64>(3)? as u64,
                    total_value: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                    description: row.get(5)?,
                    data_json: row.get(6)?,
                    created_at: row.get(7)?,
                })
            },
        );

        match result {
            Ok(marker) => Ok(Some(marker)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get step progress marker: {}", e)),
        }
    }

    /// Get all progress markers for a step checkpoint.
    ///
    /// Returns all progress markers in order of creation (oldest first).
    pub fn get_step_progress_markers(
        &self,
        checkpoint_id: &str,
    ) -> Result<Vec<crate::workflow_state::StepProgressMarker>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, checkpoint_id, marker_type, current_value, total_value,
                       description, data_json, created_at
                FROM step_progress_markers
                WHERE checkpoint_id = ?1
                ORDER BY id ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let markers = stmt
            .query_map(params![checkpoint_id], |row| {
                Ok(crate::workflow_state::StepProgressMarker {
                    id: row.get(0)?,
                    checkpoint_id: row.get(1)?,
                    marker_type: row.get(2)?,
                    current_value: row.get::<_, i64>(3)? as u64,
                    total_value: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                    description: row.get(5)?,
                    data_json: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })
            .map_err(|e| format!("Failed to get step progress markers: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(markers)
    }

    /// Delete all progress markers for a step checkpoint.
    pub fn delete_step_progress_markers(&self, checkpoint_id: &str) -> Result<usize, String> {
        let conn = self.get_conn()?;

        let deleted = conn
            .execute(
                "DELETE FROM step_progress_markers WHERE checkpoint_id = ?1",
                params![checkpoint_id],
            )
            .map_err(|e| format!("Failed to delete step progress markers: {}", e))?;

        Ok(deleted)
    }

    // ========================================================================
    // Full Workflow State (for frontend restart recovery)
    // ========================================================================

    /// Get all workflow step checkpoints for an execution (all phases).
    ///
    /// This is used by the full-state endpoint to return all checkpoints for restart recovery.
    pub fn get_all_workflow_step_checkpoints(
        &self,
        execution_id: &str,
    ) -> Result<Vec<crate::workflow_state::StepCheckpoint>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, execution_id, workflow_type, phase, iteration, step_index,
                       stage_index, step_type, step_name, status, result_json, step_config_json,
                       started_at, completed_at, duration_ms, error
                FROM workflow_step_checkpoints
                WHERE execution_id = ?1
                ORDER BY COALESCE(stage_index, 0), phase, COALESCE(iteration, 0), step_index ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let row_mapper =
            |row: &rusqlite::Row| -> rusqlite::Result<crate::workflow_state::StepCheckpoint> {
                let status_str: String = row.get(9)?;
                let status = status_str
                    .parse()
                    .unwrap_or(crate::workflow_state::StepCheckpointStatus::Pending);

                Ok(crate::workflow_state::StepCheckpoint {
                    id: row.get(0)?,
                    execution_id: row.get(1)?,
                    workflow_type: row.get(2)?,
                    phase: row.get(3)?,
                    iteration: row.get::<_, Option<i64>>(4)?.map(|i| i as u32),
                    step_index: row.get::<_, i64>(5)? as usize,
                    stage_index: row.get::<_, Option<i64>>(6)?.map(|i| i as u32),
                    step_type: row.get(7)?,
                    step_name: row.get(8)?,
                    status,
                    result_json: row.get(10)?,
                    step_config_json: row.get(11)?,
                    started_at: row.get(12)?,
                    completed_at: row.get(13)?,
                    duration_ms: row.get(14)?,
                    error: row.get(15)?,
                })
            };

        let checkpoints: Vec<crate::workflow_state::StepCheckpoint> = stmt
            .query_map(params![execution_id], row_mapper)
            .map_err(|e| format!("Failed to get all step checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(checkpoints)
    }

    /// Get the progress marker for the currently running step (if any).
    ///
    /// Finds the checkpoint that is in "running" status and returns its latest progress marker.
    pub fn get_current_step_progress(
        &self,
        execution_id: &str,
    ) -> Result<Option<crate::workflow_state::StepProgressMarker>, String> {
        let conn = self.get_conn()?;

        // First find the running checkpoint
        let running_checkpoint_id: Option<String> = conn
            .query_row(
                r#"
                SELECT id FROM workflow_step_checkpoints
                WHERE execution_id = ?1 AND status = 'running'
                ORDER BY step_index DESC
                LIMIT 1
                "#,
                params![execution_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("Failed to find running checkpoint: {}", e))?;

        match running_checkpoint_id {
            Some(checkpoint_id) => self.get_latest_step_progress_marker(&checkpoint_id),
            None => Ok(None),
        }
    }

    // ========================================================================
    // Cached App Specs
    // ========================================================================

    /// Upsert a cached spec for an external app.
    pub fn upsert_cached_spec(
        &self,
        app_url: &str,
        app_name: &str,
        spec_id: &str,
        spec_json: &str,
        page_url: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let id = format!("{}:{}", app_url, spec_id);
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO cached_app_specs (id, app_url, app_name, spec_id, spec_json, discovered_at, page_url)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(id) DO UPDATE SET
                app_name = excluded.app_name,
                spec_json = excluded.spec_json,
                discovered_at = excluded.discovered_at,
                page_url = excluded.page_url
            "#,
            params![id, app_url, app_name, spec_id, spec_json, now, page_url],
        )
        .map_err(|e| format!("Failed to upsert cached spec: {}", e))?;

        Ok(())
    }

    /// Get all cached specs for a specific app URL.
    pub fn get_cached_specs_for_app(&self, app_url: &str) -> Result<Vec<CachedAppSpec>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, app_url, app_name, spec_id, spec_json, discovered_at, page_url
                FROM cached_app_specs
                WHERE app_url = ?1
                ORDER BY spec_id
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let rows = stmt
            .query_map(params![app_url], |row| {
                Ok(CachedAppSpec {
                    id: row.get(0)?,
                    app_url: row.get(1)?,
                    app_name: row.get(2)?,
                    spec_id: row.get(3)?,
                    spec_json: row.get(4)?,
                    discovered_at: row.get(5)?,
                    page_url: row.get(6)?,
                })
            })
            .map_err(|e| format!("Failed to query cached specs: {}", e))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| format!("Failed to read row: {}", e))?);
        }
        Ok(result)
    }

    /// Get all cached specs across all apps.
    pub fn get_all_cached_specs(&self) -> Result<Vec<CachedAppSpec>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, app_url, app_name, spec_id, spec_json, discovered_at, page_url
                FROM cached_app_specs
                ORDER BY app_url, spec_id
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(CachedAppSpec {
                    id: row.get(0)?,
                    app_url: row.get(1)?,
                    app_name: row.get(2)?,
                    spec_id: row.get(3)?,
                    spec_json: row.get(4)?,
                    discovered_at: row.get(5)?,
                    page_url: row.get(6)?,
                })
            })
            .map_err(|e| format!("Failed to query cached specs: {}", e))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| format!("Failed to read row: {}", e))?);
        }
        Ok(result)
    }

    /// Delete all cached specs for a specific app URL.
    pub fn delete_cached_specs_for_app(&self, app_url: &str) -> Result<u64, String> {
        let conn = self.get_conn()?;
        let deleted = conn
            .execute(
                "DELETE FROM cached_app_specs WHERE app_url = ?1",
                params![app_url],
            )
            .map_err(|e| format!("Failed to delete cached specs: {}", e))?;

        Ok(deleted as u64)
    }

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

    // ========================================================================
    // Generator Evaluation - Pipeline Artifacts
    // ========================================================================

    /// Save a pipeline artifact from a generation run.
    pub fn save_pipeline_artifact(
        &self,
        artifact: &crate::workflow_generation::pipeline_artifacts::PipelineArtifact,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"INSERT INTO generation_pipeline_artifacts
                (id, workflow_id, task_run_id, description, category, created_at,
                 investigation_duration_ms, investigation_enriched_description,
                 specification_duration_ms, specification_criteria,
                 specification_prompt, builder_prompt, verification_prompts, hardener_prompt,
                 discovery_duration_ms, builder_duration_ms, autofix_duration_ms,
                 verification_duration_ms, hardener_duration_ms, total_duration_ms,
                 discovery_calls, builder_raw_output, builder_parsed_json, autofix_diff,
                 verification_iterations, fixer_snapshots, hardening_summary,
                 hardened_json, final_json, validation_errors,
                 success, error_message, model_used,
                 revision_duration_ms, quality_report, revision_cycles,
                 confidence_score)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                    ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27,
                    ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37)"#,
            params![
                artifact.id,
                artifact.workflow_id,
                artifact.task_run_id,
                artifact.description,
                artifact.category,
                artifact.created_at,
                artifact.investigation_duration_ms.map(|v| v as i64),
                artifact.investigation_enriched_description,
                artifact.specification_duration_ms.map(|v| v as i64),
                artifact
                    .specification_criteria
                    .as_ref()
                    .map(|v| v.to_string()),
                artifact.specification_prompt,
                artifact.builder_prompt,
                artifact
                    .verification_prompts
                    .as_ref()
                    .map(|v| v.to_string()),
                artifact.hardener_prompt,
                artifact.discovery_duration_ms.map(|v| v as i64),
                artifact.builder_duration_ms.map(|v| v as i64),
                artifact.autofix_duration_ms.map(|v| v as i64),
                artifact.verification_duration_ms.map(|v| v as i64),
                artifact.hardener_duration_ms.map(|v| v as i64),
                artifact.total_duration_ms.map(|v| v as i64),
                artifact.discovery_calls.as_ref().map(|v| v.to_string()),
                artifact.builder_raw_output,
                artifact.builder_parsed_json.as_ref().map(|v| v.to_string()),
                artifact.autofix_diff.as_ref().map(|v| v.to_string()),
                artifact
                    .verification_iterations
                    .as_ref()
                    .map(|v| v.to_string()),
                artifact
                    .fixer_snapshots
                    .as_ref()
                    .map(|v| serde_json::to_string(v).unwrap_or_default()),
                artifact.hardening_summary.as_ref().map(|v| v.to_string()),
                artifact.hardened_json.as_ref().map(|v| v.to_string()),
                artifact.final_json.as_ref().map(|v| v.to_string()),
                artifact.validation_errors.as_ref().map(|v| v.to_string()),
                artifact.success,
                artifact.error_message,
                artifact.model_used,
                artifact.revision_duration_ms.map(|v| v as i64),
                artifact.quality_report.as_ref().map(|v| v.to_string()),
                artifact.revision_cycles.map(|v| v as i32),
                artifact.confidence_score.map(|v| v as f64),
            ],
        )
        .map_err(|e| format!("Failed to save pipeline artifact: {}", e))?;
        Ok(())
    }

    /// Get a pipeline artifact by ID.
    pub fn get_pipeline_artifact(
        &self,
        id: &str,
    ) -> Result<Option<crate::workflow_generation::pipeline_artifacts::PipelineArtifact>, String>
    {
        let conn = self.get_conn()?;
        let result = conn.query_row(
            r#"SELECT id, workflow_id, task_run_id, description, category, created_at,
                      investigation_duration_ms, investigation_enriched_description,
                      specification_duration_ms, specification_criteria,
                      specification_prompt, builder_prompt, verification_prompts, hardener_prompt,
                      discovery_duration_ms, builder_duration_ms, autofix_duration_ms,
                      verification_duration_ms, hardener_duration_ms, total_duration_ms,
                      discovery_calls, builder_raw_output, builder_parsed_json, autofix_diff,
                      verification_iterations, fixer_snapshots, hardening_summary,
                      hardened_json, final_json, validation_errors,
                      success, error_message, model_used,
                      revision_duration_ms, quality_report, revision_cycles,
                      confidence_score
               FROM generation_pipeline_artifacts WHERE id = ?1"#,
            params![id],
            |row| Ok(Self::row_to_pipeline_artifact(row)),
        );
        match result {
            Ok(artifact) => Ok(Some(artifact)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get pipeline artifact: {}", e)),
        }
    }

    /// List pipeline artifacts (paginated, newest first).
    pub fn list_pipeline_artifacts(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<crate::workflow_generation::pipeline_artifacts::PipelineArtifactSummary>, String>
    {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT id, workflow_id, description, category, created_at,
                          total_duration_ms, success, model_used,
                          verification_iterations, hardening_summary
                   FROM generation_pipeline_artifacts
                   ORDER BY created_at DESC
                   LIMIT ?1 OFFSET ?2"#,
            )
            .map_err(|e| format!("Failed to prepare list query: {}", e))?;

        let rows = stmt
            .query_map(params![limit, offset], |row| {
                let verification_json: Option<String> = row.get(8)?;
                let hardening_json: Option<String> = row.get(9)?;

                let verification_count = verification_json
                    .and_then(|j| serde_json::from_str::<Vec<serde_json::Value>>(&j).ok())
                    .map(|v| v.len() as u32)
                    .unwrap_or(0);

                let hardener_count = hardening_json
                    .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
                    .and_then(|v| v.get("converted_count")?.as_u64())
                    .unwrap_or(0) as u32;

                Ok(
                    crate::workflow_generation::pipeline_artifacts::PipelineArtifactSummary {
                        id: row.get(0)?,
                        workflow_id: row.get(1)?,
                        description: row.get(2)?,
                        category: row.get(3)?,
                        created_at: row.get(4)?,
                        total_duration_ms: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
                        success: row.get(6)?,
                        model_used: row.get(7)?,
                        verification_iteration_count: verification_count,
                        hardener_converted_count: hardener_count,
                    },
                )
            })
            .map_err(|e| format!("Failed to list pipeline artifacts: {}", e))?;

        let mut artifacts = Vec::new();
        for row in rows {
            artifacts.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        Ok(artifacts)
    }

    /// Get pipeline artifact for a specific workflow.
    pub fn get_pipeline_artifact_for_workflow(
        &self,
        workflow_id: &str,
    ) -> Result<Option<crate::workflow_generation::pipeline_artifacts::PipelineArtifact>, String>
    {
        let conn = self.get_conn()?;
        let result = conn.query_row(
            r#"SELECT id, workflow_id, task_run_id, description, category, created_at,
                      investigation_duration_ms, investigation_enriched_description,
                      specification_duration_ms, specification_criteria,
                      specification_prompt, builder_prompt, verification_prompts, hardener_prompt,
                      discovery_duration_ms, builder_duration_ms, autofix_duration_ms,
                      verification_duration_ms, hardener_duration_ms, total_duration_ms,
                      discovery_calls, builder_raw_output, builder_parsed_json, autofix_diff,
                      verification_iterations, fixer_snapshots, hardening_summary,
                      hardened_json, final_json, validation_errors,
                      success, error_message, model_used,
                      revision_duration_ms, quality_report, revision_cycles,
                      confidence_score
               FROM generation_pipeline_artifacts
               WHERE workflow_id = ?1
               ORDER BY created_at DESC LIMIT 1"#,
            params![workflow_id],
            |row| Ok(Self::row_to_pipeline_artifact(row)),
        );
        match result {
            Ok(artifact) => Ok(Some(artifact)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get artifact for workflow: {}", e)),
        }
    }

    /// Delete pipeline artifacts older than N days.
    pub fn delete_pipeline_artifacts_older_than(&self, days: u32) -> Result<u32, String> {
        let conn = self.get_conn()?;
        let deleted = conn
            .execute(
                "DELETE FROM generation_pipeline_artifacts WHERE created_at < datetime('now', ?1)",
                params![format!("-{} days", days)],
            )
            .map_err(|e| format!("Failed to prune artifacts: {}", e))?;
        Ok(deleted as u32)
    }

    fn row_to_pipeline_artifact(
        row: &rusqlite::Row,
    ) -> crate::workflow_generation::pipeline_artifacts::PipelineArtifact {
        let parse_json = |s: Option<String>| -> Option<serde_json::Value> {
            s.and_then(|j| serde_json::from_str(&j).ok())
        };
        let parse_json_vec = |s: Option<String>| -> Option<Vec<serde_json::Value>> {
            s.and_then(|j| serde_json::from_str(&j).ok())
        };

        crate::workflow_generation::pipeline_artifacts::PipelineArtifact {
            id: row.get(0).unwrap_or_default(),
            workflow_id: row.get(1).unwrap_or(None),
            task_run_id: row.get(2).unwrap_or(None),
            description: row.get(3).unwrap_or_default(),
            category: row.get(4).unwrap_or(None),
            created_at: row.get(5).unwrap_or_default(),
            investigation_duration_ms: row
                .get::<_, Option<i64>>(6)
                .unwrap_or(None)
                .map(|v| v as u64),
            investigation_enriched_description: row.get(7).unwrap_or(None),
            specification_duration_ms: row
                .get::<_, Option<i64>>(8)
                .unwrap_or(None)
                .map(|v| v as u64),
            specification_criteria: parse_json(row.get(9).unwrap_or(None)),
            specification_prompt: row.get(10).unwrap_or(None),
            builder_prompt: row.get(11).unwrap_or(None),
            verification_prompts: parse_json(row.get(12).unwrap_or(None)),
            hardener_prompt: row.get(13).unwrap_or(None),
            discovery_duration_ms: row
                .get::<_, Option<i64>>(14)
                .unwrap_or(None)
                .map(|v| v as u64),
            builder_duration_ms: row
                .get::<_, Option<i64>>(15)
                .unwrap_or(None)
                .map(|v| v as u64),
            autofix_duration_ms: row
                .get::<_, Option<i64>>(16)
                .unwrap_or(None)
                .map(|v| v as u64),
            verification_duration_ms: row
                .get::<_, Option<i64>>(17)
                .unwrap_or(None)
                .map(|v| v as u64),
            hardener_duration_ms: row
                .get::<_, Option<i64>>(18)
                .unwrap_or(None)
                .map(|v| v as u64),
            total_duration_ms: row
                .get::<_, Option<i64>>(19)
                .unwrap_or(None)
                .map(|v| v as u64),
            discovery_calls: parse_json(row.get(20).unwrap_or(None)),
            builder_raw_output: row.get(21).unwrap_or(None),
            builder_parsed_json: parse_json(row.get(22).unwrap_or(None)),
            autofix_diff: parse_json(row.get(23).unwrap_or(None)),
            verification_iterations: parse_json(row.get(24).unwrap_or(None)),
            fixer_snapshots: parse_json_vec(row.get(25).unwrap_or(None)),
            hardening_summary: parse_json(row.get(26).unwrap_or(None)),
            hardened_json: parse_json(row.get(27).unwrap_or(None)),
            final_json: parse_json(row.get(28).unwrap_or(None)),
            validation_errors: parse_json(row.get(29).unwrap_or(None)),
            success: row.get(30).unwrap_or(true),
            error_message: row.get(31).unwrap_or(None),
            model_used: row.get(32).unwrap_or(None),
            revision_duration_ms: row
                .get::<_, Option<i64>>(33)
                .unwrap_or(None)
                .map(|v| v as u64),
            quality_report: parse_json(row.get(34).unwrap_or(None)),
            revision_cycles: row
                .get::<_, Option<i32>>(35)
                .unwrap_or(None)
                .map(|v| v as u32),
            confidence_score: row
                .get::<_, Option<f64>>(36)
                .unwrap_or(None)
                .map(|v| v as f32),
        }
    }

    // ========================================================================
    // Generator Evaluation - Dashboard Metrics
    // ========================================================================

    /// Get aggregated dashboard metrics for generator evaluation.
    pub fn get_generation_dashboard_metrics(&self) -> Result<GeneratorDashboardMetrics, String> {
        let conn = self.get_conn()?;

        // Total generations and success rate from pipeline artifacts
        let (total_generations, successful_generations, avg_total_duration): (
            i64,
            i64,
            Option<f64>,
        ) = conn
            .query_row(
                r#"SELECT
                    COUNT(*) as total,
                    SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) as successes,
                    AVG(total_duration_ms) as avg_duration
                FROM generation_pipeline_artifacts"#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap_or((0, 0, None));

        // Average verification iterations
        let avg_verification_iterations: Option<f64> = conn
            .query_row(
                r#"SELECT AVG(json_array_length(verification_iterations))
                FROM generation_pipeline_artifacts
                WHERE verification_iterations IS NOT NULL AND verification_iterations != '[]'"#,
                [],
                |row| row.get(0),
            )
            .unwrap_or(None);

        // Verification first-pass rate (iterations with 0 issues on first pass)
        let first_pass_rate: Option<f64> = conn
            .query_row(
                r#"SELECT
                    CAST(SUM(CASE
                        WHEN json_extract(verification_iterations, '$[0].issues') = '[]'
                        THEN 1 ELSE 0
                    END) AS REAL) / NULLIF(COUNT(*), 0)
                FROM generation_pipeline_artifacts
                WHERE verification_iterations IS NOT NULL AND verification_iterations != '[]'"#,
                [],
                |row| row.get(0),
            )
            .unwrap_or(None);

        // Hardener conversion rate
        let (total_hardened, total_converted): (i64, i64) = conn
            .query_row(
                r#"SELECT
                    COUNT(*) as total,
                    SUM(COALESCE(json_extract(hardening_summary, '$.converted_count'), 0)) as converted
                FROM generation_pipeline_artifacts
                WHERE hardening_summary IS NOT NULL"#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or((0, 0));

        // User feedback metrics from workflow_generation_feedback
        let (total_edits, total_deletes, total_ratings, avg_rating): (i64, i64, i64, Option<f64>) =
            conn.query_row(
                r#"SELECT
                    SUM(CASE WHEN feedback_type = 'edit' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN feedback_type = 'delete' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN feedback_type = 'rating' THEN 1 ELSE 0 END),
                    AVG(CASE WHEN feedback_type = 'rating' THEN rating ELSE NULL END)
                FROM workflow_generation_feedback"#,
                [],
                |row| {
                    Ok((
                        row.get(0).unwrap_or(0),
                        row.get(1).unwrap_or(0),
                        row.get(2).unwrap_or(0),
                        row.get(3)?,
                    ))
                },
            )
            .unwrap_or((0, 0, 0, None));

        Ok(GeneratorDashboardMetrics {
            total_generations,
            successful_generations,
            success_rate: if total_generations > 0 {
                successful_generations as f64 / total_generations as f64
            } else {
                0.0
            },
            avg_total_duration_ms: avg_total_duration,
            avg_verification_iterations,
            first_pass_rate,
            hardener_total_processed: total_hardened,
            hardener_total_converted: total_converted,
            total_edits,
            total_deletes,
            total_ratings,
            avg_rating,
        })
    }

    /// Get generation metrics over time (daily aggregates).
    pub fn get_generation_metrics_over_time(
        &self,
        days: u32,
    ) -> Result<Vec<GeneratorTimeSeriesPoint>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT
                    date(created_at) as day,
                    COUNT(*) as total,
                    SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) as successes,
                    AVG(total_duration_ms) as avg_duration,
                    AVG(json_array_length(verification_iterations)) as avg_iterations
                FROM generation_pipeline_artifacts
                WHERE created_at >= datetime('now', ?1)
                GROUP BY date(created_at)
                ORDER BY day ASC"#,
            )
            .map_err(|e| format!("Failed to prepare trends query: {}", e))?;

        let rows = stmt
            .query_map(params![format!("-{} days", days)], |row| {
                Ok(GeneratorTimeSeriesPoint {
                    date: row.get(0)?,
                    total_generations: row.get(1)?,
                    successful_generations: row.get(2)?,
                    avg_duration_ms: row.get(3)?,
                    avg_verification_iterations: row.get(4)?,
                })
            })
            .map_err(|e| format!("Failed to query trends: {}", e))?;

        let mut points = Vec::new();
        for row in rows {
            points.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        Ok(points)
    }

    // ========================================================================
    // Generator Evaluation - Benchmarks
    // ========================================================================

    /// Save a new benchmark.
    pub fn save_benchmark(&self, benchmark: &GeneratorBenchmark) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"INSERT INTO generator_benchmarks
                (id, name, description, category, tags, expected_structure, created_at, updated_at, enabled)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
            params![
                benchmark.id,
                benchmark.name,
                benchmark.description,
                benchmark.category,
                benchmark.tags.as_ref().map(|t| serde_json::to_string(t).unwrap_or_default()),
                serde_json::to_string(&benchmark.expected_structure).unwrap_or_default(),
                benchmark.created_at,
                benchmark.updated_at,
                benchmark.enabled,
            ],
        )
        .map_err(|e| format!("Failed to save benchmark: {}", e))?;
        Ok(())
    }

    /// Get a benchmark by ID.
    pub fn get_benchmark(&self, id: &str) -> Result<Option<GeneratorBenchmark>, String> {
        let conn = self.get_conn()?;
        let result = conn.query_row(
            r#"SELECT id, name, description, category, tags, expected_structure, created_at, updated_at, enabled
               FROM generator_benchmarks WHERE id = ?1"#,
            params![id],
            |row| Ok(Self::row_to_benchmark(row)),
        );
        match result {
            Ok(b) => Ok(Some(b)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get benchmark: {}", e)),
        }
    }

    /// List all benchmarks.
    pub fn list_benchmarks(&self) -> Result<Vec<GeneratorBenchmark>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT id, name, description, category, tags, expected_structure, created_at, updated_at, enabled
                   FROM generator_benchmarks ORDER BY created_at DESC"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let rows = stmt
            .query_map([], |row| Ok(Self::row_to_benchmark(row)))
            .map_err(|e| format!("Failed to list benchmarks: {}", e))?;

        let mut benchmarks = Vec::new();
        for row in rows {
            benchmarks.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        Ok(benchmarks)
    }

    /// Check if a benchmark with the given name already exists.
    pub fn benchmark_exists_by_name(&self, name: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM generator_benchmarks WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to check benchmark: {}", e))?;
        Ok(count > 0)
    }

    /// Update a benchmark.
    pub fn update_benchmark(&self, id: &str, update: &BenchmarkUpdate) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();

        if let Some(ref name) = update.name {
            conn.execute(
                "UPDATE generator_benchmarks SET name = ?1, updated_at = ?2 WHERE id = ?3",
                params![name, now, id],
            )
            .map_err(|e| format!("Failed to update name: {}", e))?;
        }
        if let Some(ref description) = update.description {
            conn.execute(
                "UPDATE generator_benchmarks SET description = ?1, updated_at = ?2 WHERE id = ?3",
                params![description, now, id],
            )
            .map_err(|e| format!("Failed to update description: {}", e))?;
        }
        if let Some(ref expected) = update.expected_structure {
            let json = serde_json::to_string(expected).unwrap_or_default();
            conn.execute(
                "UPDATE generator_benchmarks SET expected_structure = ?1, updated_at = ?2 WHERE id = ?3",
                params![json, now, id],
            )
            .map_err(|e| format!("Failed to update expected_structure: {}", e))?;
        }
        if let Some(enabled) = update.enabled {
            conn.execute(
                "UPDATE generator_benchmarks SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
                params![enabled, now, id],
            )
            .map_err(|e| format!("Failed to update enabled: {}", e))?;
        }
        Ok(())
    }

    /// Delete a benchmark and its results.
    pub fn delete_benchmark(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "DELETE FROM generator_benchmark_results WHERE benchmark_id = ?1",
            params![id],
        )
        .map_err(|e| format!("Failed to delete results: {}", e))?;
        conn.execute(
            "DELETE FROM generator_benchmarks WHERE id = ?1",
            params![id],
        )
        .map_err(|e| format!("Failed to delete benchmark: {}", e))?;
        Ok(())
    }

    /// Save a benchmark result.
    pub fn save_benchmark_result(&self, result: &GeneratorBenchmarkResult) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"INSERT INTO generator_benchmark_results
                (id, benchmark_id, artifact_id, run_at, model_used,
                 structure_score, content_score, step_type_score, overall_score,
                 score_breakdown, generated_json, duration_ms, passed, notes)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"#,
            params![
                result.id,
                result.benchmark_id,
                result.artifact_id,
                result.run_at,
                result.model_used,
                result.structure_score,
                result.content_score,
                result.step_type_score,
                result.overall_score,
                result.score_breakdown.as_ref().map(|v| v.to_string()),
                result.generated_json.as_ref().map(|v| v.to_string()),
                result.duration_ms.map(|v| v as i64),
                result.passed,
                result.notes,
            ],
        )
        .map_err(|e| format!("Failed to save benchmark result: {}", e))?;
        Ok(())
    }

    /// List results for a specific benchmark.
    pub fn list_benchmark_results(
        &self,
        benchmark_id: &str,
        limit: u32,
    ) -> Result<Vec<GeneratorBenchmarkResult>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT id, benchmark_id, artifact_id, run_at, model_used,
                          structure_score, content_score, step_type_score, overall_score,
                          score_breakdown, generated_json, duration_ms, passed, notes
                   FROM generator_benchmark_results
                   WHERE benchmark_id = ?1
                   ORDER BY run_at DESC
                   LIMIT ?2"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let rows = stmt
            .query_map(params![benchmark_id, limit], |row| {
                Ok(GeneratorBenchmarkResult {
                    id: row.get(0)?,
                    benchmark_id: row.get(1)?,
                    artifact_id: row.get(2)?,
                    run_at: row.get(3)?,
                    model_used: row.get(4)?,
                    structure_score: row.get(5)?,
                    content_score: row.get(6)?,
                    step_type_score: row.get(7)?,
                    overall_score: row.get(8)?,
                    score_breakdown: row
                        .get::<_, Option<String>>(9)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    generated_json: row
                        .get::<_, Option<String>>(10)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    duration_ms: row.get::<_, Option<i64>>(11)?.map(|v| v as u64),
                    passed: row.get(12)?,
                    notes: row.get(13)?,
                })
            })
            .map_err(|e| format!("Failed to list results: {}", e))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        Ok(results)
    }

    fn row_to_benchmark(row: &rusqlite::Row) -> GeneratorBenchmark {
        let tags_json: Option<String> = row.get(4).unwrap_or(None);
        let expected_json: String = row.get(5).unwrap_or_default();
        GeneratorBenchmark {
            id: row.get(0).unwrap_or_default(),
            name: row.get(1).unwrap_or_default(),
            description: row.get(2).unwrap_or_default(),
            category: row.get(3).unwrap_or(None),
            tags: tags_json.and_then(|j| serde_json::from_str(&j).ok()),
            expected_structure: serde_json::from_str(&expected_json).unwrap_or_default(),
            created_at: row.get(6).unwrap_or_default(),
            updated_at: row.get(7).unwrap_or_default(),
            enabled: row.get(8).unwrap_or(true),
        }
    }

    // ========================================================================
    // Generator Evaluation - Edit Analysis
    // ========================================================================

    /// Get aggregated edit analysis from workflow_generation_feedback.
    pub fn get_edit_analysis(&self) -> Result<EditAnalysis, String> {
        let conn = self.get_conn()?;

        // Most commonly edited fields
        let mut stmt = conn
            .prepare(
                r#"SELECT edited_field, COUNT(*) as cnt
                   FROM workflow_generation_feedback
                   WHERE feedback_type = 'edit' AND edited_field IS NOT NULL
                   GROUP BY edited_field
                   ORDER BY cnt DESC
                   LIMIT 20"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let edited_fields: Vec<(String, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| format!("Failed to query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Feedback type distribution
        let mut stmt = conn
            .prepare(
                r#"SELECT feedback_type, COUNT(*) as cnt
                   FROM workflow_generation_feedback
                   GROUP BY feedback_type
                   ORDER BY cnt DESC"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let type_distribution: Vec<(String, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| format!("Failed to query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Rating distribution
        let mut stmt = conn
            .prepare(
                r#"SELECT rating, COUNT(*) as cnt
                   FROM workflow_generation_feedback
                   WHERE feedback_type = 'rating' AND rating IS NOT NULL
                   GROUP BY rating
                   ORDER BY rating"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let rating_distribution: Vec<(i32, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| format!("Failed to query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Recent edits
        let mut stmt = conn
            .prepare(
                r#"SELECT f.id, f.workflow_id, f.feedback_type, f.edited_field,
                          f.old_value, f.new_value, f.created_at,
                          w.name as workflow_name
                   FROM workflow_generation_feedback f
                   LEFT JOIN unified_workflows w ON f.workflow_id = w.id
                   ORDER BY f.created_at DESC
                   LIMIT 50"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let recent_feedback: Vec<RecentFeedback> = stmt
            .query_map([], |row| {
                Ok(RecentFeedback {
                    id: row.get(0)?,
                    workflow_id: row.get(1)?,
                    feedback_type: row.get(2)?,
                    edited_field: row.get(3)?,
                    old_value: row.get(4)?,
                    new_value: row.get(5)?,
                    created_at: row.get(6)?,
                    workflow_name: row.get(7)?,
                })
            })
            .map_err(|e| format!("Failed to query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(EditAnalysis {
            edited_fields,
            type_distribution,
            rating_distribution,
            recent_feedback,
        })
    }

    /// Save a feedback entry to workflow_generation_feedback.
    pub fn save_generator_feedback(
        &self,
        id: &str,
        workflow_id: &str,
        workflow_name: Option<&str>,
        feedback_type: &str,
        edited_field: Option<&str>,
        old_value: Option<&str>,
        new_value: Option<&str>,
        rating: Option<i32>,
        created_at: &str,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"INSERT INTO workflow_generation_feedback
                (id, workflow_id, feedback_type, edited_field, old_value, new_value, rating, workflow_description, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
            params![
                id,
                workflow_id,
                feedback_type,
                edited_field,
                old_value,
                new_value,
                rating,
                workflow_name,
                created_at,
            ],
        )
        .map_err(|e| format!("Failed to save generator feedback: {}", e))?;
        Ok(())
    }

    // ========================================================================
    // Generator Evaluation - Example Library
    // ========================================================================

    /// List workflows that have example_status set.
    pub fn list_example_workflows(&self) -> Result<Vec<ExampleWorkflowSummary>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT id, name, description, category, example_status, created_at
                   FROM unified_workflows
                   WHERE example_status IS NOT NULL AND example_status != ''
                   ORDER BY example_status, name"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(ExampleWorkflowSummary {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    category: row.get(3)?,
                    example_status: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })
            .map_err(|e| format!("Failed to query: {}", e))?;

        let mut examples = Vec::new();
        for row in rows {
            examples.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        Ok(examples)
    }

    /// Update a workflow's example_status.
    pub fn update_example_status(
        &self,
        workflow_id: &str,
        status: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE unified_workflows SET example_status = ?1 WHERE id = ?2",
            params![status, workflow_id],
        )
        .map_err(|e| format!("Failed to update example_status: {}", e))?;
        Ok(())
    }

    // ========================================================================
    // Canvas Panels (A2UI)
    // ========================================================================

    /// Insert or update a canvas panel.
    pub fn insert_or_update_canvas_panel(
        &self,
        panel: &crate::mcp::canvas::StoredPanel,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let data_json = panel.data.to_string();

        conn.execute(
            r#"
            INSERT INTO canvas_panels (id, task_run_id, component, title, data_json, priority, size, group_name, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(id) DO UPDATE SET
                component = ?3,
                title = ?4,
                data_json = ?5,
                priority = ?6,
                size = ?7,
                group_name = ?8,
                updated_at = ?10
            "#,
            params![
                panel.panel_id,
                panel.task_run_id,
                panel.component,
                panel.title,
                data_json,
                panel.priority,
                panel.size,
                panel.group,
                panel.created_at,
                panel.updated_at,
            ],
        )
        .map_err(|e| format!("Failed to upsert canvas panel: {}", e))?;

        Ok(())
    }

    /// Get all canvas panels for a task run.
    pub fn get_canvas_panels_for_task_run(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<crate::mcp::canvas::StoredPanel>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_run_id, component, title, data_json, priority, size, group_name, created_at, updated_at
                FROM canvas_panels
                WHERE task_run_id = ?1
                ORDER BY priority ASC, created_at ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare canvas panels query: {}", e))?;

        let panels = stmt
            .query_map(params![task_run_id], |row| {
                let data_json: String = row.get(4)?;
                let data: serde_json::Value =
                    serde_json::from_str(&data_json).unwrap_or(serde_json::json!({}));
                Ok(crate::mcp::canvas::StoredPanel {
                    panel_id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    component: row.get(2)?,
                    title: row.get(3)?,
                    data,
                    priority: row.get(5)?,
                    size: row.get(6)?,
                    group: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                })
            })
            .map_err(|e| format!("Failed to query canvas panels: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(panels)
    }

    /// Delete a single canvas panel.
    pub fn delete_canvas_panel(&self, panel_id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;
        let rows = conn
            .execute("DELETE FROM canvas_panels WHERE id = ?1", params![panel_id])
            .map_err(|e| format!("Failed to delete canvas panel: {}", e))?;
        Ok(rows > 0)
    }

    /// Clear all canvas panels for a task run.
    pub fn clear_canvas_panels_for_task_run(&self, task_run_id: &str) -> Result<usize, String> {
        let conn = self.get_conn()?;
        let rows = conn
            .execute(
                "DELETE FROM canvas_panels WHERE task_run_id = ?1",
                params![task_run_id],
            )
            .map_err(|e| format!("Failed to clear canvas panels: {}", e))?;
        Ok(rows)
    }

    // ========================================================================
    // Approval Gate Operations
    // ========================================================================

    /// Record a new approval gate request (audit trail).
    pub fn insert_approval_gate(
        &self,
        id: &str,
        task_run_id: &str,
        iteration: u32,
        prompt: &str,
        context_json: &str,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "INSERT INTO approval_gates (id, task_run_id, iteration, prompt, context_json, status, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', datetime('now'))",
            rusqlite::params![id, task_run_id, iteration as i64, prompt, context_json],
        )
        .map_err(|e| format!("Failed to insert approval gate: {}", e))?;
        Ok(())
    }

    /// Resolve an approval gate (record human response).
    pub fn resolve_approval_gate(
        &self,
        id: &str,
        action: &str,
        comment: Option<&str>,
    ) -> Result<(), String> {
        let status = match action {
            "approve" => "approved",
            "reject" => "rejected",
            "abort" => "aborted",
            _ => action,
        };
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE approval_gates SET action = ?1, comment = ?2, status = ?3, resolved_at = datetime('now') WHERE id = ?4",
            rusqlite::params![action, comment, status, id],
        )
        .map_err(|e| format!("Failed to resolve approval gate: {}", e))?;
        Ok(())
    }

    /// Get approval gate history for a task run.
    pub fn get_approval_gates_for_task_run(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_run_id, iteration, prompt, context_json, action, comment, status, created_at, resolved_at \
                 FROM approval_gates WHERE task_run_id = ?1 ORDER BY created_at ASC",
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let rows = stmt
            .query_map(rusqlite::params![task_run_id], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_run_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "prompt": row.get::<_, String>(3)?,
                    "context_json": row.get::<_, String>(4).unwrap_or_default(),
                    "action": row.get::<_, Option<String>>(5)?,
                    "comment": row.get::<_, Option<String>>(6)?,
                    "status": row.get::<_, String>(7)?,
                    "created_at": row.get::<_, String>(8)?,
                    "resolved_at": row.get::<_, Option<String>>(9)?,
                }))
            })
            .map_err(|e| format!("Failed to query approval gates: {}", e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect approval gates: {}", e))?;

        Ok(rows)
    }

    // ========================================================================
    // User Skills CRUD
    // ========================================================================

    /// List all user-created skills.
    pub fn list_user_skills(&self) -> Result<Vec<crate::skills::SkillDefinition>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, slug, description, category, tags, icon, color,
                       allowed_phases, parameters, template, source,
                       version, author, checksum, depends_on, usage_count, approval_status, forked_from,
                       created_at, updated_at
                FROM user_skills
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare user_skills query: {}", e))?;

        let skills = stmt
            .query_map([], |row| {
                let tags_json: String =
                    row.get::<_, String>(5).unwrap_or_else(|_| "[]".to_string());
                let allowed_phases_json: String = row
                    .get::<_, String>(8)
                    .unwrap_or_else(|_| "[\"setup\"]".to_string());
                let parameters_json: String =
                    row.get::<_, String>(9).unwrap_or_else(|_| "[]".to_string());
                let template_json: String = row.get(10)?;

                Ok(crate::skills::SkillDefinition {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    slug: row.get(2)?,
                    description: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    category: row
                        .get::<_, Option<String>>(4)?
                        .unwrap_or_else(|| "custom".to_string()),
                    tags: serde_json::from_str(&tags_json).unwrap_or_default(),
                    icon: row
                        .get::<_, Option<String>>(6)?
                        .unwrap_or_else(|| "puzzle".to_string()),
                    color: row
                        .get::<_, Option<String>>(7)?
                        .unwrap_or_else(|| "gray".to_string()),
                    allowed_phases: serde_json::from_str(&allowed_phases_json)
                        .unwrap_or_else(|_| vec!["setup".to_string()]),
                    parameters: serde_json::from_str(&parameters_json).unwrap_or_default(),
                    template: serde_json::from_str(&template_json).unwrap_or(
                        crate::skills::SkillTemplate::SingleStep {
                            step: std::collections::HashMap::new(),
                        },
                    ),
                    source: row
                        .get::<_, Option<String>>(11)?
                        .unwrap_or_else(|| "user".to_string()),
                    version: row.get::<_, Option<String>>(12)?,
                    author: row
                        .get::<_, Option<String>>(13)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    checksum: row.get::<_, Option<String>>(14)?,
                    depends_on: row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    usage_count: row.get::<_, Option<u64>>(16)?,
                    approval_status: row.get::<_, Option<String>>(17)?,
                    forked_from: row.get::<_, Option<String>>(18)?,
                })
            })
            .map_err(|e| format!("Failed to query user skills: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(skills)
    }

    /// Get a single user skill by ID.
    pub fn get_user_skill(
        &self,
        id: &str,
    ) -> Result<Option<crate::skills::SkillDefinition>, String> {
        let conn = self.get_conn()?;

        let result = conn.query_row(
            r#"
            SELECT id, name, slug, description, category, tags, icon, color,
                   allowed_phases, parameters, template, source,
                   version, author, checksum, depends_on, usage_count, approval_status, forked_from,
                   created_at, updated_at
            FROM user_skills
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                let tags_json: String =
                    row.get::<_, String>(5).unwrap_or_else(|_| "[]".to_string());
                let allowed_phases_json: String = row
                    .get::<_, String>(8)
                    .unwrap_or_else(|_| "[\"setup\"]".to_string());
                let parameters_json: String =
                    row.get::<_, String>(9).unwrap_or_else(|_| "[]".to_string());
                let template_json: String = row.get(10)?;

                Ok(crate::skills::SkillDefinition {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    slug: row.get(2)?,
                    description: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    category: row
                        .get::<_, Option<String>>(4)?
                        .unwrap_or_else(|| "custom".to_string()),
                    tags: serde_json::from_str(&tags_json).unwrap_or_default(),
                    icon: row
                        .get::<_, Option<String>>(6)?
                        .unwrap_or_else(|| "puzzle".to_string()),
                    color: row
                        .get::<_, Option<String>>(7)?
                        .unwrap_or_else(|| "gray".to_string()),
                    allowed_phases: serde_json::from_str(&allowed_phases_json)
                        .unwrap_or_else(|_| vec!["setup".to_string()]),
                    parameters: serde_json::from_str(&parameters_json).unwrap_or_default(),
                    template: serde_json::from_str(&template_json).unwrap_or(
                        crate::skills::SkillTemplate::SingleStep {
                            step: std::collections::HashMap::new(),
                        },
                    ),
                    source: row
                        .get::<_, Option<String>>(11)?
                        .unwrap_or_else(|| "user".to_string()),
                    version: row.get::<_, Option<String>>(12)?,
                    author: row
                        .get::<_, Option<String>>(13)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    checksum: row.get::<_, Option<String>>(14)?,
                    depends_on: row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    usage_count: row.get::<_, Option<u64>>(16)?,
                    approval_status: row.get::<_, Option<String>>(17)?,
                    forked_from: row.get::<_, Option<String>>(18)?,
                })
            },
        );

        match result {
            Ok(skill) => Ok(Some(skill)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get user skill: {}", e)),
        }
    }

    /// Create a new user skill.
    pub fn create_user_skill(
        &self,
        request: &crate::mcp::skills::CreateSkillRequest,
    ) -> Result<crate::skills::SkillDefinition, String> {
        let conn = self.get_conn()?;
        let id = format!("user:{}", request.slug);
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&request.tags).unwrap_or_else(|_| "[]".to_string());
        let allowed_phases_json =
            serde_json::to_string(&request.allowed_phases).unwrap_or_else(|_| "[]".to_string());
        let parameters_json =
            serde_json::to_string(&request.parameters).unwrap_or_else(|_| "[]".to_string());
        let template_json = serde_json::to_string(&request.template)
            .map_err(|e| format!("Failed to serialize template: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO user_skills (
                id, name, slug, description, category, tags, icon, color,
                allowed_phases, parameters, template, source,
                version, author, checksum, depends_on, usage_count, approval_status, forked_from,
                created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
            "#,
            params![
                id,
                request.name,
                request.slug,
                request.description,
                request.category,
                tags_json,
                request.icon,
                request.color,
                allowed_phases_json,
                parameters_json,
                template_json,
                "user",
                "1.0.0",                    // version
                Option::<String>::None,      // author
                Option::<String>::None,      // checksum
                "[]",                        // depends_on
                0i64,                        // usage_count
                Option::<String>::None,      // approval_status
                Option::<String>::None,      // forked_from
                now,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create user skill: {}", e))?;

        self.get_user_skill(&id)?
            .ok_or_else(|| "Failed to retrieve created skill".to_string())
    }

    /// Update a user skill.
    pub fn update_user_skill(
        &self,
        id: &str,
        request: &crate::mcp::skills::UpdateSkillRequest,
    ) -> Result<crate::skills::SkillDefinition, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let current = self
            .get_user_skill(id)?
            .ok_or_else(|| format!("User skill not found: {}", id))?;

        let name = request.name.as_ref().unwrap_or(&current.name);
        let slug = request.slug.as_ref().unwrap_or(&current.slug);
        let description = request.description.as_ref().unwrap_or(&current.description);
        let category = request.category.as_ref().unwrap_or(&current.category);
        let tags = request.tags.as_ref().unwrap_or(&current.tags);
        let icon = request.icon.as_ref().unwrap_or(&current.icon);
        let color = request.color.as_ref().unwrap_or(&current.color);
        let allowed_phases = request
            .allowed_phases
            .as_ref()
            .unwrap_or(&current.allowed_phases);
        let parameters = request.parameters.as_ref().unwrap_or(&current.parameters);
        let template = request.template.as_ref().unwrap_or(&current.template);

        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());
        let allowed_phases_json =
            serde_json::to_string(allowed_phases).unwrap_or_else(|_| "[]".to_string());
        let parameters_json =
            serde_json::to_string(parameters).unwrap_or_else(|_| "[]".to_string());
        let template_json = serde_json::to_string(template)
            .map_err(|e| format!("Failed to serialize template: {}", e))?;

        // Update the ID if slug changed
        let new_id = format!("user:{}", slug);

        conn.execute(
            r#"
            UPDATE user_skills SET
                id = ?1, name = ?2, slug = ?3, description = ?4, category = ?5,
                tags = ?6, icon = ?7, color = ?8, allowed_phases = ?9,
                parameters = ?10, template = ?11, updated_at = ?12
            WHERE id = ?13
            "#,
            params![
                new_id,
                name,
                slug,
                description,
                category,
                tags_json,
                icon,
                color,
                allowed_phases_json,
                parameters_json,
                template_json,
                now,
                id,
            ],
        )
        .map_err(|e| format!("Failed to update user skill: {}", e))?;

        self.get_user_skill(&new_id)?
            .ok_or_else(|| "Failed to retrieve updated skill".to_string())
    }

    /// Export user skills for sharing.
    /// If `ids` is empty, exports all non-builtin skills.
    pub fn export_user_skills(
        &self,
        ids: &[String],
    ) -> Result<Vec<crate::skills::SkillDefinition>, String> {
        if ids.is_empty() {
            return self.list_user_skills();
        }

        let conn = self.get_conn()?;
        let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{}", i)).collect();
        let query = format!(
            r#"SELECT id, name, slug, description, category, tags, icon, color,
                      allowed_phases, parameters, template, source,
                      version, author, checksum, depends_on, usage_count, approval_status, forked_from,
                      created_at, updated_at
               FROM user_skills
               WHERE id IN ({})
               ORDER BY updated_at DESC"#,
            placeholders.join(", ")
        );

        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| format!("Failed to prepare export query: {}", e))?;

        let params: Vec<&dyn rusqlite::types::ToSql> = ids
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();

        let skills = stmt
            .query_map(params.as_slice(), |row| {
                let tags_json: String =
                    row.get::<_, String>(5).unwrap_or_else(|_| "[]".to_string());
                let allowed_phases_json: String = row
                    .get::<_, String>(8)
                    .unwrap_or_else(|_| "[\"setup\"]".to_string());
                let parameters_json: String =
                    row.get::<_, String>(9).unwrap_or_else(|_| "[]".to_string());
                let template_json: String = row.get(10)?;

                Ok(crate::skills::SkillDefinition {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    slug: row.get(2)?,
                    description: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    category: row
                        .get::<_, Option<String>>(4)?
                        .unwrap_or_else(|| "custom".to_string()),
                    tags: serde_json::from_str(&tags_json).unwrap_or_default(),
                    icon: row
                        .get::<_, Option<String>>(6)?
                        .unwrap_or_else(|| "puzzle".to_string()),
                    color: row
                        .get::<_, Option<String>>(7)?
                        .unwrap_or_else(|| "gray".to_string()),
                    allowed_phases: serde_json::from_str(&allowed_phases_json)
                        .unwrap_or_else(|_| vec!["setup".to_string()]),
                    parameters: serde_json::from_str(&parameters_json).unwrap_or_default(),
                    template: serde_json::from_str(&template_json).unwrap_or(
                        crate::skills::SkillTemplate::SingleStep {
                            step: std::collections::HashMap::new(),
                        },
                    ),
                    source: row
                        .get::<_, Option<String>>(11)?
                        .unwrap_or_else(|| "user".to_string()),
                    version: row.get::<_, Option<String>>(12)?,
                    author: row
                        .get::<_, Option<String>>(13)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    checksum: row.get::<_, Option<String>>(14)?,
                    depends_on: row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    usage_count: row.get::<_, Option<u64>>(16)?,
                    approval_status: row.get::<_, Option<String>>(17)?,
                    forked_from: row.get::<_, Option<String>>(18)?,
                })
            })
            .map_err(|e| format!("Failed to export skills: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(skills)
    }

    /// Import skills from an export. Sets source to "community" and id to "community:<slug>".
    /// `conflict_mode` is "skip" or "overwrite".
    pub fn import_skills(
        &self,
        skills: &[crate::skills::SkillDefinition],
        conflict_mode: &str,
    ) -> Result<crate::skills::SkillImportResult, String> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();
        let mut imported = 0usize;
        let mut skipped = 0usize;
        let mut overwritten = 0usize;
        let mut errors = Vec::new();

        for skill in skills {
            let slug = &skill.slug;
            let id = format!("community:{}", slug);

            let tags_json = serde_json::to_string(&skill.tags).unwrap_or_else(|_| "[]".to_string());
            let allowed_phases_json =
                serde_json::to_string(&skill.allowed_phases).unwrap_or_else(|_| "[]".to_string());
            let parameters_json =
                serde_json::to_string(&skill.parameters).unwrap_or_else(|_| "[]".to_string());
            let template_json = match serde_json::to_string(&skill.template) {
                Ok(j) => j,
                Err(e) => {
                    errors.push(format!(
                        "Failed to serialize template for '{}': {}",
                        slug, e
                    ));
                    continue;
                }
            };

            // Check if slug already exists
            let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM user_skills WHERE slug = ?1",
                    params![slug],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count > 0)
                .unwrap_or(false);

            if exists {
                if conflict_mode == "overwrite" {
                    let overwrite_author_json = skill
                        .author
                        .as_ref()
                        .map(|a| serde_json::to_string(a).unwrap_or_default());
                    let overwrite_depends_on_json =
                        serde_json::to_string(&skill.depends_on.as_deref().unwrap_or(&[]))
                            .unwrap_or_else(|_| "[]".to_string());

                    match conn.execute(
                        r#"UPDATE user_skills SET
                            id = ?1, name = ?2, description = ?3, category = ?4,
                            tags = ?5, icon = ?6, color = ?7, allowed_phases = ?8,
                            parameters = ?9, template = ?10, source = ?11, updated_at = ?12,
                            version = ?13, author = ?14, checksum = ?15, depends_on = ?16,
                            usage_count = ?17, approval_status = ?18, forked_from = ?19
                        WHERE slug = ?20"#,
                        params![
                            id,
                            skill.name,
                            skill.description,
                            skill.category,
                            tags_json,
                            skill.icon,
                            skill.color,
                            allowed_phases_json,
                            parameters_json,
                            template_json,
                            "community",
                            now,
                            skill.version.as_deref().unwrap_or("1.0.0"),
                            overwrite_author_json,
                            skill.checksum.as_deref(),
                            overwrite_depends_on_json,
                            skill.usage_count.unwrap_or(0) as i64,
                            skill.approval_status.as_deref(),
                            skill.forked_from.as_deref(),
                            slug,
                        ],
                    ) {
                        Ok(_) => overwritten += 1,
                        Err(e) => errors.push(format!("Failed to overwrite '{}': {}", slug, e)),
                    }
                } else {
                    skipped += 1;
                }
            } else {
                let author_json = skill
                    .author
                    .as_ref()
                    .map(|a| serde_json::to_string(a).unwrap_or_default());
                let depends_on_json =
                    serde_json::to_string(&skill.depends_on.as_deref().unwrap_or(&[]))
                        .unwrap_or_else(|_| "[]".to_string());

                match conn.execute(
                    r#"INSERT INTO user_skills (
                        id, name, slug, description, category, tags, icon, color,
                        allowed_phases, parameters, template, source,
                        version, author, checksum, depends_on, usage_count, approval_status, forked_from,
                        created_at, updated_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)"#,
                    params![
                        id,
                        skill.name,
                        slug,
                        skill.description,
                        skill.category,
                        tags_json,
                        skill.icon,
                        skill.color,
                        allowed_phases_json,
                        parameters_json,
                        template_json,
                        "community",
                        skill.version.as_deref().unwrap_or("1.0.0"),
                        author_json,
                        skill.checksum.as_deref(),
                        depends_on_json,
                        skill.usage_count.unwrap_or(0) as i64,
                        skill.approval_status.as_deref(),
                        skill.forked_from.as_deref(),
                        now,
                        now,
                    ],
                ) {
                    Ok(_) => imported += 1,
                    Err(e) => errors.push(format!("Failed to import '{}': {}", slug, e)),
                }
            }
        }

        Ok(crate::skills::SkillImportResult {
            imported,
            skipped,
            overwritten,
            errors,
            warnings: vec![],
        })
    }

    /// Delete a user skill by ID.
    pub fn delete_user_skill(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let deleted = conn
            .execute("DELETE FROM user_skills WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete user skill: {}", e))?;

        Ok(deleted > 0)
    }

    /// Update the approval status of a skill.
    pub fn update_skill_approval(&self, skill_id: &str, status: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let rows = conn
            .execute(
                "UPDATE user_skills SET approval_status = ?1 WHERE id = ?2",
                params![status, skill_id],
            )
            .map_err(|e| format!("Failed to update approval status: {}", e))?;

        if rows == 0 {
            return Err(format!("Skill not found: {}", skill_id));
        }
        Ok(())
    }

    /// Update the version and checksum of a skill.
    pub fn update_skill_version(
        &self,
        skill_id: &str,
        version: &str,
        checksum: &str,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let rows = conn
            .execute(
                "UPDATE user_skills SET version = ?1, checksum = ?2, updated_at = datetime('now') WHERE id = ?3",
                params![version, checksum, skill_id],
            )
            .map_err(|e| format!("Failed to update skill version: {}", e))?;

        if rows == 0 {
            return Err(format!("Skill not found: {}", skill_id));
        }
        Ok(())
    }

    /// Fork a skill by creating a copy with a new ID.
    pub fn fork_skill(
        &self,
        skill_id: &str,
        new_name: Option<&str>,
    ) -> Result<crate::skills::SkillDefinition, String> {
        let original = self
            .get_user_skill(skill_id)?
            .ok_or_else(|| format!("Skill not found: {}", skill_id))?;

        let fork_name = new_name
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("{} (fork)", original.name));
        let fork_slug = format!(
            "{}-fork-{}",
            original.slug,
            &uuid::Uuid::new_v4().to_string()[..8]
        );
        let fork_id = format!("user:{}", fork_slug);
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&original.tags).unwrap_or_else(|_| "[]".to_string());
        let allowed_phases_json =
            serde_json::to_string(&original.allowed_phases).unwrap_or_else(|_| "[]".to_string());
        let parameters_json =
            serde_json::to_string(&original.parameters).unwrap_or_else(|_| "[]".to_string());
        let template_json = serde_json::to_string(&original.template)
            .map_err(|e| format!("Failed to serialize template: {}", e))?;
        let author_json = original
            .author
            .as_ref()
            .map(|a| serde_json::to_string(a).unwrap_or_default());
        let depends_on_json = serde_json::to_string(&original.depends_on.as_deref().unwrap_or(&[]))
            .unwrap_or_else(|_| "[]".to_string());

        let conn = self.get_conn()?;
        conn.execute(
            r#"INSERT INTO user_skills (
                id, name, slug, description, category, tags, icon, color,
                allowed_phases, parameters, template, source,
                version, author, checksum, depends_on, usage_count, approval_status, forked_from,
                created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)"#,
            params![
                fork_id,
                fork_name,
                fork_slug,
                original.description,
                original.category,
                tags_json,
                original.icon,
                original.color,
                allowed_phases_json,
                parameters_json,
                template_json,
                "user",
                "1.0.0",
                author_json,
                Option::<String>::None, // checksum
                depends_on_json,
                0i64,                   // usage_count
                Option::<String>::None, // approval_status
                skill_id,               // forked_from
                now,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create forked skill: {}", e))?;

        self.get_user_skill(&fork_id)?
            .ok_or_else(|| "Failed to retrieve forked skill".to_string())
    }

    /// Increment the usage count of a skill and return the new count.
    pub fn increment_skill_usage(&self, skill_id: &str) -> Result<u64, String> {
        let conn = self.get_conn()?;
        let rows = conn
            .execute(
                "UPDATE user_skills SET usage_count = COALESCE(usage_count, 0) + 1 WHERE id = ?1",
                params![skill_id],
            )
            .map_err(|e| format!("Failed to increment usage count: {}", e))?;

        if rows == 0 {
            return Err(format!("Skill not found: {}", skill_id));
        }

        let count: u64 = conn
            .query_row(
                "SELECT COALESCE(usage_count, 0) FROM user_skills WHERE id = ?1",
                params![skill_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to read usage count: {}", e))?;

        Ok(count)
    }

    // ========================================================================
    // Phase Token Usage Operations
    // ========================================================================

    /// Record token usage for a single AI call within a workflow phase.
    pub fn create_phase_token_usage(
        &self,
        task_run_id: &str,
        phase: &str,
        stage_index: Option<u32>,
        iteration: Option<u32>,
        model_used: Option<&str>,
        provider_used: Option<&str>,
        input_tokens: u64,
        output_tokens: u64,
        cost_cents: u64,
        duration_ms: Option<u64>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"INSERT INTO phase_token_usage
                (task_run_id, phase, stage_index, iteration, model_used, provider_used,
                 input_tokens, output_tokens, cost_cents, duration_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"#,
            params![
                task_run_id,
                phase,
                stage_index.map(|v| v as i64),
                iteration.map(|v| v as i64),
                model_used,
                provider_used,
                input_tokens as i64,
                output_tokens as i64,
                cost_cents as i64,
                duration_ms.map(|v| v as i64),
            ],
        )
        .map_err(|e| format!("Failed to insert phase token usage: {}", e))?;
        Ok(())
    }

    /// Get per-phase token usage breakdown for a task run.
    pub fn get_phase_token_usage(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<PhaseTokenUsageRow>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT phase, stage_index, iteration, model_used, provider_used,
                       input_tokens, output_tokens, cost_cents, duration_ms, created_at
                FROM phase_token_usage
                WHERE task_run_id = ?1
                ORDER BY created_at ASC"#,
            )
            .map_err(|e| format!("Failed to prepare phase token usage query: {}", e))?;

        let rows = stmt
            .query_map(params![task_run_id], |row| {
                Ok(PhaseTokenUsageRow {
                    phase: row.get(0)?,
                    stage_index: row.get::<_, Option<i64>>(1)?.map(|v| v as u32),
                    iteration: row.get::<_, Option<i64>>(2)?.map(|v| v as u32),
                    model_used: row.get(3)?,
                    provider_used: row.get(4)?,
                    input_tokens: row.get::<_, i64>(5)? as u64,
                    output_tokens: row.get::<_, i64>(6)? as u64,
                    cost_cents: row.get::<_, i64>(7)? as u64,
                    duration_ms: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
                    created_at: row.get(9)?,
                })
            })
            .map_err(|e| format!("Failed to query phase token usage: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect phase token usage rows: {}", e))
    }

    /// Update the aggregate token totals on a task run.
    pub fn update_task_run_token_totals(&self, task_run_id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"UPDATE task_runs SET
                total_input_tokens = COALESCE((SELECT SUM(input_tokens) FROM phase_token_usage WHERE task_run_id = ?1), 0),
                total_output_tokens = COALESCE((SELECT SUM(output_tokens) FROM phase_token_usage WHERE task_run_id = ?1), 0),
                total_cost_cents = COALESCE((SELECT SUM(cost_cents) FROM phase_token_usage WHERE task_run_id = ?1), 0)
            WHERE id = ?1"#,
            params![task_run_id],
        )
        .map_err(|e| format!("Failed to update task run token totals: {}", e))?;
        Ok(())
    }
}

// =============================================================================
// Worktree CRUD operations
// =============================================================================

impl CheckpointDb {
    /// Insert a worktree record.
    pub fn insert_worktree(&self, record: &crate::worktree::WorktreeRecord) -> Result<(), String> {
        let conn = self.pool.get().map_err(|e| e.to_string())?;
        conn.execute(
            r#"INSERT INTO worktrees (id, worktree_path, branch_name, source_branch, source_commit, repo_path, task_run_id, workflow_name, status, created_at, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"#,
            rusqlite::params![
                record.id,
                record.worktree_path,
                record.branch_name,
                record.source_branch,
                record.source_commit,
                record.repo_path,
                record.task_run_id,
                record.workflow_name,
                record.status.to_string(),
                record.created_at,
                record.updated_at,
            ],
        )
        .map_err(|e| format!("Failed to insert worktree: {}", e))?;
        Ok(())
    }

    /// Update the status of a worktree.
    pub fn update_worktree_status(
        &self,
        id: &str,
        status: &crate::worktree::WorktreeStatus,
    ) -> Result<(), String> {
        let conn = self.pool.get().map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE worktrees SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
            rusqlite::params![status.to_string(), id],
        )
        .map_err(|e| format!("Failed to update worktree status: {}", e))?;
        Ok(())
    }

    /// List worktrees, optionally filtered by status.
    pub fn list_worktrees(
        &self,
        status: Option<&str>,
    ) -> Result<Vec<crate::worktree::WorktreeRecord>, String> {
        let conn = self.pool.get().map_err(|e| e.to_string())?;

        let (query, params): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(s) = status
        {
            (
                "SELECT id, worktree_path, branch_name, source_branch, source_commit, repo_path, task_run_id, workflow_name, status, created_at, updated_at FROM worktrees WHERE status = ?1 ORDER BY created_at DESC",
                vec![Box::new(s.to_string())],
            )
        } else {
            (
                "SELECT id, worktree_path, branch_name, source_branch, source_commit, repo_path, task_run_id, workflow_name, status, created_at, updated_at FROM worktrees ORDER BY created_at DESC",
                vec![],
            )
        };

        let mut stmt = conn.prepare(query).map_err(|e| e.to_string())?;
        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt
            .query_map(params_refs.as_slice(), |row| {
                Ok(crate::worktree::WorktreeRecord {
                    id: row.get(0)?,
                    worktree_path: row.get(1)?,
                    branch_name: row.get(2)?,
                    source_branch: row.get(3)?,
                    source_commit: row.get(4)?,
                    repo_path: row.get(5)?,
                    task_run_id: row.get(6)?,
                    workflow_name: row.get(7)?,
                    status: crate::worktree::WorktreeStatus::from_str(&row.get::<_, String>(8)?),
                    created_at: row.get(9)?,
                    updated_at: row.get(10)?,
                })
            })
            .map_err(|e| e.to_string())?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| e.to_string())?);
        }
        Ok(results)
    }

    /// Get a single worktree by ID.
    pub fn get_worktree(
        &self,
        id: &str,
    ) -> Result<Option<crate::worktree::WorktreeRecord>, String> {
        let conn = self.pool.get().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, worktree_path, branch_name, source_branch, source_commit, repo_path, task_run_id, workflow_name, status, created_at, updated_at FROM worktrees WHERE id = ?1",
            )
            .map_err(|e| e.to_string())?;

        let result = stmt
            .query_row(rusqlite::params![id], |row| {
                Ok(crate::worktree::WorktreeRecord {
                    id: row.get(0)?,
                    worktree_path: row.get(1)?,
                    branch_name: row.get(2)?,
                    source_branch: row.get(3)?,
                    source_commit: row.get(4)?,
                    repo_path: row.get(5)?,
                    task_run_id: row.get(6)?,
                    workflow_name: row.get(7)?,
                    status: crate::worktree::WorktreeStatus::from_str(&row.get::<_, String>(8)?),
                    created_at: row.get(9)?,
                    updated_at: row.get(10)?,
                })
            })
            .optional()
            .map_err(|e| e.to_string())?;

        Ok(result)
    }

    /// Delete a worktree record.
    pub fn delete_worktree(&self, id: &str) -> Result<(), String> {
        let conn = self.pool.get().map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM worktrees WHERE id = ?1", rusqlite::params![id])
            .map_err(|e| format!("Failed to delete worktree: {}", e))?;
        Ok(())
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
        assert!(!loaded.completed);

        // Update
        let updated = CheckpointData {
            current_phase: 5,
            completed: true,
            ..checkpoint.clone()
        };
        db.save_checkpoint(&updated).unwrap();

        let reloaded = db.get_checkpoint("test-workflow").unwrap().unwrap();
        assert_eq!(reloaded.current_phase, 5);
        assert!(reloaded.completed);

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
        assert!(!is_complete);
        assert_eq!(phase, 5);

        // Complete when threshold is 5
        let (is_complete, _) = db
            .check_checkpoint_status("status-test", 5)
            .unwrap()
            .unwrap();
        assert!(is_complete);
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
        let input = CreateTaskRunInput::new("test-task-1", "Test Task").with_prompt("Do something");
        let task_run = db.create_task_run(&input).unwrap();
        assert!(task_run.auto_continue);

        // Create task run with explicit auto_continue = false
        let input = CreateTaskRunInput::new("test-task-2", "Test Task 2")
            .with_prompt("Do something else")
            .with_auto_continue(false);
        let task_run_disabled = db.create_task_run(&input).unwrap();
        assert!(!task_run_disabled.auto_continue);

        // Get auto_continue setting
        let auto_continue = db.get_task_auto_continue("test-task-1").unwrap();
        assert!(auto_continue);

        let auto_continue_disabled = db.get_task_auto_continue("test-task-2").unwrap();
        assert!(!auto_continue_disabled);

        // Set auto_continue setting
        db.set_task_auto_continue("test-task-1", false).unwrap();
        let updated = db.get_task_auto_continue("test-task-1").unwrap();
        assert!(!updated);

        // Verify via get_task_run
        let loaded = db.get_task_run("test-task-1").unwrap().unwrap();
        assert!(!loaded.auto_continue);
    }

    #[test]
    fn test_get_running_task_step_data_no_running_tasks() {
        let (db, _temp) = create_test_db();

        // No tasks at all
        let result = db.get_running_task_step_data().unwrap();
        assert!(result.is_none());

        // Create a completed task (not running)
        let input = CreateTaskRunInput::new("done-1", "Done Task").with_prompt("finished");
        db.create_task_run(&input).unwrap();
        db.update_task_run_status("done-1", "complete").unwrap();

        let result = db.get_running_task_step_data().unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_running_task_step_data_with_events() {
        let (db, _temp) = create_test_db();

        let input = CreateTaskRunInput::new("run-1", "Running Task").with_prompt("do stuff");
        db.create_task_run(&input).unwrap();

        // Insert step events
        let start_event = CreateTaskRunEventInput {
            task_run_id: "run-1".to_string(),
            event_type: "step_execution".to_string(),
            event_subtype: Some("start".to_string()),
            message: "Run npm test".to_string(),
            data: Some(
                serde_json::json!({
                    "step_name": "npm test",
                    "step_index": 0,
                    "phase": "setup"
                })
                .to_string(),
            ),
            workflow_name: None,
            state_name: None,
            action_id: Some("action-1".to_string()),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            duration_ms: None,
        };
        db.create_task_run_event(&start_event).unwrap();

        let complete_event = CreateTaskRunEventInput {
            task_run_id: "run-1".to_string(),
            event_type: "step_execution".to_string(),
            event_subtype: Some("complete".to_string()),
            message: "Run npm test".to_string(),
            data: Some(
                serde_json::json!({
                    "step_name": "npm test",
                    "step_index": 0,
                    "phase": "setup",
                    "duration_ms": 1500
                })
                .to_string(),
            ),
            workflow_name: None,
            state_name: None,
            action_id: Some("action-1".to_string()),
            timestamp: "2026-01-01T00:00:02Z".to_string(),
            duration_ms: Some(1500),
        };
        db.create_task_run_event(&complete_event).unwrap();

        let result = db.get_running_task_step_data().unwrap();
        assert!(result.is_some());

        let (task, events) = result.unwrap();
        assert_eq!(task.id, "run-1");
        assert_eq!(task.status, "running");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_subtype.as_deref(), Some("start"));
        assert_eq!(events[1].event_subtype.as_deref(), Some("complete"));
    }

    #[test]
    fn test_get_completed_verification_iterations() {
        let (db, _temp) = create_test_db();

        let input = CreateTaskRunInput::new("task-v1", "Verification Task").with_prompt("verify");
        db.create_task_run(&input).unwrap();

        // Store verification results for iterations 1, 2, 3
        for i in 1..=3 {
            let result = serde_json::json!({
                "all_passed": i == 3,
                "total_steps": 5,
                "passed_steps": if i == 3 { 5 } else { 3 },
                "failed_steps": if i == 3 { 0 } else { 2 },
                "skipped_steps": 0,
                "total_duration_ms": 1000 * i,
                "critical_failure": false,
                "iteration": i
            });
            db.store_verification_phase_result("task-v1", i as u32, &result)
                .unwrap();
        }

        let iterations = db.get_completed_verification_iterations("task-v1").unwrap();
        assert_eq!(iterations, vec![1, 2, 3]);
    }

    #[test]
    fn test_get_completed_verification_iterations_empty() {
        let (db, _temp) = create_test_db();

        // No verification results for a nonexistent task
        let iterations = db
            .get_completed_verification_iterations("nonexistent-task")
            .unwrap();
        assert!(iterations.is_empty());

        // Create a task but don't add verification results
        let input =
            CreateTaskRunInput::new("task-empty", "Empty Task").with_prompt("no verification");
        db.create_task_run(&input).unwrap();

        let iterations = db
            .get_completed_verification_iterations("task-empty")
            .unwrap();
        assert!(iterations.is_empty());
    }

    #[test]
    fn test_process_session_crud() {
        let (db, _temp) = create_test_db();

        // Create a session
        db.create_process_session("sess-1", "config-a", "My Process")
            .unwrap();

        // Verify it appears in get_process_sessions (filtered by config_id)
        let sessions = db.get_process_sessions(Some("config-a"), 100).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "sess-1");
        assert_eq!(sessions[0].process_config_id, "config-a");
        assert_eq!(sessions[0].process_name, "My Process");
        assert_eq!(sessions[0].state, "running");
        assert!(sessions[0].stopped_at.is_none());
        assert!(sessions[0].exit_code.is_none());
        assert_eq!(sessions[0].error_count, 0);

        // Verify it also appears when querying without filter
        let all_sessions = db.get_process_sessions(None, 100).unwrap();
        assert_eq!(all_sessions.len(), 1);
        assert_eq!(all_sessions[0].id, "sess-1");

        // Update the session (simulate process exit)
        db.update_process_session("sess-1", "exited", Some(0), 3)
            .unwrap();

        // Verify the update
        let sessions = db.get_process_sessions(Some("config-a"), 100).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].state, "exited");
        assert_eq!(sessions[0].exit_code, Some(0));
        assert_eq!(sessions[0].error_count, 3);
        assert!(sessions[0].stopped_at.is_some());

        // Create a second session for a different config and verify filtering
        db.create_process_session("sess-2", "config-b", "Other Process")
            .unwrap();

        let config_a = db.get_process_sessions(Some("config-a"), 100).unwrap();
        assert_eq!(config_a.len(), 1);
        assert_eq!(config_a[0].id, "sess-1");

        let config_b = db.get_process_sessions(Some("config-b"), 100).unwrap();
        assert_eq!(config_b.len(), 1);
        assert_eq!(config_b[0].id, "sess-2");

        let all = db.get_process_sessions(None, 100).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_process_session_output() {
        let (db, _temp) = create_test_db();

        // Create a session to attach output to
        db.create_process_session("sess-out-1", "config-x", "Output Test")
            .unwrap();

        // Insert output lines
        let lines = vec![
            (
                "2026-01-01T00:00:01Z".to_string(),
                "stdout".to_string(),
                "Starting server...".to_string(),
            ),
            (
                "2026-01-01T00:00:02Z".to_string(),
                "stdout".to_string(),
                "Listening on port 8080".to_string(),
            ),
            (
                "2026-01-01T00:00:03Z".to_string(),
                "stderr".to_string(),
                "Warning: deprecated config option".to_string(),
            ),
        ];
        db.insert_process_session_output("sess-out-1", &lines)
            .unwrap();

        // Retrieve all output lines
        let output = db.get_process_session_output("sess-out-1", 100, 0).unwrap();
        assert_eq!(output.len(), 3);

        // Verify order (should be ordered by id ASC, matching insertion order)
        assert_eq!(output[0].line, "Starting server...");
        assert_eq!(output[0].stream, "stdout");
        assert_eq!(output[0].session_id, "sess-out-1");

        assert_eq!(output[1].line, "Listening on port 8080");
        assert_eq!(output[1].stream, "stdout");

        assert_eq!(output[2].line, "Warning: deprecated config option");
        assert_eq!(output[2].stream, "stderr");

        // Verify limit works
        let limited = db.get_process_session_output("sess-out-1", 2, 0).unwrap();
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].line, "Starting server...");
        assert_eq!(limited[1].line, "Listening on port 8080");

        // Verify offset works
        let offset = db.get_process_session_output("sess-out-1", 100, 1).unwrap();
        assert_eq!(offset.len(), 2);
        assert_eq!(offset[0].line, "Listening on port 8080");
        assert_eq!(offset[1].line, "Warning: deprecated config option");

        // Verify empty insert is a no-op
        db.insert_process_session_output("sess-out-1", &[]).unwrap();
        let output_after = db.get_process_session_output("sess-out-1", 100, 0).unwrap();
        assert_eq!(output_after.len(), 3);

        // Verify output for a different session is isolated
        db.create_process_session("sess-out-2", "config-x", "Output Test 2")
            .unwrap();
        let other_output = db.get_process_session_output("sess-out-2", 100, 0).unwrap();
        assert!(other_output.is_empty());
    }

    #[test]
    fn test_process_session_pruning() {
        let (db, _temp) = create_test_db();

        // Create 12 sessions for the same config_id with staggered timestamps.
        // create_process_session uses Utc::now() so we insert them sequentially;
        // the order in the DB will match insertion order.
        for i in 1..=12 {
            let session_id = format!("prune-sess-{}", i);
            db.create_process_session(&session_id, "config-prune", &format!("Process {}", i))
                .unwrap();
        }

        // Verify all 12 exist
        let before = db.get_process_sessions(Some("config-prune"), 100).unwrap();
        assert_eq!(before.len(), 12);

        // Prune, keeping only the 10 most recent
        let deleted = db.prune_old_process_sessions("config-prune", 10).unwrap();
        assert_eq!(deleted, 2);

        // Verify 10 remain
        let after = db.get_process_sessions(Some("config-prune"), 100).unwrap();
        assert_eq!(after.len(), 10);

        // The most recent sessions should survive (ordered DESC by started_at).
        // Sessions 3..=12 should remain; 1 and 2 should be pruned.
        // get_process_sessions returns DESC order, so first result is newest.
        let remaining_ids: Vec<String> = after.iter().map(|s| s.id.clone()).collect();
        assert!(!remaining_ids.contains(&"prune-sess-1".to_string()));
        assert!(!remaining_ids.contains(&"prune-sess-2".to_string()));
        assert!(remaining_ids.contains(&"prune-sess-12".to_string()));
        assert!(remaining_ids.contains(&"prune-sess-3".to_string()));

        // Pruning a different config should not affect these sessions
        let deleted_other = db.prune_old_process_sessions("config-other", 5).unwrap();
        assert_eq!(deleted_other, 0);

        let still_ten = db.get_process_sessions(Some("config-prune"), 100).unwrap();
        assert_eq!(still_ten.len(), 10);

        // Pruning again with same keep_count should delete nothing
        let deleted_again = db.prune_old_process_sessions("config-prune", 10).unwrap();
        assert_eq!(deleted_again, 0);
    }
}
