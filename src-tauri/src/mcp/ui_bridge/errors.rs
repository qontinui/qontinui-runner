//! Health, error snapshots/sessions/baselines, circuit-breaker & diagnostics.
//!
//! Extracted from `mod.rs` as part of the per-family handler split. Every
//! handler here is `pub` so the public re-exports preserved by `mod.rs`
//! (`pub use errors::*;`) keep `crate::mcp::ui_bridge::<name>` resolvable.
//!
//! The heavy-weight `diagnose_stuck_screen` and `page_health` handlers stay
//! in `mod.rs` for now — they drag a chain of private capture/screen helpers
//! with them that will move as part of a later screenshot/capture family.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::mcp::types::{ApiResponse, ApiState};

use super::request::{gather_readiness_diagnostics, ui_bridge_request_sync, wrap_ipc_result};

// ============================================================================
// Query / request types for the error-session / baseline endpoints
// ============================================================================

/// Query parameters for error snapshots endpoint
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorSnapshotsQuery {
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Request body for starting an error session
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorSessionStartRequest {
    #[serde(default)]
    pub label: Option<String>,
}

/// Request body for capturing/comparing an error baseline
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBaselineRequest {
    pub label: String,
}

// ============================================================================
// Combined health signals response
// ============================================================================

/// Combined UI Bridge health signals for stall detection integration.
#[derive(Debug, Serialize)]
pub struct UiBridgeHealthSignals {
    pub idle: serde_json::Value,
    pub stuck_screen: serde_json::Value,
}

// ============================================================================
// Health / error-report / error-session / error-baseline handlers
// ============================================================================

/// Get a health report from the UI Bridge BrowserCapture.
pub async fn ui_bridge_get_health_report_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting health report");

    wrap_ipc_result(
        ui_bridge_request_sync(&state, "get_health_report", serde_json::json!({})).await,
    )
}

/// Get recent error snapshots from the UI Bridge BrowserCapture.
pub async fn ui_bridge_get_error_snapshots_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ErrorSnapshotsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting error snapshots");

    let payload = serde_json::json!({
        "params": {
            "limit": query.limit
        }
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "get_error_snapshots", payload).await)
}

/// Get a comprehensive error report from the UI Bridge.
pub async fn ui_bridge_get_error_report_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting error report");

    wrap_ipc_result(ui_bridge_request_sync(&state, "get_error_report", serde_json::json!({})).await)
}

/// Start an error monitoring session.
pub async fn ui_bridge_start_error_session_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<ErrorSessionStartRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Starting error session");

    let payload = serde_json::json!({
        "params": {
            "label": body.label
        }
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "start_error_session", payload).await)
}

/// End the active error monitoring session.
pub async fn ui_bridge_end_error_session_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Ending error session");

    wrap_ipc_result(
        ui_bridge_request_sync(&state, "end_error_session", serde_json::json!({})).await,
    )
}

/// Get all error sessions (completed and active).
pub async fn ui_bridge_get_error_sessions_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting error sessions");

    wrap_ipc_result(
        ui_bridge_request_sync(&state, "get_error_sessions", serde_json::json!({})).await,
    )
}

/// Capture an error baseline with a given label.
pub async fn ui_bridge_capture_error_baseline_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<ErrorBaselineRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Capturing error baseline '{}'", body.label);

    let payload = serde_json::json!({
        "params": {
            "label": body.label
        }
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "capture_error_baseline", payload).await)
}

/// Compare current errors against a previously captured baseline.
pub async fn ui_bridge_compare_error_baseline_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<ErrorBaselineRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Comparing error baseline '{}'", body.label);

    let payload = serde_json::json!({
        "params": {
            "label": body.label
        }
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "compare_error_baseline", payload).await)
}

// ============================================================================
// Circuit breaker / diagnostics / readiness
// ============================================================================

/// Manually reset the circuit breaker.
pub async fn ui_bridge_circuit_breaker_reset_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Circuit breaker manual reset");
    state.ui_bridge_circuit_breaker.reset().await;
    Ok(Json(ApiResponse::success(serde_json::json!({
        "reset": true,
        "state": "Closed"
    }))))
}

