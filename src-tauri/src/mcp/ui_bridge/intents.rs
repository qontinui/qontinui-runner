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

use crate::mcp::types::{api_error, api_error_detailed, ApiResponse, ApiState};

use super::helpers::glob_match;
use super::request::{ui_bridge_request_sync, wrap_ipc_result};
use super::types::UiBridgeError;

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

/// State names `wait-for-element-state` accepts, in `ElementState` field
/// order.
///
/// `disabled` / `ariaDisabled` are the two independent interactivity signals
/// that ui-bridge PR #144 split out of the derived `enabled` fold
/// (`enabled == !(disabled || ariaDisabled)`). The canonical
/// `qontinui_types::ui_bridge::ElementState` is `#[serde(rename_all =
/// "camelCase")]` and every SDK serializer emits its object-literal keys
/// verbatim, so the driver-facing spelling is `ariaDisabled` — NOT
/// `aria_disabled`.
pub(super) const ALLOWED_STATES: &[&str] =
    &["visible", "enabled", "disabled", "ariaDisabled", "focused"];

/// Map a driver-supplied state name to the field polled on the registered
/// element's `state` block. `None` for anything not in [`ALLOWED_STATES`].
///
/// Each accepted name is its own field name on the wire, so this is identity
/// over the allow-list — but it stays an explicit match so a name can only be
/// accepted if it also resolves, and so both lists cannot drift apart.
pub(super) fn resolve_state_field(state_name: &str) -> Option<&'static str> {
    match state_name {
        "visible" => Some("visible"),
        "enabled" => Some("enabled"),
        "disabled" => Some("disabled"),
        "ariaDisabled" => Some("ariaDisabled"),
        "focused" => Some("focused"),
        _ => None,
    }
}

/// Result of reading one `ElementState` flag off a `get_element` payload.
///
/// The distinction is the whole point: "the bridge reports false" and "this
/// bridge build does not report that signal at all" are different answers,
/// and collapsing them is how a wait on an unimplemented signal turns into a
/// silent full-timeout `found:false` that reads exactly like "the element
/// never got there".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StateProbe {
    /// The field was present on the wire, or soundly derivable from a field
    /// that was. Carries the resolved value.
    Observed(bool),
    /// The field is absent from this bridge build's `ElementState` and has no
    /// sound derivation from the fields that ARE present.
    NotObservable,
}

