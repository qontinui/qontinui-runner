//! State derivation from accumulated co-occurrence observations.
//!
//! Reads non-invalidated observations within a time window, reshapes them
//! into the "render log" format expected by
//! `qontinui.discovery.discover_states_from_renders`, dispatches to the
//! Python bridge, and persists the result as a `state_discovery_artifacts`
//! row.
//!
//! Clustering itself lives in Python (`qontinui.discovery`). We do not
//! re-implement it in Rust — the bridge round-trip is the integration point.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::database::pg::PgDb;
use crate::executor::with_default_bridge;
use crate::mcp::types::{api_error, ApiResponse, ApiState};

/// Default lookback window for derivation, in days. 90 d matches the design
/// doc's chosen decay window — observations older than this are treated as
/// stale and excluded. Callers may override per-request.
///
/// `spec_authoring` reads the same constant when selecting a page's
/// observations, so the window a state was discovered over and the window it
/// is attributed to a page over cannot drift apart.
pub(crate) const DEFAULT_WINDOW_DAYS: i32 = 90;

/// Cadence of the background global derive loop, in seconds. Overridable via
/// [`DERIVE_INTERVAL_ENV`]; floored at [`DERIVE_INTERVAL_FLOOR_SECS`] so a
/// mis-set env var can't turn a Python clustering round-trip into a hot loop.
const DEFAULT_DERIVE_INTERVAL_SECS: u64 = 86_400;

/// Lower bound on the derive cadence. Derivation is O(fingerprints²) inside
/// Python; anything under 10 min is a misconfiguration, not a preference.
const DERIVE_INTERVAL_FLOOR_SECS: u64 = 600;

/// Delay before the loop's *first* derive. Startup (PG pool, Python bridge
/// warm-up, auto-load of the default SM) must finish first, and a heavy
/// clustering pass at process start would compete with it. Overridable via
/// [`DERIVE_INITIAL_DELAY_ENV`].
const DEFAULT_DERIVE_INITIAL_DELAY_SECS: u64 = 3_600;

/// Below this the initial delay is warned about rather than rejected. It is
/// **not** a floor: `0` stays legal because the persisted-cadence check in
/// [`global_derive_is_recent`] already prevents a frequently-restarted runner
/// from re-clustering on every boot, so the only thing a short delay can still
/// do is make the *first* derive of a genuinely-due period race the rest of
/// startup. That is worth a WARN, not a silent override of what the operator
/// asked for — and hard-flooring it would take away the one-shot behaviour the
/// env var exists to give test harnesses.
const DERIVE_INITIAL_DELAY_WARN_FLOOR_SECS: u64 = 60;

/// Wall-clock budget for one derive tick. Generously above [`BRIDGE_TIMEOUT`]
/// so the bridge's own timeout normally fires first; this only catches a
/// wedged PG pool. Exceeding it drops the tick and keeps the loop alive.
const DERIVE_TICK_BUDGET: Duration = Duration::from_secs(300);

/// Cadence override for the background derive loop, in seconds.
pub const DERIVE_INTERVAL_ENV: &str = "QONTINUI_STATE_DERIVE_INTERVAL_SECS";

/// Override for the delay before the loop's first derive, in seconds.
pub const DERIVE_INITIAL_DELAY_ENV: &str = "QONTINUI_STATE_DERIVE_INITIAL_DELAY_SECS";

/// Override for the loop's observation lookback window, in days. Replaces the
/// supervisor's retired `QONTINUI_FLYWHEEL_DERIVE_WINDOW_DAYS`.
pub const DERIVE_WINDOW_DAYS_ENV: &str = "QONTINUI_STATE_DERIVE_WINDOW_DAYS";

/// Max time budget for the Python `discover_states_from_renders` call. The
/// clustering is hierarchical agglomerative over up to O(fingerprints²), so
/// growth is quadratic in the fingerprint universe, not in observation count.
/// 60 s is generous for the fingerprint volumes we expect per spec.
const BRIDGE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Deserialize)]
pub struct DeriveQuery {
    pub spec_id: Option<String>,
    pub window_days: Option<i32>,
}

