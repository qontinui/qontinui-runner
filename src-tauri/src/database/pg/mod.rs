//! PostgreSQL database layer for qontinui-runner.
//!
//! Runs alongside SQLite during migration. New tables and queries are added here
//! via Clorinde-generated code. Callers prefer PG when available, fall back to SQLite.

pub mod activity_timeline;
pub mod adaptive_learning;
pub mod agentic_metrics;
pub mod ai_sessions;
pub mod app_deploy_state;
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
pub mod session_pr_ops;
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
/// `PgDb::new`; `false` when the runner booted degraded (as of P4: the bundled
/// cluster failed to start or connect -- previously also `QONTINUI_ALLOW_NO_DB`)
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

/// The PostgreSQL condition name for a SQLSTATE, or `None` when the code is one
/// this table does not know.
///
/// The five characters of a SQLSTATE are searchable but not READABLE: an
/// operator scanning the Coordinator panel sees `[42703]` and has to go look it
/// up before knowing whether the runner hit a schema drift, a constraint
/// violation, or a dead connection — which is the same "go read something else"
/// step [`pg_err`] exists to remove.
///
/// Every string here is PostgreSQL's OWN condition name from its errcodes table
/// (Appendix A), never a phrase invented for this codebase, so the gloss stays
/// greppable against the upstream docs. An unknown code gets NO gloss rather
/// than a guessed one — the raw code is still printed, and inventing a
/// plausible-but-wrong name would be worse than the lookup it saves.
///
/// Exact codes first, then a CLASS fallback on the two-character prefix (also
/// PostgreSQL's own class names), so a code outside the exact list still says
/// what KIND of failure it was.
fn sqlstate_gloss(code: &str) -> Option<&'static str> {
    let exact = match code {
        "42P01" => Some("undefined_table"),
        "42703" => Some("undefined_column"),
        "42883" => Some("undefined_function"),
        "42P07" => Some("duplicate_table"),
        "42701" => Some("duplicate_column"),
        "42601" => Some("syntax_error"),
        "42501" => Some("insufficient_privilege"),
        "23502" => Some("not_null_violation"),
        "23503" => Some("foreign_key_violation"),
        "23505" => Some("unique_violation"),
        "23514" => Some("check_violation"),
        "22P02" => Some("invalid_text_representation"),
        "3D000" => Some("invalid_catalog_name"),
        "28P01" => Some("invalid_password"),
        "40001" => Some("serialization_failure"),
        "40P01" => Some("deadlock_detected"),
        "53300" => Some("too_many_connections"),
        "57014" => Some("query_canceled"),
        "57P03" => Some("cannot_connect_now"),
        _ => None,
    };
    exact.or_else(|| match code.get(..2) {
        Some("08") => Some("connection_exception"),
        Some("22") => Some("data_exception"),
        Some("23") => Some("integrity_constraint_violation"),
        Some("40") => Some("transaction_rollback"),
        Some("42") => Some("syntax_error_or_access_rule_violation"),
        Some("53") => Some("insufficient_resources"),
        Some("54") => Some("program_limit_exceeded"),
        Some("57") => Some("operator_intervention"),
        Some("58") => Some("system_error"),
        Some("XX") => Some("internal_error"),
        _ => None,
    })
}

/// Render a `tokio_postgres::Error` as something an operator can act on.
///
/// THE DEFECT: `tokio_postgres::Error`'s `Display` prints the bare string
/// `db error` for every server-side failure — it ignores the alternate `{:#}`
/// flag too, so `format!("{e}")` and `format!("{e:#}")` are equally useless.
/// The SQLSTATE, the message, the offending relation and the server's own hint
/// all live in the `DbError` cause, which `Display` never walks. So a missing
/// table, a type mismatch and a permission denial all reached the Coordinator
/// panel as the identical two words, and the operator's only next move was to
/// go read the server log by hand.
///
/// Output shape: `<ctx>: [42P01] undefined_table: relation "coord.tasks" does
/// not exist (table=tasks) (hint: …)` — SQLSTATE first because it is the part
/// that searches cleanly, then its PostgreSQL condition name
/// ([`sqlstate_gloss`]) so the line is readable without a lookup. A client-side error (connection closed, TLS, encode) has
/// no `DbError`, so it falls back to `Display` plus the source chain, which for
/// those variants IS informative.
pub fn pg_err(ctx: &str, e: &tokio_postgres::Error) -> String {
    match e.as_db_error() {
        Some(db) => {
            let code = db.code().code();
            let mut out = match sqlstate_gloss(code) {
                Some(gloss) => format!("{}: [{}] {}: {}", ctx, code, gloss, db.message()),
                None => format!("{}: [{}] {}", ctx, code, db.message()),
            };
            if let Some(table) = db.table() {
                out.push_str(&format!(" (table={table})"));
            }
            if let Some(detail) = db.detail() {
                out.push_str(&format!(" (detail: {detail})"));
            }
            if let Some(hint) = db.hint() {
                out.push_str(&format!(" (hint: {hint})"));
            }
            out
        }
        None => {
            // No DbError cause ⇒ client-side (connect/TLS/encode/decode). Its
            // Display is real, and the source chain names the underlying io
            // error, which is the diagnosable part.
            let mut out = format!("{}: {}", ctx, e);
            let mut src = std::error::Error::source(e);
            while let Some(cause) = src {
                out.push_str(&format!(" <- {cause}"));
                src = cause.source();
            }
            out
        }
    }
}

