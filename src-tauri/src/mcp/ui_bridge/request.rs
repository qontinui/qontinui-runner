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

/// Default window label for single-window operation
/// (`plans/2026-06-03-runner-popout-terminal-windows.md`). A request with no
/// explicit target routes here, keeping single-window behavior byte-identical.
pub(crate) const MAIN_WINDOW_LABEL: &str = "main";

/// Reserved payload field that routes a request to a specific pop-out window.
///
/// A UI Bridge control endpoint addresses a pop-out window (discoverable via
/// `GET /ui-bridge/control/runner-windows`) by including
/// `{ "windowLabel": "term-1" }` in the request payload. `ui_bridge_request_sync`
/// consumes the field via [`split_target_window`] so it never reaches the
/// frontend handler as request *data*; [`ui_bridge_request_inner`] re-attaches it
/// to the emitted envelope so the addressed window's listener can match it
/// against its own `getCurrentWindow().label`. The name matches the routing field
/// the frontend already echoes on responses, so request → emit → response use one
/// field end-to-end.
pub(crate) const TARGET_WINDOW_FIELD: &str = "windowLabel";

/// Stamp a target window onto a request payload, for handlers that build a typed
/// payload (so a client's `windowLabel` would otherwise be dropped by struct
/// deserialization). No-op when `window_label` is `None`, empty, or
/// [`MAIN_WINDOW_LABEL`], so the single-window default payload is unchanged. A
/// `null` base is promoted to `{}` first; a non-object, non-null base is returned
/// untouched (it can't carry a field). Centralizes the field name so endpoints
/// don't hand-roll the routing convention.
pub(crate) fn target_window_payload(
    mut base: serde_json::Value,
    window_label: Option<&str>,
) -> serde_json::Value {
    let label = match window_label {
        Some(l) if !l.is_empty() && l != MAIN_WINDOW_LABEL => l,
        _ => return base,
    };
    if base.is_null() {
        base = serde_json::json!({});
    }
    if let Some(obj) = base.as_object_mut() {
        obj.insert(
            TARGET_WINDOW_FIELD.to_string(),
            serde_json::Value::String(label.to_string()),
        );
    }
    base
}

/// Split an optional `windowLabel` routing field out of a request payload.
///
/// Returns `(target_window, payload_without_label)`. Absent / empty / non-string
/// `windowLabel` (and any non-object payload) yields [`MAIN_WINDOW_LABEL`], so the
/// single-window default is unchanged. Consuming the field here is what lets every
/// handler that forwards a JSON-object payload target a window with zero per-handler
/// plumbing — the request funnels through one chokepoint.
fn split_target_window(payload: serde_json::Value) -> (String, serde_json::Value) {
    if let serde_json::Value::Object(mut map) = payload {
        let label = map
            .remove(TARGET_WINDOW_FIELD)
            .and_then(|v| v.as_str().map(str::to_string))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| MAIN_WINDOW_LABEL.to_string());
        (label, serde_json::Value::Object(map))
    } else {
        (MAIN_WINDOW_LABEL.to_string(), payload)
    }
}

/// Build the `ui_bridge_pending` map key.
///
/// Keying by `(window_label, request_id)` rather than `request_id` alone makes a
/// response from a window the request was NOT addressed to a no-op (its computed
/// key can't match the stored one), which structurally eliminates the broadcast
/// response race once multiple windows mount the SDK. `request_id` is a UUID, so
/// the delimiter can never collide with a (simple `main`/`term-N`) window label.
/// Both dispatch paths (this module and `helpers::direct_webview_evaluate_with_result`)
/// and the response dispatcher route through this single helper so the key shape
/// can never drift between producer and consumer.
pub(crate) fn pending_key(window_label: &str, request_id: &str) -> String {
    format!("{window_label}\u{1f}{request_id}")
}