/// POST /state-discovery/derive
pub async fn derive_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<DeriveQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let spec_id = query.spec_id.clone();
    let window_days = query.window_days.unwrap_or(DEFAULT_WINDOW_DAYS).max(1);

    info!(
        "State Discovery: deriving states (spec_id={:?}, window_days={})",
        spec_id, window_days
    );

    let pg_db = state.app_state.pg_db.clone();
    let app_state = state.app_state.clone();

    match derive(pg_db, app_state, spec_id, window_days).await {
        Ok(v) => Ok(Json(ApiResponse::success(v))),
        Err(e) => {
            error!("State Discovery: derivation failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Run a derivation pass: query observations → bridge → persist artifact.
///
/// Returns the persisted artifact JSON, enriched with metadata
/// (`id`, `spec_id`, `derived_at`, `window_days`, `observation_count`).
pub async fn derive(
    pg_db: Arc<PgDb>,
    app_state: Arc<crate::commands::AppState>,
    spec_id: Option<String>,
    window_days: i32,
) -> Result<serde_json::Value, String> {
    // 1. Query observations.
    let observations = load_observations(&pg_db, spec_id.as_deref(), window_days).await?;
    let observation_count = observations.len() as i32;

    if observations.is_empty() {
        info!(
            "State Discovery: no observations in window (spec_id={:?}, window_days={})",
            spec_id, window_days
        );
        // Still persist an empty artifact so callers can see "we tried, got nothing"
        // rather than silently returning.
        let empty_artifact = serde_json::json!({
            "states": [],
            "elements": [],
            "elementToRenders": {},
            "renderCount": 0,
            "uniqueElementCount": 0,
            "note": "no observations in window"
        });
        return persist_artifact(
            &pg_db,
            spec_id.as_deref(),
            window_days,
            observation_count,
            empty_artifact,
        )
        .await;
    }

    // 2. Reshape into the render-log format expected by the Python adapter.
    //    Each observation becomes one render entry; its fingerprints become
    //    element entries keyed by `id`. The Python side prefixes those with
    //    "reg:" during extraction (see ui_bridge_adapter.extract_elements_from_render).
    let renders: Vec<serde_json::Value> = observations
        .iter()
        .map(|obs| {
            let elements: Vec<serde_json::Value> = obs
                .fingerprints
                .iter()
                .map(|fp| serde_json::json!({ "id": fp }))
                .collect();
            serde_json::json!({
                "id": obs.id.to_string(),
                "elements": elements,
            })
        })
        .collect();

    let params = serde_json::json!({ "render_logs": renders });

    // 3. Dispatch to the Python bridge. Mirrors the pattern used by
    //    `auto_load_default_state_machine` — spawn_blocking around the
    //    synchronous `with_default_bridge` helper.
    let app_state_for_bridge = app_state.clone();
    let dispatch = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state_for_bridge, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait(
                "discover_states_from_renders",
                Some(params),
                BRIDGE_TIMEOUT,
            )
        })
    })
    .await
    .map_err(|e| format!("spawn_blocking join error: {}", e))?;

    let response = dispatch
        .map_err(|e| format!("with_default_bridge error: {}", e))?
        .map_err(|e| format!("bridge send_command_and_wait: {}", e))?;

    if !response.success {
        let msg = response
            .error
            .unwrap_or_else(|| "bridge returned failure with no error message".to_string());
        return Err(format!("discover_states_from_renders: {}", msg));
    }

    let artifact_body = response
        .data
        .unwrap_or_else(|| serde_json::json!({"states": [], "elements": []}));

    // 4. Persist and return.
    persist_artifact(
        &pg_db,
        spec_id.as_deref(),
        window_days,
        observation_count,
        artifact_body,
    )
    .await
}

// ---------------------------------------------------------------------------
// Background global derive loop
// ---------------------------------------------------------------------------

