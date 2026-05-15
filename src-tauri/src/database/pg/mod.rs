//! PostgreSQL database layer for qontinui-runner.
//!
//! Runs alongside SQLite during migration. New tables and queries are added here
//! via Clorinde-generated code. Callers prefer PG when available, fall back to SQLite.

pub mod activity_timeline;
pub mod adaptive_learning;
pub mod agent_worktrees;
pub mod agentic_metrics;
pub mod ai_sessions;
pub mod approval_gates;
pub mod breakpoints;
pub mod cached_specs;
pub mod canary;
pub mod canvas;
pub mod checkpoints;
pub mod checks;
pub mod chunk_labels;
pub mod comparison;
pub mod compensation;
pub mod completion_reports;
pub mod contradiction;
pub mod coordinator_decisions;
pub mod coordinator_leader;
pub mod coordinator_shadow_decisions;
pub mod decision_trail;
pub mod deferred_questions;
pub mod entailment_cache;
pub mod entity_profiles;
pub mod error_monitor;
pub mod event_log;
pub mod event_search;
pub mod export;
pub mod findings;
pub mod flows;
pub mod generation;
pub mod generation_artifacts;
pub mod generation_feedback;
pub mod graph_ops;
pub mod hooks;
pub mod instances;
pub mod knowledge;
pub mod known_issues;
pub mod learned_patterns_ops;
pub mod learning;
pub mod log_sources;
pub mod memory_query_cache;
pub mod merge_proposals;
pub mod meta_optimizer;
pub mod misc_crud;
pub mod observations;
pub mod online_learning;
pub mod orchestration_loop;
pub mod phase_results;
pub mod pipeline_traces;
pub mod plans;
pub mod pr_watch_ops;
pub mod process_sessions;
pub mod productivity_knowledge;
pub mod prompt_evolution;
pub mod prompt_registry;
pub mod q_routing;
pub mod queued_workflows;
pub mod reasoning_traces;
pub mod recordings;
pub mod reflection;
pub mod regression;
pub mod restate;
pub mod reviews;
pub mod scheduler;
pub mod security_audit;
pub mod session_file_snapshots;
pub mod session_touched_files;
pub mod settings;
pub mod skills;
pub mod spec_experimentation;
pub mod spec_proposals;
pub mod state_machine;
pub mod step_type_knowledge;
pub mod task_run_events;
pub mod task_runs;
pub mod tasks;
pub mod ticket_system_ops;
pub mod tiered_info;
pub mod token_analytics;
pub mod token_usage;
pub mod triggers;
pub mod ui_bridge;
pub mod ui_bridge_baselines;
pub mod verification_tests;
pub mod watchers;
pub mod workflow_state;
pub mod workflows;
pub mod working_representations;
pub mod worktrees;
pub mod wsv_disagreements;

use std::sync::{Arc, OnceLock};
use tracing::{info, warn};

/// Global PgDb instance, set once during app initialization.
/// Allows sync-context code (thread spawns, closures) to access PG
/// without threading Arc<PgDb> through every call chain.
static GLOBAL_PG_DB: OnceLock<Arc<PgDb>> = OnceLock::new();

/// PostgreSQL connection pool backed by deadpool-postgres.
pub struct PgDb {
    pool: deadpool_postgres::Pool,
}

impl PgDb {
    /// Set the global PgDb instance. Call once during app initialization.
    /// Warns and ignores if called more than once.
    pub fn set_global(pg_db: Arc<PgDb>) {
        GLOBAL_PG_DB
            .set(pg_db)
            .unwrap_or_else(|_| warn!("PgDb::set_global called more than once (ignored)"));
    }

    /// Get the global PgDb instance. Panics if not initialized.
    /// Use from sync contexts where Arc<PgDb> is not threaded through.
    pub fn global() -> Arc<PgDb> {
        GLOBAL_PG_DB
            .get()
            .expect("PgDb::global() called before PgDb::set_global()")
            .clone()
    }

    /// Try to get the global PgDb instance. Returns None if not initialized.
    pub fn try_global() -> Option<Arc<PgDb>> {
        GLOBAL_PG_DB.get().cloned()
    }

