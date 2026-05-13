//! HTTP surface for the D5 Phase 1 Git Supervision Channel.
//!
//! Mirror of the Tauri `git-supervision` event channel: callers that can't
//! subscribe to Tauri events (curl-based smoke tests, headless UI Bridge
//! probes, the `/manual-test` slash command) can poll
//! `GET /git-supervision/recent-events` and get the same payload shape.
//!
//! The primary write path is the `trigger_system` `SupervisionProposal`
//! dispatch (`crate::git_supervision::handle_supervision_action`). A second
//! synthetic-emit endpoint (`POST /git-supervision/test-emit`) is exposed
//! for smoke tests and operator diagnostics — it bypasses the trigger
//! system entirely and writes directly into the ring + Tauri channel.

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::git_supervision::GitSupervisionEvent;
use crate::mcp::types::{ApiResponse, ApiState};

/// Routes contributed to the runner's main router from `mcp_api.rs`.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route(
            "/git-supervision/recent-events",
            get(get_recent_supervision_events),
        )
        .route(
            "/git-supervision/test-emit",
            post(post_test_emit_supervision_event),
        )
}

/// Return the current in-process ring buffer of supervision events (oldest
/// → newest, capped at `git_supervision::RING_CAPACITY`).
async fn get_recent_supervision_events(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<GitSupervisionEvent>>> {
    let events = state.supervision_state.recent_events().await;
    Json(ApiResponse::success(events))
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct TestEmitRequest {
    /// Event kind to record. Defaults to `"commit"`.
    pub kind: Option<String>,
    /// Free-form payload to attach. Defaults to an empty object.
    pub payload: Option<serde_json::Value>,
    /// Provenance tag. Defaults to `"git-supervised"`.
    pub provenance: Option<String>,
}

/// POST /git-supervision/test-emit — synthesize a supervision event without
/// going through the trigger system. Useful for smoke-testing the
/// supervision channel (Tauri emit + ring buffer + frontend hook + UI
/// Bridge surface) when the trigger system is unavailable or its bootstrap
/// path is degraded.
///
/// Body fields are all optional; defaults emit a minimal `commit` event
/// with empty payload and `provenance: "git-supervised"`.
async fn post_test_emit_supervision_event(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<TestEmitRequest>,
) -> Json<ApiResponse<GitSupervisionEvent>> {
    let kind = req.kind.unwrap_or_else(|| "commit".to_string());
    let payload = req.payload.unwrap_or_else(|| serde_json::json!({}));
    let provenance = req
        .provenance
        .unwrap_or_else(|| "git-supervised".to_string());
    let event = GitSupervisionEvent::new(kind, payload, provenance);
    state
        .supervision_state
        .record_event(event.clone(), &state.app_handle)
        .await;
    Json(ApiResponse::success(event))
}
