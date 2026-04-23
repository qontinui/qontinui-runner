//! Network capture, console-error, browser-event, timeline, and
//! network-request-monitoring HTTP handlers.
//!
//! Extracted from `mod.rs` as part of the per-family handler split. Every
//! handler is `pub` so `mod.rs`'s `pub use network::*;` re-export keeps
//! `crate::mcp::ui_bridge::<name>` resolvable for external callers.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use tracing::{error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::helpers::deserialize_timestamp;
use super::request::{ui_bridge_request_sync, wrap_ipc_result};

// ============================================================================
// Query types
// ============================================================================

/// Query parameters for console errors endpoint.
/// Accepts `since` as either numeric (epoch ms) or ISO 8601 string.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleErrorsQuery {
    #[serde(default, deserialize_with = "deserialize_timestamp")]
    since: Option<f64>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    group: Option<bool>,
    #[serde(default)]
    group_by: Option<String>,
}

/// Query parameters for browser events endpoint
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserEventsQuery {
    #[serde(default, rename = "type")]
    event_type: Option<String>,
    #[serde(default, deserialize_with = "deserialize_timestamp")]
    since: Option<f64>,
    #[serde(default)]
    limit: Option<u32>,
}

/// Query parameters for timeline endpoint
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineQuery {
    #[serde(default, deserialize_with = "deserialize_timestamp")]
    since: Option<f64>,
    #[serde(default)]
    limit: Option<u32>,
}

/// Query parameters for network chains endpoint
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkChainsQuery {
    #[serde(default)]
    limit: Option<u32>,
}

/// Query parameters for network requests endpoint
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkRequestsQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    url_pattern: Option<String>,
    #[serde(default)]
    failures_only: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_timestamp")]
    since: Option<f64>,
    #[serde(default)]
    limit: Option<u32>,
}

// ============================================================================
// Console errors
// ============================================================================

/// Get console errors captured by the UI Bridge ConsoleCapture.
pub async fn ui_bridge_get_console_errors_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ConsoleErrorsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting console errors");

    let payload = serde_json::json!({
        "params": {
            "since": query.since,
            "limit": query.limit,
            "group": query.group,
            "groupBy": query.group_by
        }
    });

    let is_grouped = query.group.unwrap_or(false);

    match ui_bridge_request_sync(&state, "get_console_errors", payload).await {
        Ok(data) => {
            // Update the console error count for the health endpoint
            if !is_grouped {
                if let Some(errors) = data.get("errors").and_then(|e| e.as_array()) {
                    state
                        .ui_bridge_console_error_count
                        .store(errors.len() as u64, std::sync::atomic::Ordering::Relaxed);
                }
            } else if let Some(total) = data.get("totalErrors").and_then(|t| t.as_u64()) {
                state
                    .ui_bridge_console_error_count
                    .store(total, std::sync::atomic::Ordering::Relaxed);
            }
            Ok(Json(ApiResponse::success(data)))
        }
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Clear console errors captured by the UI Bridge ConsoleCapture.
pub async fn ui_bridge_clear_console_errors_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Clearing console errors");

    match ui_bridge_request_sync(&state, "clear_console_errors", serde_json::json!({})).await {
        Ok(data) => {
            state
                .ui_bridge_console_error_count
                .store(0, std::sync::atomic::Ordering::Relaxed);
            Ok(Json(ApiResponse::success(data)))
        }
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Browser events / timeline
// ============================================================================

/// Get browser events captured by the UI Bridge BrowserCapture.
pub async fn ui_bridge_get_browser_events_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<BrowserEventsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting browser events");

    let payload = serde_json::json!({
        "params": {
            "type": query.event_type,
            "since": query.since,
            "limit": query.limit
        }
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "get_browser_events", payload).await)
}

/// Get timeline events captured by the UI Bridge BrowserCapture.
pub async fn ui_bridge_get_timeline_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TimelineQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting timeline");

    let payload = serde_json::json!({
        "params": {
            "since": query.since,
            "limit": query.limit
        }
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "get_timeline", payload).await)
}

// ============================================================================
// Network chains
// ============================================================================

/// Get network request chains from the UI Bridge BrowserCapture.
pub async fn ui_bridge_get_network_chains_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<NetworkChainsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting network chains");

    let payload = serde_json::json!({
        "params": {
            "limit": query.limit
        }
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "get_network_chains", payload).await)
}

// ============================================================================
// Network request monitoring
// ============================================================================

/// List network requests with optional filters.
pub async fn ui_bridge_get_network_requests_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<NetworkRequestsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting network requests");

    let payload = serde_json::json!({
        "params": {
            "status": query.status,
            "method": query.method,
            "urlPattern": query.url_pattern,
            "failuresOnly": query.failures_only,
            "since": query.since,
            "limit": query.limit
        }
    });

    match ui_bridge_request_sync(&state, "get_network_requests", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get currently in-flight network requests.
pub async fn ui_bridge_get_network_requests_in_flight_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting in-flight network requests");

    match ui_bridge_request_sync(
        &state,
        "get_network_requests_in_flight",
        serde_json::json!({}),
    )
    .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Wait for a specific network request matching criteria.
pub async fn ui_bridge_wait_for_network_request_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Wait for network request");

    match ui_bridge_request_sync(&state, "wait_for_network_request", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a specific network request by ID.
pub async fn ui_bridge_get_network_request_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting network request {}", id);

    match ui_bridge_request_sync(
        &state,
        "get_network_request",
        serde_json::json!({ "id": id }),
    )
    .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Route registration
// ============================================================================

/// Network, console-error, browser-event, timeline, and network-request
/// routes.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/ui-bridge/control/console-errors",
            get(ui_bridge_get_console_errors_handler),
        )
        .route(
            "/ui-bridge/control/console-errors/clear",
            post(ui_bridge_clear_console_errors_handler),
        )
        .route(
            "/ui-bridge/control/browser-events",
            get(ui_bridge_get_browser_events_handler),
        )
        .route(
            "/ui-bridge/control/timeline",
            get(ui_bridge_get_timeline_handler),
        )
        .route(
            "/ui-bridge/control/network-chains",
            get(ui_bridge_get_network_chains_handler),
        )
        .route(
            "/ui-bridge/control/network-requests",
            get(ui_bridge_get_network_requests_handler),
        )
        .route(
            "/ui-bridge/control/network-requests/in-flight",
            get(ui_bridge_get_network_requests_in_flight_handler),
        )
        .route(
            "/ui-bridge/control/network-requests/wait",
            post(ui_bridge_wait_for_network_request_handler),
        )
        .route(
            "/ui-bridge/control/network-request/{id}",
            get(ui_bridge_get_network_request_handler),
        )
}

/// Static (method, path) tuples matching every route registered by `routes()`.
pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/control/console-errors"),
        ("POST", "/ui-bridge/control/console-errors/clear"),
        ("GET", "/ui-bridge/control/browser-events"),
        ("GET", "/ui-bridge/control/timeline"),
        ("GET", "/ui-bridge/control/network-chains"),
        ("GET", "/ui-bridge/control/network-requests"),
        ("GET", "/ui-bridge/control/network-requests/in-flight"),
        ("POST", "/ui-bridge/control/network-requests/wait"),
        ("GET", "/ui-bridge/control/network-request/{id}"),
    ]
}
