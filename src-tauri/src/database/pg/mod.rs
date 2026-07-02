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
pub mod apps;
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
pub mod meta_optimizer;
pub mod misc_crud;
pub mod observations;
pub mod online_learning;
pub mod orchestration;
pub mod orchestration_loop;
pub mod phase_results;
pub mod pipeline_traces;
pub mod plans;
pub mod pr_watch_ops;
pub mod process_sessions;
pub mod productivity_knowledge;
pub mod prompt_evolution;
pub mod prompt_registry;
pub mod proposal_events;
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
use tracing::{debug, info, warn};

/// Global PgDb instance, set once during app initialization.
/// Allows sync-context code (thread spawns, closures) to access PG
/// without threading Arc<PgDb> through every call chain.
static GLOBAL_PG_DB: OnceLock<Arc<PgDb>> = OnceLock::new();

/// Whether the canonical PG was reachable at boot. `true` after a successful
/// `PgDb::new`; `false` when the runner booted degraded (`QONTINUI_ALLOW_NO_DB`)
/// with PG unreachable. DB-backed HTTP handlers consult `pg_available()` to
/// return a clean `503 database unavailable` instead of hitting a dead pool.
///
/// Defaults to `true` so the overwhelmingly common path (PG present) needs no
/// explicit set and any code that reads it before boot completes does not
/// spuriously 503; the degraded-boot path explicitly flips it to `false`.
static PG_AVAILABLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Record whether the canonical PG is available. Called once at boot:
/// `true` on a verified connection, `false` on degraded boot.
pub fn set_pg_available(available: bool) {
    PG_AVAILABLE.store(available, std::sync::atomic::Ordering::Relaxed);
}

