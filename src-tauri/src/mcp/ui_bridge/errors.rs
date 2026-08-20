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

use super::request::{
    classify_frontend_state, gather_readiness_diagnostics, gather_ui_error_signals,
    ui_bridge_request_sync, wrap_ipc_result, FrontendState, FrontendStateInputs, TREE_CRASHED_HINT,
};

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
        .app_state
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
    // `saturating_sub`, not `-`: `last_pong` is a wall-clock stamp, so a
    // backwards clock step (NTP, sleep/resume) can leave it ahead of `now_ms`
    // and a plain subtraction underflows and panics. A "future" pong reads as
    // age 0 — maximally fresh, the honest answer. This age now also feeds
    // `derived_status`, so it must not be able to take the handler down.
    let last_pong_age_ms = if last_pong > 0 {
        now_ms.saturating_sub(last_pong)
    } else {
        0
    };

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

    // Join the Rust-side crash state in. `/health` and every heartbeat have
    // always carried it; the UI-Bridge routes never did, so a driver looking
    // at UI-Bridge routes could not see the throw the runner was holding.
    let ui_error = gather_ui_error_signals(&state, last_pong, last_pong_age_ms).await;

    let frontend_state = classify_frontend_state(FrontendStateInputs {
        window_exists,
        window_visible,
        last_pong,
        last_pong_age_ms,
        console_error_count,
        process_uptime_ms,
        has_ui_error: ui_error.present,
    });
    // `last_pong > 0` alone is a latch that never flips back, so it reported a
    // crashed-but-still-ponging tree as ready. `is_ready` forces false on the
    // crashed path.
    let ready = frontend_state.is_ready(last_pong);
    let readiness_hint = match frontend_state {
        FrontendState::WindowMissing => {
            "Main WebView window does not exist — window creation may have failed"
        }
        FrontendState::Booting => "Process just started — frontend may still be loading",
        FrontendState::CrashedDuringMount => {
            "Frontend never sent pong and console errors recorded — likely crashed during mount"
        }
        FrontendState::WindowNotVisible => {
            "Frontend never sent pong and main window not visible — WebView may not have rendered"
        }
        FrontendState::NeverPonged => {
            "Frontend never sent initial pong — WebView may not have loaded"
        }
        FrontendState::TreeCrashed => TREE_CRASHED_HINT,
        FrontendState::Stale => {
            "Frontend stopped responding over 30s ago — may have crashed or frozen"
        }
        FrontendState::Responsive => "Frontend is responsive",
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
        // Rust-side crash state — null / "healthy" when the React tree is fine.
        // Mirrors the fields `/health` and the heartbeat already publish.
        "uiError": ui_error.json,
        "derivedStatus": ui_error.derived_status,
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
        .app_state
        .ui_bridge_last_pong
        .load(std::sync::atomic::Ordering::Relaxed);

    // Reuse the verdict `gather_readiness_diagnostics` already computed rather
    // than re-deriving `last_pong > 0` here. That expression is a latch: it
    // reported 200 "ready" for a React tree that had crashed under the error
    // boundary while its pong loop kept running. 503 is the correct answer on
    // that path, and the 503 body carries `uiError` + the crash hint.
    if diag.sdk_connected {
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
        let mut result = diag.body;
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

// ============================================================================
// Native event-loop unresponsiveness
// ============================================================================
//
// Plan `2026-08-19-runner-blocked-ui-thread-cannot-be-closed`, Phase 3 step 1.
//
// # Why this needed a coded body rather than the flat one
//
// There was **no coded-503 precedent in this route family**. The one surface
// that could previously express "the UI thread is wedged" —
// `/health/diagnostic-screenshot` — returns a flat, untyped
// `{success:false, error:"…"}`, and the only coded 503 nearby is
// [`UiBridgeErrorCode::FrontendNotReady`]. Both readings are wrong for this
// condition:
//
// * the flat shape carries no machine-readable code at all, so an agent has to
//   pattern-match prose to learn what happened;
// * `FRONTEND_NOT_READY` says the frontend is still booting, which invites a
//   readiness retry. In this failure the frontend is usually **healthy** — it
//   is the native Win32 message loop underneath it that is wedged — so that
//   retry can never succeed.
//
// Hence [`UiBridgeErrorCode::EventLoopUnresponsive`] and the builder below.
// The variant itself has to live in `types.rs` beside the enum it belongs to;
// everything about *this* condition lives here.

use super::types::{RecoveryHint, UiBridgeError, UiBridgeErrorCode};

impl UiBridgeError {
    /// Body for a route that cannot honour a request because the native event
    /// loop is not pumping.
    ///
    /// `reason` is a stable, machine-readable discriminator for *how* the
    /// verdict was reached (see `page::EventLoopVerdict`), so a caller can tell
    /// "the wedge detector has latched" from "the probe could not answer" —
    /// both refuse the request, but only the first is a confirmed hang.
    ///
    /// `context.escapeHatch` names the door that still works in this state.
    /// That is not decoration: the whole defect being fixed is a surface that
    /// reported success and left the caller with nowhere to go.
    pub fn event_loop_unresponsive(reason: &str, confirmed: bool) -> Self {
        Self {
            code: UiBridgeErrorCode::EventLoopUnresponsive,
            message: if confirmed {
                "The runner's native event loop is not pumping messages; the request would \
                 queue behind the wedge instead of running"
                    .to_string()
            } else {
                "The runner could not verify that its native event loop is pumping messages; \
                 refusing rather than reporting a success it cannot confirm"
                    .to_string()
            },
            recovery: Some(RecoveryHint::WaitForRecovery),
            context: Some(serde_json::json!({
                "code": "EVENT_LOOP_UNRESPONSIVE",
                "reason": reason,
                "confirmed": confirmed,
                "escapeHatch": "POST /ui-bridge/control/page/force-close",
                // The escape hatch is CSRF-gated: it requires this header
                // (any value), which forces a CORS preflight a drive-by
                // cross-origin POST cannot satisfy. Named here so a caller
                // reading the refusal has everything it needs to act on it.
                "escapeHatchHeader": "X-Qontinui-Force-Close: 1",
            })),
        }
    }
}

#[cfg(test)]
mod event_loop_unresponsive_tests {
    //! The wire contract of the coded 503 the close door returns during a
    //! native-event-loop hang.

    use super::super::types::{recovery_hint_for, RecoveryHint, UiBridgeError, UiBridgeErrorCode};

    /// The whole reason for a new variant rather than reusing
    /// `FRONTEND_NOT_READY` is that an agent must be able to discriminate on
    /// the code. If the serialized string ever changes, every caller that
    /// matched on it silently falls through to its generic error path.
    #[test]
    fn code_serializes_to_the_documented_screaming_snake_string() {
        let err = UiBridgeError::event_loop_unresponsive("wedge_detector_latched", true);
        let body = serde_json::to_value(&err).expect("serialize");
        assert_eq!(body["code"], "EVENT_LOOP_UNRESPONSIVE");
    }

    /// `confirmed` is the honest half of the contract: a latched wedge and a
    /// probe that could not answer both refuse the request, but only the first
    /// is an observation. The two must stay distinguishable on the wire, and
    /// the prose must not claim more than the flag does.
    #[test]
    fn confirmed_flag_and_prose_agree_on_both_arms() {
        let observed = UiBridgeError::event_loop_unresponsive("probe_no_round_trip", true);
        let ctx = observed.context.as_ref().expect("context");
        assert_eq!(ctx["confirmed"], true);
        assert_eq!(ctx["reason"], "probe_no_round_trip");
        assert!(
            observed.message.contains("is not pumping"),
            "a confirmed hang should state it: {}",
            observed.message
        );

        let unverified = UiBridgeError::event_loop_unresponsive("probe_unavailable", false);
        let ctx = unverified.context.as_ref().expect("context");
        assert_eq!(ctx["confirmed"], false);
        assert_eq!(ctx["reason"], "probe_unavailable");
        assert!(
            unverified.message.contains("could not verify"),
            "an unverified refusal must not claim an observation: {}",
            unverified.message
        );
    }

    /// A refusal that names no way forward is the defect this route family had
    /// (a `200` that did nothing). The body must point at the door that still
    /// works while the loop is wedged.
    #[test]
    fn body_names_the_escape_hatch() {
        let err = UiBridgeError::event_loop_unresponsive("wedge_detector_latched", true);
        assert_eq!(
            err.context.as_ref().expect("context")["escapeHatch"],
            "POST /ui-bridge/control/page/force-close"
        );
    }

    /// NOT `RetryAfterMs`: a fixed backoff cannot clear a wedge, and each retry
    /// re-enqueues onto the same blocked queue. NOT `Unrecoverable` either: a
    /// long synchronous handler does eventually return.
    #[test]
    fn recovery_hint_is_wait_for_recovery() {
        assert!(matches!(
            recovery_hint_for(&UiBridgeErrorCode::EventLoopUnresponsive),
            RecoveryHint::WaitForRecovery
        ));
        let err = UiBridgeError::event_loop_unresponsive("wedge_detector_latched", true);
        assert!(matches!(err.recovery, Some(RecoveryHint::WaitForRecovery)));
    }
}
