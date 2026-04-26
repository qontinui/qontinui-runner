//! Wait-for / navigation intent handlers.
//!
//! All wait-for-* endpoints that poll the SDK or page state until a condition
//! is satisfied:
//!   - `wait-for-navigation` (with `/ai/` alias)
//!   - `wait-for-idle` (with `/ai/` alias)
//!   - `wait-for-idle/{signal}`
//!   - `wait-for-targets`
//!   - `wait-for-element-stable`
//!   - `wait-for-element-condition` (already `/ai/`-prefixed)
//!   - `wait-for-element-state`
//!   - `wait-for-route` (with `/ai/` alias)
//!   - `wait-for-route-change` (with `/ai/` alias — forwards to SDK)

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use tracing::{error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::helpers::glob_match;
use super::request::{ui_bridge_request_sync, wrap_ipc_result};

/// Wait for a deterministic "navigation complete" signal from the SDK.
/// Falls back to idle-based detection if no explicit signal arrives.
pub async fn ui_bridge_wait_for_navigation_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Wait for navigation complete");

    let timeout = body
        .get("timeout")
        .and_then(|v| v.as_i64())
        .unwrap_or(30000);

    let payload = serde_json::json!({
        "params": {
            "timeout": timeout,
            "since": body.get("since"),
            "urlPattern": body.get("urlPattern")
        }
    });

    match ui_bridge_request_sync(&state, "wait_for_navigation_complete", payload).await {
        Ok(data) => {
            // Check if navigation completed or timed out
            let completed = data
                .get("completed")
                .or_else(|| data.get("data").and_then(|d| d.get("completed")))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if completed {
                Ok(Json(ApiResponse::success(data)))
            } else {
                // Return 408 Request Timeout when navigation did not complete
                Err((
                    StatusCode::REQUEST_TIMEOUT,
                    Json(api_error(format!(
                        "Navigation did not complete within {}ms",
                        timeout
                    ))),
                ))
            }
        }
        Err(e) => {
            error!("UI Bridge API: Wait for navigation failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Wait for composite idle state.
pub async fn ui_bridge_wait_for_idle_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Wait for idle");

    let payload = serde_json::json!({
        "params": {
            "timeout": body.get("timeout").and_then(|v| v.as_i64()).unwrap_or(30000),
            "minStableMs": body.get("minStableMs").and_then(|v| v.as_i64()).unwrap_or(500),
            "exclude": body.get("exclude")
        }
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "wait_for_idle", payload).await)
}

/// Wait for a specific element to become visually and structurally stable.
///
/// Relays `wait_for_element_stable` to the frontend SDK which uses a scoped
/// MutationObserver plus requestAnimationFrame bounding-box polling.
/// Returns 200 with `{ stable: true, elapsed }` on success, or 408 on timeout.
pub async fn ui_bridge_wait_for_element_stable_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let element_id = body
        .get("elementId")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    if element_id.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "wait-for-element-stable: 'elementId' is required",
            )),
        ));
    }

    let quiet_ms = body.get("quietMs").and_then(|v| v.as_u64()).unwrap_or(500);
    let timeout_ms = body.get("timeout").and_then(|v| v.as_u64()).unwrap_or(5000);
    let observe_attributes = body
        .get("observeAttributes")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let observe_subtree = body
        .get("observeSubtree")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    info!(
        "UI Bridge API: wait-for-element-stable elementId={} quietMs={} timeout={}ms",
        element_id, quiet_ms, timeout_ms
    );

    let payload = serde_json::json!({
        "params": {
            "elementId": element_id,
            "quietMs": quiet_ms,
            "timeout": timeout_ms,
            "observeAttributes": observe_attributes,
            "observeSubtree": observe_subtree
        }
    });

    match ui_bridge_request_sync(&state, "wait_for_element_stable", payload).await {
        Ok(data) => {
            let stable = data
                .get("stable")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if stable {
                Ok(Json(ApiResponse::success(data)))
            } else {
                let elapsed = data.get("elapsed").and_then(|v| v.as_u64()).unwrap_or(0);
                let msg = format!(
                    "wait-for-element-stable: element {} did not stabilize within {}ms (elapsed {}ms)",
                    element_id, timeout_ms, elapsed
                );
                info!("UI Bridge API: {}", msg);
                Err((StatusCode::REQUEST_TIMEOUT, Json(api_error(msg))))
            }
        }
        Err(e) => {
            error!("UI Bridge API: wait-for-element-stable failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// POST /ui-bridge/ai/wait-for-element-condition
///
/// Forwards to the JS SDK's `waitForElementByCondition` handler which uses
/// registry-based polling and supports structured selectors (id, title,
/// aria_label, text, type) and conditions (present / visible / clickable /
/// text-matches).
pub async fn ui_bridge_wait_for_element_condition_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let body = body
        .map(|Json(v)| v)
        .unwrap_or_else(|| serde_json::json!({}));

    let timeout_ms = body
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(5_000)
        .min(60_000);

    let condition = body
        .get("condition")
        .and_then(|v| v.as_str())
        .unwrap_or("present");

    info!(
        "UI Bridge API: wait-for-element-condition condition={} timeout={}ms",
        condition, timeout_ms
    );

    // Forward the entire body to the JS SDK handler.
    let payload = serde_json::json!({ "params": body });

    match ui_bridge_request_sync(&state, "wait_for_element_by_condition", payload).await {
        Ok(data) => {
            let matched = data
                .get("matched")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if matched {
                Ok(Json(ApiResponse::success(data)))
            } else {
                let waited = data.get("waited_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                info!(
                    "UI Bridge API: wait-for-element-condition: no match within {}ms (waited {}ms)",
                    timeout_ms, waited
                );
                // Return 408 with the body so callers can inspect waited_ms.
                Err((
                    StatusCode::REQUEST_TIMEOUT,
                    Json(api_error(format!(
                        "wait-for-element-condition: element not found within {}ms",
                        timeout_ms
                    ))),
                ))
            }
        }
        Err(e) => {
            error!("UI Bridge API: wait-for-element-condition failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Wait for a specific idle signal.
pub async fn ui_bridge_wait_for_idle_signal_handler(
    State(state): State<Arc<ApiState>>,
    Path(signal): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Wait for idle signal '{}'", signal);
    let mut params = body;
    if let Some(obj) = params.as_object_mut() {
        obj.insert("signal".to_string(), serde_json::json!(signal));
    } else {
        params = serde_json::json!({ "signal": signal });
    }
    let payload = serde_json::json!({ "params": params });
    wrap_ipc_result(ui_bridge_request_sync(&state, "wait_for_idle_signal", payload).await)
}

/// Wait for multiple idle targets.
pub async fn ui_bridge_wait_for_targets_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Wait for targets");
    let payload = serde_json::json!({ "params": body });
    wrap_ipc_result(ui_bridge_request_sync(&state, "wait_for_targets", payload).await)
}

/// POST /ui-bridge/control/wait-for-element-state
///
/// Convenience wrapper around `wait_for_element` for the common case of
/// "wait until element <id> is visible / enabled / focused". Polls the
/// element registry every ~100ms until the state matches or the timeout
/// elapses.
pub async fn ui_bridge_wait_for_element_state_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let body = body
        .map(|Json(v)| v)
        .unwrap_or_else(|| serde_json::json!({}));

    let id = body
        .get("id")
        .or_else(|| body.get("elementId"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let state_name = body
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("visible")
        .to_string();
    let timeout_ms = body
        .get("timeout_ms")
        .or_else(|| body.get("timeout"))
        .and_then(|v| v.as_u64())
        .unwrap_or(5000);

    let id = match id {
        Some(s) if !s.is_empty() => s,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error("wait-for-element-state: 'id' is required")),
            ));
        }
    };

    // Map the simple state name to the field on the registered element that
    // we're polling. The registry exposes these as booleans on the element
    // object's `state` block.
    let state_field = match state_name.as_str() {
        "visible" => "visible",
        "enabled" => "enabled",
        "focused" => "focused",
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!(
                    "wait-for-element-state: unknown state '{other}', expected visible|enabled|focused"
                ))),
            ));
        }
    };

    let poll_interval_ms = 100u64;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let start = std::time::Instant::now();

    info!(
        "UI Bridge API: wait-for-element-state id={} state={} timeout={}ms",
        id, state_name, timeout_ms
    );

    loop {
        let lookup = ui_bridge_request_sync(
            &state,
            "get_element",
            serde_json::json!({ "elementId": id }),
        )
        .await;

        if let Ok(data) = lookup {
            // Element returned — extract the state block (either nested at
            // `data.element.state` or at `data.state` depending on shape).
            let element = data.get("element").cloned().unwrap_or_else(|| data.clone());
            let matched = element
                .get("state")
                .and_then(|s| s.get(state_field))
                .and_then(|v| v.as_bool())
                .or_else(|| {
                    // Some shapes expose `visible`/`enabled`/`focused` at the
                    // top level rather than under `state`.
                    element.get(state_field).and_then(|v| v.as_bool())
                })
                .unwrap_or(false);
            if matched {
                let elapsed_ms = start.elapsed().as_millis() as u64;
                return Ok(Json(ApiResponse::success(serde_json::json!({
                    "found": true,
                    "elapsed_ms": elapsed_ms,
                    "state": element.get("state").cloned().unwrap_or(serde_json::Value::Null),
                }))));
            }
        }

        if std::time::Instant::now() + std::time::Duration::from_millis(poll_interval_ms) > deadline
        {
            let elapsed_ms = start.elapsed().as_millis() as u64;
            return Ok(Json(ApiResponse::success(serde_json::json!({
                "found": false,
                "elapsed_ms": elapsed_ms,
            }))));
        }

        tokio::time::sleep(std::time::Duration::from_millis(poll_interval_ms)).await;
    }
}

