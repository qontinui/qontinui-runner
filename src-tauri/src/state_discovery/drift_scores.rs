//! HTTP handler for reading recent drift-score rows (Step 8 of the
//! observation pipeline plan).
//!
//! Kept intentionally separate from `drift.rs` (which hosts the long-running
//! background detector task) so the HTTP surface and the ticker can evolve
//! independently. Registered via `state_discovery::routes()`.
//!
//! Returns the most recent `state_discovery_drift_scores` rows — optionally
//! filtered by `spec_id` — sorted by `computed_at DESC`. `limit` is clamped
//! to `[1, MAX_LIMIT]` so a stray `?limit=100000` can't DOS the DB.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use tracing::error;

use crate::mcp::types::{api_error, ApiResponse, ApiState};

/// Upper bound on the number of rows returned. The UI only renders the most
/// recent widget and a short sparkline — it does not need the full history.
const MAX_LIMIT: i64 = 100;
/// Default row count when the caller omits `limit`.
const DEFAULT_LIMIT: i64 = 20;

#[derive(Debug, Deserialize)]
pub struct DriftScoresQuery {
    pub spec_id: Option<String>,
    pub limit: Option<i64>,
}

/// GET /state-discovery/drift-scores
///
/// Query params:
/// - `spec_id` — optional; if omitted, matches `WHERE spec_id IS NULL` to
///   mirror the current drift-detector which writes NULL until SM configs
///   carry spec_id natively.
/// - `limit` — optional; clamped to [1, MAX_LIMIT], default DEFAULT_LIMIT.
pub async fn list_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<DriftScoresQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    let conn = match state.app_state.pg_db.pool().get().await {
        Ok(c) => c,
        Err(e) => {
            error!("drift-scores: PG pool error: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("PG pool error: {}", e))),
            ));
        }
    };

    let rows_result = if let Some(ref sid) = query.spec_id {
        conn.query(
            r#"SELECT id, spec_id, computed_at, window_size,
                      fit_score, observations_considered, states_matched
               FROM state_discovery_drift_scores
               WHERE spec_id = $1
               ORDER BY computed_at DESC
               LIMIT $2::bigint"#,
            &[sid, &limit],
        )
        .await
    } else {
        conn.query(
            r#"SELECT id, spec_id, computed_at, window_size,
                      fit_score, observations_considered, states_matched
               FROM state_discovery_drift_scores
               WHERE spec_id IS NULL
               ORDER BY computed_at DESC
               LIMIT $1::bigint"#,
            &[&limit],
        )
        .await
    };

    let rows = match rows_result {
        Ok(r) => r,
        Err(e) => {
            error!("drift-scores: PG query error: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("PG query error: {}", e))),
            ));
        }
    };

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let id: i64 = row.get(0);
        let spec_id: Option<String> = row.get(1);
        let computed_at: chrono::DateTime<chrono::Utc> = row.get(2);
        let window_size: i32 = row.get(3);
        let fit_score: f64 = row.get(4);
        let observations_considered: i32 = row.get(5);
        let states_matched: i32 = row.get(6);

        out.push(serde_json::json!({
            "id": id,
            "spec_id": spec_id,
            "computed_at": computed_at.to_rfc3339(),
            "window_size": window_size,
            "fit_score": fit_score,
            "observations_considered": observations_considered,
            "states_matched": states_matched,
        }));
    }

    Ok(Json(ApiResponse::success(serde_json::json!(out))))
}
