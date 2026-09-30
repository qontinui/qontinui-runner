//! Capture of journey edges at the runner's UI action choke points.
//!
//! Plan `2026-09-20-ui-bridge-represents-the-users-path-and-the-passage-of-time`
//! D3: every agent UI action already passes through the runner, so the edge
//! ledger is fed at the action handlers — all of them, or edges silently go
//! missing for whichever transport a skill happens to use. Modelled on
//! `state_discovery::capture::enqueue_observation`: fire-and-forget, a
//! once-per-process schema probe, and never an error to the caller.
//!
//! ## Shape
//!
//! Handlers call one of [`record_action`], [`record_snapshot`] or
//! [`record_diff`] AFTER their response is assembled. Each is a synchronous,
//! non-blocking `try_send` of a [`JourneyEvent`] onto one bounded channel;
//! ONE worker task (spawned on first use) owns all journey state and does all
//! node resolution and every database write. That single consumer is what
//! keeps an action and the snapshot that closes it in order — two independent
//! `tokio::spawn`s could run in either order and close an edge with the wrong
//! destination.
//!
//! - **Every action opens a PENDING edge** (see [`super::cursor`]) whose
//!   `from_node` is the last node this runner resolved for the cursor; the
//!   next control/SDK snapshot of that cursor closes it. Execute-with-diff is
//!   no exception: its `SemanticSnapshot`s are not a shape the spec evaluator
//!   reads, so they would be a SECOND node-identity source. A diff contributes
//!   only an outcome hint (`error` / `settle_timeout`).
//! - A second action arriving first, or a pending edge older than
//!   [`super::cursor::PENDING_TTL`], closes as `to_node_unobserved` (D3).
//!
//! Node resolution (Phase 0 decision 1) never fetches a snapshot and never
//! runs on a request path: it evaluates the ONE page spec matched to the
//! snapshot in hand, on the worker.
//!
//! ## Degrade loudly
//!
//! Unlike the co-occurrence precedent (which only `warn!`s), every outcome —
//! schema absent, a validation failure, a refused INSERT, a full queue — is
//! counted into [`super::health`], which `GET /apps/{app_id}/journey/health`
//! reports. A ledger that stopped growing must never read as a finished one.

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use qontinui_types::journey::{EdgeOutcome, JourneyEdgeObservation, JourneyNode, RunKind};
use tokio::sync::mpsc;
use tracing::warn;

use crate::database::pg::PgDb;

use super::cursor::{
    failure_hint, ActionSpec, CursorKey, Cursors, EdgeDraft, Observed, Provenance,
};
use super::frontier;
use super::health;
use super::node::{
    affordance_digest, build_node, extract_affordances, page_identity, present_state_ids,
    AffordanceIndex, SpecLookup,
};

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

/// This binary's build id — the value `/health` reports as `buildId`.
pub(crate) const RUNNER_BUILD_ID: &str = env!("RUNNER_BUILD_ID");

/// The runner instance label, with the same meaning as
/// `co_occurrence_observations.runner_instance`: `QONTINUI_RUNNER_ROLE`, else
/// `primary` — byte-for-byte the expression the snapshot handler always used,
/// now shared so the two ledgers cannot label one runner differently.
pub(crate) fn runner_instance() -> String {
    std::env::var("QONTINUI_RUNNER_ROLE")
        .ok()
        .unwrap_or_else(|| "primary".to_string())
}

/// Did a control-route action act on the UI, and did it fail?
///
/// - `None` — the runner refused the request itself (a 4xx: malformed body,
///   unknown element, unknown action) — no UI action occurred, so no edge;
/// - `Some(true)` — the action was attempted and failed (a 5xx, or a 200 whose
///   envelope says `success: false`) — an `error` edge;
/// - `Some(false)` — the action succeeded.
pub(crate) fn control_action_verdict<T: serde::Serialize>(
    result: &Result<
        axum::Json<crate::mcp::types::ApiResponse<T>>,
        (
            axum::http::StatusCode,
            axum::Json<crate::mcp::types::ApiResponse<()>>,
        ),
    >,
) -> Option<bool> {
    match result {
        Ok(body) => Some(!body.success),
        Err((status, _)) if status.is_client_error() => None,
        Err(_) => Some(true),
    }
}

/// `task_run_id` (the run the action routes already attribute events to) as
/// the ledger's `run_id`.
pub(crate) fn run_id_from_task_run(task_run_id: Option<i64>) -> Option<String> {
    task_run_id.map(|id| id.to_string())
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// Turn a draft into the contract row: a fresh id, the close time, and this
/// runner's own provenance.
pub(crate) fn finalize(draft: EdgeDraft) -> JourneyEdgeObservation {
    JourneyEdgeObservation {
        id: uuid::Uuid::new_v4().to_string(),
        observed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        app_id: draft.key.app_id,
        app_version: draft.provenance.app_version,
        runner_build_id: RUNNER_BUILD_ID.to_string(),
        runner_instance: draft.key.runner_instance,
        run_id: draft.provenance.run_id,
        run_kind: RunKind::AgentAction,
        from_node: draft.from_node,
        to_node: draft.to_node,
        trigger: draft.trigger,
        outcome: draft.outcome,
        invalidated_at: None,
        invalidated_reason: None,
        invalidated_by: None,
        invalidation_token: None,
    }
}

/// The edge INSERT and, in the same statement, the frontier DELETE it
/// implies: once an affordance has been the trigger of an edge from a node it
/// is no longer frontier there. One statement, so the two cannot disagree.
///
/// Casts: `$1::text::uuid` and `$2::text::timestamptz` so the row carries
/// exactly the id / close time the validated struct carries; the three JSONB
/// columns bind `serde_json::Value`s. `$10` (`to_node`) binds
/// `Option<serde_json::Value>`, and `None` is SQL NULL — NEVER a JSON `null`,
/// which the migration's `jsonb_typeof` CHECK rejects and which would satisfy
/// `NOT NULL`-style reasoning while naming nothing (see [`EdgeInsert`]).
///
/// The DELETE matches nothing when `$14` (the trigger's fingerprint) is NULL.
pub(crate) const EDGE_WRITE_SQL: &str = r#"WITH inserted AS (
    INSERT INTO project.journey_edge_observations
        (id, observed_at, app_id, app_version, runner_build_id, runner_instance,
         run_id, run_kind, from_node, to_node, trigger, outcome)
    VALUES ($1::text::uuid, $2::text::timestamptz, $3, $4, $5, $6,
            $7, $8, $9::jsonb, $10::jsonb, $11::jsonb, $12)
    RETURNING id
), cleared AS (
    DELETE FROM project.journey_frontier f
     WHERE f.app_id = $3
       AND f.node_key = $13
       AND f.affordance_fingerprint = $14
       AND EXISTS (SELECT 1 FROM inserted)
    RETURNING 1
)
SELECT (SELECT count(*) FROM inserted) AS inserted,
       (SELECT count(*) FROM cleared) AS cleared"#;

