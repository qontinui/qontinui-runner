//! Core IPC request machinery for the UI Bridge.
//!
//! Hosts the main `ui_bridge_request_sync` entrypoint used by every handler,
//! the response dispatcher called from the Tauri event listener, and helpers
//! for readiness diagnostics and error classification wrapping.

use axum::{http::StatusCode, response::Json};
use std::sync::Arc;
use tauri::Emitter;
use tracing::{debug, error, info, warn};

use crate::mcp::types::{api_error_detailed, ApiResponse, ApiState};
use crate::timeout_config::Timeouts;

use super::circuit_breaker::CircuitBreakerState;
use super::types::{classify_transport_error, UiBridgeErrorCode};

/// UI Bridge timeout is fetched from centralized config
/// This needs a reasonable timeout since it's synchronous communication with the frontend.
pub(super) fn get_ui_bridge_timeout_ms() -> u64 {
    Timeouts::ui_bridge_ipc().as_millis() as u64
}

/// Gather structured readiness diagnostics when the frontend readiness gate
/// times out. Returns a JSON object with all available diagnostic fields so
/// agents can diagnose why the WebView never became ready.
pub(super) async fn gather_readiness_diagnostics(state: &Arc<ApiState>) -> serde_json::Value {
    use tauri::Manager;

    let last_pong = state
        .ui_bridge_last_pong
        .load(std::sync::atomic::Ordering::Relaxed);
    let console_error_count = state
        .ui_bridge_console_error_count
        .load(std::sync::atomic::Ordering::Relaxed);
    let cb_state = state.ui_bridge_circuit_breaker.get_state().await;
    let cb_failures = state.ui_bridge_circuit_breaker.get_failure_count().await;
    let pending_count = state
        .ui_bridge_pending_count
        .load(std::sync::atomic::Ordering::Relaxed);
    let available_permits = state.ui_bridge_semaphore.available_permits();
    let process_uptime_ms = state.started_at.elapsed().as_millis() as u64;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

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
    let sdk_connected = last_pong > 0;
    let webview_url = main_window
        .as_ref()
        .and_then(|w| w.url().ok())
        .map(|u| u.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Build a human-readable hint based on the diagnostic state
    let hint = if !window_exists {
        "Main WebView window does not exist — window creation may have failed."
    } else if last_pong == 0 && process_uptime_ms < 3000 {
        "Process just started (<3s uptime). Frontend may still be loading — consider retrying."
    } else if last_pong == 0 && console_error_count > 0 {
        "Frontend never sent initial pong and console errors were recorded. Check the runner devtools console — frontend likely crashed during mount."
    } else if last_pong == 0 && !window_visible {
        "Frontend never sent initial pong and main window is not visible — WebView may not have rendered."
    } else if last_pong == 0 {
        "Frontend never sent initial pong. Check if the WebView loaded successfully."
    } else if last_pong_age_ms > 30000 {
        "Frontend was responsive but stopped responding over 30s ago. It may have crashed or frozen."
    } else {
        "Frontend was responsive recently but the readiness gate was not notified. Possible race condition."
    };

    // Try to read window.__BOOT_ERRORS from the webview (set by index.html's
    // error catcher script in <head>). This captures JS errors that occur during
    // IIFE bundle execution, before the UI Bridge SDK can initialize.
    let boot_errors: serde_json::Value = if let Some(ref win) = main_window {
        match win.eval("window.__QONTINUI_DIAG_CALLBACK && window.__QONTINUI_DIAG_CALLBACK(JSON.stringify(window.__BOOT_ERRORS || []))") {
            _ => {
                // eval() is fire-and-forget in Tauri v2; we can't get return values.
                // Instead, the readiness endpoint reads __BOOT_ERRORS via page/evaluate.
                serde_json::json!(null)
            }
        }
    } else {
        serde_json::json!(null)
    };
    let _ = boot_errors; // reserved for future use when Tauri eval returns values

    serde_json::json!({
        "error": "frontend_not_ready",
        "diagnostics": {
            "last_pong_age_ms": last_pong_age_ms,
            "window_visible": window_visible,
            "webview_url": webview_url,
            "sdk_connected": sdk_connected,
            "uptime_ms": process_uptime_ms,
            "hint": hint,
            "lastPongMs": last_pong,
            "consoleErrorCount": console_error_count,
            "circuitBreakerState": format!("{:?}", cb_state),
            "circuitBreakerFailures": cb_failures,
            "pendingRequestCount": pending_count,
            "semaphoreAvailablePermits": available_permits,
            "tauriMainWindowExists": window_exists,
            "bootErrorsNote": "Call GET /ui-bridge/control/page/evaluate with expression 'JSON.stringify(window.__BOOT_ERRORS)' to retrieve boot-time JS errors"
        }
    })
}

/// Send a UI Bridge request and wait for the response synchronously.
///
/// This creates a oneshot channel, stores the sender in the pending map,
/// emits the request to the frontend, and waits for the response with a timeout.
///
/// Includes circuit breaker, concurrency limiting, frontend liveness check,
/// and request deduplication for read-only operations.
pub async fn ui_bridge_request_sync(
    state: &Arc<ApiState>,
    request_type: &str,
    additional_payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    // 1. Check circuit breaker
    state.ui_bridge_circuit_breaker.check().await?;

    // 1.5. Wait for frontend readiness if no pong has ever been received.
    // This prevents the race condition where requests arrive before React's
    // event listeners are set up after a supervisor-triggered restart.
    {
        let pong_check = state
            .ui_bridge_last_pong
            .load(std::sync::atomic::Ordering::Relaxed);
        if pong_check == 0 {
            tracing::info!("UI Bridge: Waiting for frontend readiness (no pong received yet)");
            let ready_timeout = std::time::Duration::from_secs(10);
            if tokio::time::timeout(ready_timeout, state.ui_bridge_ready.notified())
                .await
                .is_err()
            {
                // Gather structured diagnostics instead of returning a bare string
                let diag = gather_readiness_diagnostics(state).await;
                return Err(serde_json::to_string(&diag).unwrap_or_else(|_| {
                    "UI Bridge: Frontend did not become ready within 10s (diagnostics serialization failed)".to_string()
                }));
            }
            tracing::info!("UI Bridge: Frontend is now ready");
        }
    }

    // 2. Check frontend liveness (warn if stale, but don't fail — let IPC timeout handle it)
    let last_pong = state
        .ui_bridge_last_pong
        .load(std::sync::atomic::Ordering::Relaxed);
    if last_pong > 0 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let pong_age = now - last_pong;
        if pong_age > 15000 {
            warn!(
                "UI Bridge: Frontend may be unresponsive (last pong {}ms ago)",
                pong_age
            );
        }
    }

    // 3. Check for dedup opportunity on read-only requests
    let dedup_key = match request_type {
        "get_elements" | "get_snapshot" | "get_components" => Some(request_type.to_string()),
        _ => None,
    };

    if let Some(ref key) = dedup_key {
        let dedup = state.ui_bridge_dedup.lock().await;
        if let Some(tx) = dedup.get(key) {
            // Subscribe to existing in-flight request
            let mut rx = tx.subscribe();
            drop(dedup);
            debug!("UI Bridge: Deduplicating {} request", key);
            // Apply the same timeout to dedup waits so stale entries don't block forever
            let timeout_ms = get_ui_bridge_timeout_ms();
            match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx.recv())
                .await
            {
                Ok(Ok(result)) => return result,
                Ok(Err(_)) => return Err("Dedup channel closed".to_string()),
                Err(_) => {
                    // Dedup wait timed out — remove stale entry and fall through
                    // to make a fresh request
                    warn!(
                        "UI Bridge: Dedup wait timed out for {}, removing stale entry and retrying",
                        key
                    );
                    let mut dedup_map = state.ui_bridge_dedup.lock().await;
                    dedup_map.remove(key);
                    // Fall through to make a fresh request below
                }
            }
        }
    }

    // 4. Acquire semaphore permit (max 6 concurrent, 2s timeout)
    let _permit = match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        state.ui_bridge_semaphore.acquire(),
    )
    .await
    {
        Ok(Ok(permit)) => permit,
        Ok(Err(_)) => return Err("UI Bridge semaphore closed".to_string()),
        Err(_) => {
            return Err(
                "UI Bridge concurrency limit reached (timeout acquiring permit)".to_string(),
            );
        }
    };

    // 5. Set up dedup broadcast for read-only requests
    let dedup_tx = if let Some(ref key) = dedup_key {
        let (tx, _) = tokio::sync::broadcast::channel(1);
        let mut dedup = state.ui_bridge_dedup.lock().await;
        dedup.insert(key.clone(), tx.clone());
        Some(tx)
    } else {
        None
    };

    // 6. Execute the actual request
    let result = ui_bridge_request_inner(state, request_type, additional_payload).await;

    // 7. Update circuit breaker and attempt recovery if it opens
    match &result {
        Ok(_) => state.ui_bridge_circuit_breaker.record_success().await,
        Err(e) if e.contains("timed out") => {
            state.ui_bridge_circuit_breaker.record_failure().await;
            // If circuit breaker just opened, attempt auto-recovery
            if state.ui_bridge_circuit_breaker.get_state().await == CircuitBreakerState::Open {
                state
                    .ui_bridge_circuit_breaker
                    .attempt_recovery(&state.app_handle);
            }
        }
        Err(_) => {} // Non-timeout errors don't trigger circuit breaker
    }

    // 8. Broadcast dedup result
    if let (Some(ref key), Some(tx)) = (&dedup_key, &dedup_tx) {
        let _ = tx.send(result.clone());
        let mut dedup = state.ui_bridge_dedup.lock().await;
        dedup.remove(key);
    }

    result
}