/// POST /ui-bridge/control/wait-for-route
///
/// Wait until the current page route matches the given glob pattern.
/// Polls `page.route.pattern` (falling back to `page.pathname`) on the
/// snapshot every ~100ms.
pub async fn ui_bridge_wait_for_route_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let body = body
        .map(|Json(v)| v)
        .unwrap_or_else(|| serde_json::json!({}));
    let pattern = match body.get("pattern").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error("wait-for-route: 'pattern' is required")),
            ));
        }
    };
    let timeout_ms = body
        .get("timeout_ms")
        .or_else(|| body.get("timeout"))
        .and_then(|v| v.as_u64())
        .unwrap_or(5000);

    let poll_interval_ms = 100u64;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let start = std::time::Instant::now();
    let mut last_route: serde_json::Value = serde_json::Value::Null;

    info!(
        "UI Bridge API: wait-for-route pattern={} timeout={}ms",
        pattern, timeout_ms
    );

    loop {
        let snap = ui_bridge_request_sync(&state, "get_snapshot", serde_json::json!({})).await;
        if let Ok(data) = snap {
            // Snapshot exposes `page.route.pattern`, `page.pathname`,
            // `page.url`. We try them in order so this works for apps that
            // haven't called `setRouteInfo` yet.
            let page = data.get("page").cloned().unwrap_or(serde_json::Value::Null);
            let route_pattern = page
                .get("route")
                .and_then(|r| r.get("pattern"))
                .and_then(|v| v.as_str())
                .map(String::from);
            let pathname = page
                .get("pathname")
                .and_then(|v| v.as_str())
                .map(String::from);
            let candidate = route_pattern
                .clone()
                .or_else(|| pathname.clone())
                .unwrap_or_default();
            last_route = serde_json::json!({
                "pattern": route_pattern,
                "pathname": pathname,
                "url": page.get("url").cloned().unwrap_or(serde_json::Value::Null),
            });
            if glob_match(&pattern, &candidate) {
                let elapsed_ms = start.elapsed().as_millis() as u64;
                return Ok(Json(ApiResponse::success(serde_json::json!({
                    "matched": true,
                    "route": last_route,
                    "elapsed_ms": elapsed_ms,
                }))));
            }
        }

        if std::time::Instant::now() + std::time::Duration::from_millis(poll_interval_ms) > deadline
        {
            let elapsed_ms = start.elapsed().as_millis() as u64;
            return Ok(Json(ApiResponse::success(serde_json::json!({
                "matched": false,
                "route": last_route,
                "elapsed_ms": elapsed_ms,
            }))));
        }

        tokio::time::sleep(std::time::Duration::from_millis(poll_interval_ms)).await;
    }
}