/// Number of parameters [`EDGE_WRITE_SQL`] binds; sizes the call site's
/// params array so SQL/param arity drift is a compile error plus a test.
pub(crate) const EDGE_WRITE_BINDS: usize = 14;

/// The bind values of [`EDGE_WRITE_SQL`], built by a pure function so the
/// null-handling of `to_node` is testable without a database.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EdgeInsert {
    pub id: String,
    pub observed_at: String,
    pub app_id: String,
    pub app_version: Option<String>,
    pub runner_build_id: String,
    pub runner_instance: String,
    pub run_id: Option<String>,
    pub run_kind: String,
    pub from_node: serde_json::Value,
    /// `None` binds SQL NULL. Never `Some(Value::Null)` — see [`edge_insert`].
    pub to_node: Option<serde_json::Value>,
    pub trigger: serde_json::Value,
    pub outcome: String,
    pub from_node_key: String,
    pub target_fingerprint: Option<String>,
}

/// Validate a row against the contract and produce its binds.
///
/// Validation runs here, before every write: a failure is returned as the
/// contract error's text and counted as a write failure by the caller —
/// never a silent drop.
pub(crate) fn edge_insert(obs: &JourneyEdgeObservation) -> Result<EdgeInsert, String> {
    obs.validate()
        .map_err(|e| format!("contract validation: {e}"))?;
    let to_json = |n: &JourneyNode| serde_json::to_value(n).map_err(|e| e.to_string());
    let to_node = match &obs.to_node {
        // SQL NULL. `serde_json::to_value(&None::<JourneyNode>)` would be a
        // JSON `null`, which is exactly what must never reach the column.
        None => None,
        Some(n) => Some(to_json(n)?),
    };
    Ok(EdgeInsert {
        id: obs.id.clone(),
        observed_at: obs.observed_at.clone(),
        app_id: obs.app_id.clone(),
        app_version: obs.app_version.clone(),
        runner_build_id: obs.runner_build_id.clone(),
        runner_instance: obs.runner_instance.clone(),
        run_id: obs.run_id.clone(),
        run_kind: obs.run_kind.as_str().to_string(),
        from_node: to_json(&obs.from_node)?,
        to_node,
        trigger: serde_json::to_value(&obs.trigger).map_err(|e| e.to_string())?,
        outcome: obs.outcome.as_str().to_string(),
        from_node_key: obs.from_node.key(),
        target_fingerprint: obs.trigger.target_fingerprint.clone(),
    })
}

// ---------------------------------------------------------------------------
// Schema probe
// ---------------------------------------------------------------------------

/// Do both journey tables exist? Schema-qualified `to_regclass`, so the answer
/// does not depend on the pool's `search_path`; `pg_catalog`-backed, so a
/// grant quirk cannot read as "absent" (same reasoning as
/// `state_discovery::capture::OBSERVATION_SCHEMA_PROBE_SQL`).
pub(crate) const JOURNEY_SCHEMA_PROBE_SQL: &str = r#"SELECT to_regclass('project.journey_edge_observations') IS NOT NULL,
                      to_regclass('project.journey_frontier') IS NOT NULL"#;

/// "Exists" is not "readable": read at most one row from each table so a
/// privilege failure surfaces as a probe error instead of every write failing.
pub(crate) const JOURNEY_READ_PROBE_SQL: &str = r#"SELECT (SELECT e.id FROM project.journey_edge_observations e LIMIT 1),
                      (SELECT f.node_key FROM project.journey_frontier f LIMIT 1)"#;

/// Bound on the once-per-process probe — the same 5 s the pool uses for
/// itself, for the same stall reason as the co-occurrence probe.
pub(crate) const JOURNEY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The answer to "can the journey ledger be written here?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SchemaProbe {
    /// Both tables exist and are readable.
    Present,
    /// At least one table does not exist. The names are the missing ones.
    Absent { missing: Vec<&'static str> },
    /// The probe itself failed (error or timeout). Writes are not attempted;
    /// health reports it as a failure, never as "absent" and never as
    /// "writing". NOT cached: the next call after [`PROBE_RETRY_BACKOFF`]
    /// probes again, so a transient database fault heals by itself.
    Failed { error: String },
}

/// How long a failed probe is reused before the next call probes again.
pub(crate) const PROBE_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_secs(60);