/// Whether targeted per-window `ui-bridge-request` emit is enabled.
///
/// Default ON now that pop-out windows answer the bridge (capabilities + per-realm
/// port seed shipped). Each request is emitted only to its target window
/// (`app.get_webview_window(label).emit(...)`), falling back to broadcast if the
/// webview can't be resolved (mid-teardown) so a request is never dropped. For the
/// single-window case this targets "main" only — functionally identical to the old
/// broadcast, minus the cross-window noise. Set `QONTINUI_UI_BRIDGE_MULTI_WINDOW=0`
/// (or `false`/`off`) to revert to unconditional broadcast. Read per-call so it can
/// be toggled without a restart.
fn multi_window_dispatch_enabled() -> bool {
    std::env::var("QONTINUI_UI_BRIDGE_MULTI_WINDOW")
        .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off")))
        .unwrap_or(true)
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
/// Derives the target window from an optional `windowLabel` field in
/// `additional_payload` (see [`TARGET_WINDOW_FIELD`]) and delegates to
/// [`ui_bridge_request_sync_in_window`]. With no `windowLabel` the request targets
/// the main window, byte-identical to single-window behavior. This is the
/// entrypoint every HTTP handler funnels through, so addressing a pop-out window
/// needs no per-handler plumbing.
pub async fn ui_bridge_request_sync(
    state: &Arc<ApiState>,
    request_type: &str,
    additional_payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let (window_label, additional_payload) = split_target_window(additional_payload);
    ui_bridge_request_sync_in_window(state, request_type, additional_payload, &window_label).await
}

/// Send a UI Bridge request to an explicit target window and wait for the
/// response synchronously.
///
/// This creates a oneshot channel, stores the sender in the pending map keyed by
/// `(window_label, request_id)`, emits the request to the frontend, and waits for
/// the response with a timeout.
///
/// Includes circuit breaker, concurrency limiting, frontend liveness check, and
/// request deduplication for read-only operations. Rust callers that already hold
/// a target label (e.g. terminal-window commands) call this directly; the public
/// [`ui_bridge_request_sync`] derives the label from the payload.
pub async fn ui_bridge_request_sync_in_window(
    state: &Arc<ApiState>,
    request_type: &str,
    additional_payload: serde_json::Value,
    window_label: &str,
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

    // 3. Check for dedup opportunity on read-only requests. The key is scoped to
    // the target window so a read for one pop-out window never collapses into an
    // in-flight read for a different window (which would return the wrong window's
    // elements). For the main window this is just the legacy per-type dedup.
    let dedup_key = match request_type {
        "get_elements" | "get_snapshot" | "get_components" => {
            Some(format!("{window_label}\u{1f}{request_type}"))
        }
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
    let result =
        ui_bridge_request_inner(state, request_type, additional_payload, window_label).await;

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

/// Inner implementation of ui_bridge_request_sync (the actual IPC logic).
///
/// `window_label` is the resolved target window ([`MAIN_WINDOW_LABEL`] for the
/// single-window default); the caller has already stripped any `windowLabel`
/// routing field from `additional_payload`.
async fn ui_bridge_request_inner(
    state: &Arc<ApiState>,
    request_type: &str,
    additional_payload: serde_json::Value,
    window_label: &str,
) -> Result<serde_json::Value, String> {
    let request_id = uuid::Uuid::new_v4().to_string();

    // Fail fast for a non-existent target window. Without this, an unknown label
    // would emit (or broadcast) an event every window's listener filters out by
    // label, so the caller would wait the full IPC timeout for a request nothing
    // can answer. A clear, immediate error naming the discovery route is the
    // honest failure. The main window is always present (created at boot), so the
    // default path skips the lookup entirely.
    if window_label != MAIN_WINDOW_LABEL {
        use tauri::Manager;
        if state.app_handle.get_webview_window(window_label).is_none() {
            return Err(format!(
                "No runner window labeled '{window_label}'. Discover live windows via \
                 GET /ui-bridge/control/runner-windows."
            ));
        }
    }

    // Carry the target window in the (flattened) envelope payload ONLY when it
    // is non-default, so the default single-window request is byte-identical on
    // the wire. The frontend listener reads `windowLabel` and ignores events not
    // addressed to its own `getCurrentWindow().label`.
    let additional_payload = if window_label != MAIN_WINDOW_LABEL {
        let mut p = additional_payload;
        if let Some(obj) = p.as_object_mut() {
            obj.insert(
                TARGET_WINDOW_FIELD.to_string(),
                serde_json::Value::String(window_label.to_string()),
            );
        }
        p
    } else {
        additional_payload
    };

    // Build the typed envelope (Stage 1 of the ui-bridge-request envelope
    // concretization — see commit ea5d9a61f deferral note). Wire shape:
    // `{ requestId, type, ...additional_payload }`. Empty `Value::Object`
    // (the `json!({})` no-extra-payload case) flattens to nothing, so the
    // emitted payload is `{ requestId, type }` exactly as before.
    //
    // Non-Object additional_payload values would be silently dropped by the
    // legacy merge loop too (which short-circuited on `as_object()` =
    // `None`). All current call sites pass `Value::Object`, verified at
    // task time; if a caller starts passing `Value::String`/`Value::Null`,
    // the envelope will emit `requestId` + `type` only — matching the
    // legacy behavior — but the signal that something's off should surface
    // via the empty payload reaching the frontend handler.
    let envelope = qontinui_types::app_events::UiBridgeRequestEnvelope {
        request_id: request_id.clone(),
        request_type: request_type.to_string(),
        data: additional_payload,
    };
    let event_payload = serde_json::to_value(&envelope)
        .map_err(|e| format!("Failed to serialize UiBridgeRequestEnvelope: {}", e))?;

    // Create oneshot channel for the response
    let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();

    // Store the sender in the pending map under the composite (window, id) key.
    let pkey = pending_key(window_label, &request_id);
    {
        let mut pending = state.ui_bridge_pending.lock().await;
        pending.insert(pkey.clone(), tx);
        state
            .ui_bridge_pending_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    // Emit request to the React frontend. Default (flag off): broadcast to all
    // windows exactly as before. Flag on (Phase 1): target the specific window,
    // falling back to broadcast if its webview can't be resolved (e.g. mid
    // teardown) so a request is never silently dropped.
    let emit_result = if multi_window_dispatch_enabled() {
        use tauri::Manager;
        match state.app_handle.get_webview_window(window_label) {
            Some(win) => win.emit("ui-bridge-request", &event_payload),
            None => state.app_handle.emit("ui-bridge-request", &event_payload),
        }
    } else {
        state.app_handle.emit("ui-bridge-request", &event_payload)
    };
    if let Err(e) = emit_result {
        // Clean up the pending entry
        let mut pending = state.ui_bridge_pending.lock().await;
        if pending.remove(&pkey).is_some() {
            state
                .ui_bridge_pending_count
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
        return Err(format!("Failed to emit UI Bridge request: {}", e));
    }

    // Wait for response with timeout
    let timeout_duration = std::time::Duration::from_millis(get_ui_bridge_timeout_ms());
    match tokio::time::timeout(timeout_duration, rx).await {
        Ok(Ok(response)) => {
            // First successful IPC response means the React frontend has
            // mounted past `App.tsx`'s loading-screen branch and its
            // ui-bridge-response listener is wired up. Flip the one-way
            // readiness flag so /health can report `frontendReady: true`.
            // (Stays true for the rest of the process lifetime — see
            // `AppState::frontend_ready` doc.)
            state
                .app_state
                .frontend_ready
                .store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(response)
        }
        Ok(Err(_)) => Err("UI Bridge request channel closed unexpectedly".to_string()),
        Err(_) => {
            // Timeout - clean up the pending entry
            let mut pending = state.ui_bridge_pending.lock().await;
            if pending.remove(&pkey).is_some() {
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

    // The responding window echoes its own label; absent for the single-window
    // default (and for any pre-window-aware frontend), so fall back to "main" —
    // which is exactly the key both dispatch paths store under in Phase 0.
    let window_label = response
        .get("windowLabel")
        .and_then(|v| v.as_str())
        .unwrap_or(MAIN_WINDOW_LABEL);

    if let Some(request_id) = request_id {
        let pkey = pending_key(window_label, &request_id);
        let mut pending_map = pending.lock().await;
        if let Some(sender) = pending_map.remove(&pkey) {
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

/// Wrap a UI Bridge IPC result into an API response, flattening any inner
/// `{success:false, error}` envelope from the frontend into a flat HTTP 400.
///
/// **F2 two-tier envelope contract** (sweep applied 2026-04-22):
/// - Inner `success: true` (or no `success` field at all — some handlers omit
///   it on the happy path) → HTTP 200 with `ApiResponse::success(data)`.
/// - Inner `success: false` → HTTP 400 with a flat `{success: false, error}`
///   body (no nested `data`, no inner success field). Falls back to a generic
///   "UI bridge call failed" message if `data.error` is missing or non-string.
/// - Transport-level `Err(_)` → HTTP 503 (frontend not ready) or HTTP 500
///   (everything else), with structured `error_detail` for machine-readable
///   recovery hints.
///
/// This mirrors the F2 fix originally landed in `design.rs` for the audit
/// handler (`unwrap_inner_audit_error`) and is now the canonical unwrapper
/// every IPC-backed handler funnels through.
///
/// Note: this is a **back-compat shift** for callers that previously saw
/// `HTTP 200 + {success:false, ...}` on soft failures — they now get HTTP 400.
pub(crate) fn wrap_ipc_result(
    result: Result<serde_json::Value, String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    match result {
        Ok(data) => {
            if data.get("success").and_then(|v| v.as_bool()) == Some(false) {
                // Inner-failure envelope: flatten to HTTP 400 + flat error body.
                let error_msg = data
                    .get("error")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "UI bridge call failed".to_string());
                let detail = classify_transport_error(&error_msg);
                // Forward an optional `hint` sibling field from the inner
                // IPC envelope (set by frontend handlers like
                // `useControlEvents` for typo-recovery on element-not-found
                // / action-not-allowed). The hint stays a sibling of
                // `error` — the success/error envelope shape is unchanged.
                let hint = data.get("hint").cloned();
                let mut body = api_error_detailed(error_msg, detail);
                body.hint = hint;
                Err((StatusCode::BAD_REQUEST, Json(body)))
            } else {
                // Healthy IPC response (success: true OR success absent).
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod wrap_ipc_result_tests {
    //! F2 two-tier envelope flattening tests for the canonical
    //! `wrap_ipc_result` helper. Lock down each decision point: inner
    //! success, inner failure (with/without error field), absent success
    //! field, and non-bool success values.
    use super::{
        handle_ui_bridge_response, pending_key, split_target_window, wrap_ipc_result,
        MAIN_WINDOW_LABEL,
    };
    use axum::http::StatusCode;
    use serde_json::json;
    use std::collections::HashMap;
    use std::ops::Deref;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::{oneshot, Mutex};

    // ── Window-aware dispatch (Phase 0) ─────────────────────────────────────

    #[test]
    fn split_target_window_defaults_to_main_when_absent() {
        // No windowLabel → main, payload untouched (single-window default).
        let (label, payload) = split_target_window(json!({ "selector": "#btn" }));
        assert_eq!(label, MAIN_WINDOW_LABEL);
        assert_eq!(payload, json!({ "selector": "#btn" }));
    }

    #[test]
    fn split_target_window_extracts_and_strips_label() {
        // windowLabel routes the request AND is consumed (not forwarded as data).
        let (label, payload) =
            split_target_window(json!({ "windowLabel": "term-1", "selector": "#btn" }));
        assert_eq!(label, "term-1");
        assert_eq!(payload, json!({ "selector": "#btn" }));
    }

    #[test]
    fn split_target_window_empty_or_non_string_label_falls_back_to_main() {
        let (label, payload) = split_target_window(json!({ "windowLabel": "", "x": 1 }));
        assert_eq!(label, MAIN_WINDOW_LABEL);
        assert_eq!(payload, json!({ "x": 1 }), "empty label still consumed");

        let (label, payload) = split_target_window(json!({ "windowLabel": 7, "x": 1 }));
        assert_eq!(label, MAIN_WINDOW_LABEL);
        assert_eq!(
            payload,
            json!({ "x": 1 }),
            "non-string label still consumed"
        );
    }

    #[test]
    fn split_target_window_non_object_payload_is_unchanged() {
        // A non-object payload can't carry a routing field — pass it through as-is.
        let (label, payload) = split_target_window(json!("raw"));
        assert_eq!(label, MAIN_WINDOW_LABEL);
        assert_eq!(payload, json!("raw"));
    }

    #[test]
    fn pending_key_is_distinct_per_window_for_same_request_id() {
        let id = "11111111-2222-3333-4444-555555555555";
        // Same request id in two windows must NOT collide on the pending map.
        assert_ne!(pending_key("main", id), pending_key("term-1", id));
        // Same (window, id) is stable so insert and remove agree.
        assert_eq!(pending_key("main", id), pending_key("main", id));
    }

    #[tokio::test]
    async fn response_routes_only_to_the_addressed_window() {
        // Two windows registered the SAME request id; a response from one must
        // resolve only that window's sender and leave the other pending.
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let count = Arc::new(AtomicUsize::new(0));
        let id = "req-abc";
        let (tx_main, rx_main) = oneshot::channel::<serde_json::Value>();
        let (tx_term, rx_term) = oneshot::channel::<serde_json::Value>();
        {
            let mut p = pending.lock().await;
            p.insert(pending_key("main", id), tx_main);
            p.insert(pending_key("term-1", id), tx_term);
        }
        count.store(2, Ordering::Relaxed);

        let response = json!({ "requestId": id, "windowLabel": "term-1", "data": { "ok": true } });
        handle_ui_bridge_response(pending.clone(), count.clone(), response).await;

        // term-1's sender fired with the unwrapped data...
        assert_eq!(
            rx_term.await.expect("term-1 sender fired"),
            json!({ "ok": true })
        );
        // ...main's entry is untouched (still pending, count decremented by one).
        let p = pending.lock().await;
        assert!(p.contains_key(&pending_key("main", id)));
        assert!(!p.contains_key(&pending_key("term-1", id)));
        assert_eq!(count.load(Ordering::Relaxed), 1);
        drop(rx_main);
    }

    #[tokio::test]
    async fn response_without_label_defaults_to_main() {
        // A response that omits windowLabel (pre-window-aware frontend, or the
        // direct-eval path) must resolve the "main" key it was stored under.
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let count = Arc::new(AtomicUsize::new(0));
        let id = "req-xyz";
        let (tx, rx) = oneshot::channel::<serde_json::Value>();
        {
            let mut p = pending.lock().await;
            p.insert(pending_key(MAIN_WINDOW_LABEL, id), tx);
        }
        count.store(1, Ordering::Relaxed);

        let response = json!({ "requestId": id, "data": { "v": 1 } });
        handle_ui_bridge_response(pending.clone(), count.clone(), response).await;

        assert_eq!(rx.await.expect("main sender fired"), json!({ "v": 1 }));
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn inner_success_returns_http_200() {
        let data = json!({"success": true, "report": {"violations": []}});
        let resp = wrap_ipc_result(Ok(data.clone())).expect("inner success must produce Ok");
        let body = resp.deref();
        assert!(body.success);
        assert!(body.error.is_none());
        assert_eq!(body.data.as_ref().unwrap(), &data);
    }

    #[test]
    fn inner_failure_with_explicit_error_flattens_to_400() {
        let data = json!({
            "success": false,
            "error": "No style guide provided or loaded.",
            "type": "design_run_audit",
        });
        let (status, body) =
            wrap_ipc_result(Ok(data)).expect_err("inner failure must produce Err with HTTP 400");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let inner = body.deref();
        assert!(!inner.success);
        let msg = inner.error.as_deref().unwrap_or_default();
        assert!(
            msg.contains("No style guide provided"),
            "expected inner error to surface, got: {msg}"
        );
        // Outer body must be flat (no nested `data` from the inner envelope).
        assert!(inner.data.is_none(), "outer body must not nest inner data");
    }

    #[test]
    fn inner_failure_without_error_field_uses_fallback_message() {
        let data = json!({"success": false});
        let (status, body) = wrap_ipc_result(Ok(data)).expect_err("inner failure must produce Err");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let msg = body.deref().error.as_deref().unwrap_or_default();
        assert_eq!(msg, "UI bridge call failed");
    }

    #[test]
    fn absent_success_field_passes_through_as_200() {
        // Some IPC responses don't include `success` at all — those should
        // be treated as healthy success (no misclassification as failure).
        let data = json!({"report": {"violations": []}});
        let resp = wrap_ipc_result(Ok(data.clone())).expect("absent success field must produce Ok");
        let body = resp.deref();
        assert!(body.success);
        assert_eq!(body.data.as_ref().unwrap(), &data);
    }

    #[test]
    fn non_bool_success_value_passes_through_as_200() {
        // Robustness: if `success` is a string or number rather than a bool,
        // treat it as "shape unknown, don't flag failure" rather than
        // panicking or misclassifying it.
        let data = json!({"success": "true", "payload": 1});
        let resp = wrap_ipc_result(Ok(data)).expect("string success must produce Ok");
        assert!(resp.deref().success);

        let data = json!({"success": 1, "payload": 2});
        let resp = wrap_ipc_result(Ok(data)).expect("numeric success must produce Ok");
        assert!(resp.deref().success);
    }

    #[test]
    fn transport_error_returns_5xx() {
        // Sanity: a transport-level Err (e.g. timeout) still surfaces as
        // HTTP 5xx, not 400.
        let (status, _body) =
            wrap_ipc_result(Err("UI Bridge request timed out after 5000ms".to_string()))
                .expect_err("transport error must produce Err");
        // Either 500 (default) or 503 (frontend not ready) — not 400.
        assert_ne!(status, StatusCode::BAD_REQUEST);
        assert!(status.is_server_error());
    }
}
