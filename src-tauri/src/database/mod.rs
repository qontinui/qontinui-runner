//! SQLite database for qontinui-runner persistence.
//!
//! Provides transaction-safe storage for sessions, checkpoints, settings,
//! prompts, workflows, and scheduler state.
//!
//! The database module is organized into submodules by domain:
//! - `task_runs` — Task run CRUD, AI session management
//! - `checkpoint_ops` — Workflow checkpoints, session tracking
//! - `settings_ops` — Settings, config storage, JSON file migration
//! - `findings_ops` — Findings wrapper methods
//! - `task_run_events` — Event logging, screenshots, Playwright results, API requests
//! - `learning_ops` — Learning outcomes, patterns, dashboard stats
//! - `orchestrator_ops` — Verification plans, knowledge, constraints
//! - `orchestrator_checkpoint_ops` — Orchestrator checkpoint CRUD and queries
//! - `flow_ops` — Flow designer CRUD, version history, executions
//! - `workflow_state_ops` — Workflow execution state, step checkpoints, progress markers
//! - `workflow_ops` — Unified workflow CRUD
//! - `verification_ops` — Verification results
//! - `generator_eval` — Pipeline artifacts, benchmarks, edit analysis, examples
//! - `skills_ops` — User skills CRUD, import/export
//! - `cached_specs` — Cached app specs for UI Bridge
//! - `process_sessions` — Process session persistence and output logging
//! - `canvas_ops` — Canvas panels (A2UI)
//! - `approval_gates` — Human-in-the-loop approval gates
//! - `token_usage` — Phase token usage tracking
//! - `worktree_ops` — Git worktree management
//! - `scheduler` — Scheduled task management
//! - `embeddings` / `embedding_client` / `embedding_jobs` — Vector embeddings
//! - `hybrid_search` — Hybrid search (text + vector)
//! - `export_import` — Data export/import
//! - `pipeline_traces` — Pipeline trace storage
//! - `query_builder` — Dynamic query building utilities

// Existing submodules
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

// PostgreSQL layer (Clorinde-generated queries, runs alongside SQLite during migration)
pub mod pg;

// Newly extracted submodules
pub mod agentic_metrics_ops;
pub mod approval_gates;
pub mod artifact_ops;
pub mod cached_specs;
pub mod canvas_ops;
pub mod checkpoint_ops;
pub mod findings_ops;
pub mod flow_ops;
pub mod generator_eval;
pub mod learning_ops;
pub mod orchestrator_checkpoint_ops;
pub mod process_sessions;
pub mod queue_ops;
pub mod settings_ops;
pub mod skills_ops;
pub mod task_run_events;
pub mod token_usage;
pub mod workflow_state_ops;
pub mod worktree_ops;

// Graph-based workflow improvement
pub mod cross_run_ops;
pub mod graph_ops;
pub mod token_analytics;

// UI Bridge persistence
pub mod ui_bridge_ops;

// Re-export all types for backward compatibility
pub use types::*;

use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tracing::{info, warn};

/// Global CheckpointDb instance, set once during app initialization.
/// Allows code to access the SQLite database without threading Arc<CheckpointDb>
/// through every call chain.
static GLOBAL_CHECKPOINT_DB: OnceLock<Arc<CheckpointDb>> = OnceLock::new();

/// Database handle for checkpoint and session persistence.
pub struct CheckpointDb {
    pool: Pool<SqliteConnectionManager>,
    db_path: PathBuf,
    /// The runner's API port, used to tag task_runs for instance-level filtering.
    /// Set after the HTTP server binds. Defaults to 0 (unset).
    runner_port: std::sync::atomic::AtomicU16,
}

impl CheckpointDb {
    /// Set the global CheckpointDb instance. Call once during app initialization.
    /// Warns and ignores if called more than once.
    pub fn set_global(db: Arc<CheckpointDb>) {
        GLOBAL_CHECKPOINT_DB
            .set(db)
            .unwrap_or_else(|_| warn!("CheckpointDb::set_global called more than once (ignored)"));
    }

    /// Get the global CheckpointDb instance. Panics if not initialized.
    /// Use from sync contexts where Arc<CheckpointDb> is not threaded through.
    pub fn global() -> Arc<CheckpointDb> {
        GLOBAL_CHECKPOINT_DB
            .get()
            .expect("CheckpointDb::global() called before CheckpointDb::set_global()")
            .clone()
    }

    /// Try to get the global CheckpointDb instance. Returns None if not initialized.
    pub fn try_global() -> Option<Arc<CheckpointDb>> {
        GLOBAL_CHECKPOINT_DB.get().cloned()
    }

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

        let db = Self {
            pool,
            db_path: db_path.clone(),
            runner_port: std::sync::atomic::AtomicU16::new(0),
        };

        // Backfill deterministic agentic scores for historical runs (idempotent, fast)
        match db.backfill_deterministic_scores() {
            Ok(0) => {}
            Ok(n) => info!("Backfilled agentic scores for {} historical runs", n),
            Err(e) => tracing::warn!("Agentic score backfill failed (non-fatal): {}", e),
        }

        Ok(db)
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

    // ========================================================================
    // Database Maintenance Operations
    // ========================================================================

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
            tracing::warn!("Database integrity check failed - skipping VACUUM");
            return Err("Database integrity check failed".to_string());
        }

        // Clean up stale auto-extracted skills (pending > 30 days, 0 usage)
        match self.cleanup_stale_auto_skills(30) {
            Ok(n) if n > 0 => info!("Cleaned up {} stale auto-skills during maintenance", n),
            Err(e) => tracing::warn!("Stale skill cleanup failed: {}", e),
            _ => {}
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
    /// Only allows SELECT queries to prevent arbitrary SQL execution.
    pub fn explain_query_plan(&self, query: &str) -> Result<String, String> {
        let trimmed = query.trim();
        if !trimmed.to_uppercase().starts_with("SELECT") {
            return Err("Only SELECT queries are allowed for EXPLAIN QUERY PLAN".to_string());
        }
        if trimmed.contains(';') {
            return Err("Multiple statements are not allowed".to_string());
        }

        let conn = self.get_conn()?;

        let explain_query = format!("EXPLAIN QUERY PLAN {}", trimmed);
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