/// What the process knows about the schema.
///
/// `Settled` holds only `Present` / `Absent` — the schema's shape, which does
/// not change under a running process without a migration. A probe FAILURE is
/// a fact about the database's health at one moment, so it is held only with
/// its time and expires after [`PROBE_RETRY_BACKOFF`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProbeCache {
    Unprobed,
    Settled(SchemaProbe),
    FailedAt {
        error: String,
        at: std::time::Instant,
    },
}

/// The answer the cache can give without probing, if any. Pure.
pub(crate) fn cached_answer(cache: &ProbeCache, now: std::time::Instant) -> Option<SchemaProbe> {
    match cache {
        ProbeCache::Unprobed => None,
        ProbeCache::Settled(answer) => Some(answer.clone()),
        ProbeCache::FailedAt { error, at } => (now.saturating_duration_since(*at)
            < PROBE_RETRY_BACKOFF)
            .then(|| SchemaProbe::Failed {
                error: error.clone(),
            }),
    }
}

/// What to remember after a probe answered `answer` at `now`. Pure.
pub(crate) fn cache_after(answer: &SchemaProbe, now: std::time::Instant) -> ProbeCache {
    match answer {
        SchemaProbe::Failed { error } => ProbeCache::FailedAt {
            error: error.clone(),
            at: now,
        },
        settled => ProbeCache::Settled(settled.clone()),
    }
}

/// Process-wide probe cache. Keyed on nothing, like
/// `OBSERVATION_APP_ID_SUPPORTED`: the runner constructs exactly one `PgDb`.
static JOURNEY_SCHEMA: std::sync::Mutex<ProbeCache> = std::sync::Mutex::new(ProbeCache::Unprobed);

/// Serializes probing, so concurrent callers wait for one probe instead of
/// each running their own.
static PROBE_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn probe_cache() -> std::sync::MutexGuard<'static, ProbeCache> {
    JOURNEY_SCHEMA.lock().unwrap_or_else(|p| p.into_inner())
}

/// The probe's most recent answer, without probing. `None` until a probe has
/// answered; a failure older than the backoff still reads as `Failed` here
/// (health reports it until a probe replaces it).
pub(crate) fn journey_schema_state() -> Option<SchemaProbe> {
    match &*probe_cache() {
        ProbeCache::Unprobed => None,
        ProbeCache::Settled(answer) => Some(answer.clone()),
        ProbeCache::FailedAt { error, .. } => Some(SchemaProbe::Failed {
            error: error.clone(),
        }),
    }
}

/// Resolve whether the journey tables are present: `Present` / `Absent` are
/// probed once per process; a `Failed` probe is retried after
/// [`PROBE_RETRY_BACKOFF`]. Warns on every probe that does not find the
/// tables — at most once per backoff window, since answers are cached.
pub(crate) async fn journey_schema_supported(client: &tokio_postgres::Client) -> SchemaProbe {
    // Each guard is a temporary of its own `let`, dropped before any await.
    let cached = cached_answer(&probe_cache(), std::time::Instant::now());
    if let Some(answer) = cached {
        return answer;
    }
    let _gate = PROBE_GATE.lock().await;
    let cached = cached_answer(&probe_cache(), std::time::Instant::now());
    if let Some(answer) = cached {
        return answer;
    }
    let probed = tokio::time::timeout(JOURNEY_PROBE_TIMEOUT, async {
        let row = client.query_one(JOURNEY_SCHEMA_PROBE_SQL, &[]).await?;
        let has_edges = row.try_get::<_, bool>(0).unwrap_or(false);
        let has_frontier = row.try_get::<_, bool>(1).unwrap_or(false);
        if has_edges && has_frontier {
            client.query_one(JOURNEY_READ_PROBE_SQL, &[]).await?;
        }
        Ok::<_, tokio_postgres::Error>((has_edges, has_frontier))
    })
    .await;
    let answer = match probed {
        Ok(Ok((true, true))) => SchemaProbe::Present,
        Ok(Ok((has_edges, has_frontier))) => {
            let mut missing = Vec::new();
            if !has_edges {
                missing.push("project.journey_edge_observations");
            }
            if !has_frontier {
                missing.push("project.journey_frontier");
            }
            SchemaProbe::Absent { missing }
        }
        Ok(Err(e)) => SchemaProbe::Failed {
            error: crate::database::pg::pg_err("journey schema probe", &e),
        },
        Err(_) => SchemaProbe::Failed {
            error: format!("journey schema probe did not answer within {JOURNEY_PROBE_TIMEOUT:?}"),
        },
    };
    *probe_cache() = cache_after(&answer, std::time::Instant::now());
    if answer != SchemaProbe::Present {
        warn!(
            "journey::capture: the journey ledger is NOT being written ({:?}). \
             GET /apps/<app_id>/journey/health reports the cause. A missing table needs \
             qontinui-web migration journey_01_edge_ledger (an existing embedded database never \
             receives new tables — plan Phase 0 decision 4); a failed probe is retried \
             automatically after {:?}.",
            answer, PROBE_RETRY_BACKOFF
        );
    }
    answer
}

// ---------------------------------------------------------------------------
// Events and the worker
// ---------------------------------------------------------------------------

/// One thing a handler saw. Carries only what the worker needs; a snapshot
/// is shared (`Arc`), never deep-copied, with the response and the
/// co-occurrence capture.
#[derive(Debug)]
pub(crate) enum JourneyEvent {
    /// An action (opens a pending edge). `hint` is the outcome it forces
    /// (`error` / `settle_timeout`), if any.
    Action {
        key: CursorKey,
        provenance: Provenance,
        action: ActionSpec,
        hint: Option<EdgeOutcome>,
    },
    /// A successful, UNFILTERED snapshot of the cursor's page (closes a
    /// pending edge). May be the SDK's `{success, data}` envelope; the worker
    /// reads through it ([`snapshot_view`]).
    Snapshot {
        key: CursorKey,
        app_version: Option<String>,
        snapshot: Arc<serde_json::Value>,
    },
    /// Close every pending edge older than the TTL (the retention tick).
    Sweep,
}