    /// Connect to PostgreSQL using the given connection string.
    ///
    /// The connection string should NOT include search_path — it's set per-connection
    /// via the pool's post_create hook to avoid parsing issues.
    ///
    /// Returns `Err` if the connection string is invalid or initial connectivity fails.
    pub async fn new(database_url: &str) -> Result<Self, String> {
        let pg_config: tokio_postgres::Config = database_url
            .parse()
            .map_err(|e| format!("Invalid PostgreSQL connection string: {}", e))?;

        let mgr_config = deadpool_postgres::ManagerConfig {
            recycling_method: deadpool_postgres::RecyclingMethod::Fast,
        };

        let mgr =
            deadpool_postgres::Manager::from_config(pg_config, tokio_postgres::NoTls, mgr_config);

        // max_size lowered from 16 -> 8: keep the pool's idle-connection
        // footprint small so a force-killed runner that left orphaned PG
        // backends doesn't leave 16 zombie sessions blocking advisory locks
        // for the next runner.
        //
        // Do NOT add `.recycle_timeout(Some(_))` here — `Pool::builder`'s
        // timeout setters require a tokio runtime to be entered for their
        // timer machinery (deadpool 0.12 panics with "Timeouts require a
        // runtime" otherwise). The PG bootstrap is called from a synchronous
        // `rt.block_on(PgDb::new(...))` in `main.rs`, which builds its own
        // single-threaded runtime for the bootstrap. That runtime IS active
        // during the await inside `block_on`, but the `Pool::builder` chain
        // runs synchronously before the first `.await`, so the timer
        // registration races with runtime entry and panics.
        // See: src/main.rs:285 for the calling context.
        let pool = deadpool_postgres::Pool::builder(mgr)
            .max_size(8)
            .post_create(deadpool_postgres::Hook::async_fn(|conn, _| {
                Box::pin(async move {
                    conn.simple_query("SET search_path TO project, public")
                        .await
                        .map_err(|e| {
                            deadpool_postgres::HookError::Message(
                                format!("Failed to set search_path: {}", e).into(),
                            )
                        })?;
                    Ok(())
                })
            }))
            .build()
            .map_err(|e| format!("Failed to create PG pool: {}", e))?;

        // Verify connectivity and schema
        let conn = pool
            .get()
            .await
            .map_err(|e| format!("PostgreSQL connection failed: {}", e))?;

        // Self-bootstrap: ensure prerequisites exist regardless of which
        // deployment substrate the runner is talking to. Two responsibilities
        // remain in this hot path:
        //
        //   1. `CREATE EXTENSION IF NOT EXISTS vector` — pgvector is the
        //      documented escape hatch from declarative schema management
        //      (Atlas Community can't declaratively own extensions). Keep it
        //      imperative; it costs nothing on a warm DB.
        //   2. `CREATE SCHEMA IF NOT EXISTS runner` — back-compat for any
        //      deployment still referencing the legacy runner namespace.
        //
        // Table-shaped DDL for `project.regression_*` previously lived here as
        // CREATE TABLE IF NOT EXISTS self-heal. That set is now Atlas-managed
        // out of `qontinui-runner/atlas/schema.hcl` (Row 3 schema-half pilot,
        // Wave 1.4). Atlas is invoked out-of-process (CI / migrator container)
        // against the canonical PG; this hot path no longer enforces the
        // table shape. The historical alembic migration
        // `f9d3e8a4c1b6_add_regression_tables.py` remains in
        // `qontinui-web/backend/alembic/versions/` as frozen history.
        conn.batch_execute(
            "CREATE SCHEMA IF NOT EXISTS runner; \
             CREATE EXTENSION IF NOT EXISTS vector;",
        )
        .await
        .map_err(|e| format!("Failed to bootstrap runner schema/extension: {}", e))?;

        info!("PostgreSQL connected (deadpool, max_size=8, schema=runner)");

        let db = Self { pool };
        Ok(db)
    }

    // ========================================================================
    // Execution State Snapshots
    // ========================================================================

    /// Record a state snapshot for replay / observability.
    pub async fn record_state_snapshot(
        &self,
        execution_id: &str,
        span_id: &str,
        snapshot_ts: &str,
        state_type: &str,
        summary: Option<&str>,
        context_json: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            r#"INSERT INTO execution_state_snapshots
               (execution_id, span_id, snapshot_ts, state_type, summary, context_json)
               VALUES ($1, $2, $3::timestamptz, $4, $5, $6)"#,
            &[
                &execution_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &span_id,
                &snapshot_ts,
                &state_type,
                &summary as &(dyn tokio_postgres::types::ToSql + Sync),
                &context_json as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG record_state_snapshot: {}", e))?;
        Ok(())
    }

    /// Query execution state snapshots for replay.
    pub async fn get_state_snapshots(
        &self,
        execution_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let client = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = client
            .query(
                "SELECT id, execution_id, span_id, snapshot_ts, state_type, summary, context_json, created_at
                 FROM execution_state_snapshots
                 WHERE execution_id = $1
                 ORDER BY snapshot_ts ASC",
                &[&execution_id as &(dyn tokio_postgres::types::ToSql + Sync)],
            )
            .await
            .map_err(|e| format!("Failed to query snapshots: {}", e))?;

        Ok(rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "id": row.get::<usize, i64>(0),
                    "execution_id": row.get::<usize, String>(1),
                    "span_id": row.get::<usize, String>(2),
                    "snapshot_ts": row.get::<usize, String>(3),
                    "state_type": row.get::<usize, String>(4),
                    "summary": row.get::<usize, Option<String>>(5),
                    "context_json": row.get::<usize, Option<String>>(6),
                    "created_at": row.get::<usize, String>(7),
                })
            })
            .collect())
    }

    /// Get a reference to the connection pool.
    pub fn pool(&self) -> &deadpool_postgres::Pool {
        &self.pool
    }

    /// Blocking helper for tests: create a PgDb using DATABASE_URL.
    /// Panics if PG is not available.
    #[cfg(test)]
    pub fn new_blocking_for_test() -> std::sync::Arc<Self> {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://localhost:5432/qontinui_test".to_string());
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime for test");
        std::sync::Arc::new(
            rt.block_on(Self::new(&url))
                .expect("PgDb connection for test"),
        )
    }

    /// Test-only: create a PgDb with a pool that will error on any actual
    /// query. Use this for unit tests that need a `LoopContext` or
    /// `CompensationManager` but never actually call database methods.
    /// Any attempt to `pool.get()` returns an immediate error because
    /// the underlying manager has an unreachable host.
    #[cfg(test)]
    pub fn new_noop_for_test() -> std::sync::Arc<Self> {
        use deadpool_postgres::{Config, Runtime};

        let mut cfg = Config::new();
        cfg.host = Some("__noop_test_host_that_does_not_exist__".to_string());
        cfg.dbname = Some("noop".to_string());
        let pool = cfg
            .create_pool(Some(Runtime::Tokio1), tokio_postgres::NoTls)
            .expect("noop pool creation should not fail (no connection attempted)");
        std::sync::Arc::new(Self { pool })
    }
}