/// Coerce one JSON value to a state flag.
///
/// Accepts both wire shapes the runner already handles elsewhere: a real JSON
/// bool, and the `aria-disabled` attribute passthrough that arrives as the
/// string `"true"` / `"false"`. The string form is not hypothetical — the
/// `withDisabledOnly` snapshot filter in `elements.rs` reads
/// `state.ariaDisabled` with `as_str() == Some("true")`, mirroring the SDK's
/// own `handlers.ts` predicate, and `execute_action`'s disabled pre-check
/// does the same. A bool-only read here would disagree with both.
fn coerce_state_flag(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Bool(b) => Some(*b),
        serde_json::Value::String(s) => match s.as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// Read a state flag from an element payload, preferring the nested `state`
/// block and falling back to the top level (some shapes — notably the
/// `discover()` fallback that `get_element` uses when the registry misses —
/// expose the flags flat rather than under `state`).
fn read_state_flag(element: &serde_json::Value, field: &str) -> Option<bool> {
    element
        .get("state")
        .and_then(|s| s.get(field))
        .and_then(coerce_state_flag)
        .or_else(|| element.get(field).and_then(coerce_state_flag))
}

/// Probe one `ElementState` field on a `get_element` payload.
///
/// `disabled` / `ariaDisabled` were added to [`ALLOWED_STATES`] ahead of the
/// bridge that emits them: `@qontinui/ui-bridge@0.22.0` — the version this
/// repo depends on (`^0.22.0`) — builds element state in `getElementState()`
/// as `visible` / `enabled` / `focused` only, and its published
/// `ElementState` type declares no `disabled` or `ariaDisabled` at all. The
/// ui-bridge commit that splits them is not on that package's release
/// lineage yet. Reading an absent key as `false` therefore made BOTH new
/// names permanently unmatchable: accepted at the door, then polled against a
/// key the bridge never emits until the timeout expired.
///
/// So each name gets the honest answer available on the live build:
///
/// - present on the wire -> its value (a real post-split build wins outright,
///   which keeps this forward-compatible),
/// - `disabled` absent -> derived from `enabled`, the same derivation the
///   SDK's own predicate runtime uses (`evaluateElementPredicate`'s
///   `disabled` arm is `state.enabled === false`) and the third arm of the
///   `withDisabledOnly` snapshot filter. Deriving it keeps the two wait
///   endpoints answering the same question.
/// - `ariaDisabled` absent -> [`StateProbe::NotObservable`]. There is no
///   sound derivation: pre-split `enabled` is the fold
///   `!(disabled || ariaDisabled)`, so deriving from it would report a
///   natively-disabled control as ARIA-disabled. Saying "cannot observe"
///   beats inventing an answer.
/// - `visible` / `enabled` / `focused` absent -> `false`. These are required
///   on every build, so an absent key is a genuinely absent signal rather
///   than a build gap; preserving `false` keeps their behavior unchanged.
pub(super) fn probe_state_field(element: &serde_json::Value, field: &str) -> StateProbe {
    if let Some(value) = read_state_flag(element, field) {
        return StateProbe::Observed(value);
    }
    match field {
        "disabled" => match read_state_flag(element, "enabled") {
            Some(enabled) => StateProbe::Observed(!enabled),
            None => StateProbe::NotObservable,
        },
        "ariaDisabled" => StateProbe::NotObservable,
        _ => StateProbe::Observed(false),
    }
}

/// POST /ui-bridge/control/wait-for-element-state
///
/// Convenience wrapper around `wait_for_element` for the common case of
/// "wait until element <id> is visible / enabled / disabled / ariaDisabled /
/// focused". Polls the element registry every ~100ms until the state matches
/// or the timeout elapses.
///
/// Each tick reads the requested flag through [`probe_state_field`], which
/// knows which signals the live bridge build can actually report. When the
/// wait expires on a signal this build does not emit, the `found:false`
/// envelope carries `reason: "state-not-observable"` plus the
/// `requested_state`, so a driver can tell "the element never got there" from
/// "this bridge cannot answer that question" instead of retrying forever.
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
    //
    // Loop iter-2 item 3 — on an unknown state name return a flat HTTP 400
    // envelope carrying `error_detail.code = "INVALID_STATE"` and the
    // `context.allowed_states` enum so callers can self-correct without
    // parsing prose. Previously emitted `api_error(prose)` with no code,
    // making the error indistinguishable from a generic transport failure.
    let state_field = match resolve_state_field(&state_name) {
        Some(field) => field,
        None => {
            let detail = UiBridgeError::invalid_state(&state_name, ALLOWED_STATES);
            let msg = detail.message.clone();
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error_detailed(msg, detail)),
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

    // Whether the MOST RECENT successful lookup could not report the
    // requested signal at all. Recomputed every poll rather than latched, so
    // a bridge that starts emitting the field mid-wait is not still reported
    // as unable to answer.
    let mut signal_unobservable = false;

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
            match probe_state_field(&element, state_field) {
                StateProbe::Observed(true) => {
                    let elapsed_ms = start.elapsed().as_millis() as u64;
                    return Ok(Json(ApiResponse::success(serde_json::json!({
                        "found": true,
                        "elapsed_ms": elapsed_ms,
                        "state": element.get("state").cloned().unwrap_or(serde_json::Value::Null),
                    }))));
                }
                StateProbe::Observed(false) => signal_unobservable = false,
                StateProbe::NotObservable => signal_unobservable = true,
            }
        }

        if std::time::Instant::now() + std::time::Duration::from_millis(poll_interval_ms) > deadline
        {
            let elapsed_ms = start.elapsed().as_millis() as u64;
            let mut payload = serde_json::json!({
                "found": false,
                "elapsed_ms": elapsed_ms,
            });
            // A timeout on a signal this bridge build cannot report is not
            // the same answer as "the element never got there", and a driver
            // that cannot tell them apart will retry forever against a build
            // that will never say yes. Name the cause instead of letting the
            // bare `found:false` imply an observation we never made.
            if signal_unobservable {
                payload["reason"] = serde_json::Value::from("state-not-observable");
                payload["requested_state"] = serde_json::Value::from(state_name.clone());
            }
            return Ok(Json(ApiResponse::success(payload)));
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

#[cfg(test)]
mod invalid_state_envelope_tests {
    //! Iter-2 item 3 — `wait-for-element-state` with an unknown `state`
    //! value must return a flat HTTP 400 envelope whose machine-readable
    //! `error_detail.code` is `INVALID_STATE` and whose
    //! `error_detail.context.allowed_states` enumerates the valid options.
    //!
    //! Locks down the constructor's shape so callers can match on
    //! `error_detail.code` and self-correct via `context.allowed_states`
    //! without parsing the prose `error` field. The handler-side wiring
    //! (`api_error_detailed(msg, detail)` + HTTP 400) is too IPC-coupled
    //! for a pure unit test; this test guards the envelope contract.
    use super::{resolve_state_field, UiBridgeError, ALLOWED_STATES};
    use crate::mcp::types::api_error_detailed;
    use crate::mcp::ui_bridge::types::UiBridgeErrorCode;

    /// The handler's own allow-list — NOT a copy. A local duplicate is how
    /// this test stayed green while the handler rejected the post-#144
    /// `disabled`/`ariaDisabled` names.
    const ALLOWED: &[&str] = ALLOWED_STATES;

    #[test]
    fn invalid_state_constructor_sets_code_and_context() {
        let detail = UiBridgeError::invalid_state("banana", ALLOWED);
        assert!(
            matches!(detail.code, UiBridgeErrorCode::InvalidState),
            "code must be InvalidState"
        );
        assert!(
            detail.message.contains("banana"),
            "message echoes the rejected value"
        );
        assert!(
            detail
                .message
                .contains("visible|enabled|disabled|ariaDisabled|focused"),
            "message lists the allowed states pipe-separated"
        );
        let ctx = detail
            .context
            .as_ref()
            .expect("context must carry the structured payload");
        assert_eq!(
            ctx.get("code").and_then(|v| v.as_str()),
            Some("INVALID_STATE")
        );
        assert_eq!(
            ctx.get("provided_state").and_then(|v| v.as_str()),
            Some("banana")
        );
        let allowed_arr = ctx
            .get("allowed_states")
            .and_then(|v| v.as_array())
            .expect("context.allowed_states must be an array");
        let names: Vec<&str> = allowed_arr.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(
            names,
            vec!["visible", "enabled", "disabled", "ariaDisabled", "focused"]
        );
    }

    /// ui-bridge PR #144 split `ElementState.enabled` into `disabled` +
    /// `ariaDisabled`, and schemas PR #128 landed both on the canonical Rust
    /// `ElementState`. A driver must be able to `waitFor` either one: the
    /// allow-list has to accept the name AND `resolve_state_field` has to
    /// resolve it, or the handler 400s on a name the schema declares.
    #[test]
    fn post_144_disabled_states_are_accepted_and_resolve() {
        for name in ["disabled", "ariaDisabled"] {
            assert!(
                ALLOWED_STATES.contains(&name),
                "'{name}' must be in ALLOWED_STATES"
            );
            assert_eq!(
                resolve_state_field(name),
                Some(name),
                "'{name}' must resolve to the identically-spelled ElementState field"
            );
        }
    }

    /// The wire spelling is camelCase (`ElementState` is
    /// `#[serde(rename_all = "camelCase")]`), so the Rust-side `aria_disabled`
    /// must NOT be accepted — silently taking it would poll a field the
    /// bridge never emits and time out instead of 400ing.
    #[test]
    fn snake_case_aria_disabled_is_rejected() {
        assert!(!ALLOWED_STATES.contains(&"aria_disabled"));
        assert_eq!(resolve_state_field("aria_disabled"), None);
    }

    /// Every accepted name resolves, and nothing resolves that is not
    /// accepted — the drift this test module previously failed to catch.
    #[test]
    fn allow_list_and_resolver_agree() {
        for name in ALLOWED_STATES {
            assert!(
                resolve_state_field(name).is_some(),
                "allowed state '{name}' does not resolve to a field"
            );
        }
        assert_eq!(resolve_state_field("banana"), None);
    }

    #[test]
    fn invalid_state_envelope_serialises_with_code_key() {
        // The "flat envelope with code key" the spec asks for: the response
        // body must include a machine-readable `code` discoverable without
        // parsing the prose `error` field. `error_detail.code` is the
        // canonical channel; `error_detail.context.code` mirrors it so
        // callers that key on `data.code` (per the iter-2 spec example) see
        // it without an extra dereference.
        let detail = UiBridgeError::invalid_state("banana", ALLOWED);
        let msg = detail.message.clone();
        let envelope = api_error_detailed(msg, detail);
        let env_json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(
            env_json.get("success").and_then(|v| v.as_bool()),
            Some(false),
            "contract: success:false"
        );
        let ed = env_json
            .get("error_detail")
            .expect("contract: error_detail field must be present (camelCased ApiResponse)");
        assert_eq!(
            ed.get("code").and_then(|v| v.as_str()),
            Some("INVALID_STATE"),
            "contract: error_detail.code must be the literal 'INVALID_STATE'"
        );
        let ctx = ed
            .get("context")
            .expect("error_detail.context must be present");
        assert_eq!(
            ctx.get("code").and_then(|v| v.as_str()),
            Some("INVALID_STATE"),
            "contract: context.code mirrors error_detail.code so flat consumers see it"
        );
        assert!(
            env_json.get("error").and_then(|v| v.as_str()).is_some(),
            "contract: prose `error` field must still be populated for human readers"
        );
    }
}

#[cfg(test)]
mod state_probe_tests {
    //! `wait-for-element-state` polls `get_element` and asks one question per
    //! tick: "is <field> true yet?". Accepting a state name is only half the
    //! contract — the probe has to give an honest answer on the bridge build
    //! that is actually running.
    //!
    //! The regression these lock down: `disabled` / `ariaDisabled` were added
    //! to `ALLOWED_STATES` ahead of the bridge that emits them.
    //! `@qontinui/ui-bridge@0.22.0` (this repo depends on `^0.22.0`) builds
    //! element state as `visible` / `enabled` / `focused` only, so an
    //! absent-reads-as-`false` probe made both names permanently unmatchable
    //! — accepted at the door, then silently timed out.
    use super::{probe_state_field, resolve_state_field, StateProbe, ALLOWED_STATES};

    /// The element shape `@qontinui/ui-bridge@0.22.0` actually serializes:
    /// `getElementState()` writes exactly these interactivity flags, and the
    /// published `ElementState` type declares no `disabled`/`ariaDisabled`.
    fn pre_split_element(enabled: bool) -> serde_json::Value {
        serde_json::json!({
            "id": "btn",
            "state": { "visible": true, "enabled": enabled, "focused": false },
        })
    }

    /// A post-split bridge, where both signals are on the wire independently.
    fn post_split_element(disabled: bool, aria_disabled: bool) -> serde_json::Value {
        serde_json::json!({
            "id": "btn",
            "state": {
                "visible": true,
                "enabled": !(disabled || aria_disabled),
                "disabled": disabled,
                "ariaDisabled": aria_disabled,
                "focused": false,
            },
        })
    }

    #[test]
    fn plain_bool_flags_are_read_from_the_state_block() {
        let el = pre_split_element(true);
        assert_eq!(
            probe_state_field(&el, "visible"),
            StateProbe::Observed(true)
        );
        assert_eq!(
            probe_state_field(&el, "enabled"),
            StateProbe::Observed(true)
        );
        assert_eq!(
            probe_state_field(&el, "focused"),
            StateProbe::Observed(false)
        );
    }

    /// The `discover()` fallback path inside `get_element` returns the match
    /// verbatim rather than a `serializeElement` result, so the flags can sit
    /// at the top level. That fallback predates this change and must survive.
    #[test]
    fn top_level_flags_are_read_when_there_is_no_state_block() {
        let el = serde_json::json!({ "id": "btn", "visible": true });
        assert_eq!(
            probe_state_field(&el, "visible"),
            StateProbe::Observed(true)
        );
    }

    /// `aria-disabled` is a DOM *attribute*, so it reaches the runner as the
    /// string `"true"` on the passthrough paths. `elements.rs` already reads
    /// it that way in the `withDisabledOnly` snapshot filter and in
    /// `execute_action`'s disabled pre-check; a bool-only probe here would
    /// disagree with both and never match.
    #[test]
    fn string_true_is_accepted_for_the_aria_attribute_passthrough() {
        let el = serde_json::json!({
            "state": { "visible": true, "enabled": false, "ariaDisabled": "true" }
        });
        assert_eq!(
            probe_state_field(&el, "ariaDisabled"),
            StateProbe::Observed(true)
        );
        let off = serde_json::json!({
            "state": { "visible": true, "enabled": true, "ariaDisabled": "false" }
        });
        assert_eq!(
            probe_state_field(&off, "ariaDisabled"),
            StateProbe::Observed(false)
        );
    }

    /// On the shipped bridge `disabled` is not on the wire, but it IS soundly
    /// derivable from the `enabled` fold — and this is the same derivation
    /// the SDK's own predicate runtime uses (`evaluateElementPredicate`'s
    /// `disabled` arm is `state.enabled === false`). Without it, a wait on
    /// `disabled` could never return `found:true`.
    #[test]
    fn disabled_derives_from_enabled_on_a_pre_split_bridge() {
        assert_eq!(
            probe_state_field(&pre_split_element(false), "disabled"),
            StateProbe::Observed(true),
            "enabled:false must satisfy a wait on `disabled`"
        );
        assert_eq!(
            probe_state_field(&pre_split_element(true), "disabled"),
            StateProbe::Observed(false),
        );
    }

    /// Forward compatibility: once a post-split bridge puts the real field on
    /// the wire it must win outright, never be masked by the derivation.
    #[test]
    fn an_explicit_disabled_field_wins_over_the_derivation() {
        // Contradictory on purpose — `enabled:true` would derive `false`.
        let el = serde_json::json!({
            "state": { "visible": true, "enabled": true, "disabled": true }
        });
        assert_eq!(
            probe_state_field(&el, "disabled"),
            StateProbe::Observed(true),
            "the wire field is authoritative when present"
        );
        assert_eq!(
            probe_state_field(&post_split_element(false, true), "ariaDisabled"),
            StateProbe::Observed(true)
        );
        assert_eq!(
            probe_state_field(&post_split_element(true, false), "ariaDisabled"),
            StateProbe::Observed(false),
            "a natively-disabled control is not ARIA-disabled"
        );
    }

    /// `ariaDisabled` has no sound pre-split derivation: `enabled` is the
    /// fold `!(disabled || ariaDisabled)`, so deriving from it would report a
    /// natively-disabled control as ARIA-disabled. Report "cannot observe"
    /// rather than inventing an answer or silently timing out on `false`.
    #[test]
    fn aria_disabled_is_not_observable_on_a_pre_split_bridge() {
        assert_eq!(
            probe_state_field(&pre_split_element(false), "ariaDisabled"),
            StateProbe::NotObservable,
            "enabled:false must NOT be reported as ARIA-disabled"
        );
        assert_eq!(
            probe_state_field(&pre_split_element(true), "ariaDisabled"),
            StateProbe::NotObservable,
        );
    }

    /// `visible` / `enabled` / `focused` are required on every bridge build,
    /// so an absent key there is a genuinely absent signal rather than a
    /// build gap. They keep reading `false`, exactly as before this change.
    #[test]
    fn required_flags_absent_still_read_false_not_unobservable() {
        let el = serde_json::json!({ "id": "btn", "state": {} });
        for name in ["visible", "enabled", "focused"] {
            assert_eq!(
                probe_state_field(&el, name),
                StateProbe::Observed(false),
                "'{name}' must keep its pre-existing absent-reads-false behavior"
            );
        }
    }

    /// The contract that ties the allow-list to reality: on the bridge build
    /// this repo actually depends on, no accepted state may be silently stuck
    /// at `false` forever. Either the probe observes it, or it says it cannot
    /// — the one outcome ruled out is an unmatchable name that looks like a
    /// normal negative.
    #[test]
    fn no_allowed_state_is_silently_unmatchable_on_the_shipped_bridge() {
        // Between them these two pre-split snapshots drive every emitted flag
        // to the value a wait is looking for: `visible`/`enabled`/`focused`
        // true in the first, `enabled:false` (the `disabled` source) in the
        // second. Nothing else is on the wire on this bridge build.
        let all_true = serde_json::json!({
            "state": { "visible": true, "enabled": true, "focused": true }
        });
        let disabled_el = pre_split_element(false);
        for name in ALLOWED_STATES {
            let field = resolve_state_field(name).expect("allowed state must resolve");
            let reachable = [&all_true, &disabled_el].iter().any(|el| {
                matches!(
                    probe_state_field(el, field),
                    StateProbe::Observed(true) | StateProbe::NotObservable
                )
            });
            assert!(
                reachable,
                "'{name}' can never return found:true and never says why"
            );
        }
    }
}