/// Namespace the derive loop's Postgres advisory-lock key is derived from.
///
/// The string, not the number, is the thing a human picks; the number is
/// mechanical (see [`DERIVE_LOCK_KEY`]). Anything else that wants a
/// `pg_advisory_lock` in this database should pick its own namespace string
/// here and get its own key the same way, so keys cannot be chosen by
/// eyeballing and colliding.
const DERIVE_LOCK_NAMESPACE: &str = "qontinui:state-discovery:global-derive";

/// FNV-1a/64. Const so the key below is computed at compile time.
///
/// Deliberately not a cryptographic hash and deliberately not a runtime one:
/// the requirement is only "the same string always yields the same 64-bit
/// number, on every machine, forever", and a hand-written 20-line const fn is a
/// far smaller commitment than pulling a hasher into a `const` context.
const fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

/// Advisory-lock key for the global derive loop: FNV-1a/64 of
/// [`DERIVE_LOCK_NAMESPACE`], reinterpreted as the `bigint` Postgres wants.
///
/// # Why an advisory lock at all
///
/// Every runner instance runs this loop, and the runner is explicitly
/// multi-instance — secondaries listen on `:9877`+ on the same box and share
/// the same Postgres (see `runner-instances.md`). Without a guard, N instances
/// each write a byte-identical global artifact per night, which is *precisely*
/// the defect this loop exists to remove from the supervisor's per-app cron. It
/// also compounds downstream: `spec_authoring::load_latest_global_artifact` is
/// `ORDER BY derived_at DESC LIMIT 1`, so N racing writers make every page's
/// projection depend on which same-second row happened to win.
///
/// # Why a *Postgres* advisory lock and not an in-process guard
///
/// The database is the only thing all the writers share. A `OnceLock`/mutex
/// would serialise the instances on one box and do nothing at all for the
/// two-machines-one-database case, which is a supported topology here.
///
/// # How the key was chosen
///
/// It is the FNV-1a/64 digest of [`DERIVE_LOCK_NAMESPACE`], evaluated in a
/// `const fn` so the value is fixed at compile time and can never drift from
/// the name it is documented as meaning. Postgres advisory locks live in one
/// flat 2^64 keyspace per database, so the only real requirement on the value
/// is that nothing else picks it; a digest of a repo-qualified namespace string
/// makes accidental collision as unlikely as the keyspace allows, and makes the
/// derivation reproducible by anyone reading this file. (At the time of
/// writing, nothing else in the runner takes a `pg_advisory_lock` at all.)
const DERIVE_LOCK_KEY: i64 = fnv1a64(DERIVE_LOCK_NAMESPACE.as_bytes()) as i64;

/// Resolve the loop cadence from [`DERIVE_INTERVAL_ENV`], defaulting to
/// [`DEFAULT_DERIVE_INTERVAL_SECS`] and flooring at
/// [`DERIVE_INTERVAL_FLOOR_SECS`].
fn derive_interval_secs() -> u64 {
    std::env::var(DERIVE_INTERVAL_ENV)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_DERIVE_INTERVAL_SECS)
        .max(DERIVE_INTERVAL_FLOOR_SECS)
}

/// Resolve the pre-first-tick delay from [`DERIVE_INITIAL_DELAY_ENV`],
/// defaulting to [`DEFAULT_DERIVE_INITIAL_DELAY_SECS`]. Not floored — `0` is a
/// legitimate value for a test harness that wants an immediate derive; see
/// [`DERIVE_INITIAL_DELAY_WARN_FLOOR_SECS`] for why it is warned about instead.
fn derive_initial_delay_secs() -> u64 {
    std::env::var(DERIVE_INITIAL_DELAY_ENV)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_DERIVE_INITIAL_DELAY_SECS)
}

/// Resolve the lookback window from [`DERIVE_WINDOW_DAYS_ENV`], defaulting to
/// [`DEFAULT_WINDOW_DAYS`]. Non-positive values are ignored rather than
/// clamped — a `0` in the environment is a typo, not a request for an empty
/// window.
fn derive_window_days() -> i32 {
    std::env::var(DERIVE_WINDOW_DAYS_ENV)
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .filter(|d| *d > 0)
        .unwrap_or(DEFAULT_WINDOW_DAYS)
}