/// Inner implementation of ui_bridge_request_sync (the actual IPC logic)
async fn ui_bridge_request_inner(
    state: &Arc<ApiState>,
    request_type: &str,
    additional_payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let request_id = uuid::Uuid::new_v4().to_string();

    // Create the full event payload
    let mut event_payload = serde_json::json!({
        "requestId": request_id,
        "type": request_type
    });

    // Merge additional payload fields
    if let (Some(base), Some(additional)) = (
        event_payload.as_object_mut(),
        additional_payload.as_object(),
    ) {
        for (key, value) in additional {
            base.insert(key.clone(), value.clone());
        }
    }

    // Create oneshot channel for the response
    let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();

    // Store the sender in the pending map
    {
        let mut pending = state.ui_bridge_pending.lock().await;
        pending.insert(request_id.clone(), tx);
        state
            .ui_bridge_pending_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    // Emit request to React frontend
    if let Err(e) = state.app_handle.emit("ui-bridge-request", &event_payload) {
        // Clean up the pending entry
        let mut pending = state.ui_bridge_pending.lock().await;
        if pending.remove(&request_id).is_some() {
            state
                .ui_bridge_pending_count
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
        return Err(format!("Failed to emit UI Bridge request: {}", e));
    }

    // Wait for response with timeout
    let timeout_duration = std::time::Duration::from_millis(get_ui_bridge_timeout_ms());
    match tokio::time::timeout(timeout_duration, rx).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(_)) => Err("UI Bridge request channel closed unexpectedly".to_string()),
        Err(_) => {
            // Timeout - clean up the pending entry
            let mut pending = state.ui_bridge_pending.lock().await;
            if pending.remove(&request_id).is_some() {
                state
                    .ui_bridge_pending_count
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }
            Err(format!(
                "UI Bridge request timed out after {}ms. Is the frontend running?",
                get_ui_bridge_timeout_ms()
            ))
        }
    }
}

