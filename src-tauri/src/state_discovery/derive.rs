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
const DEFAULT_WINDOW_DAYS: i32 = 90;

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

    // $1::uuid cast — tokio-postgres lacks the uuid feature here, so we
    // pass the id as text and let PG coerce.
    conn.execute(
        r#"INSERT INTO state_discovery_artifacts
           (id, spec_id, window_days, artifact, observation_count)
           VALUES ($1::uuid, $2, $3, $4::jsonb, $5)"#,
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