/// Background loop that derives states **once, globally**, on a nightly-ish
/// cadence. Spawned from `main.rs` alongside the drift detector.
///
/// # Why one global derive and not one per app / per spec
///
/// A state is a set of elements sharing a render-set signature, and several
/// states are active at once: elements `{a,b,c}` seen on pages 1-4 form one
/// state, `{d,e}` seen on pages 2-3 form another, and page 2 has BOTH.
/// Clustering therefore needs the cross-view render pool — deriving per page
/// would give every persistent element on that page an identical render-set,
/// collapsing the page into one mega-state and duplicating shared chrome N
/// times. The page label is a **selection key, not a partition key**; the
/// per-page projection happens downstream in
/// `workflow_generation::spec_authoring::load_and_project_skeleton`.
///
/// So `spec_id` is deliberately `None`: the artifact is written with
/// `spec_id IS NULL`, which is exactly what
/// `spec_authoring::load_latest_global_artifact` selects on. Passing a concrete
/// spec id here would make the artifacts unreadable to the only consumer.
///
/// This loop also replaces the supervisor's per-app flywheel derive step, which
/// ran the identical global derivation once per registered app (N byte-identical
/// artifacts per night) and only ever ran on the operator's dev-only supervisor,
/// so no ordinary user got derivation at all.
///
/// # Single writer, and a cadence that survives restarts
///
/// Every instance runs this loop, so the tick is guarded twice: a Postgres
/// advisory lock ([`DERIVE_LOCK_KEY`]) makes at most one instance *anywhere*
/// derive at a time, and a persisted-cadence check
/// ([`global_derive_is_recent`]) makes the schedule wall-clock rather than
/// restart-relative. Neither alone is enough — the lock lets a restarted
/// instance derive again immediately after another finished, and the cadence
/// check alone leaves two instances racing inside the same second.
///
/// Errors inside a tick log but never kill the loop.
pub async fn run_derive_loop(app_state: Arc<crate::commands::AppState>) {
    // All three knobs are resolved ONCE, here. Re-reading only some of them per
    // tick made the configuration half-live: `tokio::time::interval` is built
    // once so a changed interval could never take effect, while a re-read
    // window silently could — two env vars documented identically behaving
    // differently. One resolution point, one log line.
    let interval_secs = derive_interval_secs();
    let initial_delay_secs = derive_initial_delay_secs();
    let window_days = derive_window_days();

    info!(
        "state-derive: starting global derive loop (interval={}s, initial_delay={}s, \
         window_days={}, cadence_threshold={}s, lock_key={})",
        interval_secs,
        initial_delay_secs,
        window_days,
        cadence_threshold_secs(interval_secs),
        DERIVE_LOCK_KEY
    );

    if initial_delay_secs < DERIVE_INITIAL_DELAY_WARN_FLOOR_SECS {
        warn!(
            "state-derive: {}={}s is below the {}s advisory floor — a derive that is \
             genuinely due will start while PG bootstrap, the Python bridge warm-up and the \
             default-SM auto-load are still running, and clustering is O(fingerprints²). \
             Intended for test harnesses only.",
            DERIVE_INITIAL_DELAY_ENV, initial_delay_secs, DERIVE_INITIAL_DELAY_WARN_FLOOR_SECS
        );
    }

    // One-shot initial delay so PG bootstrap, the Python bridge warm-up and the
    // default-SM auto-load all finish before the first (heavy) derivation. The
    // drift detector achieves the same by discarding `interval`'s immediate
    // first tick; a 24 h cadence can't use that trick, because the first derive
    // would then be a day out on every restart.
    tokio::time::sleep(Duration::from_secs(initial_delay_secs)).await;

    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    // Explicit, NOT the `Burst` default. The tick is hard-capped at
    // `DERIVE_TICK_BUDGET` (300 s) and the interval floors at 600 s, so a tick
    // cannot in fact outrun its interval today and this is defence-in-depth: if
    // either constant is ever retuned so they overlap, `Skip` drops the missed
    // tick instead of queueing back-to-back clustering passes.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        // First tick resolves immediately — i.e. at `initial_delay_secs` after
        // start, never at process start.
        ticker.tick().await;
        run_guarded_tick(&app_state, window_days, interval_secs).await;
    }
}