/// Events buffered between the handlers and the worker. A snapshot can be
/// megabytes, so the queue is bounded; an event that does not fit is COUNTED
/// as a failed write (never dropped silently).
const EVENT_QUEUE_CAPACITY: usize = 64;

static EVENT_TX: OnceLock<mpsc::Sender<JourneyEvent>> = OnceLock::new();

/// Hand one event to the journey worker, spawning it on first use.
/// Fire-and-forget: never blocks, never fails the caller.
pub(crate) fn enqueue_edge_observation(pg_db: Arc<PgDb>, event: JourneyEvent) {
    let tx = EVENT_TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel(EVENT_QUEUE_CAPACITY);
        tokio::spawn(run_worker(pg_db, rx));
        tx
    });
    if let Err(e) = tx.try_send(event) {
        let reason = match e {
            mpsc::error::TrySendError::Full(_) => "journey event queue full — event dropped",
            mpsc::error::TrySendError::Closed(_) => "journey worker is not running — event dropped",
        };
        health::record_write_failure(reason.to_string());
    }
}

/// An action through a choke point. `failed` becomes the `error` hint.
pub(crate) fn record_action(
    pg_db: Arc<PgDb>,
    key: CursorKey,
    action: ActionSpec,
    provenance: Provenance,
    failed: bool,
) {
    enqueue_edge_observation(
        pg_db,
        JourneyEvent::Action {
            key,
            provenance,
            action,
            hint: failure_hint(failed),
        },
    );
}

/// A successful, unfiltered snapshot.
pub(crate) fn record_snapshot(
    pg_db: Arc<PgDb>,
    key: CursorKey,
    app_version: Option<String>,
    snapshot: Arc<serde_json::Value>,
) {
    enqueue_edge_observation(
        pg_db,
        JourneyEvent::Snapshot {
            key,
            app_version,
            snapshot,
        },
    );
}

/// The outcome hint of an execute-with-diff response: `error` when the route
/// failed (`failed`) or the diff result's `actionSuccess` is present and not
/// `true` (top level or under `data`); else `settle_timeout` when
/// `settleTimedOut` is true; else none (the closing snapshot decides).
pub(crate) fn diff_outcome_hint(response: &serde_json::Value, failed: bool) -> Option<EdgeOutcome> {
    let field = |name: &str| {
        response
            .get(name)
            .or_else(|| response.get("data").and_then(|d| d.get(name)))
            .filter(|v| !v.is_null())
    };
    let action_failed = field("actionSuccess").is_some_and(|v| v != &serde_json::Value::Bool(true));
    if failed || action_failed {
        return Some(EdgeOutcome::Error);
    }
    (field("settleTimedOut").and_then(|v| v.as_bool()) == Some(true))
        .then_some(EdgeOutcome::SettleTimeout)
}

/// An execute-with-diff: a pending edge like every other action, carrying
/// the diff's outcome hint.
pub(crate) fn record_diff(
    pg_db: Arc<PgDb>,
    key: CursorKey,
    request_body: &serde_json::Value,
    response: &serde_json::Value,
    provenance: Provenance,
    failed: bool,
) {
    enqueue_edge_observation(
        pg_db,
        JourneyEvent::Action {
            key,
            provenance,
            action: ActionSpec::with_diff(request_body),
            hint: diff_outcome_hint(response, failed),
        },
    );
}

/// The snapshot inside a body: the SDK `{success, data}` envelope's `data`
/// when it is an object, else the body itself.
pub(crate) fn snapshot_view(body: &serde_json::Value) -> &serde_json::Value {
    match body.get("data") {
        Some(inner) if inner.is_object() => inner,
        _ => body,
    }
}

async fn run_worker(pg_db: Arc<PgDb>, mut rx: mpsc::Receiver<JourneyEvent>) {
    let mut cursors = Cursors::default();
    while let Some(event) = rx.recv().await {
        process_event(&pg_db, &mut cursors, event).await;
        health::set_pending_open(cursors.pending_count() as u64);
    }
}

async fn process_event(pg: &PgDb, cursors: &mut Cursors, event: JourneyEvent) {
    match event {
        JourneyEvent::Action {
            key,
            provenance,
            action,
            hint,
        } => {
            if let Some(displaced) = cursors.open(&key, &action, provenance, hint, Instant::now()) {
                write_edge(pg, displaced).await;
            }
        }
        JourneyEvent::Snapshot {
            key,
            app_version,
            snapshot,
        } => {
            let node = resolve_node(pg, &key.app_id, app_version, Arc::clone(&snapshot)).await;
            let affordances = extract_affordances(snapshot_view(&snapshot));
            let observed = Observed {
                node: node.clone(),
                digest: affordance_digest(&affordances),
            };
            let closed = cursors.observe(&key, observed, affordances.clone(), Instant::now());
            let run_id = closed.as_ref().and_then(|d| d.provenance.run_id.clone());
            if let Some(draft) = closed {
                write_edge(pg, draft).await;
            }
            write_frontier(pg, &key.app_id, &node, &affordances, run_id).await;
        }
        JourneyEvent::Sweep => {
            for draft in cursors.sweep(Instant::now()) {
                write_edge(pg, draft).await;
            }
        }
    }
}

/// One-shot gate for the spec-lookup warning (a configuration fact that would
/// otherwise repeat on every snapshot).
fn should_warn_spec_lookup_once() -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    WARNED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