/// Handle incoming UI Bridge response from the frontend.
///
/// This is called by the Tauri event listener set up in create_router.
pub async fn handle_ui_bridge_response(
    pending: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>,
        >,
    >,
    pending_count: Arc<std::sync::atomic::AtomicUsize>,
    response: serde_json::Value,
) {
    let request_id = response
        .get("requestId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if let Some(request_id) = request_id {
        let mut pending_map = pending.lock().await;
        if let Some(sender) = pending_map.remove(&request_id) {
            pending_count.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            // Extract the data portion of the response
            let data = response.get("data").cloned().unwrap_or(response.clone());
            if sender.send(data).is_err() {
                warn!(
                    "UI Bridge: Failed to send response, receiver dropped for request {}",
                    request_id
                );
            } else {
                debug!("UI Bridge: Delivered response for request {}", request_id);
            }
        } else {
            warn!(
                "UI Bridge: No pending request found for response {}",
                request_id
            );
        }
    } else {
        warn!("UI Bridge: Response missing requestId: {:?}", response);
    }
}

/// Wrap a UI Bridge IPC result into an API response, propagating inner success/error status.
///
/// When the frontend returns `{success: false, error: "..."}` in the IPC data,
/// this propagates the failure to the outer API envelope instead of wrapping
/// it in `ApiResponse::success()` (which would create a misleading double-envelope:
/// `{success: true, data: {success: false, error: "..."}}`).
///
/// Also populates `error_detail` with a structured `UiBridgeError` for machine-readable
/// error handling by AI agents.
pub(super) fn wrap_ipc_result(
    result: Result<serde_json::Value, String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    match result {
        Ok(data) => {
            // Check if the IPC response indicates failure
            if data.get("success").and_then(|v| v.as_bool()) == Some(false) {
                let error_msg = data
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Operation failed")
                    .to_string();
                let error_detail = classify_transport_error(&error_msg);
                Ok(Json(ApiResponse {
                    success: false,
                    data: Some(data),
                    error: Some(error_msg),
                    error_detail: Some(error_detail),
                }))
            } else {
                Ok(Json(ApiResponse::success(data)))
            }
        }
        Err(e) => {
            error!("UI Bridge API: {}", e);
            let detail = classify_transport_error(&e);
            let status = match detail.code {
                UiBridgeErrorCode::FrontendNotReady => StatusCode::SERVICE_UNAVAILABLE,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            Err((status, Json(api_error_detailed(e, detail))))
        }
    }
}