/// Tolerance subtracted from the interval when asking "has a derive already
/// happened this period?".
///
/// `derived_at` is stamped when the artifact is *inserted*, i.e. one derive
/// duration after the tick that produced it, while ticks fire at a fixed offset
/// from loop start. Comparing against the bare interval would therefore find
/// the previous artifact a few seconds too young on every tick and skip it —
/// halving the real cadence, forever. The tolerance has to cover one derive,
/// which is bounded by [`DERIVE_TICK_BUDGET`]; it is additionally capped at 10%
/// of the interval so a short configured cadence cannot be pulled to twice its
/// configured rate by a large fixed tolerance.
fn cadence_threshold_secs(interval_secs: u64) -> u64 {
    let tolerance = DERIVE_TICK_BUDGET.as_secs().min(interval_secs / 10);
    interval_secs.saturating_sub(tolerance)
}

/// One tick: take the cross-instance lock, run the (cadence-gated) derive,
/// release the lock.
///
/// # The unlock contract
///
/// A `pg_advisory_lock` is held by the *session*, i.e. by the pooled
/// connection, and deadpool's default recycling is `Fast` — it does not
/// `DISCARD ALL` — so a lock left behind is not cleaned up when the connection
/// goes back to the pool. It would sit there wedging every future tick on every
/// instance until that backend died. So the release is structural, not
/// best-effort:
///
/// - the lock-held region is exactly one expression ([`run_locked_tick`]), with
///   the unlock on the next line, so there is no `return` or `?` between them —
///   the skip path, the error path and the timeout path all leave through the
///   same statement;
/// - the derive itself runs on its own task, so a panic inside it arrives here
///   as a `JoinError` instead of unwinding past the unlock;
/// - and if the process dies outright, its backend connection dies with it and
///   Postgres releases the lock for us.
///
/// The connection is taken for the lock's lifetime rather than re-fetched from
/// the pool, because a session-scoped lock taken on one pooled connection and
/// released on another simply would not release. (`pg_advisory_xact_lock` would
/// self-release, but only by holding a transaction open for the whole
/// clustering pass — trading a leak risk for a guaranteed long
/// idle-in-transaction. Not worth it.) The pool is `max_size=8` and a derive
/// uses two connections briefly, so holding one more is comfortable.
async fn run_guarded_tick(
    app_state: &Arc<crate::commands::AppState>,
    window_days: i32,
    interval_secs: u64,
) {
    let pg_db = app_state.pg_db.clone();

    let conn = match pg_db.pool().get().await {
        Ok(c) => c,
        Err(e) => {
            warn!("state-derive: tick skipped, PG pool error: {}", e);
            return;
        }
    };

    match conn
        .query_one("SELECT pg_try_advisory_lock($1)", &[&DERIVE_LOCK_KEY])
        .await
    {
        Ok(row) => {
            if !row.get::<_, bool>(0) {
                info!(
                    "state-derive: another instance holds the derive lock ({}); skipping tick",
                    DERIVE_LOCK_KEY
                );
                return;
            }
        }
        Err(e) => {
            warn!(
                "state-derive: could not take the derive lock ({}): {}; skipping tick",
                DERIVE_LOCK_KEY, e
            );
            return;
        }
    }

    // ---- LOCK HELD. Nothing between here and the unlock may return early. ----
    run_locked_tick(&conn, &pg_db, app_state, window_days, interval_secs).await;

    if let Err(e) = conn
        .query_one("SELECT pg_advisory_unlock($1)", &[&DERIVE_LOCK_KEY])
        .await
    {
        // Not fatal for this tick, but it does mean the lock is still held on
        // this connection, so say so loudly rather than logging at debug.
        error!(
            "state-derive: failed to release the derive lock ({}): {} — it stays held until \
             this pooled connection is closed, which will stall derivation fleet-wide",
            DERIVE_LOCK_KEY, e
        );
    }
    // ---- LOCK RELEASED. ----
}