/// Returns `false` only when the runner booted degraded with PG unreachable.
/// DB-backed handlers use this to short-circuit to a 503 in degraded mode.
pub fn pg_available() -> bool {
    PG_AVAILABLE.load(std::sync::atomic::Ordering::Relaxed)
}

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

    /// Build the deadpool connection pool WITHOUT verifying connectivity or
    /// running the self-provision DDL. Shared by `new` (which then probes and
    /// self-provisions) and `new_degraded` (which does neither). The builder
    /// does no I/O and registers no timers (we deliberately avoid
    /// `.recycle_timeout` — see the comment below), so it is safe to call from
    /// a synchronous context outside a Tokio runtime.
    fn build_pool(database_url: &str) -> Result<deadpool_postgres::Pool, String> {
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
        deadpool_postgres::Pool::builder(mgr)
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
            .map_err(|e| format!("Failed to create PG pool: {}", e))
    }

    /// Construct a DEGRADED PgDb for `QONTINUI_ALLOW_NO_DB` boot: the pool is
    /// built (and reconnects lazily if PG later becomes reachable) but initial
    /// connectivity is NOT verified and the self-provision DDL is skipped.
    ///
    /// Used only by the degraded-boot path in `main.rs` when the canonical PG
    /// is unreachable AND degraded boot is explicitly enabled. The caller MUST
    /// flip `set_pg_available(false)` so DB-backed handlers can return 503
    /// instead of repeatedly hitting a pool that has no live backend.
    pub fn new_degraded(database_url: &str) -> Result<Self, String> {
        let pool = Self::build_pool(database_url)?;
        Ok(Self { pool })
    }

    /// Connect to PostgreSQL using the given connection string.
    ///
    /// The connection string should NOT include search_path — it's set per-connection
    /// via the pool's post_create hook to avoid parsing issues.
    ///
    /// Returns `Err` if the connection string is invalid or initial connectivity fails.
    pub async fn new(database_url: &str) -> Result<Self, String> {
        let pool = Self::build_pool(database_url)?;
        let db = Self { pool };
        db.verify_and_provision().await?;
        Ok(db)
    }

    /// Verify connectivity and run the idempotent self-provision DDL (schema +
    /// pgvector extension + the workflow-verification self-heals). Shared by
    /// `new` at boot and the degraded-mode reconnect probe
    /// (`spawn_reconnect_probe`), so a runner that booted with
    /// `QONTINUI_ALLOW_NO_DB` provisions exactly as a normal boot would once PG
    /// becomes reachable. Idempotent — safe to re-run on every retry.
    pub async fn verify_and_provision(&self) -> Result<(), String> {
        // Verify connectivity and schema
        let conn = self
            .pool
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
        conn.batch_execute("CREATE SCHEMA IF NOT EXISTS runner;")
            .await
            .map_err(|e| format!("Failed to bootstrap runner schema: {}", e))?;

        // pgvector is OPTIONAL. The runner stores embeddings as `bytea` blobs
        // and computes cosine similarity in Rust (see `database::embeddings`
        // and `database::hybrid_search`) — no column uses the `vector` type and
        // no query uses pgvector distance operators. Creating the extension is
        // therefore best-effort: a bundled/standalone Postgres (which does not
        // ship pgvector) must still boot. A missing extension is logged, not
        // fatal. Kept as a no-op on managed clusters that DO ship pgvector.
        if let Err(e) = conn
            .batch_execute("CREATE EXTENSION IF NOT EXISTS vector;")
            .await
        {
            warn!(
                "pgvector extension unavailable — continuing without it \
                 (embeddings use bytea + in-Rust cosine similarity): {}",
                e
            );
        }

        // CR-5 (Plan 03): convert `project.workflow_verification_phase_results
        // .result_json` from `text` to `jsonb` so design-context §5.16's
        // indexable JSONB-path expressions work and Plan 06's JSONB indexes
        // have a valid target. Idempotent: the `USING` cast is a no-op if the
        // column is already jsonb, and the whole statement is skipped when the
        // column type is already `jsonb` (re-running `ALTER COLUMN … TYPE
        // jsonb` against a jsonb column is itself a no-op but the explicit
        // guard avoids the rewrite cost on every runner start). The table is
        // alembic-owned (qontinui-web); this runner-side self-heal applies the
        // change on the next user-controlled runner startup without an
        // out-of-band `ALTER TABLE` against the live DB. The alembic chain
        // and `schema.pg.sql.generated` carry the same `jsonb` shape so the
        // CI schema-fresh / clorinde-fresh gates agree with this runtime
        // contract.
        conn.batch_execute(
            "DO $cr5$ \
             BEGIN \
               IF EXISTS ( \
                 SELECT 1 FROM information_schema.columns \
                 WHERE table_schema = 'project' \
                   AND table_name = 'workflow_verification_phase_results' \
                   AND column_name = 'result_json' \
                   AND data_type <> 'jsonb' \
               ) THEN \
                 ALTER TABLE project.workflow_verification_phase_results \
                   ALTER COLUMN result_json TYPE jsonb USING result_json::jsonb; \
               END IF; \
             END \
             $cr5$;",
        )
        .await
        .map_err(|e| format!("CR-5 result_json text→jsonb self-heal failed: {}", e))?;

        // Plan 06 Step 3 (G.3): expression indexes for the workflow-verification
        // dashboard hot paths. Each runs at every PgDb::new() boot — IF NOT
        // EXISTS makes them no-ops after the first run.
        //
        // `project.workflow_verification_phase_results` is alembic-owned
        // (qontinui-web), so on a bare/fresh PG where the migrator hasn't run
        // yet the table is absent. `CREATE INDEX IF NOT EXISTS` only skips when
        // the *index* already exists — against a missing TABLE it still errors
        // (`relation does not exist`), which previously hard-panicked the
        // runner at boot. Wrap all six CREATE INDEX statements in one
        // table-existence DO block (the same guard pattern the proposal_events
        // self-heal below uses) so they run when the table is present and skip
        // cleanly when it is not. IF NOT EXISTS keeps each individual index
        // idempotent on warm boots.
        conn.batch_execute(
            "DO $wfver$
             BEGIN
               IF EXISTS (
                 SELECT 1 FROM information_schema.tables
                 WHERE table_schema = 'project'
                   AND table_name = 'workflow_verification_phase_results'
               ) THEN
                 CREATE INDEX IF NOT EXISTS idx_wf_ver_phase_match_outcome
                     ON project.workflow_verification_phase_results
                     ((result_json->'summary'->>'match_outcome'));
                 CREATE INDEX IF NOT EXISTS idx_wf_ver_phase_overall_match_rate
                     ON project.workflow_verification_phase_results
                     (((result_json->'summary'->>'overall_match_rate')::float));
                 CREATE INDEX IF NOT EXISTS idx_wf_ver_phase_spec_version
                     ON project.workflow_verification_phase_results
                     ((result_json->>'spec_version'));
                 CREATE INDEX IF NOT EXISTS idx_wf_ver_phase_recommendation_reason
                     ON project.workflow_verification_phase_results
                     ((result_json->'recommendation_recommended_state'->>'recommendation_reason'));
                 CREATE INDEX IF NOT EXISTS idx_wf_ver_phase_snapshot_id
                     ON project.workflow_verification_phase_results
                     ((result_json->>'snapshot_id'));
                 CREATE INDEX IF NOT EXISTS idx_wf_ver_phase_severity_gin
                     ON project.workflow_verification_phase_results
                     USING GIN ((result_json->'summary'->'assertions_failed_by_severity'));
               END IF;
             END
             $wfver$;",
        )
        .await
        .map_err(|e| {
            format!(
                "Plan 06 Step 3 wf-ver expression indexes create failed: {}",
                e
            )
        })?;

        // spec-multi-app Stream B: project.apps — multi-tenant app registry.
        //
        // Authored declaratively in `atlas/schema.hcl`; mirrored here as a
        // CREATE TABLE IF NOT EXISTS self-heal so a fresh PG without Atlas
        // applied still boots cleanly. The runner's spec API depends on this
        // table being present from the first /apps/* request.
        conn.batch_execute(
            "CREATE SCHEMA IF NOT EXISTS project; \
             CREATE TABLE IF NOT EXISTS project.apps ( \
                 app_id            TEXT PRIMARY KEY, \
                 repo_root         TEXT NOT NULL, \
                 ui_bridge_url     TEXT NOT NULL, \
                 display_name      TEXT NOT NULL, \
                 created_at_ms     BIGINT NOT NULL, \
                 last_seen_at_ms   BIGINT NOT NULL, \
                 auth_required     BOOLEAN DEFAULT false, \
                 red_threshold     DOUBLE PRECISION DEFAULT 0.5 CHECK (red_threshold >= 0.0 AND red_threshold <= 1.0), \
                 yellow_threshold  DOUBLE PRECISION DEFAULT 0.8 CHECK (yellow_threshold >= 0.0 AND yellow_threshold <= 1.0) \
             ); \
             CREATE INDEX IF NOT EXISTS idx_apps_last_seen \
                 ON project.apps (last_seen_at_ms DESC);",
        )
        .await
        .map_err(|e| format!("Stream B project.apps self-heal failed: {}", e))?;

        // B v1 Polish Step 1c: backfill threshold columns if apps table already exists.
        // Idempotent: UPDATE WHERE IS NULL only affects rows missing the columns after ADD COLUMN IF NOT EXISTS.
        conn.batch_execute(
            "DO $$
             BEGIN
               IF EXISTS (
                 SELECT 1 FROM information_schema.tables
                 WHERE table_schema = 'project' AND table_name = 'apps'
               ) THEN
                 ALTER TABLE project.apps
                     ADD COLUMN IF NOT EXISTS auth_required BOOLEAN DEFAULT false;
                 ALTER TABLE project.apps
                     ADD COLUMN IF NOT EXISTS red_threshold DOUBLE PRECISION DEFAULT 0.5;
                 ALTER TABLE project.apps
                     ADD COLUMN IF NOT EXISTS yellow_threshold DOUBLE PRECISION DEFAULT 0.8;
                 -- Backfill existing rows with defaults (no-op if columns already populated)
                 UPDATE project.apps SET auth_required = false WHERE auth_required IS NULL;
                 UPDATE project.apps SET red_threshold = 0.5 WHERE red_threshold IS NULL;
                 UPDATE project.apps SET yellow_threshold = 0.8 WHERE yellow_threshold IS NULL;
               END IF;
             END $$;",
        )
        .await
        .map_err(|e| format!("B v1 Polish project.apps auth/threshold backfill failed: {}", e))?;

        // spec-multi-app Stream E.1: backfill `app_id` onto
        // project.proposal_events. The table predates the multi-tenant model,
        // so existing rows are migrated under the bootstrap app_id
        // `qontinui-runner` (matching Stream F's bootstrap registration).
        //
        // Gated on table existence — on a fresh canonical PG where Atlas
        // hasn't created project.proposal_events yet, this whole block is a
        // no-op. Atlas owns the CREATE TABLE; the self-heal owns the
        // app_id column migration once the table exists. Wrapped in a
        // PL/pgSQL DO block so the table-existence check + locking + ALTER
        // run in one round-trip and skip cleanly when the table isn't
        // present. ACCESS EXCLUSIVE prevents any concurrent writer from
        // inserting a NULL row between the backfill and the NOT NULL
        // constraint flip. Idempotent: `ADD COLUMN IF NOT EXISTS` + the
        // `SET NOT NULL` becomes a no-op on a subsequent boot because the
        // backfill keeps the column dense.
        conn.batch_execute(
            "DO $$
             BEGIN
               IF EXISTS (
                 SELECT 1 FROM information_schema.tables
                 WHERE table_schema = 'project' AND table_name = 'proposal_events'
               ) THEN
                 LOCK TABLE project.proposal_events IN ACCESS EXCLUSIVE MODE;
                 ALTER TABLE project.proposal_events
                     ADD COLUMN IF NOT EXISTS app_id TEXT;
                 UPDATE project.proposal_events SET app_id = 'qontinui-runner'
                     WHERE app_id IS NULL;
                 ALTER TABLE project.proposal_events ALTER COLUMN app_id SET NOT NULL;
                 CREATE INDEX IF NOT EXISTS idx_proposal_events_app_id
                     ON project.proposal_events(app_id);
                 CREATE INDEX IF NOT EXISTS idx_proposal_events_app_id_at_ms
                     ON project.proposal_events(app_id, at DESC);
               END IF;
             END $$;",
        )
        .await
        .map_err(|e| format!("Stream E.1 proposal_events.app_id self-heal failed: {}", e))?;

        // Approach-D Conductor/Engine Phase 1 — runner-owned `orchestration`
        // schema (durable run + subtask DAG ledger).
        //
        // Authored declaratively in `atlas/schema.hcl`; mirrored here as a
        // CREATE SCHEMA / CREATE TABLE IF NOT EXISTS self-heal (same idiom as
        // `project.apps` above) so a fresh PG without Atlas applied still boots
        // the conductor loop. This is a runner-owned namespace — NOT a coord.*
        // table — so there is no alembic migration and no `require_table` here;
        // the runner heals the shape itself.
        //
        // `subtasks.artifact` stores a serde-serialized `CompletionReport`
        // (see `database/pg/completion_reports.rs`); `produced_by` is the
        // elaborating parent task_id (null for DESIGN-origin rows) and is the
        // idempotent splice key used by progressive elaboration (Phase 4).
        conn.batch_execute(
            "CREATE SCHEMA IF NOT EXISTS orchestration; \
             CREATE TABLE IF NOT EXISTS orchestration.runs ( \
                 run_id     UUID PRIMARY KEY, \
                 goal       TEXT NOT NULL, \
                 recipe     TEXT, \
                 phases     TEXT[] NOT NULL DEFAULT '{}', \
                 status     TEXT NOT NULL, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT now() \
             ); \
             CREATE TABLE IF NOT EXISTS orchestration.subtasks ( \
                 task_id         TEXT NOT NULL, \
                 run_id          UUID NOT NULL REFERENCES orchestration.runs(run_id) ON DELETE CASCADE, \
                 idx             INTEGER NOT NULL, \
                 title           TEXT NOT NULL, \
                 brief           TEXT NOT NULL, \
                 phase           TEXT NOT NULL, \
                 repo            TEXT, \
                 depends_on      TEXT[] NOT NULL DEFAULT '{}', \
                 expected_output TEXT NOT NULL, \
                 emits_subtasks  BOOLEAN NOT NULL DEFAULT false, \
                 state           TEXT NOT NULL, \
                 task_run_id     UUID, \
                 artifact        JSONB, \
                 produced_by     TEXT, \
                 gate_id         TEXT, \
                 gate_status     TEXT, \
                 created_at      TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 PRIMARY KEY (run_id, task_id) \
             ); \
             ALTER TABLE orchestration.subtasks ADD COLUMN IF NOT EXISTS gate_id TEXT; \
             ALTER TABLE orchestration.subtasks ADD COLUMN IF NOT EXISTS gate_status TEXT; \
             CREATE INDEX IF NOT EXISTS idx_orchestration_subtasks_run \
                 ON orchestration.subtasks (run_id, idx); \
             CREATE INDEX IF NOT EXISTS idx_orchestration_subtasks_produced_by \
                 ON orchestration.subtasks (run_id, produced_by);",
        )
        .await
        .map_err(|e| format!("Phase 1 orchestration schema self-heal failed: {}", e))?;

        info!("PostgreSQL connected (deadpool, max_size=8, schema=runner)");

        Ok(())
    }

    /// D3 (self-healing degraded mode): when the runner booted degraded
    /// (`QONTINUI_ALLOW_NO_DB` + PG unreachable, so `pg_available()` is false),
    /// spawn a background task that retries `verify_and_provision` with
    /// exponential backoff (2s → 60s cap). On the first success it provisions
    /// the schema/extension that degraded boot skipped, flips
    /// `set_pg_available(true)` so `pg_guard` stops 503-ing DB-backed routes,
    /// and exits. The deadpool pool reconnects lazily, so a transient PG outage
    /// self-heals without a runner restart. No-op if PG is already available.
    pub fn spawn_reconnect_probe(self: Arc<Self>) {
        if pg_available() {
            return;
        }
        tokio::spawn(async move {
            let mut delay = std::time::Duration::from_secs(2);
            let max_delay = std::time::Duration::from_secs(60);
            loop {
                tokio::time::sleep(delay).await;
                // Another path may have already lifted the gate — stop probing.
                if pg_available() {
                    break;
                }
                match self.verify_and_provision().await {
                    Ok(()) => {
                        set_pg_available(true);
                        info!(
                            "PG reconnect probe: PostgreSQL is reachable and provisioned — \
                             exiting degraded mode, DB-backed routes resumed"
                        );
                        break;
                    }
                    Err(e) => {
                        debug!(
                            "PG reconnect probe: still unavailable ({}); retrying in {:?}",
                            e, delay
                        );
                        delay = (delay * 2).min(max_delay);
                    }
                }
            }
        });
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

    /// Assert that a PG table exists, hard-failing if it doesn't.
    ///
    /// alembic (qontinui-web) is the sole author of the `coord.*` schema the
    /// runner connects to. The runner no longer self-heals those tables; it
    /// requires them to be present and fails fast with an actionable message
    /// if the alembic migrations haven't been applied.
    pub async fn require_table(&self, schema: &str, table: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;
        let row = conn
            .query_one(
                "SELECT count(*) FROM information_schema.tables \
                 WHERE table_schema = $1 AND table_name = $2",
                &[&schema, &table],
            )
            .await
            .map_err(|e| format!("check {schema}.{table}: {e}"))?;
        let n: i64 = row.get(0);
        if n == 0 {
            return Err(format!(
                "missing PG table {schema}.{table} — run alembic migrations before starting"
            ));
        }
        Ok(())
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