/// Parse a workflow id string into the uuid `project.unified_workflows.id`
/// (and its inbound FK columns) actually require — see migration
/// `d7a3f1c8e024_realign_unified_workflows_to_model`. Runner-generated ids
/// are always `uuid::Uuid::new_v4().to_string()`, so this only fails on a
/// malformed caller-supplied id. Shared by `workflows.rs`, `graph_ops.rs`,
/// `triggers.rs`, and callers outside this module (e.g. `workflow::dag_sync`)
/// that hand-bind raw SQL against these columns; all parse/format at the
/// PG-layer boundary and keep `String`/`&str` everywhere else (the wire type
/// in `qontinui_types::workflow::UnifiedWorkflow`).
pub(crate) fn parse_workflow_id(id: &str) -> Result<uuid::Uuid, String> {
    uuid::Uuid::parse_str(id).map_err(|e| format!("invalid workflow id '{}': {}", id, e))
}

/// [`parse_workflow_id`] for the nullable-FK call sites (`generation_pipeline_events.workflow_id`,
/// `rule_influence_log.workflow_id`, `workflow_triggers.workflow_id`) — `None`
/// in, `None` out. `Some("")` is also treated as `None`: the old `text`
/// column silently accepted an empty-string workflow_id as "no workflow"
/// (any client that sends `""` rather than omitting the key — e.g. a
/// non-UI/MCP caller that doesn't replicate `TriggerEditor.tsx`'s own submit
/// validation — must keep getting that same "no workflow" result, not a
/// parse error).
pub(crate) fn parse_optional_workflow_id(id: Option<&str>) -> Result<Option<uuid::Uuid>, String> {
    id.filter(|s| !s.is_empty())
        .map(parse_workflow_id)
        .transpose()
}

/// The machine-local tables the runner re-homed out of `coord.*` into
/// `project.*` — plan
/// `2026-08-18-runner-embedded-pg-parity-and-coord-http-migration` (P3).
///
/// WHY THIS LIST EXISTS. The runner ships a private per-machine PostgreSQL
/// (`postgresql_embedded`). On an end-user machine that embedded cluster IS
/// the production database — and qontinui-web's alembic, the sole author of
/// the `coord.*` schema, never runs there. So every `coord.*` statement the
/// runner issued was either a hard error against a table that was never
/// provisioned, or a write into a private table no fleet member would ever
/// read. It is invisible on a dev box (which reaches an external cluster
/// whose `coord.*` schema alembic really does own) and load-bearing exactly
/// where users are. Tables the runner reads back as its OWN operational
/// state therefore belong in `project.*`, where the runner is allowed to be
/// their author, and are provisioned by [`MACHINE_LOCAL_TABLES_DDL`].
///
/// DELIBERATELY NOT IN THIS LIST: `coord.agent_worktrees`, the single
/// exclusion. coord genuinely authors those rows (`POST /agents/allocate`),
/// so it stays `coord.*`; a later phase moves the runner off it to HTTP.
pub const REHOMED_MACHINE_LOCAL_TABLES: [&str; 12] = [
    "plans",
    "tasks",
    "reviews",
    "worktrees",
    "runner_instances",
    "process_sessions",
    "process_session_output",
    "session_touched_files",
    "session_file_snapshots",
    "coordinator_leader",
    "coordinator_decisions",
    "coordinator_shadow_decisions",
];

