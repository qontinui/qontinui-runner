//! HTTP surface for the D4+D6 Blind-Spot Recommender (Phase 2).
//!
//! Mirror of `git_supervision_api.rs`: callers that can't subscribe to a
//! Tauri channel (curl smoke tests, headless UI Bridge probes, an operator
//! reading the recommender from outside the app) poll `GET /blind-spots` and
//! get the recommender's full output.
//!
//! It has no in-repo consumer. `BlindSpotsPanel`, the one webview reader, went
//! with the board in Phase 4 of
//! `2026-09-12-consolidate-local-orchestration-onto-conductor`, and the route
//! is deliberately kept: it is the only door onto a recommender
//! (`blind_spots::compute_scored_blind_spots`) this phase does NOT delete, and
//! a live subsystem with no way to read it is worse than an unused route.
//! Retiring it is a separate decision about the HTTP surface.
//!
//! This header used to cite the `/manual-test` slash command as a poller. That
//! could not be confirmed — no `fleet_commands/*.md` mentions `/blind-spots` —
//! so the claim is dropped rather than repeated. (`git_supervision_api.rs`
//! carries the same unverified citation for its own route; that file is
//! untouched here.)
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
