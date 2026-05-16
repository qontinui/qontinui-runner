//! HTTP surface for the D4+D6 Blind-Spot Recommender (Phase 2).
//!
//! Mirror of `git_supervision_api.rs`: callers that can't subscribe to a
//! Tauri channel (curl smoke tests, headless UI Bridge probes, the
//! `/manual-test` slash command) poll `GET /blind-spots` and get the
//! recommender's full output.
//!
//! The endpoint is a pure read: it snapshots the supervision demand ring,
//! reads the observer registry (a read-through facade — no watchers
//! started), reads persisted git-supervised provenance, scores every blind
//! region, and returns the list sorted by `score` descending. No write
//! path exists — closing a blind spot is an operator/agent action driven by
//! the returned `recommendation`, not a side effect of this endpoint.

use axum::{extract::State, routing::get, Json, Router};
use std::sync::Arc;

use crate::blind_spots::{compute_scored_blind_spots, ScoredBlindSpot};
use crate::mcp::types::{ApiResponse, ApiState};

/// Routes contributed to the runner's main router from `mcp_api.rs`.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/blind-spots", get(get_blind_spots))
}

/// GET /blind-spots — enumerate + score every observation blind spot,
/// sorted by information-value `score` (descending). Each entry carries the
/// blind region, the score (with per-source breakdown), the specific
/// members it would unblock, and a concrete remediation recommendation.
async fn get_blind_spots(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<ScoredBlindSpot>>> {
    let scored = compute_scored_blind_spots(
        &state.observer_registry,
        &state.supervision_state,
        &state.app_state.pg_db,
    )
    .await;
    Json(ApiResponse::success(scored))
}