/// Idempotent self-provision DDL for [`REHOMED_MACHINE_LOCAL_TABLES`].
///
/// Same `CREATE SCHEMA` / `CREATE TABLE IF NOT EXISTS` idiom as the
/// `project.apps`, `project.session_prs` and `orchestration.*` self-heals in
/// [`PgDb::verify_and_provision`] — and it is the load-bearing half of the
/// re-home: without it a fresh embedded cluster has none of these tables and
/// re-qualifying the SQL changes nothing.
///
/// Statement order is FK order: `plans` → `tasks` → `reviews`, and
/// `process_sessions` → `process_session_output`.
///
/// COLUMN PROVENANCE.
/// - `plans` — recreated from the `downgrade()` of alembic revision
///   `coord_p4_03_drop_plans`, whose `upgrade()` DROPPED `coord.plans` (and
///   `coord.tasks.plan_id`, and `coord.plan_status_history`) on the shared
///   cluster. The runner's plan/task subsystem still binds both, so this
///   table is not merely re-homed — it is the only place `plans` exists at
///   all for the runner, and provisioning it here repairs a second live
///   defect that was independent of the embedded-PG one.
///   `plan_status_history` is NOT recreated: nothing in the runner reads it.
/// - `tasks` — the live `coord.tasks` shape (`schema.pg.sql.generated`) PLUS
///   the `plan_id UUID` column, its FK and `idx_tasks_plan`, all of which the
///   same alembic revision dropped. `insert_task`, `list_tasks_for_plan`,
///   `list_active_tasks_for_plan`, `mark_ready_for_unblocked` and the
///   `WITH RECURSIVE depends_on` walk every one of them bind `plan_id`; the
///   FK is now a within-schema one (`project.plans`), which is what
///   `delete_plan`'s "tasks cascade-delete via FK" contract needs.
/// - the other ten — the live shapes in `schema.pg.sql.generated`, checked
///   column by column against everything the owning `database/pg/*.rs`
///   module SELECTs, INSERTs or binds.
///
/// ONE DELIBERATE OMISSION from the live `coord.*` shapes:
/// `coordinator_decisions.agent_session_id` (and its partial index). Its
/// only purpose was an FK to `coord.agent_sessions`, which stays coord-owned
/// and does not exist on an embedded cluster; no runner code binds it.
pub const MACHINE_LOCAL_TABLES_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS project;