/// The body of a tick, run with [`DERIVE_LOCK_KEY`] held. Never returns a
/// result: the caller's only job afterwards is to unlock, and giving it a value
/// to inspect would invite an early return between the two.
async fn run_locked_tick(
    conn: &deadpool_postgres::Client,
    pg_db: &Arc<PgDb>,
    app_state: &Arc<crate::commands::AppState>,
    window_days: i32,
    interval_secs: u64,
) {
    match global_derive_is_recent(conn, interval_secs).await {
        Ok(Some(last)) => {
            info!(
                "state-derive: newest global artifact was derived at {}, inside the {}s cadence \
                 threshold; skipping tick",
                last,
                cadence_threshold_secs(interval_secs)
            );
            return;
        }
        Ok(None) => {}
        Err(e) => {
            // Fail closed. An unreadable artifact table is exactly the state in
            // which a blind derive is most likely to be the second one today.
            warn!("state-derive: cadence check failed: {}; skipping tick", e);
            return;
        }
    }

    // spec_id = None → global corpus, artifact persisted with spec_id NULL.
    //
    // On its own task so a panic surfaces here as a `JoinError` rather than
    // unwinding through the caller's frame while the advisory lock is held.
    let mut handle = tokio::spawn(derive(pg_db.clone(), app_state.clone(), None, window_days));

    // Bound to a `let` rather than matched inline: the temporary `Timeout`
    // future holds the `&mut handle` borrow for as long as it lives, and a
    // `match` on it would keep it alive across the arms — where the timeout arm
    // needs `handle` back to abort it.
    let outcome = tokio::time::timeout(DERIVE_TICK_BUDGET, &mut handle).await;

    match outcome {
        Ok(Ok(Ok(v))) => {
            let observation_count = v.get("observation_count").and_then(|n| n.as_i64());
            match observation_count {
                Some(n) => info!(
                    "state-derive: global derivation complete (observations={}, window_days={})",
                    n, window_days
                ),
                None => warn!(
                    "state-derive: global derivation complete but observation_count was \
                     absent from the persisted artifact — response shape may have drifted"
                ),
            }
        }
        Ok(Ok(Err(e))) => warn!("state-derive: tick failed: {}", e),
        Ok(Err(join_err)) => warn!("state-derive: derivation task died: {}", join_err),
        Err(_) => {
            // Dropping a `JoinHandle` detaches the task rather than cancelling
            // it, so the abort is what actually stops the run — without it the
            // derive would carry on writing after the lock came off.
            handle.abort();
            warn!(
                "state-derive: tick exceeded {}s budget; aborted",
                DERIVE_TICK_BUDGET.as_secs()
            );
        }
    }
}

/// Has a global derive already happened inside the current cadence period?
///
/// Returns `Some(derived_at)` when it has (caller skips the tick), `None` when
/// a derive is due. This is what makes the schedule wall-clock rather than
/// restart-relative: the loop's own timer starts at process start with nothing
/// persisted, so a runner restarted every ≤1 h would never reach its first
/// nightly tick, and one restarted every 2 h would derive every 2 h — 12× the
/// intended load, each pass a full O(fingerprints²) clustering round-trip. The
/// supervisor cron this loop replaces was wall-clock scheduled; this restores
/// that property from the only durable clock available, the artifact table.
///
/// `spec_id IS NULL` matches exactly what the loop writes (and what
/// `spec_authoring::load_latest_global_artifact` reads). Empty artifacts count:
/// "we ran and found nothing" is still a run, and excluding them would make a
/// corpus-less runner re-cluster on every tick.
///
/// The comparison is done in Postgres, not in Rust, because `derived_at` is
/// written by Postgres' own `DEFAULT now()` — comparing it against `now()` on
/// the same server is immune to runner/DB clock skew.
async fn global_derive_is_recent(
    conn: &deadpool_postgres::Client,
    interval_secs: u64,
) -> Result<Option<String>, String> {
    let threshold_secs = cadence_threshold_secs(interval_secs) as i64;

    // `($1::bigint || ' seconds')::interval` mirrors the `::int || ' days'`
    // idiom in `load_observations`: it keeps $1 typed as a plain integer
    // parameter rather than asking tokio-postgres for an `INTERVAL`.
    // `MAX(derived_at)` is an index-only lookup on `idx_discovery_spec_derived`.
    let row = conn
        .query_one(
            r#"SELECT MAX(derived_at)::text,
                      COALESCE(
                          MAX(derived_at) > now() - ($1::bigint || ' seconds')::interval,
                          false
                      )
               FROM state_discovery_artifacts
               WHERE spec_id IS NULL"#,
            &[&threshold_secs],
        )
        .await
        .map_err(|e| format!("PG query state_discovery_artifacts: {}", e))?;

    let last: Option<String> = row.get(0);
    let too_recent: bool = row.get(1);
    Ok(if too_recent { last } else { None })
}