/// Resolve a snapshot to its node (Phase 0 decision 1): evaluate the ONE page
/// spec matched to the snapshot's page against the snapshot in hand. Never
/// fetches a snapshot; runs only on the worker, and the CPU-bound parse and
/// evaluation run on a blocking thread over the shared snapshot (no copy).
async fn resolve_node(
    pg: &PgDb,
    app_id: &str,
    app_version: Option<String>,
    snapshot: Arc<serde_json::Value>,
) -> JourneyNode {
    let identity = page_identity(snapshot_view(&snapshot));
    let lookup = match identity.spec_lookup_label.clone() {
        None => SpecLookup::NoSpec,
        Some(page_id) => {
            match crate::spec_api::spec_check::load_page_spec(pg, app_id, &page_id).await {
                Ok(None) => SpecLookup::NoSpec,
                Err(e) => {
                    if should_warn_spec_lookup_once() {
                        warn!(
                            "journey::capture: spec lookup for app {} page {} failed ({}); \
                             the node is recorded unmodelled. Warned once per process.",
                            app_id, page_id, e
                        );
                    }
                    SpecLookup::NoSpec
                }
                Ok(Some(loaded)) => {
                    let app_id = app_id.to_string();
                    let snap = Arc::clone(&snapshot);
                    let evaluated = tokio::task::spawn_blocking(move || {
                        evaluate_present_states(snapshot_view(&snap), &loaded, app_id, app_version)
                    })
                    .await;
                    match evaluated {
                        Ok(Some(present_state_ids)) => SpecLookup::Evaluated {
                            spec_id: page_id,
                            present_state_ids,
                        },
                        // Not a canonical control snapshot, or the blocking
                        // task failed: a spec exists but was not evaluated.
                        Ok(None) | Err(_) => SpecLookup::NotEvaluable { spec_id: page_id },
                    }
                }
            }
        }
    };
    build_node(&identity, &lookup)
}

/// Parse the snapshot (borrowing, not copying) and evaluate the loaded spec
/// against it. `None` when the snapshot is not the canonical control shape.
fn evaluate_present_states(
    snapshot: &serde_json::Value,
    loaded: &crate::spec_api::spec_check::LoadedPageSpec,
    app_id: String,
    app_version: Option<String>,
) -> Option<Vec<String>> {
    use serde::Deserialize;
    let parsed = qontinui_types::ui_bridge::UIBridgeSnapshot::deserialize(snapshot).ok()?;
    let fingerprint = qontinui_types::spec_check::BridgeFingerprint {
        app_id,
        app_version,
        route: None,
        bridge_version: None,
        snapshot_timestamp: String::new(),
        element_count: parsed.elements.len() as u32,
    };
    let result = crate::spec_api::spec_check::evaluate_loaded(&parsed, loaded, fingerprint);
    Some(present_state_ids(&result))
}

/// Get a pooled connection and the probe's answer, or record why not.
async fn writable_connection(pg: &PgDb) -> Option<deadpool_postgres::Object> {
    let conn = match pg.pool().get().await {
        Ok(c) => c,
        Err(e) => {
            health::record_write_failure(format!("PG pool error: {e}"));
            return None;
        }
    };
    let probe = journey_schema_supported(&conn).await;
    match probe {
        SchemaProbe::Present => Some(conn),
        SchemaProbe::Absent { .. } | SchemaProbe::Failed { .. } => {
            health::record_not_written();
            None
        }
    }
}

async fn write_edge(pg: &PgDb, draft: EdgeDraft) {
    let obs = finalize(draft);
    let binds = match edge_insert(&obs) {
        Ok(b) => b,
        Err(e) => {
            health::record_write_failure(e);
            return;
        }
    };
    let Some(conn) = writable_connection(pg).await else {
        return;
    };
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); EDGE_WRITE_BINDS] = [
        &binds.id,
        &binds.observed_at,
        &binds.app_id,
        &binds.app_version,
        &binds.runner_build_id,
        &binds.runner_instance,
        &binds.run_id,
        &binds.run_kind,
        &binds.from_node,
        &binds.to_node,
        &binds.trigger,
        &binds.outcome,
        &binds.from_node_key,
        &binds.target_fingerprint,
    ];
    match conn.query_one(EDGE_WRITE_SQL, &params).await {
        Ok(row) => {
            let cleared = row.try_get::<_, i64>(1).unwrap_or(0);
            health::record_edge_written(cleared.max(0) as u64);
        }
        Err(e) => {
            health::record_write_failure(crate::database::pg::pg_err("journey edge insert", &e));
        }
    }
}