CREATE TABLE IF NOT EXISTS project.plans (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    markdown_path   TEXT,
    version_hash    TEXT,
    status          TEXT NOT NULL DEFAULT 'draft',
    title           TEXT,
    summary         TEXT,
    slug            TEXT,
    content         TEXT,
    authored_by     TEXT,
    origin_path     TEXT,
    archive_path    TEXT,
    metadata        JSONB NOT NULL DEFAULT '{}'::jsonb,
    ingested_status TEXT,
    tenant_id       UUID,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_plans_slug
    ON project.plans (slug) WHERE slug IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_plans_updated_at
    ON project.plans (updated_at DESC);

CREATE TABLE IF NOT EXISTS project.tasks (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    plan_id                 UUID REFERENCES project.plans(id) ON DELETE CASCADE,
    plan_version_hash       TEXT,
    phase_name              TEXT,
    sequence_in_phase       INTEGER,
    description             TEXT,
    expected_file_claims    TEXT[] NOT NULL DEFAULT '{}'::text[],
    expected_dirs           TEXT[] NOT NULL DEFAULT '{}'::text[],
    depends_on              UUID[] NOT NULL DEFAULT '{}'::uuid[],
    status                  TEXT NOT NULL DEFAULT 'pending',
    assigned_session_id     TEXT,
    started_at              TIMESTAMPTZ,
    completed_at            TIMESTAMPTZ,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    notes                   TEXT,
    completion_report       JSONB,
    completion_source       TEXT,
    assignment_brief_extras JSONB,
    identity_hash           TEXT,
    origin                  TEXT,
    work_unit_id            UUID,
    CONSTRAINT tasks_done_requires_report CHECK (
        status <> 'done'
        OR (completion_report IS NOT NULL AND completion_source IS NOT NULL)
    )
);
CREATE INDEX IF NOT EXISTS idx_tasks_plan ON project.tasks (plan_id);
CREATE INDEX IF NOT EXISTS idx_tasks_status ON project.tasks (status);
CREATE INDEX IF NOT EXISTS idx_tasks_assigned_session
    ON project.tasks (assigned_session_id) WHERE assigned_session_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_tasks_completion_source
    ON project.tasks (completion_source) WHERE completion_source IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_tasks_completion_report_gin
    ON project.tasks USING gin (completion_report jsonb_path_ops)
    WHERE completion_report IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_tasks_work_unit
    ON project.tasks (work_unit_id) WHERE work_unit_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_tasks_emergent_per_session
    ON project.tasks (assigned_session_id) WHERE origin = 'session_emergent';
CREATE UNIQUE INDEX IF NOT EXISTS idx_tasks_plan_identity_hash
    ON project.tasks (plan_id, identity_hash) WHERE identity_hash IS NOT NULL;

CREATE TABLE IF NOT EXISTS project.reviews (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id             UUID NOT NULL REFERENCES project.tasks(id) ON DELETE CASCADE,
    reviewer_session_id TEXT NOT NULL,
    reviewed_session_id TEXT NOT NULL,
    verdict             TEXT NOT NULL,
    confidence          DOUBLE PRECISION NOT NULL,
    reasoning           TEXT NOT NULL,
    diff_summary        JSONB,
    test_results        JSONB,
    user_decision       TEXT,
    user_decided_at     TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_reviews_task ON project.reviews (task_id);
CREATE INDEX IF NOT EXISTS idx_reviews_verdict ON project.reviews (verdict);
CREATE INDEX IF NOT EXISTS idx_reviews_reviewed_session
    ON project.reviews (reviewed_session_id);
CREATE INDEX IF NOT EXISTS idx_reviews_pending_recommendations
    ON project.reviews (created_at DESC)
    WHERE verdict = 'approved'
      AND confidence >= 0.7
      AND confidence < 0.85
      AND user_decision IS NULL;

CREATE TABLE IF NOT EXISTS project.worktrees (
    id            TEXT PRIMARY KEY,
    worktree_path TEXT NOT NULL,
    branch_name   TEXT NOT NULL,
    source_branch TEXT NOT NULL,
    source_commit TEXT NOT NULL,
    repo_path     TEXT NOT NULL,
    task_run_id   TEXT,
    workflow_name TEXT,
    status        TEXT NOT NULL DEFAULT 'active',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_worktrees_status ON project.worktrees (status);
CREATE INDEX IF NOT EXISTS idx_worktrees_task_run ON project.worktrees (task_run_id);

CREATE TABLE IF NOT EXISTS project.runner_instances (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    port           INTEGER NOT NULL UNIQUE,
    hostname       TEXT NOT NULL DEFAULT 'localhost',
    is_primary     BOOLEAN NOT NULL DEFAULT false,
    pid            INTEGER,
    status         TEXT NOT NULL DEFAULT 'starting',
    last_heartbeat TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    running_tasks  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_ri_heartbeat ON project.runner_instances (last_heartbeat);
CREATE INDEX IF NOT EXISTS idx_ri_port ON project.runner_instances (port);
CREATE INDEX IF NOT EXISTS idx_ri_status ON project.runner_instances (status);

CREATE TABLE IF NOT EXISTS project.process_sessions (
    id                TEXT PRIMARY KEY,
    process_config_id TEXT NOT NULL,
    process_name      TEXT NOT NULL,
    started_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    stopped_at        TIMESTAMPTZ,
    exit_code         INTEGER,
    state             TEXT NOT NULL DEFAULT 'running',
    error_count       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_process_sessions_config_id
    ON project.process_sessions (process_config_id);
CREATE INDEX IF NOT EXISTS idx_process_sessions_started_at
    ON project.process_sessions (started_at);

CREATE TABLE IF NOT EXISTS project.process_session_output (
    id          BIGSERIAL PRIMARY KEY,
    session_id  TEXT NOT NULL
        REFERENCES project.process_sessions(id) ON DELETE CASCADE,
    "timestamp" TEXT NOT NULL,
    stream      TEXT NOT NULL,
    line        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_process_session_output_session
    ON project.process_session_output (session_id);

CREATE TABLE IF NOT EXISTS project.session_touched_files (
    task_run_id TEXT NOT NULL,
    file_path   TEXT NOT NULL,
    worktree_id TEXT,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (task_run_id, file_path)
);
CREATE INDEX IF NOT EXISTS idx_session_touched_files_task_run
    ON project.session_touched_files (task_run_id);
CREATE INDEX IF NOT EXISTS idx_session_touched_files_recorded_at
    ON project.session_touched_files (recorded_at);

CREATE TABLE IF NOT EXISTS project.session_file_snapshots (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id         TEXT NOT NULL,
    file_path          TEXT NOT NULL,
    snapshot_blob_path TEXT NOT NULL,
    blob_sha256        TEXT NOT NULL,
    captured_before    BOOLEAN NOT NULL,
    taken_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_sfs_session
    ON project.session_file_snapshots (session_id);
CREATE INDEX IF NOT EXISTS idx_sfs_session_file
    ON project.session_file_snapshots (session_id, file_path);

CREATE TABLE IF NOT EXISTS project.coordinator_leader (
    id           BOOLEAN PRIMARY KEY DEFAULT true,
    instance_id  TEXT NOT NULL,
    leased_until TIMESTAMPTZ NOT NULL,
    acquired_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    renewed_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT coordinator_leader_singleton CHECK (id = true)
);

CREATE TABLE IF NOT EXISTS project.coordinator_decisions (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id       TEXT NOT NULL,
    iteration        BIGINT NOT NULL,
    rule             TEXT NOT NULL,
    action           TEXT NOT NULL,
    target_id        TEXT,
    reasoning        TEXT NOT NULL,
    auto_acted       BOOLEAN NOT NULL,
    resolved         BOOLEAN NOT NULL DEFAULT false,
    resolution       TEXT,
    resolved_at      TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    observation_hash TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_cd_created
    ON project.coordinator_decisions (created_at DESC);
CREATE INDEX IF NOT EXISTS idx_cd_rule_action
    ON project.coordinator_decisions (rule, action);
CREATE INDEX IF NOT EXISTS idx_cd_session
    ON project.coordinator_decisions (session_id);
CREATE INDEX IF NOT EXISTS idx_cd_open_escalations
    ON project.coordinator_decisions (created_at DESC)
    WHERE resolved = false
      AND auto_acted = false
      AND action IN ('escalate', 'kill-session', 'force-promote-to-worktree');

CREATE TABLE IF NOT EXISTS project.coordinator_shadow_decisions (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    instance_id      TEXT NOT NULL,
    iteration        BIGINT NOT NULL,
    observation_hash TEXT NOT NULL,
    rule             TEXT NOT NULL,
    action           TEXT NOT NULL,
    target_id        TEXT,
    reasoning        TEXT NOT NULL,
    would_have_acted BOOLEAN NOT NULL,
    taken_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_csd_taken_at
    ON project.coordinator_shadow_decisions (taken_at DESC);
CREATE INDEX IF NOT EXISTS idx_csd_obs_hash
    ON project.coordinator_shadow_decisions (observation_hash);
CREATE INDEX IF NOT EXISTS idx_csd_instance
    ON project.coordinator_shadow_decisions (instance_id, taken_at DESC);
"#;

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
    /// does no I/O — it only records config (including the bounded
    /// `create`/`wait`/`recycle` timeouts and the `Tokio1` runtime; timers are
    /// registered lazily at `get()` time, never at build time) — so it is safe
    /// to call from a synchronous context outside a Tokio runtime.
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
        // BOUNDED TIMEOUTS (iter4 B-1 fix). Previously this builder set NO
        // timeouts, so every `pool.get().await` blocked FOREVER when a usable
        // connection could not be obtained (exhausted pool, or a checked-out
        // connection whose driver task stopped being polled). That turned
        // every PG-backed Tauri command / HTTP handler into an infinite
        // "Loading…" spinner with no error ever surfaced.
        //
        // How deadpool 0.12 actually gates timeouts (verified against the
        // vendored source, NOT the old comment's claim of a panic):
        // `PoolBuilder::build()` returns `Err(BuildError::NoRuntimeSpecified)`
        // — it does NOT panic — *only* when a timeout is configured AND no
        // `Runtime` was set. The timer itself is registered lazily at
        // `get()` time via the configured `Runtime`, on the caller's async
        // task. So the safe, panic-free recipe is: set the timeouts AND
        // declare `.runtime(Runtime::Tokio1)`. `Runtime::Tokio1` is a plain
        // enum variant — constructing it needs no active runtime — so this is
        // equally safe on the synchronous `new_degraded` bootstrap path
        // (`main.rs` `rt.block_on`) where `build_pool` runs before the first
        // `.await`. Every actual `get()` is awaited inside a Tokio runtime, so
        // the timer registration always has a runtime available.
        //
        // 5s bounds the three phases: waiting for a free slot (`wait`),
        // establishing a new connection (`create`), and the recycle/health
        // check on checkout (`recycle`). On expiry `get()` returns an `Err`
        // that callers map to `Err("PG pool error: …")` → the UI shows an
        // error state instead of spinning.
        let timeout = Some(std::time::Duration::from_secs(5));
        deadpool_postgres::Pool::builder(mgr)
            .max_size(8)
            .runtime(deadpool_postgres::Runtime::Tokio1)
            .create_timeout(timeout)
            .wait_timeout(timeout)
            .recycle_timeout(timeout)
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
                 yellow_threshold  DOUBLE PRECISION DEFAULT 0.8 CHECK (yellow_threshold >= 0.0 AND yellow_threshold <= 1.0), \
                 update_strategy   TEXT NOT NULL DEFAULT 'pull_only', \
                 build_command     TEXT, \
                 start_command     TEXT \
             ); \
             ALTER TABLE project.apps ADD COLUMN IF NOT EXISTS auth_required    BOOLEAN NOT NULL DEFAULT false; \
             ALTER TABLE project.apps ADD COLUMN IF NOT EXISTS red_threshold    DOUBLE PRECISION NOT NULL DEFAULT 0.5; \
             ALTER TABLE project.apps ADD COLUMN IF NOT EXISTS yellow_threshold DOUBLE PRECISION NOT NULL DEFAULT 0.8; \
             ALTER TABLE project.apps ADD COLUMN IF NOT EXISTS update_strategy  TEXT NOT NULL DEFAULT 'pull_only'; \
             ALTER TABLE project.apps ADD COLUMN IF NOT EXISTS build_command    TEXT; \
             ALTER TABLE project.apps ADD COLUMN IF NOT EXISTS start_command    TEXT; \
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
        .map_err(|e| {
            format!(
                "B v1 Polish project.apps auth/threshold backfill failed: {}",
                e
            )
        })?;

        // fleet-fresh P3: project.app_deploy_state — per-(device, app) deployment
        // outcome written by the auto-fresh engine (fleet.rs), read by the P4
        // dispatcher on the web side. Same self-heal posture as project.apps
        // above (project.* is runner-authored; the coord.* alembic-sole-authority
        // rule does not apply here).
        //
        // MIRROR DDL: the web alembic revision `app_deploy_state_tracking`
        // carries an equivalent CREATE for migrator-first fresh DBs — keep
        // the two in lockstep (columns, CHECK, indexes) or whichever side
        // creates the table first strands the other's expectations
        // (review blocker 2 of the fleet-fresh pre-PR pass).
        conn.batch_execute(
            "CREATE TABLE IF NOT EXISTS project.app_deploy_state ( \
                 device_id    UUID NOT NULL, \
                 app_id       TEXT NOT NULL, \
                 deployed_sha TEXT, \
                 freshness    TEXT NOT NULL DEFAULT 'failed', \
                 deployed_at  TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 last_error   TEXT, \
                 updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 PRIMARY KEY (device_id, app_id) \
             ); \
             ALTER TABLE project.app_deploy_state \
                 ADD COLUMN IF NOT EXISTS deployed_at TIMESTAMPTZ NOT NULL DEFAULT now(); \
             ALTER TABLE project.app_deploy_state \
                 DROP CONSTRAINT IF EXISTS app_deploy_state_freshness_check; \
             ALTER TABLE project.app_deploy_state \
                 ADD CONSTRAINT app_deploy_state_freshness_check \
                 CHECK (freshness IN ('fresh', 'building', 'failed')); \
             CREATE INDEX IF NOT EXISTS ix_app_deploy_state_app_id \
                 ON project.app_deploy_state (app_id); \
             CREATE INDEX IF NOT EXISTS ix_app_deploy_state_fresh_hosts \
                 ON project.app_deploy_state (device_id) WHERE freshness = 'fresh';",
        )
        .await
        .map_err(|e| format!("fleet-fresh app_deploy_state self-heal failed: {}", e))?;

        // runner-centric session-PR dropdown: project.session_prs — the
        // per-session PR-status projection the Terminal zone-header dropdown
        // reads (`commands::session_info::session_info_get` →
        // `session_pr_ops::list_session_prs`). Written by the runner-local
        // attribution reconciler (`session_pr_reconciler`), which resolves
        // "which PRs did session S open" from the `Session-Id: <id>` git
        // trailer every commit carries, then hydrates open/merged status from
        // the GitHub API.
        //
        // DELIBERATE POSTURE — a SINGLE-CONSUMER, RUNNER-LOCAL PROJECTION.
        // Unlike `project.app_deploy_state` above, this table is BOTH written
        // AND read only by the local runner: there is NO cross-component
        // reader (no web/coord side) to keep in lockstep. So — intentionally —
        // it has NO companion web alembic migration and is intentionally
        // absent from `atlas/schema.hcl` / `schema.pg.sql.generated`; this
        // CREATE TABLE IF NOT EXISTS self-heal is its sole authority. Schema
        // `project` (NOT `runner.` — that namespace is CI-retired via
        // `forbid-runner-schema.yml`).
        //
        // LAND-SIGNAL CASCADE columns (`land_signal`, `land_reason`,
        // `landed_at`) are added by the `ADD COLUMN IF NOT EXISTS` block
        // below, matching the `project.apps` self-heal idiom above. They exist
        // because "landed" is NOT GitHub's `merged` boolean on this fleet:
        // coord fast-forward lands are the majority of landings and leave
        // `merged=false` / `state=closed`. Same single-consumer posture — no
        // companion web alembic revision for these columns either.
        conn.batch_execute(
            "CREATE TABLE IF NOT EXISTS project.session_prs ( \
                 claude_session_id UUID        NOT NULL, \
                 repo              TEXT        NOT NULL, \
                 pr_number         BIGINT      NOT NULL, \
                 head_branch       TEXT, \
                 pr_state          TEXT, \
                 merged            BOOLEAN     NOT NULL DEFAULT false, \
                 merged_at         TIMESTAMPTZ, \
                 land_signal       TEXT, \
                 land_reason       TEXT, \
                 landed_at         TIMESTAMPTZ, \
                 created_at        TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 PRIMARY KEY (claude_session_id, repo, pr_number) \
             ); \
             ALTER TABLE project.session_prs ADD COLUMN IF NOT EXISTS land_signal TEXT; \
             ALTER TABLE project.session_prs ADD COLUMN IF NOT EXISTS land_reason TEXT; \
             ALTER TABLE project.session_prs ADD COLUMN IF NOT EXISTS landed_at   TIMESTAMPTZ; \
             CREATE INDEX IF NOT EXISTS ix_session_prs_session \
                 ON project.session_prs (claude_session_id);",
        )
        .await
        .map_err(|e| format!("session_prs projection self-heal failed: {}", e))?;

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

        // P3 of plan
        // `2026-08-18-runner-embedded-pg-parity-and-coord-http-migration`:
        // provision the 12 machine-local tables re-homed out of `coord.*`
        // into `project.*`. See [`REHOMED_MACHINE_LOCAL_TABLES`] for the list
        // and [`MACHINE_LOCAL_TABLES_DDL`] for the per-column provenance.
        //
        // This is the load-bearing half of the re-home: the runner ships a
        // private per-machine PostgreSQL, alembic (the sole author of
        // `coord.*`) never runs on a user's box, so without this DDL a fresh
        // embedded cluster has none of these tables and re-qualifying the SQL
        // would change nothing.
        conn.batch_execute(MACHINE_LOCAL_TABLES_DDL)
            .await
            .map_err(|e| format!("P3 machine-local project.* self-heal failed: {}", e))?;

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
               VALUES ($1, $2, $3::text::timestamptz, $4, $5, $6)"#,
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

#[cfg(test)]
mod machine_local_schema_tests {
    use super::{MACHINE_LOCAL_TABLES_DDL, REHOMED_MACHINE_LOCAL_TABLES};

    /// Every `database/pg` module whose SQL was re-homed by P3, paired with
    /// its source text. `include_str!` pins the *shipped* SQL, so this is a
    /// real regression guard and not a restatement of the constant below.
    const REHOMED_MODULE_SOURCES: [(&str, &str); 12] = [
        ("plans.rs", include_str!("plans.rs")),
        ("tasks.rs", include_str!("tasks.rs")),
        ("reviews.rs", include_str!("reviews.rs")),
        ("worktrees.rs", include_str!("worktrees.rs")),
        ("instances.rs", include_str!("instances.rs")),
        ("process_sessions.rs", include_str!("process_sessions.rs")),
        (
            "session_touched_files.rs",
            include_str!("session_touched_files.rs"),
        ),
        (
            "session_file_snapshots.rs",
            include_str!("session_file_snapshots.rs"),
        ),
        (
            "coordinator_leader.rs",
            include_str!("coordinator_leader.rs"),
        ),
        (
            "coordinator_decisions.rs",
            include_str!("coordinator_decisions.rs"),
        ),
        (
            "coordinator_shadow_decisions.rs",
            include_str!("coordinator_shadow_decisions.rs"),
        ),
        (
            "completion_reports.rs",
            include_str!("completion_reports.rs"),
        ),
    ];

    /// THE FRESH-CLUSTER CHECK. This is the test that would have caught the
    /// original defect: the runner issued SQL against `coord.*` tables that
    /// alembic — which never runs on an end-user box — was the sole author
    /// of, so on a fresh embedded cluster they simply did not exist. Assert
    /// the self-heal DDL provisions every re-homed table by name.
    #[test]
    fn self_heal_ddl_creates_every_rehomed_table() {
        for table in REHOMED_MACHINE_LOCAL_TABLES {
            let needle = format!("CREATE TABLE IF NOT EXISTS project.{} (", table);
            assert!(
                MACHINE_LOCAL_TABLES_DDL.contains(&needle),
                "self-heal DDL is missing `{}` — a fresh embedded cluster \
                 would have no such table and every statement against it \
                 would fail at runtime on a user's machine",
                needle
            );
        }
    }

    /// The DDL must not create anything the list does not declare, and vice
    /// versa — otherwise the list stops being a usable inventory.
    #[test]
    fn self_heal_ddl_creates_exactly_the_listed_tables() {
        let created = MACHINE_LOCAL_TABLES_DDL
            .matches("CREATE TABLE IF NOT EXISTS project.")
            .count();
        assert_eq!(
            created,
            REHOMED_MACHINE_LOCAL_TABLES.len(),
            "DDL creates {} tables but REHOMED_MACHINE_LOCAL_TABLES lists {}",
            created,
            REHOMED_MACHINE_LOCAL_TABLES.len()
        );
    }

    /// The schema itself has to exist before any of the tables can. On a
    /// fresh embedded cluster nothing else will have created it.
    #[test]
    fn self_heal_ddl_creates_the_project_schema_first() {
        let schema_at = MACHINE_LOCAL_TABLES_DDL
            .find("CREATE SCHEMA IF NOT EXISTS project;")
            .expect("DDL must create the project schema");
        let first_table_at = MACHINE_LOCAL_TABLES_DDL
            .find("CREATE TABLE IF NOT EXISTS project.")
            .expect("DDL must create at least one table");
        assert!(
            schema_at < first_table_at,
            "CREATE SCHEMA must precede the first CREATE TABLE"
        );
    }

    /// `verify_and_provision` re-runs on every boot and on every degraded-mode
    /// reconnect, so every statement has to be a no-op the second time.
    #[test]
    fn self_heal_ddl_is_idempotent() {
        for (n, line) in MACHINE_LOCAL_TABLES_DDL.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("CREATE TABLE")
                || t.starts_with("CREATE INDEX")
                || t.starts_with("CREATE UNIQUE INDEX")
                || t.starts_with("CREATE SCHEMA")
            {
                assert!(
                    t.contains("IF NOT EXISTS"),
                    "line {} is not idempotent: {}",
                    n + 1,
                    t
                );
            }
        }
    }

    /// FK targets must be created before the tables that reference them —
    /// `batch_execute` runs the statements in order in one round trip.
    #[test]
    fn self_heal_ddl_orders_tables_after_their_fk_targets() {
        let at = |t: &str| {
            MACHINE_LOCAL_TABLES_DDL
                .find(&format!("CREATE TABLE IF NOT EXISTS project.{} (", t))
                .unwrap_or_else(|| panic!("missing CREATE TABLE for {}", t))
        };
        assert!(at("plans") < at("tasks"), "tasks.plan_id references plans");
        assert!(
            at("tasks") < at("reviews"),
            "reviews.task_id references tasks"
        );
        assert!(
            at("process_sessions") < at("process_session_output"),
            "process_session_output.session_id references process_sessions"
        );
    }

    /// The whole point of the re-home is that the runner's own DDL never
    /// depends on the alembic-owned `coord.*` namespace.
    #[test]
    fn self_heal_ddl_references_no_coord_object() {
        assert!(
            !MACHINE_LOCAL_TABLES_DDL.contains("coord."),
            "self-heal DDL must not reference the alembic-owned coord schema"
        );
    }

    /// `coord.agent_worktrees` is the ONE table deliberately left behind:
    /// coord authors those rows via `POST /agents/allocate`, and a later
    /// phase moves the runner off it to HTTP.
    #[test]
    fn agent_worktrees_is_deliberately_not_rehomed() {
        assert!(
            !REHOMED_MACHINE_LOCAL_TABLES.contains(&"agent_worktrees"),
            "agent_worktrees is coord-authored and must stay in coord.*"
        );
        assert!(!MACHINE_LOCAL_TABLES_DDL.contains("project.agent_worktrees"));
    }

    /// Schema-qualification pin: no re-homed module may issue SQL against a
    /// re-homed table under the `coord.` prefix again. Comment lines are
    /// skipped — `tasks.rs` legitimately narrates the alembic drop of the
    /// old `coord.plans` in prose.
    #[test]
    fn rehomed_modules_issue_no_coord_qualified_sql() {
        for (name, src) in REHOMED_MODULE_SOURCES {
            for (n, line) in src.lines().enumerate() {
                let t = line.trim_start();
                if t.starts_with("//") {
                    continue;
                }
                for table in REHOMED_MACHINE_LOCAL_TABLES {
                    let needle = format!("coord.{}", table);
                    assert!(
                        !t.contains(&needle),
                        "{}:{} still issues SQL against `{}` — that table is \
                         absent from a fresh embedded cluster; it must be \
                         `project.{}`",
                        name,
                        n + 1,
                        needle,
                        table
                    );
                }
            }
        }
    }

    /// The re-homed modules must actually be talking to `project.*` now —
    /// a module that lost its `coord.` prefix but gained no `project.` one
    /// would pass the negative test above while issuing unqualified SQL that
    /// silently resolves through `search_path`.
    #[test]
    fn rehomed_modules_are_project_qualified() {
        for (name, src) in REHOMED_MODULE_SOURCES {
            assert!(
                src.contains("project."),
                "{} issues no project-qualified SQL after the re-home",
                name
            );
        }
    }
}

#[cfg(test)]
mod sqlstate_gloss_tests {
    use super::sqlstate_gloss;

    /// The gloss must not displace the bracketed code — `[42703]` is what an
    /// operator greps for, and the Coordinator panel is checked for exactly
    /// that literal.
    #[test]
    fn exact_codes_map_to_postgres_condition_names() {
        assert_eq!(sqlstate_gloss("42703"), Some("undefined_column"));
        assert_eq!(sqlstate_gloss("42P01"), Some("undefined_table"));
        assert_eq!(sqlstate_gloss("23505"), Some("unique_violation"));
        assert_eq!(sqlstate_gloss("40P01"), Some("deadlock_detected"));
    }

    /// A code outside the exact table still says what KIND of failure it was,
    /// using PostgreSQL's own class name.
    #[test]
    fn unlisted_codes_fall_back_to_their_class() {
        assert_eq!(
            sqlstate_gloss("42P18"),
            Some("syntax_error_or_access_rule_violation")
        );
        assert_eq!(sqlstate_gloss("08003"), Some("connection_exception"));
    }

    /// An unrecognized code gets NO gloss. The raw code is still printed; a
    /// guessed name would be worse than the lookup it saves.
    #[test]
    fn unknown_codes_get_no_invented_gloss() {
        assert_eq!(sqlstate_gloss("99999"), None);
        assert_eq!(sqlstate_gloss(""), None);
        assert_eq!(sqlstate_gloss("4"), None);
    }
}