/// Minimal shape we need from the observations table for derivation.
struct LoadedObservation {
    id: String,
    fingerprints: Vec<String>,
}

async fn load_observations(
    pg_db: &PgDb,
    spec_id: Option<&str>,
    window_days: i32,
) -> Result<Vec<LoadedObservation>, String> {
    let conn = pg_db
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    // We pin input order by fingerprint-array hash via ORDER BY captured_at,
    // id — the id is a UUID so the tiebreaker is deterministic. Pinning order
    // matters because hierarchical clustering can produce different
    // dendrograms for different orderings (see "Known risks #5" in the plan).
    // tokio-postgres doesn't have the uuid feature enabled in this crate, so
    // we `::text`-cast the UUID column and read it as a String.
    let rows = if let Some(sid) = spec_id {
        conn.query(
            r#"SELECT id::text, fingerprints
               FROM co_occurrence_observations
               WHERE invalidated_at IS NULL
                 AND captured_at >= now() - ($1::int || ' days')::interval
                 AND spec_id = $2
               ORDER BY captured_at, id"#,
            &[&window_days, &sid],
        )
        .await
    } else {
        conn.query(
            r#"SELECT id::text, fingerprints
               FROM co_occurrence_observations
               WHERE invalidated_at IS NULL
                 AND captured_at >= now() - ($1::int || ' days')::interval
               ORDER BY captured_at, id"#,
            &[&window_days],
        )
        .await
    }
    .map_err(|e| format!("PG query co_occurrence_observations: {}", e))?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let id: String = row.get(0);
        let fingerprints_json: serde_json::Value = row.get(1);
        let fingerprints: Vec<String> = match fingerprints_json.as_array() {
            Some(arr) => arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            None => {
                warn!(
                    "State Discovery: observation {} has non-array fingerprints — skipping",
                    id
                );
                continue;
            }
        };
        if fingerprints.is_empty() {
            continue;
        }
        out.push(LoadedObservation { id, fingerprints });
    }
    Ok(out)
}

