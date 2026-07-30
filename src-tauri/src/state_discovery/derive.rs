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
/// legitimate value for a test harness that wants an immediate derive.
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
/// Errors inside a tick log but never kill the loop.
pub async fn run_derive_loop(app_state: Arc<crate::commands::AppState>) {
    let interval_secs = derive_interval_secs();
    let initial_delay_secs = derive_initial_delay_secs();

    info!(
        "state-derive: starting global derive loop (interval={}s, initial_delay={}s, window_days={})",
        interval_secs,
        initial_delay_secs,
        derive_window_days()
    );

    // One-shot initial delay so PG bootstrap, the Python bridge warm-up and the
    // default-SM auto-load all finish before the first (heavy) derivation. The
    // drift detector achieves the same by discarding `interval`'s immediate
    // first tick; a 24 h cadence can't use that trick, because the first derive
    // would then be a day out on every restart.
    tokio::time::sleep(Duration::from_secs(initial_delay_secs)).await;

    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    // Explicit, NOT the `Burst` default: a tick that outruns the interval must
    // drop the missed tick rather than queue back-to-back clustering passes.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        // First tick resolves immediately — i.e. at `initial_delay_secs` after
        // start, never at process start.
        ticker.tick().await;

        let window_days = derive_window_days();
        let pg_db = app_state.pg_db.clone();
        // spec_id = None → global corpus, artifact persisted with spec_id NULL.
        match tokio::time::timeout(
            DERIVE_TICK_BUDGET,
            derive(pg_db, app_state.clone(), None, window_days),
        )
        .await
        {
            Ok(Ok(v)) => {
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
            Ok(Err(e)) => warn!("state-derive: tick failed: {}", e),
            Err(_) => warn!(
                "state-derive: tick exceeded {}s budget; skipping",
                DERIVE_TICK_BUDGET.as_secs()
            ),
        }
    }
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
}