/// POST /ui-bridge/control/wait-for-route-change
/// POST /ui-bridge/ai/wait-for-route-change
///
/// Forward to the SDK's `waitForRouteChange` handler on the frontend.
/// The SDK subscribes to the ChangeTracker's route-change stream with
/// optional `fromRoute` / `toRoute` filters and returns the first match,
/// or a `{reason: 'timeout', ...}` payload on expiry.
///
/// Body shape: `{ fromRoute?, toRoute?, matchMode? ('exact'|'prefix'|'regex'), timeoutMs? }`.
/// We bound `timeout_ms` to `[100, 60_000]` (default 5000) on the Rust side
/// as a safety cap before forwarding. The SDK re-clamps inside the same
/// window, so this bound is defensive: a runaway client can't park a
/// listener indefinitely.
pub async fn ui_bridge_wait_for_route_change_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mut body = body
        .map(|Json(v)| v)
        .unwrap_or_else(|| serde_json::json!({}));

    // Clamp timeoutMs to [100, 60_000] (default 5000) before forwarding.
    let raw_timeout = body
        .get("timeoutMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(5000);
    let timeout_ms = raw_timeout.clamp(100, 60_000);
    if let serde_json::Value::Object(ref mut map) = body {
        map.insert("timeoutMs".to_string(), serde_json::Value::from(timeout_ms));
    }

    info!(
        "UI Bridge API: wait-for-route-change fromRoute={:?} toRoute={:?} matchMode={:?} timeoutMs={}",
        body.get("fromRoute").and_then(|v| v.as_str()),
        body.get("toRoute").and_then(|v| v.as_str()),
        body.get("matchMode").and_then(|v| v.as_str()),
        timeout_ms,
    );

    let payload = serde_json::json!({ "params": body });
    wrap_ipc_result(ui_bridge_request_sync(&state, "wait_for_route_change", payload).await)
}