/// UI Bridge diagnostics endpoint.
pub async fn ui_bridge_diagnostics_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    use tauri::Manager;
    info!("UI Bridge API: Diagnostics");

    let cb_state = state.ui_bridge_circuit_breaker.get_state().await;
    let failure_count = state.ui_bridge_circuit_breaker.get_failure_count().await;
    let available_permits = state.ui_bridge_semaphore.available_permits();
    let last_pong = state
        .ui_bridge_last_pong
        .load(std::sync::atomic::Ordering::Relaxed);
    let pending_count = state
        .ui_bridge_pending_count
        .load(std::sync::atomic::Ordering::Relaxed);
    let console_error_count = state
        .ui_bridge_console_error_count
        .load(std::sync::atomic::Ordering::Relaxed);
    let process_uptime_ms = state.started_at.elapsed().as_millis() as u64;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let last_pong_age_ms = if last_pong > 0 { now_ms - last_pong } else { 0 };

    // Check Tauri main window state
    let main_window = state
        .app_handle
        .get_webview_window(qontinui_runner_lib::get_main_window_label());
    let window_exists = main_window.is_some();
    let window_visible = main_window
        .as_ref()
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false);
    let webview_url = main_window
        .as_ref()
        .and_then(|w| w.url().ok())
        .map(|u| u.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let ready = last_pong > 0;
    let readiness_hint = if !window_exists {
        "Main WebView window does not exist — window creation may have failed"
    } else if !ready && process_uptime_ms < 3000 {
        "Process just started — frontend may still be loading"
    } else if !ready && console_error_count > 0 {
        "Frontend never sent pong and console errors recorded — likely crashed during mount"
    } else if !ready && !window_visible {
        "Frontend never sent pong and main window not visible — WebView may not have rendered"
    } else if !ready {
        "Frontend never sent initial pong — WebView may not have loaded"
    } else if last_pong_age_ms > 30000 {
        "Frontend stopped responding over 30s ago — may have crashed or frozen"
    } else {
        "Frontend is responsive"
    };

    Ok(Json(ApiResponse::success(serde_json::json!({
        "circuitBreaker": {
            "state": format!("{:?}", cb_state),
            "failuresInWindow": failure_count
        },
        "semaphore": {
            "availablePermits": available_permits,
            "maxPermits": 6
        },
        "frontend": {
            "lastPongTimestamp": last_pong,
            "lastPongAgeMs": last_pong_age_ms,
            "ready": ready,
            "sdkConnected": ready,
            "consoleErrorCount": console_error_count
        },
        "tauriMainWindow": {
            "exists": window_exists,
            "visible": window_visible,
            "webviewUrl": webview_url
        },
        "pendingRequestCount": pending_count,
        "processUptimeMs": process_uptime_ms,
        "readiness": {
            "ready": ready,
            "hint": readiness_hint
        }
    }))))
}

/// Proactive readiness check endpoint.
/// Agents can call this before issuing real requests to check if the frontend is ready.
pub async fn ui_bridge_readiness_handler(
    State(state): State<Arc<ApiState>>,
) -> (StatusCode, Json<serde_json::Value>) {
    let diag = gather_readiness_diagnostics(&state).await;
    let last_pong = state
        .ui_bridge_last_pong
        .load(std::sync::atomic::Ordering::Relaxed);

    if last_pong > 0 {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let age_ms = now_ms.saturating_sub(last_pong);
        (
            StatusCode::OK,
            Json(serde_json::json!({
                "ready": true,
                "sdk_connected": true,
                "last_pong_age_ms": age_ms,
                "uptime_ms": state.started_at.elapsed().as_millis() as u64,
                "lastPongMs": last_pong
            })),
        )
    } else {
        // When the frontend is not ready and the process has been up for >30s,
        // attach a native screenshot + boot errors so agents can diagnose the
        // webview state without needing a separate call.
        let uptime_ms = state.started_at.elapsed().as_millis() as u64;
        let mut result = diag;
        if uptime_ms >= 30_000 {
            if let Some((screenshot, width, height)) =
                super::capture_runner_window_base64(&state).await
            {
                if let Some(obj) = result.as_object_mut() {
                    obj.insert(
                        "diagnosticScreenshot".to_string(),
                        serde_json::json!({
                            "screenshot": screenshot,
                            "width": width,
                            "height": height,
                        }),
                    );
                }
            }
        }
        (StatusCode::SERVICE_UNAVAILABLE, Json(result))
    }
}

// ============================================================================
// Idle status / idle signals
// ============================================================================

/// Get idle status (loading indicators, network activity, etc.)
pub async fn ui_bridge_get_idle_status_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting idle status");

    wrap_ipc_result(ui_bridge_request_sync(&state, "get_idle_status", serde_json::json!({})).await)
}

