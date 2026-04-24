//! GET /ui-bridge/control/toasts — transient-banner ring-buffer query.
//!
//! Proxies through the IPC channel to the SDK-side `ToastRingBuffer`, which
//! captures every emitted toast lifecycle event. Tests can query after the
//! fact even if the banner has auto-dismissed.
//!
//! Falls back to an empty list (with `success: true`) if the SDK isn't
//! connected — tests should treat this as "no toasts emitted yet."

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
    Router,
};
use serde::Deserialize;
use tracing::{debug, info};

use crate::mcp::types::{ApiResponse, ApiState};

use super::request::ui_bridge_request_sync;

// ============================================================================
// Types
// ============================================================================

/// Query parameters for GET /ui-bridge/control/toasts.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToastBufferQuery {
    /// Epoch-ms lower bound; entries with `timestamp < sinceMs` are
    /// excluded. Absent means "all retained entries."
    #[serde(default)]
    pub since_ms: Option<i64>,
    /// Max entries to return. Ring buffer caps at 200 entries / 10-minute
    /// TTL SDK-side regardless of this value.
    #[serde(default)]
    pub limit: Option<u32>,
}

// ============================================================================
// Handler
// ============================================================================

/// GET /ui-bridge/control/toasts[?sinceMs=...&limit=50]
///
/// Returns `{ toasts: [{id, message, kind, timestamp, dismissedAt?}] }` from
/// the SDK's ring buffer. When the SDK hasn't connected yet, returns an empty
/// list instead of 503 — matches the plan's spec ("don't 503").
pub async fn ui_bridge_control_toasts_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ToastBufferQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: GET /control/toasts sinceMs={:?} limit={:?}",
        query.since_ms, query.limit
    );

    // Short-circuit when the SDK isn't connected. Matches the plan: "if the
    // SDK isn't connected, return an empty list with success: true."
    let last_pong = state
        .ui_bridge_last_pong
        .load(std::sync::atomic::Ordering::Relaxed);
    if last_pong == 0 {
        debug!("control/toasts: SDK not connected, returning empty list");
        return Ok(Json(ApiResponse::success(serde_json::json!({
            "toasts": [],
            "note": "SDK not connected",
        }))));
    }

    let payload = serde_json::json!({
        "params": {
            "sinceMs": query.since_ms,
            "limit": query.limit,
        }
    });

    match ui_bridge_request_sync(&state, "get_toast_buffer", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            // Frontend IPC error -- return an empty list rather than
            // 500ing so polling tests don't break on a transient bridge
            // hiccup. Surface the error via `note` for debugging.
            debug!("control/toasts: IPC error, returning empty list: {}", e);
            Ok(Json(ApiResponse::success(serde_json::json!({
                "toasts": [],
                "note": format!("IPC error: {}", e),
            }))))
        }
    }
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route(
        "/ui-bridge/control/toasts",
        get(ui_bridge_control_toasts_handler),
    )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[("GET", "/ui-bridge/control/toasts")]
}