// ============================================================================
// Routes + manifest
// ============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use super::routing::add_dual;
    use axum::routing::post;
    let router = axum::Router::new();
    // Four wait-for-* endpoints are reachable under both /control/ and /ai/
    // with the same handler. /ai/wait-for-element-condition has no /control/
    // twin, and wait-for-element-state / wait-for-element-stable /
    // wait-for-idle/{signal} / wait-for-targets are /control/-only —
    // those stay as plain .route() calls below.
    let router = add_dual!(
        router,
        post,
        "wait-for-navigation",
        ui_bridge_wait_for_navigation_handler
    );
    let router = add_dual!(
        router,
        post,
        "wait-for-idle",
        ui_bridge_wait_for_idle_handler
    );
    let router = add_dual!(
        router,
        post,
        "wait-for-route",
        ui_bridge_wait_for_route_handler
    );
    let router = add_dual!(
        router,
        post,
        "wait-for-route-change",
        ui_bridge_wait_for_route_change_handler
    );
    router
        .route(
            "/ui-bridge/ai/wait-for-element-condition",
            post(ui_bridge_wait_for_element_condition_handler),
        )
        .route(
            "/ui-bridge/control/wait-for-element-state",
            post(ui_bridge_wait_for_element_state_handler),
        )
        .route(
            "/ui-bridge/control/wait-for-element-stable",
            post(ui_bridge_wait_for_element_stable_handler),
        )
        .route(
            "/ui-bridge/control/wait-for-idle/{signal}",
            post(ui_bridge_wait_for_idle_signal_handler),
        )
        .route(
            "/ui-bridge/control/wait-for-targets",
            post(ui_bridge_wait_for_targets_handler),
        )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("POST", "/ui-bridge/control/wait-for-navigation"),
        ("POST", "/ui-bridge/ai/wait-for-navigation"),
        ("POST", "/ui-bridge/control/wait-for-idle"),
        ("POST", "/ui-bridge/ai/wait-for-idle"),
        ("POST", "/ui-bridge/ai/wait-for-element-condition"),
        ("POST", "/ui-bridge/control/wait-for-element-state"),
        ("POST", "/ui-bridge/control/wait-for-element-stable"),
        ("POST", "/ui-bridge/control/wait-for-route"),
        ("POST", "/ui-bridge/ai/wait-for-route"),
        ("POST", "/ui-bridge/control/wait-for-route-change"),
        ("POST", "/ui-bridge/ai/wait-for-route-change"),
        ("POST", "/ui-bridge/control/wait-for-idle/{signal}"),
        ("POST", "/ui-bridge/control/wait-for-targets"),
    ]
}