async fn persist_artifact(
    pg_db: &PgDb,
    spec_id: Option<&str>,
    window_days: i32,
    observation_count: i32,
    artifact_body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let artifact_id = Uuid::new_v4().to_string();

    let conn = pg_db
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    let spec_id_owned: Option<String> = spec_id.map(|s| s.to_string());

    // $1::text::uuid — tokio-postgres lacks the uuid feature; a bare $1::uuid
    // makes PG infer the parameter as uuid (fails on String). ::text::uuid
    // keeps $1 typed as text and coerces at insertion.
    conn.execute(
        r#"INSERT INTO state_discovery_artifacts
           (id, spec_id, window_days, artifact, observation_count)
           VALUES ($1::text::uuid, $2, $3, $4::jsonb, $5)"#,
        &[
            &artifact_id,
            &spec_id_owned as &(dyn tokio_postgres::types::ToSql + Sync),
            &window_days,
            &artifact_body,
            &observation_count,
        ],
    )
    .await
    .map_err(|e| format!("PG insert state_discovery_artifacts: {}", e))?;

    Ok(serde_json::json!({
        "id": artifact_id,
        "spec_id": spec_id_owned,
        "window_days": window_days,
        "observation_count": observation_count,
        "artifact": artifact_body,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The env helpers must fall back to the documented defaults when nothing
    /// is set. Guarded on the var being absent so a deliberately-configured
    /// dev box doesn't fail its own test run (same pattern as
    /// `agent_worktree::fs_backstop`).
    #[test]
    fn env_helpers_default_when_unset() {
        if std::env::var(DERIVE_INTERVAL_ENV).is_err() {
            assert_eq!(derive_interval_secs(), DEFAULT_DERIVE_INTERVAL_SECS);
        }
        if std::env::var(DERIVE_INITIAL_DELAY_ENV).is_err() {
            assert_eq!(
                derive_initial_delay_secs(),
                DEFAULT_DERIVE_INITIAL_DELAY_SECS
            );
        }
        if std::env::var(DERIVE_WINDOW_DAYS_ENV).is_err() {
            assert_eq!(derive_window_days(), DEFAULT_WINDOW_DAYS);
        }
    }

    /// The cadence floor must sit at or below the default, otherwise the
    /// `.max()` in `derive_interval_secs` would silently override the default.
    #[test]
    fn interval_floor_is_below_default() {
        const { assert!(DERIVE_INTERVAL_FLOOR_SECS <= DEFAULT_DERIVE_INTERVAL_SECS) };
    }

    /// The tick budget must exceed the bridge timeout, so a slow-but-working
    /// clustering pass is cut off by the bridge (with a real error) rather
    /// than by the loop's outer guard (with a generic "budget exceeded").
    #[test]
    fn tick_budget_exceeds_bridge_timeout() {
        assert!(DERIVE_TICK_BUDGET > BRIDGE_TIMEOUT);
    }

    /// `fnv1a64` must be real FNV-1a/64, not something that merely looks like
    /// it — the advisory-lock key's whole claim to being reproducible rests on
    /// the algorithm being the published one. These are the reference vectors.
    #[test]
    fn fnv1a64_matches_the_reference_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    /// The lock key is pinned to a literal on purpose. Two instances only
    /// exclude each other if they agree on the number, so changing
    /// `DERIVE_LOCK_NAMESPACE` is a rolling-upgrade hazard (old and new
    /// binaries would take different locks and both derive) and must be a
    /// deliberate, visible edit rather than a silent consequence of a rename.
    #[test]
    fn lock_key_is_pinned_to_the_namespace_digest() {
        assert_eq!(DERIVE_LOCK_KEY, -5_268_553_599_984_485_251);
        assert_eq!(
            DERIVE_LOCK_KEY as u64,
            fnv1a64(DERIVE_LOCK_NAMESPACE.as_bytes())
        );
    }

    /// The cadence threshold must sit *below* the interval, or `derived_at`
    /// (stamped one derive-duration after the tick that produced it) would look
    /// too young on every tick and halve the real cadence forever.
    #[test]
    fn cadence_threshold_leaves_room_for_one_derive() {
        // Default 24 h cadence: the tolerance is the whole tick budget.
        let day = DEFAULT_DERIVE_INTERVAL_SECS;
        assert_eq!(
            cadence_threshold_secs(day),
            day - DERIVE_TICK_BUDGET.as_secs()
        );

        // At the floor the 10% cap binds instead, so a short configured cadence
        // can't be pulled to twice its rate by a large fixed tolerance.
        let floor = DERIVE_INTERVAL_FLOOR_SECS;
        assert_eq!(cadence_threshold_secs(floor), floor - floor / 10);

        // And it is always a real skip window, never zero or negative.
        for interval in [floor, 3_600, day] {
            let t = cadence_threshold_secs(interval);
            assert!(
                t > 0 && t < interval,
                "threshold {t} out of range for {interval}"
            );
        }
    }
}