async fn write_frontier(
    pg: &PgDb,
    app_id: &str,
    node: &JourneyNode,
    affordances: &AffordanceIndex,
    run_id: Option<String>,
) {
    let batch = match frontier::frontier_batch(app_id, node, affordances, run_id) {
        Ok(Some(b)) => b,
        Ok(None) => return,
        Err(e) => {
            health::record_write_failure(e);
            return;
        }
    };
    let Some(conn) = writable_connection(pg).await else {
        return;
    };
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); frontier::FRONTIER_UPSERT_BINDS] = [
        &batch.app_id,
        &batch.node_key,
        &batch.node,
        &batch.run_id,
        &batch.fingerprints,
        &batch.roles,
        &batch.effects,
    ];
    match conn.execute(frontier::FRONTIER_UPSERT_SQL, &params).await {
        Ok(n) => health::record_frontier_upserted(n),
        Err(e) => {
            health::record_write_failure(crate::database::pg::pg_err("journey frontier upsert", &e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journey::cursor::resolve_trigger;
    use crate::journey::node::{Affordance, ElementAffordance};
    use qontinui_types::ir::IrEffect;
    use qontinui_types::journey::ChokePoint;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn key() -> CursorKey {
        CursorKey {
            app_id: "qontinui-web".into(),
            runner_instance: "primary".into(),
            scope: None,
        }
    }

    fn node(spec: &str, states: &[&str]) -> JourneyNode {
        JourneyNode::new(
            Some(spec.to_string()),
            states.iter().map(|s| s.to_string()),
            None,
            None,
        )
    }

    fn seen(spec: &str, states: &[&str]) -> Observed {
        Observed {
            node: node(spec, states),
            digest: "d".into(),
        }
    }

    fn click(id: &str) -> ActionSpec {
        ActionSpec::element(id, "click", ChokePoint::ElementAction)
    }

    fn affordances_with(id: &str, fp: &str) -> AffordanceIndex {
        let mut idx = AffordanceIndex::default();
        idx.elements.insert(
            id.to_string(),
            ElementAffordance {
                affordance: Affordance {
                    fingerprint: fp.to_string(),
                    role: Some("button".into()),
                    declared_effect: Some(IrEffect::Write),
                    navigation: false,
                },
                action_effects: BTreeMap::new(),
            },
        );
        idx
    }

    // ---- row construction -------------------------------------------------

    #[test]
    fn an_unobserved_edge_binds_sql_null_never_json_null() {
        let now = Instant::now();
        let mut c = Cursors::default();
        c.observe(&key(), seen("a", &["1"]), AffordanceIndex::default(), now);
        c.open(&key(), &click("x"), Provenance::default(), None, now);
        let displaced = c
            .open(&key(), &click("y"), Provenance::default(), None, now)
            .unwrap();
        let binds = edge_insert(&finalize(displaced)).expect("a valid row");
        assert_eq!(binds.to_node, None, "to_node must bind SQL NULL");
        assert_ne!(binds.to_node, Some(serde_json::Value::Null));
        assert_eq!(binds.outcome, "to_node_unobserved");
    }

    #[test]
    fn an_observed_edge_binds_its_nodes_as_objects() {
        let now = Instant::now();
        let mut c = Cursors::default();
        c.observe(
            &key(),
            seen("a", &["1"]),
            affordances_with("x", "fp-x"),
            now,
        );
        c.open(&key(), &click("x"), Provenance::default(), None, now);
        let edge = c
            .observe(&key(), seen("b", &["2"]), AffordanceIndex::default(), now)
            .unwrap();
        let binds = edge_insert(&finalize(edge)).unwrap();
        assert!(binds.from_node.is_object());
        assert!(binds.to_node.as_ref().is_some_and(|v| v.is_object()));
        assert!(binds.trigger.is_object());
        assert_eq!(binds.from_node_key, "a#1");
        assert_eq!(binds.target_fingerprint.as_deref(), Some("fp-x"));
        assert_eq!(binds.run_kind, "agent_action");
        assert_eq!(binds.runner_build_id, RUNNER_BUILD_ID);
        assert!(chrono::DateTime::parse_from_rfc3339(&binds.observed_at).is_ok());
        assert!(uuid::Uuid::parse_str(&binds.id).is_ok());
    }

    #[test]
    fn a_contract_violation_is_an_error_not_a_row() {
        // A pageLabel carrying a '/' is the schemas crate's tripwire for a
        // leaked path; the producer must refuse the row, and the caller
        // counts the refusal.
        let bad = JourneyNode::new(
            None,
            Vec::<String>::new(),
            None,
            Some("search/secret".into()),
        );
        let draft = EdgeDraft {
            key: key(),
            provenance: Provenance::default(),
            from_node: bad,
            to_node: Some(node("a", &["1"])),
            trigger: resolve_trigger(&click("x"), &AffordanceIndex::default()),
            outcome: EdgeOutcome::Changed,
        };
        let err = edge_insert(&finalize(draft)).expect_err("validation must refuse it");
        assert!(err.contains("contract validation"), "{err}");
    }

    #[test]
    fn edge_write_binds_match_the_sql() {
        let max = (1..=40)
            .rev()
            .find(|n| EDGE_WRITE_SQL.contains(&format!("${n}")))
            .unwrap_or(0);
        assert_eq!(max, EDGE_WRITE_BINDS);
        for n in 1..=EDGE_WRITE_BINDS {
            assert!(EDGE_WRITE_SQL.contains(&format!("${n}")), "${n} unused");
        }
    }

    #[test]
    fn edge_write_sql_names_the_contract_columns_and_casts() {
        for needle in [
            "INSERT INTO project.journey_edge_observations",
            "(id, observed_at, app_id, app_version, runner_build_id, runner_instance,",
            "run_id, run_kind, from_node, to_node, trigger, outcome)",
            "$1::text::uuid",
            "$2::text::timestamptz",
            "$9::jsonb, $10::jsonb, $11::jsonb",
            "DELETE FROM project.journey_frontier",
            "f.affordance_fingerprint = $14",
        ] {
            assert!(EDGE_WRITE_SQL.contains(needle), "missing {needle:?}");
        }
        assert!(
            !EDGE_WRITE_SQL.contains("timeline"),
            "the timeline column is Phase 4's; this producer never writes it"
        );
    }

    #[test]
    fn schema_probe_is_schema_qualified_and_reads() {
        assert!(
            JOURNEY_SCHEMA_PROBE_SQL.contains("to_regclass('project.journey_edge_observations')")
        );
        assert!(JOURNEY_SCHEMA_PROBE_SQL.contains("to_regclass('project.journey_frontier')"));
        assert!(!JOURNEY_SCHEMA_PROBE_SQL.contains("information_schema"));
        assert_eq!(JOURNEY_READ_PROBE_SQL.matches("LIMIT 1").count(), 2);
    }

    #[test]
    fn a_client_error_opens_no_edge() {
        use crate::mcp::types::ApiResponse;
        use axum::http::StatusCode;
        use axum::Json;
        let refused: Result<Json<ApiResponse<serde_json::Value>>, _> = Err((
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::error("nope".to_string())),
        ));
        assert_eq!(control_action_verdict(&refused), None);
        let failed: Result<Json<ApiResponse<serde_json::Value>>, _> = Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::error("boom".to_string())),
        ));
        assert_eq!(control_action_verdict(&failed), Some(true));
        let ok: Result<_, (StatusCode, Json<ApiResponse<()>>)> =
            Ok(Json(ApiResponse::success(serde_json::json!({}))));
        assert_eq!(control_action_verdict(&ok), Some(false));
    }

    // ---- diff outcome hint (M5, m4) -----------------------------------------

    #[test]
    fn diff_hint_follows_the_route_failure_rule() {
        let ok = json!({"actionSuccess": true, "settleTimedOut": false});
        assert_eq!(diff_outcome_hint(&ok, false), None);
        assert_eq!(
            diff_outcome_hint(&ok, true),
            Some(EdgeOutcome::Error),
            "success:false"
        );
        let action_failed = json!({"success": true, "data": {"actionSuccess": false}});
        assert_eq!(
            diff_outcome_hint(&action_failed, false),
            Some(EdgeOutcome::Error)
        );
        let settle = json!({"data": {"actionSuccess": true, "settleTimedOut": true}});
        assert_eq!(
            diff_outcome_hint(&settle, false),
            Some(EdgeOutcome::SettleTimeout)
        );
        let relay = json!({"actionResult": {}, "diff": {}});
        assert_eq!(
            diff_outcome_hint(&relay, false),
            None,
            "absent means undecided"
        );
    }

    /// M5: ONE node-identity source. The same page, seen through the control
    /// snapshot route and through the SDK's `{success, data}` envelope,
    /// resolves to one key — and a diff-path action closes onto that key.
    #[test]
    fn the_same_page_resolves_to_one_key_regardless_of_path() {
        let page = json!({
            "activeTab": "settings",
            "page": {"pathname": "/", "route": {"pattern": "/settings"}},
            "elements": [{"id": "b", "label": "Save", "actions": ["click"]}]
        });
        let enveloped = json!({"success": true, "data": page.clone()});
        let control = build_node(&page_identity(snapshot_view(&page)), &SpecLookup::NoSpec);
        let sdk = build_node(
            &page_identity(snapshot_view(&enveloped)),
            &SpecLookup::NoSpec,
        );
        assert_eq!(control.key(), sdk.key());
        assert_eq!(
            affordance_digest(&extract_affordances(snapshot_view(&page))),
            affordance_digest(&extract_affordances(snapshot_view(&enveloped)))
        );

        let now = Instant::now();
        let mut c = Cursors::default();
        let observed = |n: &JourneyNode, v: &serde_json::Value| Observed {
            node: n.clone(),
            digest: affordance_digest(&extract_affordances(snapshot_view(v))),
        };
        c.observe(
            &key(),
            observed(&control, &page),
            extract_affordances(&page),
            now,
        );
        c.open(
            &key(),
            &ActionSpec::with_diff(&json!({"elementId": "b", "action": "click"})),
            Provenance::default(),
            diff_outcome_hint(&json!({"actionSuccess": true}), false),
            now,
        );
        let edge = c
            .observe(
                &key(),
                observed(&sdk, &enveloped),
                extract_affordances(snapshot_view(&enveloped)),
                now,
            )
            .unwrap();
        assert_eq!(edge.from_node.key(), edge.to_node.unwrap().key());
        assert_eq!(edge.outcome, EdgeOutcome::NoChange);
    }

    // ---- schema probe cache (M1) --------------------------------------------

    #[test]
    fn a_failed_probe_is_not_cached_past_the_backoff() {
        let t0 = std::time::Instant::now();
        let failed = SchemaProbe::Failed {
            error: "connection reset".into(),
        };
        let cache = cache_after(&failed, t0);
        assert_eq!(
            cached_answer(&cache, t0 + std::time::Duration::from_secs(1)),
            Some(failed.clone()),
            "inside the backoff the failure is reused (no probe storm)"
        );
        assert_eq!(
            cached_answer(&cache, t0 + PROBE_RETRY_BACKOFF),
            None,
            "after the backoff the next call probes again"
        );
    }

    #[test]
    fn present_and_absent_are_settled() {
        let t0 = std::time::Instant::now();
        let later = t0 + std::time::Duration::from_secs(86_400);
        for answer in [
            SchemaProbe::Present,
            SchemaProbe::Absent {
                missing: vec!["project.journey_frontier"],
            },
        ] {
            assert_eq!(
                cached_answer(&cache_after(&answer, t0), later),
                Some(answer)
            );
        }
        assert_eq!(cached_answer(&ProbeCache::Unprobed, t0), None);
    }

    // ---- choke-point coverage -----------------------------------------------

    /// The body of the top-level item `fn <name>` in `source`: from its
    /// signature to the next top-level item. Text-level on purpose — the
    /// handlers need a live `ApiState` to run, and what this pins is the
    /// WIRING, which is otherwise invisible: a transport whose handler never
    /// calls the ledger writes no row and fails nothing.
    fn item_body<'a>(source: &'a str, name: &str) -> &'a str {
        let start = [
            format!("\npub async fn {name}("),
            format!("\nasync fn {name}("),
            format!("\nfn {name}("),
            format!("\npub(crate) fn {name}("),
        ]
        .iter()
        .find_map(|sig| source.find(sig.as_str()))
        .unwrap_or_else(|| panic!("handler `{name}` not found"));
        let rest = source.get(start + 1..).unwrap_or_default();
        let end = rest
            .match_indices("\n}\n")
            .next()
            .map(|(i, _)| i + 3)
            .unwrap_or(rest.len());
        rest.get(..end).unwrap_or(rest)
    }

    /// Every choke point — the five D3 paths plus the SDK component and batch
    /// routes recorded under the same action kinds — and both snapshot
    /// routes that close pending edges, must call the ledger. A missing
    /// transport would otherwise be invisible: edges for whichever transport a
    /// skill uses would simply never appear.
    #[test]
    fn every_choke_point_calls_the_ledger() {
        // cargo runs tests with CWD = crate root (src-tauri).
        let read = |p: &str| std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"));
        let elements = read("src/mcp/ui_bridge/elements.rs");
        let bookmarks = read("src/mcp/ui_bridge/bookmarks.rs");
        let sdk = read("src/mcp/sdk_client.rs");

        let cases: [(&str, &str, &str); 14] = [
            // (1) control element action
            (
                "elements",
                "ui_bridge_execute_action_handler",
                "record_action(",
            ),
            // (2) control batch
            (
                "elements",
                "ui_bridge_batch_actions_handler",
                "record_action(",
            ),
            // (3) control component action
            (
                "elements",
                "ui_bridge_execute_component_action_handler",
                "record_action(",
            ),
            // (4) SDK element action
            ("sdk", "handle_element_action", "record_action("),
            // (5) execute-with-diff: two runner routes + the SDK twin
            (
                "bookmarks",
                "ui_bridge_execute_with_diff_handler",
                "record_diff_result(",
            ),
            (
                "bookmarks",
                "ui_bridge_with_diff_handler",
                "record_diff_result(",
            ),
            ("bookmarks", "record_diff_result", "record_diff("),
            ("sdk", "handle_ct_execute_with_diff", "record_diff("),
            // SDK component action — recorded as `component_action`
            (
                "sdk",
                "handle_component_action",
                "record_sdk_component_action(",
            ),
            (
                "sdk",
                "record_sdk_component_action",
                "ActionSpec::component(",
            ),
            // SDK batch routes — recorded as `batch_action`
            ("sdk", "handle_execute_batch_action", "record_sdk_batch("),
            ("sdk", "handle_control_batch", "record_sdk_batch("),
            // Snapshots that close pending edges
            (
                "elements",
                "ui_bridge_get_snapshot_handler",
                "record_snapshot(",
            ),
            ("sdk", "handle_snapshot", "record_journey_snapshot("),
        ];
        for (file, handler, call) in cases {
            let source = match file {
                "elements" => &elements,
                "bookmarks" => &bookmarks,
                _ => &sdk,
            };
            assert!(
                item_body(source, handler).contains(call),
                "{file}::{handler} must call `{call}` — without it this transport writes no \
                 journey row"
            );
        }
        assert!(item_body(&sdk, "record_journey_snapshot").contains("record_snapshot("));
        assert!(item_body(&sdk, "record_sdk_batch").contains("ActionSpec::batch("));
    }

    // ---- privacy -----------------------------------------------------------

    /// Typed text never reaches a row: the sentinel is typed through an action
    /// request, sits in an input's value in both snapshots, AND is the URL
    /// path's last segment — and appears in no serialized edge or frontier
    /// row.
    #[test]
    fn a_typed_sentinel_appears_in_no_row() {
        const SENTINEL: &str = "zz-sentinel-8f3a91";
        let request = json!({"action": "type", "params": {"text": SENTINEL}});
        let action_name = request["action"].as_str().unwrap();
        let spec = ActionSpec::element("search-box", action_name, ChokePoint::ElementAction);

        let snapshot = |value: &str| {
            json!({
                "page": {
                    "pathname": format!("/search/{SENTINEL}"),
                    "url": format!("http://app.local/search/{SENTINEL}")
                },
                "elements": [{
                    "id": "search-box", "type": "input", "role": "searchbox",
                    "label": "Search", "value": value, "text": value,
                    "actions": ["type", "click"]
                }]
            })
        };
        let before = snapshot("");
        let after = snapshot(SENTINEL);

        let now = Instant::now();
        let mut c = Cursors::default();
        let from = build_node(&page_identity(&before), &SpecLookup::NoSpec);
        let from_aff = extract_affordances(&before);
        c.observe(
            &key(),
            Observed {
                node: from.clone(),
                digest: affordance_digest(&from_aff),
            },
            from_aff.clone(),
            now,
        );
        c.open(&key(), &spec, Provenance::default(), None, now);
        let to = build_node(&page_identity(&after), &SpecLookup::NoSpec);
        let to_aff = extract_affordances(&after);
        let edge = c
            .observe(
                &key(),
                Observed {
                    node: to.clone(),
                    digest: affordance_digest(&to_aff),
                },
                to_aff.clone(),
                now,
            )
            .unwrap();

        let row = serde_json::to_string(&finalize(edge.clone())).unwrap();
        let binds = edge_insert(&finalize(edge)).unwrap();
        let frontier_after = frontier::frontier_batch("qontinui-web", &to, &to_aff, None)
            .unwrap()
            .unwrap();
        let frontier_before = frontier::frontier_batch("qontinui-web", &from, &from_aff, None)
            .unwrap()
            .unwrap();

        for (what, text) in [
            ("edge row", row),
            ("edge binds", format!("{binds:?}")),
            ("frontier (after)", format!("{frontier_after:?}")),
            ("frontier (before)", format!("{frontier_before:?}")),
        ] {
            assert!(
                !text.contains(SENTINEL),
                "{what} leaked the sentinel: {text}"
            );
        }
        assert_eq!(
            from.page_label, None,
            "a pathname-only page has no pageLabel"
        );
    }
}