/// Get a specific named idle signal (e.g. "network", "animations").
pub async fn ui_bridge_get_idle_signal_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(signal): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get idle signal '{}'", signal);
    let payload = serde_json::json!({ "params": { "signal": signal } });
    wrap_ipc_result(ui_bridge_request_sync(&state, "get_idle_signal", payload).await)
}

// ============================================================================
// Combined health signals
// ============================================================================

/// Get combined health signals from the UI Bridge SDK.
///
/// Combines idle status and stuck screen diagnosis into a single response
/// for use by the stall detection system.
pub async fn ui_bridge_health_signals_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<UiBridgeHealthSignals>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Fetch idle status and stuck screen diagnosis in parallel
    let idle_future = ui_bridge_request_sync(&state, "get_idle_status", serde_json::json!({}));
    let stuck_future = ui_bridge_request_sync(
        &state,
        "diagnose_stuck_screen",
        serde_json::json!({"observationWindowMs": 2000}),
    );

    let (idle_result, stuck_result) = tokio::join!(idle_future, stuck_future);

    let idle = idle_result.unwrap_or_else(|e| {
        warn!("Failed to get idle status: {}", e);
        serde_json::json!({"error": e})
    });

    let stuck_screen = stuck_result.unwrap_or_else(|e| {
        warn!("Failed to diagnose stuck screen: {}", e);
        serde_json::json!({"error": e})
    });

    Ok(Json(ApiResponse::success(UiBridgeHealthSignals {
        idle,
        stuck_screen,
    })))
}

// ============================================================================
// Route registration
// ============================================================================

/// Health / error / diagnostic routes.
///
/// Note: `ui_bridge_diagnose_stuck_screen_handler` and
/// `ui_bridge_page_health_handler` are still registered in `mod.rs::routes()`
/// because their private native-capture helpers have not moved yet.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use super::routing::add_dual;
    use axum::routing::{get, post};
    let router = axum::Router::new()
        .route(
            "/ui-bridge/control/error-snapshots",
            get(ui_bridge_get_error_snapshots_handler),
        )
        .route(
            "/ui-bridge/control/error-report",
            get(ui_bridge_get_error_report_handler),
        )
        .route(
            "/ui-bridge/control/error-sessions/start",
            post(ui_bridge_start_error_session_handler),
        )
        .route(
            "/ui-bridge/control/error-sessions/end",
            post(ui_bridge_end_error_session_handler),
        )
        .route(
            "/ui-bridge/control/error-sessions",
            get(ui_bridge_get_error_sessions_handler),
        )
        .route(
            "/ui-bridge/control/error-baselines/capture",
            post(ui_bridge_capture_error_baseline_handler),
        )
        .route(
            "/ui-bridge/control/error-baselines/compare",
            post(ui_bridge_compare_error_baseline_handler),
        )
        .route(
            "/ui-bridge/control/health",
            get(ui_bridge_get_health_report_handler),
        )
        .route(
            "/ui-bridge/circuit-breaker/reset",
            post(ui_bridge_circuit_breaker_reset_handler),
        )
        .route("/ui-bridge/diagnostics", get(ui_bridge_diagnostics_handler))
        .route(
            "/ui-bridge/diagnostics/readiness",
            get(ui_bridge_readiness_handler),
        )
        .route(
            "/ui-bridge/control/idle-status/{signal}",
            get(ui_bridge_get_idle_signal_handler),
        )
        .route(
            "/ui-bridge/control/health-signals",
            get(ui_bridge_health_signals_handler),
        );
    // idle-status: identical handler under /control + /ai.
    add_dual!(
        router,
        get,
        "idle-status",
        ui_bridge_get_idle_status_handler
    )
}

/// Static (method, path) tuples matching every route registered by `routes()`.
pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/control/error-snapshots"),
        ("GET", "/ui-bridge/control/error-report"),
        ("POST", "/ui-bridge/control/error-sessions/start"),
        ("POST", "/ui-bridge/control/error-sessions/end"),
        ("GET", "/ui-bridge/control/error-sessions"),
        ("POST", "/ui-bridge/control/error-baselines/capture"),
        ("POST", "/ui-bridge/control/error-baselines/compare"),
        ("GET", "/ui-bridge/control/health"),
        ("POST", "/ui-bridge/circuit-breaker/reset"),
        ("GET", "/ui-bridge/diagnostics"),
        ("GET", "/ui-bridge/diagnostics/readiness"),
        ("GET", "/ui-bridge/control/idle-status"),
        ("GET", "/ui-bridge/ai/idle-status"),
        ("GET", "/ui-bridge/control/idle-status/{signal}"),
        ("GET", "/ui-bridge/control/health-signals"),
    ]
}
