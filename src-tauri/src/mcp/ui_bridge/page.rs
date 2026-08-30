//! Page navigation, evaluation, and tab-switching HTTP handlers.
//!
//! Covers `/control/page/*` (refresh, hard-refresh, close-request, navigate,
//! back, forward, evaluate/evaluate-raw/evaluate-safe/evaluate-batch, set-tab,
//! summary), `/control/query-selector`, `/control/activate-tab/{id}`, and
//! `/control/navigate-and-wait`.
//!
//! Private helpers `tagged_page_evaluate` and `page_evaluate_inner` live here
//! too because the evaluate handlers are their only callers.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, EventTarget};
use tracing::{debug, error, info, warn};

use super::types::{RecoveryHint, UiBridgeError, UiBridgeErrorCode};
use crate::mcp::envelope::{RequestHints, UiBridgeJson};
use crate::mcp::types::{api_error, api_error_detailed, ApiResponse, ApiState};

use super::helpers::{
    direct_webview_evaluate_with_result, evaluate_js_expression, parse_eval_result,
    return_expression_js, safe_evaluate,
};
use super::request::{
    multi_window_dispatch_enabled, ui_bridge_request_sync, wrap_ipc_result, MAIN_WINDOW_LABEL,
    TARGET_WINDOW_FIELD,
};
use super::{ipc_handler_get, ipc_handler_post};

// Macro-generated IPC forwarders for SDK-relayed page-control routes.
// These forward `params` to the webview via `ui_bridge_request_sync` using the
// IPC `kind` shown in each macro invocation. The matching React command
// handlers live in
// `D:\qontinui-root\ui-bridge\packages\ui-bridge\src\react\commandHandlers.ts`
// (only `setViewportConstraints` is currently registered there; the other two
// kinds will return "handler not found" until added).
ipc_handler_post!(
    ui_bridge_set_viewport_constraints_handler,
    "set_viewport_constraints"
);
ipc_handler_get!(ui_bridge_page_get_routes_handler, "get_routes");
ipc_handler_post!(ui_bridge_page_navigate_to_handler, "navigate_by_adapter");

// ============================================================================
// Request / response types
// ============================================================================

/// Navigate-and-wait: execute an action, wait for the DOM to stabilize, then
/// return a fresh snapshot.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigateAndWaitRequest {
    pub element_id: String,
    #[serde(default = "default_nav_action")]
    pub action: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    /// How long (ms) the element count must be stable before we consider the
    /// page settled. Default: 800ms.
    #[serde(default = "default_stable_ms")]
    pub wait_for_stable_ms: u64,
    /// Overall timeout (ms) for the entire operation. Default: 8000ms.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

impl RequestHints for NavigateAndWaitRequest {
    fn shape_error_suggestions() -> Option<Vec<String>> {
        Some(vec![
            "Required fields: `elementId` (string). Optional: `action` (default \"click\"), \
             `params`, `waitForStableMs` (default 800), `timeoutMs` (default 8000)."
                .to_string(),
            "The `action` field must be a supported element action (click, type, hover, etc.)."
                .to_string(),
        ])
    }
    fn shape_error_data() -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "allowedActions": [
                "click", "doubleClick", "double_click", "double", "right", "middle",
                "type", "sendKeys", "clear", "select", "focus", "blur", "hover",
                "scroll", "scrollIntoView", "scroll_into_view", "check", "uncheck",
                "toggle", "drag", "setValue", "submit", "reset", "autocomplete"
            ]
        }))
    }
}

fn default_nav_action() -> String {
    "click".into()
}
fn default_stable_ms() -> u64 {
    800
}
fn default_timeout_ms() -> u64 {
    8000
}

/// Request for page navigation.
///
/// `mode` is optional and defaults to `"hard"`. Both modes perform a tab
/// navigation and neither reloads the document — `"hard"` additionally
/// `pushState`s the URL, `"soft"` additionally fires a synthetic `popstate`
/// and a `ui-bridge:navigate` event. Both preserve injected window state
/// (fetch patches, spies, `window.__*` globals). Any value other than
/// `"hard"` / `"soft"` is rejected with a 400. See
/// [`ui_bridge_page_navigate_handler`] for why `"hard"` is not a reload.
#[derive(Debug, Deserialize)]
pub struct PageNavigateRequest {
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) mode: Option<String>,
}

impl RequestHints for PageNavigateRequest {
    fn shape_error_suggestions() -> Option<Vec<String>> {
        Some(vec![
            "Required field: `url` (string). Optional: `mode` (\"hard\" | \"soft\", \
             default \"hard\")."
                .to_string(),
            "Both modes do a tab navigation and NEITHER reloads the document: \
             \"hard\" also pushState's the URL, \"soft\" also fires a synthetic \
             popstate. Both preserve injected window state. The response's \
             `hard`/`reloaded` fields are therefore always false."
                .to_string(),
        ])
    }
    fn shape_error_data() -> Option<serde_json::Value> {
        Some(serde_json::json!({ "allowedModes": ["hard", "soft"] }))
    }
}

/// Request for CSS selector query
#[derive(Debug, Deserialize)]
pub struct QuerySelectorRequest {
    pub selector: String,
    /// Optional action to perform on matched element(s): "click"
    pub action: Option<String>,
    /// Index of the matched element to perform the action on (default: 0)
    pub index: Option<u32>,
}

impl RequestHints for QuerySelectorRequest {
    fn shape_error_suggestions() -> Option<Vec<String>> {
        Some(vec![
            "Required field: `selector` (CSS selector string). \
             Optional: `action` (\"click\"), `index` (number, default 0)."
                .to_string(),
        ])
    }
}

/// Request for page evaluation
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageEvaluateRequest {
    pub expression: String,
    /// Optional timeout override in milliseconds (clamped to [1000, 600000]).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// When true, if the expression returns a Promise the runner awaits it.
    ///
    /// Only affects the **direct-eval fallback**
    /// (`helpers.rs::direct_webview_evaluate_with_result`), which is reached
    /// when the tagged IPC path fails for the main window. The tagged path
    /// itself always auto-awaits regardless of this flag, because handing back
    /// an unresolved Promise there just structured-clones to `{}` — a
    /// success envelope carrying no data.
    #[serde(default)]
    pub await_promise: bool,
    /// EXPLICIT opt-in. When true, the frontend evaluate handler relaxes
    /// only the four network-related blocks (fetch / XMLHttpRequest /
    /// sendBeacon / WebSocket) so test assertions can hit runner APIs
    /// directly. Structural code-injection blocks stay in force regardless.
    #[serde(default)]
    pub allow_network_requests: Option<bool>,
    /// Optional target pop-out window (`{ "windowLabel": "term-1" }`),
    /// mirroring `execute_action` / `get_elements`. Absent / empty → the main
    /// window. A `?windowLabel=` query param takes precedence over this body
    /// field (see [`EvaluateQueryParams`]). Without this, `page/evaluate`
    /// broadcasts the expression to every open runner window.
    #[serde(default)]
    pub window_label: Option<String>,
}

/// Optional query parameters for `page/evaluate`.
///
/// `?windowLabel=term-1` targets a pop-out terminal window (discoverable via
/// `GET /ui-bridge/control/runner-windows`). Omitted → the main window. Mirrors
/// [`super::elements::ActionQueryParams`] so the routing convention is identical
/// across control routes. A query param takes precedence over the body's
/// `windowLabel` so a caller can address a window without rewriting the body.
#[derive(Debug, Deserialize, Default)]
pub struct EvaluateQueryParams {
    #[serde(default, rename = "windowLabel")]
    pub window_label: Option<String>,
}

impl RequestHints for PageEvaluateRequest {
    fn shape_error_suggestions() -> Option<Vec<String>> {
        Some(vec!["Required field: `expression` (JavaScript string). \
             Optional: `timeoutMs` (number, 1000–600000), `awaitPromise` (bool), \
             `unwrap` (bool), `allowNetworkRequests` (bool)."
            .to_string()])
    }
}

/// Request to evaluate multiple JS expressions in one round-trip.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchEvaluateRequest {
    pub expressions: Vec<BatchExpression>,
}

impl RequestHints for BatchEvaluateRequest {
    fn shape_error_suggestions() -> Option<Vec<String>> {
        Some(vec![
            "Required field: `expressions` (array of `{id, expression}` objects). \
             Maximum 50 expressions per batch."
                .to_string(),
        ])
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchExpression {
    pub id: String,
    pub expression: String,
}

/// Result of a single batch expression evaluation.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchExpressionResult {
    pub id: String,
    pub success: bool,
    pub value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTabRequest {
    pub tab: String,
}

impl RequestHints for SetTabRequest {
    fn shape_error_suggestions() -> Option<Vec<String>> {
        Some(vec![
            "Required field: `tab` (string). Must be a known tab id from VALID_TAB_IDS \
             (e.g. \"specs\", \"tasks\", \"terminal\", \"settings\"). \
             Use GET /ui-bridge/control/tabs for the full list."
                .to_string(),
        ])
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTabResponse {
    pub success: bool,
    pub tab: String,
    /// Value of `[data-page-id]` on the active page after the tab change
    /// (null if no element with that attribute is present).
    pub page_id: Option<String>,
}

/// Request body for `POST /ui-bridge/control/tab/activate` (F4).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabActivateRequest {
    pub tab_id: String,
}

impl RequestHints for TabActivateRequest {
    fn shape_error_suggestions() -> Option<Vec<String>> {
        Some(vec![
            "Required field: `tabId` (string). Must be a known tab id \
             (e.g. \"specs\", \"tasks\", \"terminal\", \"settings\"). \
             Use GET /ui-bridge/control/tabs for the full list."
                .to_string(),
        ])
    }
}

// ============================================================================
// Tab id registry (shared between set-tab and activate-tab)
// ============================================================================

// `VALID_TAB_IDS` — the valid `MainTabId` values. GENERATED at build time from
// the TypeScript `VALID_TAB_IDS` array in `src/components/app/tab-types.ts`
// (see `src-tauri/build.rs::generate_valid_tab_ids`).
//
// This slice used to be a hand-copied mirror annotated "Kept in sync manually"
// — and it was not: it had drifted to 103 entries against the union's 106, so
// ids the frontend advertised via `GET /control/tabs` were rejected by
// `tab/activate` as `unknown_tab` (e.g. `memory-federation`). The gate itself
// is still enforced here; only its CONTENTS are derived, so there is exactly
// one place to add a tab id.
//
// (A `///` doc comment cannot precede an `include!` — rustc's
// `unused_doc_comments` fires, and cargo-prepush's `clippy --all-targets` is
// stricter than CI. Hence the plain `//` block.)
include!(concat!(env!("OUT_DIR"), "/valid_tab_ids.rs"));

// `VALID_NAVIGATE_PAGES` — the page keys `POST /control/page/navigate` can
// actually reach. GENERATED at build time from the TypeScript `PAGE_TO_TAB`
// map in `src/components/app/useAppNavigation.ts` (see
// `src-tauri/build.rs::generate_valid_navigate_pages`), for the same
// one-source-of-truth reason `VALID_TAB_IDS` above is generated.
//
// NOT the same set as `VALID_TAB_IDS`: `PAGE_TO_TAB` also carries navigation
// aliases that are not tab ids (`home`, `run`, `ai`, `history`, `logs`,
// `run-summary`, …), and `page/navigate` is routed through `PAGE_TO_TAB`
// alone — the `ui-bridge-navigate` listener in `useAppNavigation.ts` looks up
// nothing else. Gating on tab ids would reject real pages.
include!(concat!(env!("OUT_DIR"), "/valid_navigate_pages.rs"));

/// Map a `page/navigate` URL onto the page key the frontend will look up in
/// `PAGE_TO_TAB`, then answer whether that key resolves to anything.
///
/// The transform MIRRORS `usePageEvents.ts`'s `page_navigate` case byte for
/// byte — strip leading slashes, turn the remaining `/` into `-`, empty means
/// `gui-automation`. Deliberately no query/hash stripping: the frontend does
/// not strip them either, so `/terminal?x=1` genuinely does NOT navigate, and
/// a gate that accepted it would go straight back to reporting success for a
/// navigation that never happened. The gate's job is to predict the
/// frontend's behaviour exactly, not to be more generous than it.
///
/// Returns the resolved page key on success, or the rejected key on failure.
pub(crate) fn resolve_navigate_page(url: &str) -> Result<String, String> {
    let page = url.trim_start_matches('/').replace('/', "-");
    let page = if page.is_empty() {
        "gui-automation".to_string()
    } else {
        page
    };
    if VALID_NAVIGATE_PAGES.contains(&page.as_str()) {
        Ok(page)
    } else {
        Err(page)
    }
}

// ============================================================================
// Navigate-and-wait
// ============================================================================

pub async fn ui_bridge_navigate_and_wait_handler(
    State(state): State<Arc<ApiState>>,
    UiBridgeJson(req): UiBridgeJson<NavigateAndWaitRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    use tokio::time::{sleep, Instant};

    info!(
        "UI Bridge API: navigate-and-wait — {} on {}",
        req.action, req.element_id
    );

    let deadline = Instant::now() + std::time::Duration::from_millis(req.timeout_ms);
    let stable_dur = std::time::Duration::from_millis(req.wait_for_stable_ms);

    // 1. Execute the action
    let action_payload = serde_json::json!({
        "elementId": req.element_id,
        "action": {
            "action": req.action,
            "params": req.params.unwrap_or(serde_json::json!({})),
        }
    });
    match ui_bridge_request_sync(&state, "execute_action", action_payload).await {
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
        Ok(ref data) => {
            if let Some(false) = data.get("success").and_then(|v| v.as_bool()) {
                let msg = data
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Action failed on element");
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(api_error(format!(
                        "navigate-and-wait: action '{}' on '{}' failed: {}",
                        req.action, req.element_id, msg
                    ))),
                ));
            }
        }
    }

    // 2. Brief initial delay for route transition / React render to start
    sleep(std::time::Duration::from_millis(200)).await;

    // 3. Poll for DOM stability: discover repeatedly, wait until element count
    //    is unchanged for `waitForStableMs`.
    let mut last_count: Option<usize> = None;
    let mut stable_since = Instant::now();
    let mut timed_out = false;

    loop {
        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }

        let count = match ui_bridge_request_sync(
            &state,
            "discover",
            serde_json::json!({"interactive_only": false}),
        )
        .await
        {
            Ok(data) => data
                .get("elements")
                .and_then(|e| e.as_array())
                .map(|a| a.len())
                .unwrap_or(0),
            Err(_) => 0,
        };

        match last_count {
            Some(prev) if prev == count => {
                if stable_since.elapsed() >= stable_dur {
                    break; // DOM is stable
                }
            }
            _ => {
                last_count = Some(count);
                stable_since = Instant::now();
            }
        }

        sleep(std::time::Duration::from_millis(200)).await;
    }

    // 4. Return fresh snapshot
    let snapshot = ui_bridge_request_sync(&state, "get_snapshot", serde_json::json!({})).await;

    match snapshot {
        Ok(mut data) => {
            if timed_out {
                if let Some(obj) = data.as_object_mut() {
                    obj.insert(
                        "navigateAndWait".to_string(),
                        serde_json::json!({
                            "timedOut": true,
                            "timeoutMs": req.timeout_ms,
                            "lastElementCount": last_count,
                        }),
                    );
                }
            }
            Ok(Json(ApiResponse::success(data)))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

// ============================================================================
// Page lifecycle (refresh, hard-refresh, close-request, navigate, back, forward)
// ============================================================================

/// Refresh the page.
pub async fn ui_bridge_page_refresh_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Page refresh");

    wrap_ipc_result(ui_bridge_request_sync(&state, "page_refresh", serde_json::json!({})).await)
}

/// Hard refresh the page, bypassing browser cache.
pub async fn ui_bridge_page_hard_refresh_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    use tauri::Manager;
    info!("UI Bridge API: Hard refresh (cache bypass)");

    if let Some(window) = state
        .app_handle
        .get_webview_window(qontinui_runner_lib::get_main_window_label())
    {
        // Use fetch cache-busting + location replacement to bypass browser cache.
        let js = r#"
            (function() {
                var url = new URL(location.href);
                url.searchParams.set('_hrc', Date.now());
                location.replace(url.toString());
            })();
        "#;
        window.eval(js).map_err(|e| {
            let msg = format!("Failed to hard refresh: {}", e);
            error!("{}", msg);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(msg)))
        })?;
        Ok(Json(ApiResponse::success(serde_json::json!({
            "success": true,
            "message": "Hard refresh triggered"
        }))))
    } else {
        Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error("Main webview window not found".to_string())),
        ))
    }
}

// ============================================================================
// Native event-loop liveness gate for the close doors
// ============================================================================
//
// Plan `2026-08-19-runner-blocked-ui-thread-cannot-be-closed`, Phase 3.
//
// `window.close()` is NOT a call — it is an enqueue.
// `tauri-runtime-wry-2.11.2/src/lib.rs:2270-2277` deliberately bypasses
// `send_user_message` and does a bare
// `proxy.send_event(Message::Window(id, WindowMessage::Close))`, carrying
// upstream's own NOTE saying so. Onto a wedged loop that queues exactly like
// `WM_CLOSE`: nothing happens, and this route used to answer
// `200 {"success": true, "message": "Close requested; …handler should now
// fire"}` — a claim it had no way to check and, during the 2026-08-19
// incident, an actively false one.
//
// So the door asks first, using the ONE probe that does not share fate with
// the thing it measures: `health_monitor::ui_thread_pumping()`
// (`SendMessageTimeoutW(WM_NULL, SMTO_ABORTIFHUNG)`), plus the latched
// `ui_thread_wedged()` verdict the monitor already maintains.

/// What the liveness gate concluded about the native event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventLoopVerdict {
    /// A `WM_NULL` round-trip completed inside the probe's timeout.
    Pumping,
    /// Confirmed not pumping. `reason` discriminates how we know.
    Wedged(&'static str),
    /// Could not ask. **Treated as a refusal on Windows** — see
    /// [`event_loop_verdict`].
    Unknown(&'static str),
}

impl EventLoopVerdict {
    /// The refusal body, or `None` when the door may proceed.
    ///
    /// # UNKNOWN is a refusal on Windows, and a pass everywhere else
    ///
    /// This asymmetry is deliberate and is the crux of the step.
    ///
    /// On **Windows**, UNKNOWN means the probe exists, the window exists, and
    /// the probe still could not establish that the loop is pumping (no
    /// resolvable `HWND`, or a handle the OS rejects). Answering `200` there
    /// is precisely the defect this step removes: the route would once again
    /// be claiming a success it cannot verify. `503` with
    /// `context.confirmed: false` says the honest thing instead — *refused,
    /// not observed* — and keeps the two cases distinguishable on the wire, so
    /// a caller is never told a maybe is a yes.
    ///
    /// On **non-Windows**, UNKNOWN means something different: there is no
    /// probe at all. `SendMessageTimeoutW` asks a Win32 question and
    /// `ui_thread_pumping()` is a `None`-returning stub off Windows, so a
    /// blanket refusal would break this route on every macOS and Linux build
    /// for a condition it cannot even be in the same way — those platforms run
    /// the webview on the same main thread, so a blocked loop stops the HTTP
    /// pong too and the shipped 90 s `ui_stale` arm does cover it. Absence of
    /// a detector is not evidence of a hang, so the pre-existing behaviour is
    /// preserved there verbatim.
    fn refusal(self) -> Option<(bool, &'static str)> {
        match self {
            EventLoopVerdict::Pumping => None,
            EventLoopVerdict::Wedged(reason) => Some((true, reason)),
            EventLoopVerdict::Unknown(reason) => {
                #[cfg(windows)]
                {
                    Some((false, reason))
                }
                #[cfg(not(windows))]
                {
                    let _ = reason;
                    None
                }
            }
        }
    }
}

/// Ask whether the native event loop is pumping, without blocking a tokio
/// worker for the probe's timeout.
///
/// Order matters. The latched wedge verdict — read through
/// `ui_error::native_ui_probe_verdict()`, the same guarded reader every
/// `derived_status` sink uses — is a free atomic read maintained by the health
/// monitor's own OS thread across `WEDGE_FAILURE_THRESHOLD` consecutive
/// samples, so it is both cheaper and *better evidence* than a single fresh
/// sample: it has already survived the noise floor. Only when it is clear do
/// we pay for a live probe, and that probe (`SendMessageTimeoutW`, up to 3 s)
/// goes to `spawn_blocking` under a ceiling: it is a synchronous Win32 round
/// trip and must neither park an async worker nor await one without a bound.
async fn event_loop_verdict() -> EventLoopVerdict {
    // The SAME guarded read the `derived_status` sinks use
    // (`ui_error::native_ui_probe_verdict`), not the raw atomic. Reading
    // `health_monitor::ui_thread_wedged()` directly made this route disagree
    // with `/health` in a way that is worse than either answer alone:
    // `stop_health_monitor()` clears `MONITOR_RUNNING` without clearing the
    // wedge atomics, so this door answered `Wedged("wedge_detector_latched")`
    // off a DEAD monitor — refusing every close forever — while `/health`
    // correctly reported UNKNOWN. One reader, one guard.
    if crate::ui_error::native_ui_probe_verdict() == Some(true) {
        return EventLoopVerdict::Wedged("wedge_detector_latched");
    }

    // BOUNDED. `ui_thread_pumping` is a blocking `SendMessageTimeoutW` capped
    // at `UI_THREAD_PROBE_TIMEOUT_MS` (3 s), but "capped" is a property of the
    // Win32 call, not of this `await`: a `spawn_blocking` join has no deadline
    // of its own, so a future regression in the probe (or a saturated blocking
    // pool that never starts the task) would park `close-request` with no
    // ceiling at all — on the one route an operator reaches for when the
    // window will not close. The ceiling sits just above the probe's own so a
    // healthy-but-slow sample still lands.
    let probe = tokio::task::spawn_blocking(crate::health_monitor::ui_thread_pumping);
    match tokio::time::timeout(EVENT_LOOP_PROBE_CEILING, probe).await {
        Ok(Ok(Some(sample))) if sample.pumping => EventLoopVerdict::Pumping,
        Ok(Ok(Some(_))) => EventLoopVerdict::Wedged("probe_no_round_trip"),
        Ok(Ok(None)) => EventLoopVerdict::Unknown("probe_unavailable"),
        Ok(Err(e)) => {
            warn!("Event-loop liveness probe task failed: {e}");
            EventLoopVerdict::Unknown("probe_task_failed")
        }
        Err(_elapsed) => {
            // The probe is itself bounded, so overrunning this ceiling means
            // the blocking pool never ran it (or the bound regressed). Either
            // way we did not observe the loop — UNKNOWN, which on Windows is
            // already a refusal.
            warn!(
                "Event-loop liveness probe did not return within {:?} — reporting UNKNOWN",
                EVENT_LOOP_PROBE_CEILING
            );
            EventLoopVerdict::Unknown("probe_timed_out")
        }
    }
}

/// Ceiling on the whole `spawn_blocking` round-trip in [`event_loop_verdict`].
///
/// One second of headroom over `health_monitor::UI_THREAD_PROBE_TIMEOUT_MS`
/// (3 s), which is the probe's own internal bound; the margin covers pool
/// scheduling, not a slower probe. A test pins the ordering.
const EVENT_LOOP_PROBE_CEILING: std::time::Duration = std::time::Duration::from_secs(4);

/// Simulate a user clicking the window's X button.
///
/// Returns `503 EVENT_LOOP_UNRESPONSIVE` instead of a fabricated `200` when
/// the native event loop is not pumping — see [`EventLoopVerdict`] for the
/// mechanism and for why UNKNOWN also refuses on Windows. The refusal body
/// names `POST /ui-bridge/control/page/force-close`, which is the door that
/// still works in that state.
pub async fn ui_bridge_page_close_request_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    use tauri::Manager;
    info!("UI Bridge API: Close request (simulating X-button click)");

    let Some(window) = state
        .app_handle
        .get_webview_window(qontinui_runner_lib::get_main_window_label())
    else {
        // Checked BEFORE the liveness gate on purpose: with no main window
        // there is nothing to close and nothing to probe, and this route has
        // always answered 500 for it. A headless/server-mode runner must keep
        // getting that answer rather than a misleading "the loop is wedged".
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error("Main webview window not found".to_string())),
        ));
    };

    if let Some((confirmed, reason)) = event_loop_verdict().await.refusal() {
        warn!(
            reason,
            confirmed, "Refusing close-request: the native event loop is not pumping"
        );
        let detail = UiBridgeError::event_loop_unresponsive(reason, confirmed);
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error_detailed(detail.message.clone(), detail)),
        ));
    }

    window.close().map_err(|e| {
        let msg = format!("Failed to close main window: {}", e);
        error!("{}", msg);
        (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(msg)))
    })?;
    Ok(Json(ApiResponse::success(serde_json::json!({
        "success": true,
        "message": "Close requested; Tauri WindowEvent::CloseRequested handler should now fire"
    }))))
}

/// Request body for `POST /ui-bridge/control/page/force-close`.
///
/// Parsed leniently (see [`ui_bridge_page_force_close_handler`]) rather than
/// through the `UiBridgeJson` envelope extractor the rest of this family uses:
/// a door whose entire purpose is to work when the runner is wedged must not
/// be defeated by a missing `Content-Type` header on an operator's bare
/// `curl -XPOST`. There is exactly one optional field, so the shape-error
/// hints the envelope would add have nothing to say.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForceCloseRequest {
    /// Free-text audit line recording *who asked and why*. Optional — the
    /// route name is itself explicit — but it is logged and echoed back, and
    /// callers should set it.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Mandatory header on `POST /ui-bridge/control/page/force-close`.
///
/// # Why a header, on this route specifically
///
/// The MCP API's CORS layer is `allow_origin(Any)` by deliberate design
/// (`mcp_api.rs`: MCP clients, WSL, and the `tauri://localhost` webview all
/// have to reach it, and the security boundary is the loopback bind). A
/// **simple** cross-origin `POST` — no custom header, no JSON `Content-Type`,
/// no body — is exempt from the CORS preflight entirely, so any page in the
/// operator's browser could previously force-close the runner and, with it,
/// `taskkill /F /T` every tracked agent tree. `close-request` and
/// `/restart-runner` share that shape, so the class is not new; what is new is
/// that force-close is the most *reliable* of the three and the only one that
/// still works when the others refuse.
///
/// Requiring ANY custom header takes the request out of the simple-request set
/// and forces an `OPTIONS` preflight, which a drive-by page cannot satisfy
/// without the runner opting in. It costs a legitimate caller one flag:
///
/// ```text
/// curl -X POST -H "X-Qontinui-Force-Close: 1" http://127.0.0.1:9876/ui-bridge/control/page/force-close
/// ```
///
/// The body stays optional and lenient (see [`parse_force_close_body`]) — the
/// header is the whole gate, so a wedged-runner rescue still needs nothing but
/// `curl`.
pub const FORCE_CLOSE_HEADER: &str = "x-qontinui-force-close";

/// True when the mandatory [`FORCE_CLOSE_HEADER`] is present.
///
/// Presence is the entire test: the value is unconstrained on purpose, because
/// it is the *preflight* that does the work, not the contents. Pure, so the
/// contract can be asserted without an HTTP stack.
fn force_close_header_present(headers: &axum::http::HeaderMap) -> bool {
    headers.contains_key(FORCE_CLOSE_HEADER)
}

/// Parse a force-close body without ever failing the request.
///
/// An empty body, a body with no `Content-Type`, or malformed JSON all degrade
/// to the default (no stated reason) instead of a 4xx. Rejecting the one door
/// that works during a wedge because the operator forgot a header would
/// reintroduce, in a new place, exactly the "no way out of the process" defect
/// this route exists to close. (The one header that IS required is
/// [`FORCE_CLOSE_HEADER`], and it is checked before the body is looked at.)
fn parse_force_close_body(body: &str) -> ForceCloseRequest {
    if body.trim().is_empty() {
        return ForceCloseRequest::default();
    }
    match serde_json::from_str::<ForceCloseRequest>(body) {
        Ok(parsed) => parsed,
        Err(e) => {
            warn!("Force-close body was not valid JSON ({e}) — proceeding with no stated reason");
            ForceCloseRequest::default()
        }
    }
}

/// Shut the runner down along a path that does not need the event loop.
///
/// # This is the only door out of a wedged process that is not a `taskkill`
///
/// Everything else routes through the blocked loop.
/// [`ui_bridge_page_close_request_handler`] enqueues `WindowMessage::Close`;
/// `AppHandle::exit(0)` (`tauri-2.11.1/src/app.rs:573-580` → `request_exit`,
/// `tauri-runtime-wry-2.11.2/src/lib.rs:2751-2757`) is *structurally
/// identical* — a bare `proxy.send_event(Message::RequestExit(code))` that
/// also bypasses `send_user_message`. Its internal `std::process::exit` escape
/// hatch fires only when `send_event` returns `Err`, i.e. when the loop is
/// already **dead**, never when it is merely **wedged**. So on this door
/// `std::process::exit(0)` after the teardown is the *expected* terminator,
/// not an edge case — `main.rs`'s own force-exit watchdog documents having
/// learned the same thing.
///
/// # Bound to an explicit close action — design decision 3
///
/// Force-exit is permitted **only** downstream of an explicit user/operator
/// close request. Nothing in this tree force-exits on bare hang detection: the
/// health monitor detects and surfaces, and stops there. A user who asked to
/// close has consented to losing the window; nobody consents to losing every
/// in-flight agent session because a background probe timed out (102 live
/// `claude.exe` children in the incident that prompted this). This handler is
/// reachable only by an explicit `POST` to a route whose name says what it
/// does, and is called by no detector.
///
/// Unlike `POST /restart-runner` — the only pre-existing HTTP-reachable
/// `process::exit`, which skips teardown entirely — this runs the bounded
/// teardown sequence first, so WIP is stashed, terminals and orphaned agent
/// process trees are killed, the clean-shutdown marker is stamped and
/// `active_instances.json` is cleared.
///
/// # Required header
///
/// Every call must carry [`FORCE_CLOSE_HEADER`] (`X-Qontinui-Force-Close`,
/// any value). Without it the route answers `400` and does nothing — see that
/// constant for why a header rather than an origin allowlist.
pub async fn ui_bridge_page_force_close_handler(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    body: String,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // CSRF gate — see `FORCE_CLOSE_HEADER`. Checked FIRST, before any state is
    // touched, so a rejected request cannot have started a teardown.
    if !force_close_header_present(&headers) {
        warn!(
            "Rejecting force-close: the mandatory {FORCE_CLOSE_HEADER} header is absent (a \
             simple cross-origin POST cannot send it)"
        );
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "force-close requires the '{FORCE_CLOSE_HEADER}' header (any value); it forces \
                 a CORS preflight that a drive-by cross-origin POST cannot satisfy. \
                  e.g. curl -X POST -H '{FORCE_CLOSE_HEADER}: 1' <url>"
            ))),
        ));
    }

    let request = parse_force_close_body(&body);
    let reason = request
        .reason
        .unwrap_or_else(|| "operator force-close via /ui-bridge/control/page/force-close".into());

    // Report the loop's state alongside the acknowledgement. The door does not
    // *depend* on the verdict — an operator who asked to close is entitled to
    // close a healthy runner too — but it is the one moment we can cheaply
    // record whether this was a wedge or a routine quit.
    let verdict = event_loop_verdict().await;

    let accepted = crate::mcp::ai_session::emergency_quit::request_force_close(
        state.app_handle.clone(),
        state.app_state.clone(),
        reason.clone(),
    );

    Ok(Json(ApiResponse::success(serde_json::json!({
        "success": true,
        "accepted": accepted,
        "reason": reason,
        "eventLoop": match verdict {
            EventLoopVerdict::Pumping => "pumping",
            EventLoopVerdict::Wedged(_) => "wedged",
            EventLoopVerdict::Unknown(_) => "unknown",
        },
        "budgetSecs":
            crate::mcp::ai_session::emergency_quit::force_close_budget().as_secs(),
        "message": if accepted {
            "Force-close accepted; teardown is running off the event loop and the process \
             will exit within its budget"
        } else {
            "Force-close already in progress; the existing teardown owns the exit"
        },
    }))))
}

/// Same-origin absolute URL → the app-relative path the frontend can act on.
///
/// Returns `Some(path)` for `http(s)://localhost[:port]/…` and
/// `http(s)://127.0.0.1[:port]/…`, `None` for anything else (a relative URL, a
/// foreign host, a non-HTTP scheme).
///
/// WHY this exists (manual-test-loop iteration 21): the runner's frontend
/// refuses absolute URLs outright — `usePageEvents.ts` logs "ignoring absolute
/// URL navigation in runner" and does nothing — but still answers
/// `success: true`. So `page/navigate` on `http://localhost:9881/terminal`
/// returned HTTP 200 `success: true` with the route unchanged, and so did
/// `http://localhost:9881/zzz-bogus`. Iteration 12 closed exactly this
/// false-PASS hole for relative URLs; the absolute branch bypassed the gate it
/// added because the handler only ran `resolve_navigate_page` under
/// `url.starts_with('/')`.
///
/// Normalizing here rather than gating separately is what keeps ONE resolver:
/// after this runs, every accepted target is a relative path, so the
/// unrouted-target gate and the payload sent to the frontend are the same code
/// for both spellings. Query and fragment are carried through verbatim so an
/// absolute URL resolves exactly as its relative form would — an absolute
/// spelling must never be more permissive than the relative one.
pub(crate) fn same_origin_absolute_path(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    // Userinfo (`http://evil@localhost/`) would let a foreign-looking authority
    // through the host check below — reject rather than guess.
    if authority.contains('@') {
        return None;
    }
    let host = authority.split(':').next().unwrap_or("");
    if host != "localhost" && host != "127.0.0.1" {
        return None;
    }
    Some(if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    })
}

/// Why a `page/navigate` target was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NavigateRejection {
    /// Neither a relative path nor a same-origin absolute URL. The runner
    /// cannot navigate here at all — a foreign host, a non-HTTP scheme.
    NotNavigable,
    /// Normalized fine, but resolves to a page key `PAGE_TO_TAB` has no route
    /// for. Carries the rejected key.
    UnroutedPage(String),
}

/// THE decision every `page/navigate` surface makes about a target: normalize
/// first, then gate. Returns the NORMALIZED, app-relative url alongside the
/// page key it resolves to.
///
/// One function, three callers — the REST control route, the SDK route's
/// runner-local fallback (`sdk_client.rs`) and the GraphQL mutation
/// (`graphql/mutation.rs`). All three previously wrote the gate as
/// `if url.starts_with('/') { resolve_navigate_page(url) }`, which is three
/// copies of the SAME false PASS: an absolute URL skipped the gate, and the
/// frontend then discarded it while every surface answered success. Callers
/// must forward the returned normalized url, not their original input —
/// `usePageEvents.ts` acts on relative paths only.
pub(crate) fn resolve_navigate_target(url: &str) -> Result<(String, String), NavigateRejection> {
    let normalized = if url.starts_with('/') {
        url.to_string()
    } else {
        same_origin_absolute_path(url).ok_or(NavigateRejection::NotNavigable)?
    };
    match resolve_navigate_page(&normalized) {
        Ok(page) => Ok((normalized, page)),
        Err(rejected) => Err(NavigateRejection::UnroutedPage(rejected)),
    }
}

/// Navigate to a URL.
///
/// Accepts an optional `mode` field. **Neither mode reloads the document** —
/// see "Why `hard` is not a reload" below:
/// - `"hard"` (default): tab navigation (`ui-bridge-navigate`) plus a
///   `history.pushState` so the address bar agrees. Preserves React state.
/// - `"soft"`: the same tab navigation plus a synthetic `popstate` and a
///   `ui-bridge:navigate` event for non-router listeners.
///
/// The response `data` block carries `{ url, mode, hard, reloaded }`.
/// `mode` echoes the negotiated mode; `hard` and `reloaded` report whether a
/// full document reload ACTUALLY happened, and are therefore always `false`.
///
/// ## Why `hard` is not a reload
///
/// This doc used to promise "full webview reload via
/// `window.location.href = url`", and the response asserted `hard: true`.
/// The handler never did that, and measurement caught it: across four `hard`
/// navigations the SDK's in-memory navigation ring kept all 20 of its old
/// entries (oldest 43 minutes stale) and the single boot-time
/// `[PROJECT_SELECTION]` console error stayed single — two witnesses a real
/// reload would have reset. Callers using "hard reload" to recover a wedged
/// webview were silently getting a soft navigation.
///
/// The fix is the contract, not the handler, because a reload here cannot
/// work and would not be wanted:
///
/// 1. **The runner has no URL router.** `useAppNavigation.ts` says so
///    outright; navigation is a tab id dispatched over `ui-bridge-navigate`,
///    and `PAGE_TO_TAB` is the only mapping. Nothing reads
///    `location.pathname` at boot, so a reload to `/settings` would come back
///    on the persisted/default tab with the requested navigation LOST.
/// 2. **The path is not an asset.** The app is served from the embedded
///    Tauri asset protocol (`frontendDist: ../dist`) with no SPA fallback, so
///    `location.href = "/settings"` asks for an asset that does not exist.
/// 3. **A reload here is already banned by design.** The sibling
///    `page_refresh` handler deliberately does NOT call `location.reload()`
///    — "a full page reload resets all React state (auth, execution,
///    terminals), causing the 'Checking authentication…' screen to flash
///    repeatedly" — and `useUIBridgeEvaluateHandler` rejects `location.reload`
///    outright so a caller cannot smuggle one in through `evaluate`.
///
/// The honest reload door already exists and is deliberately out of band from
/// this HTTP surface: the `ui_bridge_reload_webview` Tauri command (and
/// `webview_recovery`'s rung 1) eval `location.reload()` into the webview
/// from the Rust side, which is where "recover a wedged webview" belongs.
///
/// `hard` is kept on the wire rather than dropped so a client reading
/// `data.hard` gets an accurate `false` instead of `undefined`.
///
/// `url` in that block is the NORMALIZED target: a same-origin absolute URL
/// (`http://localhost:9881/terminal`) is rewritten to its path (`/terminal`)
/// before anything else happens, because that is what actually gets navigated
/// to. See [`same_origin_absolute_path`].
pub async fn ui_bridge_page_navigate_handler(
    State(state): State<Arc<ApiState>>,
    UiBridgeJson(request): UiBridgeJson<PageNavigateRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: Page navigate to {} (mode={:?})",
        request.url, request.mode
    );

    let url = request.url.trim();
    if url.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("URL cannot be empty".to_string())),
        ));
    }
    if url.starts_with("about:") || url.starts_with("javascript:") {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Unsafe URL scheme rejected: {}", url))),
        ));
    }
    // ── Normalize, then gate ─────────────────────────────────
    //
    // Reject a target the app cannot reach, INSTEAD of reporting `success:
    // true` for a navigation that never happens. The runner has no URL
    // router: `usePageEvents.ts` turns the path into a page key and dispatches
    // `ui-bridge-navigate`, whose only listener looks the key up in
    // `PAGE_TO_TAB` and silently does nothing when it is absent — after
    // `history.pushState(url)` has already moved the address bar. So the
    // snapshot's `route` (which the SDK reads from `window.location.pathname`)
    // came back echoing `/zzz`, and the caller had a `success: true` plus a
    // matching `route` for a page that was never rendered.
    //
    // That is not merely a wrong status code: it is a false-PASS SOURCE. Any
    // automated run that navigates and then asserts on `route` was reading its
    // own request back as evidence, so every such assertion passed regardless
    // of what the app did.
    //
    // Of the two available fixes — validate the target, or report the RESOLVED
    // route in the snapshot — this is the first. The second is not available
    // to the runner: `route` is produced by the UI Bridge SDK's
    // `createSnapshotAsync` from `window.location.pathname`, and the SDK is a
    // separate package. Validating is also the better answer on its own
    // merits: not performing a `pushState` to a page that does not exist
    // leaves nothing to misreport in the first place, and the caller gets the
    // rejected key back so it can self-correct.
    //
    // Iteration 21: absolute same-origin URLs used to skip this gate entirely
    // — the SAME false PASS, one gate over. `resolve_navigate_target`
    // normalizes them to their path FIRST, so both spellings share one
    // resolver and the gate is unconditional. The frontend is then handed the
    // NORMALIZED url, because it discards absolute ones outright ("ignoring
    // absolute URL navigation in runner") while still answering success.
    let (normalized_url, _page) = match resolve_navigate_target(url) {
        Ok(resolved) => resolved,
        Err(NavigateRejection::NotNavigable) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!(
                    "Only relative URLs (starting with /) or localhost URLs are allowed, got: {}",
                    url
                ))),
            ));
        }
        Err(NavigateRejection::UnroutedPage(rejected)) => {
            let preview: Vec<&str> = VALID_NAVIGATE_PAGES.iter().take(12).copied().collect();
            warn!(
                "UI Bridge API: page navigate to {} rejected — `{}` is not in PAGE_TO_TAB",
                url, rejected
            );
            let detail = UiBridgeError {
                code: UiBridgeErrorCode::InvalidRequest,
                message: format!(
                    "page/navigate: `{}` resolves to page `{}`, which the runner has no route \
                     for. Known pages include: {} (and {} more — see PAGE_TO_TAB in \
                     src/components/app/useAppNavigation.ts for the full list).",
                    url,
                    rejected,
                    preview.join(", "),
                    VALID_NAVIGATE_PAGES.len() - preview.len()
                ),
                recovery: Some(RecoveryHint::Unrecoverable),
                context: Some(serde_json::json!({
                    "code": "INVALID_REQUEST",
                    "url": url,
                    "page": rejected,
                    "knownPages": VALID_NAVIGATE_PAGES,
                })),
            };
            let message = detail.message.clone();
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error_detailed(message, detail)),
            ));
        }
    };
    if normalized_url != url {
        info!(
            "UI Bridge API: page navigate normalized same-origin absolute URL {} -> {}",
            url, normalized_url
        );
    }
    let url: &str = normalized_url.as_str();

    // Validate and normalize the mode flag. Default = "hard" for back-compat.
    let mode = match normalize_navigate_mode(request.mode.as_deref()) {
        Ok(m) => m,
        Err(bad) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!(
                    "invalid mode `{}` (expected \"hard\" or \"soft\")",
                    bad
                ))),
            ));
        }
    };

    let payload = serde_json::json!({ "url": url, "mode": mode });

    let result = ui_bridge_request_sync(&state, "page_navigate", payload)
        .await
        .map(|mut data| {
            augment_navigate_response(&mut data, url, mode);
            data
        });
    wrap_ipc_result(result)
}

/// Stamp the outcome fields onto a `page_navigate` IPC result.
///
/// `url` and `mode` are filled in only when the frontend omitted them.
/// `hard` and `reloaded` are **overwritten unconditionally** — they are the
/// two fields that previously asserted work that did not happen, and this
/// route's contract owns them. Deferring to the frontend (the old
/// `.or_insert` on `hard`) is what let `hard: true` reach callers for a
/// navigation that never reloaded anything; a frontend regression must not be
/// able to re-break the claim.
///
/// If the runner ever gains a real reload path, this is the one place that
/// has to learn to report `true` — and it should do so from an observed
/// reload, never from the requested `mode`.
pub(crate) fn augment_navigate_response(data: &mut serde_json::Value, url: &str, mode: &str) {
    let Some(obj) = data.as_object_mut() else {
        return;
    };
    obj.entry("url".to_string())
        .or_insert_with(|| serde_json::Value::String(url.to_string()));
    obj.entry("mode".to_string())
        .or_insert_with(|| serde_json::Value::String(mode.to_string()));
    obj.insert("hard".to_string(), serde_json::Value::Bool(false));
    obj.insert("reloaded".to_string(), serde_json::Value::Bool(false));
}

/// Go back in browser history.
pub async fn ui_bridge_page_go_back_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Page go back");

    wrap_ipc_result(ui_bridge_request_sync(&state, "page_go_back", serde_json::json!({})).await)
}

/// Go forward in browser history.
pub async fn ui_bridge_page_go_forward_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Page go forward");

    wrap_ipc_result(ui_bridge_request_sync(&state, "page_go_forward", serde_json::json!({})).await)
}

// ============================================================================
// Query selector
// ============================================================================

/// Query elements by CSS selector, optionally performing an action.
pub async fn ui_bridge_query_selector_handler(
    State(state): State<Arc<ApiState>>,
    UiBridgeJson(request): UiBridgeJson<QuerySelectorRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Query selector '{}'", request.selector);

    let payload = serde_json::json!({
        "selector": request.selector,
        "index": request.index,
        "params": {
            "action": request.action,
        },
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "query_selector", payload).await)
}

// ============================================================================
// Page evaluate + variants
// ============================================================================

/// Default timeout for a tagged `/page/evaluate` call if the caller doesn't
/// pin one explicitly. Matches the legacy IPC timeout envelope.
const DEFAULT_PAGE_EVALUATE_TIMEOUT_MS: u64 = 10_000;

/// Resolve a caller's optional `timeoutMs` into the budget to emit AND whether
/// that budget is the caller's own choice.
///
/// Both halves have to leave this function together. `unwrap_or` alone erases
/// the distinction, and the erasure is not recoverable downstream: the frontend
/// receives a concrete number either way, so it has no way to tell a budget the
/// caller asked for from one we supplied. That is not cosmetic — the frontend's
/// timeout message names the provenance, so an erased flag makes it tell a
/// caller who sent nothing that the budget "came from the `timeoutMs` you
/// sent", which is the misattribution the message was rewritten to remove.
/// `POST /ui-bridge/control/page/evaluate-raw` is the unarguable case: the
/// whole body is the expression, so it has no `timeoutMs` field at all and
/// always arrives here as `None`.
///
/// Consumer: `describeEvaluateBudget` in
/// `src/hooks/ui-bridge-events/utils.ts`, via the `timeout_from_default` field
/// on the `ui-bridge:evaluate-request` payload.
fn resolve_evaluate_budget(requested_ms: Option<u64>) -> (u64, bool) {
    match requested_ms {
        Some(ms) => (ms, false),
        None => (DEFAULT_PAGE_EVALUATE_TIMEOUT_MS, true),
    }
}

/// Dispatch a page/evaluate request over the tagged
/// `ui-bridge:evaluate-request` / `ui-bridge:evaluate-response` event pair,
/// correlating the response through [`EvaluateRequestStore`].
///
/// `window_label` is the resolved target window ([`MAIN_WINDOW_LABEL`] for the
/// single-window default). The request is registered under
/// `(window_label, request_id)` and the emit is scoped to that window only via
/// `emit_to(EventTarget::labeled(window_label), …)` (so a `page/evaluate` no
/// longer fires the expression in every open window). Delivery is authoritative
/// on the Rust side: unlike the bare `Emitter::emit`, which broadcasts to ALL
/// targets, `emit_to` is filtered down to the listener registered under that
/// label. The emitted payload still carries `windowLabel` so the frontend
/// evaluate listener can additionally ignore any event not addressed to its own
/// `getCurrentWindow().label` (defense-in-depth, no longer load-bearing).
async fn tagged_page_evaluate(
    state: &Arc<ApiState>,
    expression: &str,
    timeout_ms: Option<u64>,
    allow_network_requests: bool,
    window_label: &str,
) -> Result<serde_json::Value, String> {
    let (timeout_ms, timeout_from_default) = resolve_evaluate_budget(timeout_ms);
    let request_id = uuid::Uuid::new_v4().to_string();

    // Fail fast for a non-existent target window — mirrors the guard in
    // `request::ui_bridge_request_inner`. Without it, the emit lands on a window
    // every listener filters out by label, so the caller waits the full timeout
    // for a request nothing can answer. The main window always exists, so the
    // default path skips the lookup.
    if window_label != MAIN_WINDOW_LABEL {
        use tauri::Manager;
        if state.app_handle.get_webview_window(window_label).is_none() {
            return Err(format!(
                "No runner window labeled '{window_label}'. Discover live windows via \
                 GET /ui-bridge/control/runner-windows."
            ));
        }
    }

    let (sender, receiver) = tokio::sync::oneshot::channel();
    state
        .ui_bridge_evaluate_store
        .register(window_label, &request_id, sender)
        .await;

    let mut payload = serde_json::json!({
        "request_id": request_id,
        "expression": expression,
        // `timeout_ms` is the caller's clamped budget and the frontend awaits a
        // top-level Promise for exactly this long, so neither end gives up
        // before the other. (It used to ignore the field and cap every await at
        // a fixed 30 s, which made the documented 600000 ceiling unreachable.)
        //
        // `await_promise` is deliberately NOT forwarded: this flow always
        // auto-awaits, so the frontend never read it. Not awaiting would return
        // a bare Promise that structured-clones to `{}` — a success envelope
        // with no data. `awaitPromise` still applies to the direct-eval
        // fallback in `helpers.rs`, which can legitimately report
        // `"[object Promise]"`.
        "timeout_ms": timeout_ms,
        // Provenance for `timeout_ms`, so the frontend's timeout message can
        // say whether the caller chose that budget. `timeout_ms` alone cannot
        // answer that — it is always a concrete number by the time it is
        // emitted, whether the caller sent it or the `unwrap_or` above
        // supplied it. snake_case to match the rest of this payload
        // (request_id / timeout_ms / allow_network_requests). Additive: a
        // frontend that predates the field falls back to reading the number
        // as caller-supplied, which is the pre-existing behaviour.
        "timeout_from_default": timeout_from_default,
        // Forward the explicit network-request opt-in to the frontend
        // blocklist gate. snake_case to match the rest of this IPC payload's
        // convention (await_promise / timeout_ms / request_id).
        "allow_network_requests": allow_network_requests,
    });
    // The frontend evaluate listener filters on `windowLabel` against its own
    // `getCurrentWindow().label`; it also echoes it back on the
    // `ui-bridge:evaluate-response` so the store delivers under the same
    // `(window_label, request_id)` key it was registered with. Use the shared
    // `TARGET_WINDOW_FIELD` constant (camelCase `windowLabel`) so the routing
    // field name can't drift from the other control routes.
    if let Some(obj) = payload.as_object_mut() {
        obj.insert(
            TARGET_WINDOW_FIELD.to_string(),
            serde_json::Value::String(window_label.to_string()),
        );
    }

    // Route the emit to the target window ONLY (Phase 1 multi-window dispatch),
    // honoring the same `QONTINUI_UI_BRIDGE_MULTI_WINDOW` flag as the main IPC
    // path.
    //
    // ROOT-CAUSE NOTE: `WebviewWindow::emit` / `AppHandle::emit` (the `Emitter`
    // trait's default `emit`) deliver to ALL targets — they are NOT scoped to
    // the receiver. The previous `get_webview_window(label).emit(...)` therefore
    // broadcast the expression to every open window, and single-window behavior
    // depended entirely on each window's frontend own-label filter. That filter
    // is kept as defense-in-depth, but it is no longer load-bearing: scope the
    // delivery on the Rust side with `emit_to(EventTarget::labeled(label), …)`,
    // which the runtime filters down to the listener registered under that label
    // (a webview `listen()` registers as `WebviewWindow { label }`, matched by
    // `AnyLabel`). Now an eval that clicks a button fires in ONE window only,
    // even if a pop-out's frontend filter were ever wrong.
    //
    // When the flag is off we fall back to the legacy process-global broadcast
    // (the frontend own-label filter is then the only scoping), preserving the
    // documented escape hatch.
    let emit_result = if multi_window_dispatch_enabled() {
        state.app_handle.emit_to(
            EventTarget::labeled(window_label),
            "ui-bridge:evaluate-request",
            &payload,
        )
    } else {
        state
            .app_handle
            .emit("ui-bridge:evaluate-request", &payload)
    };

    if let Err(e) = emit_result {
        state
            .ui_bridge_evaluate_store
            .cancel(window_label, &request_id)
            .await;
        return Err(format!("Failed to emit ui-bridge:evaluate-request: {}", e));
    }

    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), receiver).await {
        Ok(Ok(resp)) => {
            if resp.ok {
                // The frontend emits ONE shape — the discriminated
                // `{value, type}` envelope — so it rides through verbatim.
                // There is no longer a conditional re-wrap here: the old
                // "objects bare, everything else boxed" conversion is what
                // made `data.result.value` unparseable without a try/except,
                // and it reported a genuine `undefined` as an empty object.
                Ok(resp.result.unwrap_or(serde_json::Value::Null))
            } else {
                let err = resp.error.unwrap_or_else(|| {
                    "page/evaluate: frontend reported failure without an error message".to_string()
                });
                Ok(serde_json::json!({
                    "success": false,
                    "error": err,
                }))
            }
        }
        Ok(Err(_)) => {
            state
                .ui_bridge_evaluate_store
                .cancel(window_label, &request_id)
                .await;
            Err(format!(
                "page/evaluate: response channel closed before delivery (request_id={})",
                request_id
            ))
        }
        Err(_) => {
            state
                .ui_bridge_evaluate_store
                .cancel(window_label, &request_id)
                .await;
            // The LEADING CLAUSE stays verbatim: it is the documented
            // discriminator between this generic Rust timeout and the
            // frontend's precise "Promise did not resolve within Xs" (see
            // PAGE_EVALUATE_TIMEOUT_MARGIN_MS in
            // `src/hooks/ui-bridge-events/utils.ts`). Only a provenance clause
            // is appended.
            //
            // The frontend normally wins the race by that 250 ms margin, so
            // this branch is what a caller sees when NO frontend listener
            // answers at all — every window rejecting the request on the
            // own-label filter, a dedupe drop, or a window not yet mounted.
            // Without the clause, such a caller gets a bare `10000ms` and no
            // hint that the budget was ours or that a knob exists — the same
            // defect the frontend message was rewritten to close, on the
            // sibling path.
            Err(format!(
                "UI Bridge page_evaluate timed out after {timeout_ms}ms. {}",
                if timeout_from_default {
                    "That is the DEFAULT budget, not a cap - pass `timeoutMs` on \
                     POST /ui-bridge/control/page/evaluate to raise it (clamped to \
                     1000-600000ms)."
                } else {
                    "That budget came from the `timeoutMs` you sent."
                }
            ))
        }
    }
}

async fn page_evaluate_inner(
    state: Arc<ApiState>,
    expression: String,
    timeout_ms: Option<u64>,
    await_promise: bool,
    allow_network_requests: bool,
    window_label: &str,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let preview: String = expression.chars().take(80).collect();
    info!(
        "UI Bridge API: Page evaluate (window={}, {}...)",
        window_label, preview
    );

    // NOTE: `await_promise` is not passed to the tagged path — that flow always
    // auto-awaits (see the emit payload comment in `tagged_page_evaluate`). It
    // is still honored by the `direct_webview_evaluate_with_result` fallback
    // below, which is the only place the flag changes anything.
    let ipc_result = tagged_page_evaluate(
        &state,
        &expression,
        timeout_ms,
        allow_network_requests,
        window_label,
    )
    .await;
    match ipc_result {
        Ok(data) => {
            if let Some(false) = data.get("success").and_then(|v| v.as_bool()) {
                let error_msg = data
                    .get("error")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "JS evaluation failed".to_string());
                // Static-guard rejection enrichment: the frontend evaluator
                // (useUIBridgeEvaluateHandler / usePageEvents) emits a
                // distinctive "Expression rejected: contains prohibited
                // pattern (<regex.source>)" message when the static
                // blocklist matches. Surface a workaround `hint` so callers
                // can recover without scraping the regex source.
                let hint = static_guard_hint(&error_msg);
                let mut body = api_error(format!("JS evaluation error: {}", error_msg));
                if let Some(h) = hint {
                    body.hint = Some(serde_json::Value::String(h));
                }
                return Err((StatusCode::BAD_REQUEST, Json(body)));
            }
            Ok(Json(ApiResponse::success(data)))
        }
        Err(ipc_err) => {
            // The `direct_webview_evaluate_with_result` fallback below targets
            // the MAIN window only (see helpers.rs). Falling back to it for a
            // request addressed to a *non-main* window is wrong on two counts:
            //
            //  1. An unknown-window fail-fast (e.g. `?windowLabel=ghost`, which
            //     `tagged_page_evaluate` rejects before emitting) would be
            //     silently "recovered" by running the expression in main —
            //     turning a should-have-failed call into a false success. This
            //     is exactly the `ghost → {success:true}` bug.
            //  2. Even a genuine IPC/transport failure for a *real* pop-out
            //     window must NOT be retried against main — that would run the
            //     caller's expression in the wrong window.
            //
            // So the main-only fallback is permitted ONLY when the caller
            // actually addressed the main window. Any error while addressing a
            // non-main window propagates verbatim.
            if window_label != MAIN_WINDOW_LABEL {
                error!(
                    "UI Bridge API: evaluate failed for window '{}' (no main-window fallback for a non-main target): {}",
                    window_label, ipc_err
                );
                return Err((StatusCode::BAD_REQUEST, Json(api_error(ipc_err))));
            }

            debug!(
                "UI Bridge: IPC evaluate failed for main window ({}), trying direct WebView eval",
                ipc_err
            );

            match direct_webview_evaluate_with_result(
                &state,
                &expression,
                timeout_ms,
                await_promise,
            )
            .await
            {
                Ok(result) => {
                    // Direct-eval fallback: build the {value, type} shape
                    // here, since this path never reached the frontend
                    // handler that would otherwise produce it. `result` is a
                    // JSON-encoded string (direct_webview_evaluate_with_result
                    // sends the serialised value back over HTTP) — parse it
                    // before classifying so null/objects/arrays get the
                    // correct discriminator. The fallback must not report a
                    // DIFFERENT envelope than the tagged path: a caller can't
                    // tell which one served it.
                    let parsed: serde_json::Value = serde_json::from_str(&result)
                        .unwrap_or_else(|_| serde_json::Value::String(result.clone()));
                    let (value, type_name) = classify_direct_eval_value(parsed);
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "value": value,
                        "type": type_name,
                        "source": "direct_eval",
                    }))))
                }
                Err(direct_err) => {
                    error!(
                        "UI Bridge API: Both IPC and direct eval failed. IPC: {}, Direct: {}",
                        ipc_err, direct_err
                    );
                    Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!(
                            "IPC: {}. Direct eval: {}",
                            ipc_err, direct_err
                        ))),
                    ))
                }
            }
        }
    }
}

/// Map a direct-eval JSON value to the unwrap discriminator the frontend
/// would have produced via `typeof`. Functions are unreachable here because
/// the direct-eval path serialises through JSON — they'd have surfaced as
/// `Null`.
fn classify_direct_eval_value(result: serde_json::Value) -> (serde_json::Value, &'static str) {
    match &result {
        serde_json::Value::Null => (serde_json::Value::Null, "null"),
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => (result, "object"),
        _ => (result, "scalar"),
    }
}

/// Map a frontend "Expression rejected: contains prohibited pattern (...)"
/// error to a human-readable workaround `hint` string. The hint is emitted
/// as a sibling of the error message in the HTTP response so callers can
/// recover from `fetch(`-style guards without parsing the regex source.
///
/// Returns `None` when the message isn't a static-guard rejection — leaves
/// the response un-hinted in that case.
fn static_guard_hint(error_msg: &str) -> Option<String> {
    if !error_msg.contains("Expression rejected: contains prohibited pattern") {
        return None;
    }
    // The fetch() rejection is the most-frequently-tripped guard for AI
    // agents (they reach for it as the simplest "make a request"
    // primitive). Give it a targeted hint that points at the
    // `window['fet'+'ch']` workaround AND the preferred alternatives so
    // agents don't keep rediscovering them by trial and error.
    if error_msg.contains("\\bfetch") {
        return Some(
            "Use window['fet'+'ch'] to bypass the static guard if you need raw fetch \
             — but prefer /control/network/stubs for stubbing or evaluate-with-await \
             for one-shot calls."
                .to_string(),
        );
    }
    // Generic catch-all for the other prohibited patterns
    // (XMLHttpRequest, sendBeacon, WebSocket, eval, new Function,
    // import(), require(), __proto__, document.cookie, window.open,
    // location.assign / replace, crypto.subtle, ...).
    Some(
        "Static-guard match. If this is necessary, see /control/network/stubs \
         or open an issue to widen the allowlist."
            .to_string(),
    )
}

#[cfg(test)]
mod static_guard_hint_tests {
    use super::static_guard_hint;

    #[test]
    fn fetch_rejection_emits_targeted_hint() {
        let msg = "Expression rejected: contains prohibited pattern (\\bfetch\\s*\\()";
        let hint = static_guard_hint(msg).expect("fetch rejection must produce a hint");
        assert!(
            hint.contains("window['fet'+'ch']"),
            "fetch hint must point at the bracket-access workaround, got: {hint}"
        );
        assert!(
            hint.contains("/control/network/stubs"),
            "fetch hint must mention stubs as the preferred alternative, got: {hint}"
        );
    }

    #[test]
    fn other_rejections_emit_generic_hint() {
        let msg = "Expression rejected: contains prohibited pattern (\\beval\\s*\\()";
        let hint = static_guard_hint(msg).expect("eval rejection must produce a hint");
        assert!(
            hint.contains("Static-guard match"),
            "non-fetch rejection must use the generic hint, got: {hint}"
        );
        // The generic hint must NOT leak the fetch-specific guidance.
        assert!(
            !hint.contains("window['fet'+'ch']"),
            "generic hint must not include the fetch-specific workaround, got: {hint}"
        );
    }

    #[test]
    fn non_guard_errors_get_no_hint() {
        // Plain JS exceptions (e.g. "ReferenceError: foo is not defined")
        // are not static-guard rejections — they should leave the response
        // un-hinted rather than emitting the generic catch-all.
        assert!(static_guard_hint("ReferenceError: foo is not defined").is_none());
        assert!(static_guard_hint("TypeError: undefined").is_none());
        assert!(static_guard_hint("").is_none());
    }
}

/// Resolve the effective target window for an evaluate request.
///
/// Precedence: `?windowLabel=` query param → body `windowLabel` → the main
/// window. Empty strings are treated as "not provided" (so `?windowLabel=`
/// falls through to the body, and an empty body field falls through to main),
/// matching how [`super::request::split_target_window`] normalizes the field.
fn resolve_evaluate_window(query: Option<&str>, body: Option<&str>) -> String {
    query
        .filter(|s| !s.is_empty())
        .or_else(|| body.filter(|s| !s.is_empty()))
        .unwrap_or(MAIN_WINDOW_LABEL)
        .to_string()
}

#[cfg(test)]
mod resolve_evaluate_window_tests {
    use super::{resolve_evaluate_window, MAIN_WINDOW_LABEL};

    /// Goal behavior 1: with NO `windowLabel` anywhere, an evaluate targets the
    /// MAIN window — so the dispatcher's `emit_to(EventTarget::labeled("main"))`
    /// runs the expression in the main window only.
    #[test]
    fn no_label_anywhere_resolves_to_main() {
        assert_eq!(resolve_evaluate_window(None, None), MAIN_WINDOW_LABEL);
    }

    /// Empty strings are "not provided" — `?windowLabel=` (and an empty body
    /// field) fall through to main, matching `split_target_window`.
    #[test]
    fn empty_strings_fall_through_to_main() {
        assert_eq!(resolve_evaluate_window(Some(""), None), MAIN_WINDOW_LABEL);
        assert_eq!(
            resolve_evaluate_window(Some(""), Some("")),
            MAIN_WINDOW_LABEL
        );
        assert_eq!(resolve_evaluate_window(None, Some("")), MAIN_WINDOW_LABEL);
    }

    /// Goal behavior 2: `?windowLabel=term-1` targets `term-1` only. The query
    /// param wins over the body.
    #[test]
    fn query_label_wins_and_is_used_verbatim() {
        assert_eq!(resolve_evaluate_window(Some("term-1"), None), "term-1");
        assert_eq!(
            resolve_evaluate_window(Some("term-1"), Some("term-2")),
            "term-1",
            "query param must win over the body field"
        );
    }

    /// Body `windowLabel` is the fallback when the query param is absent/empty.
    #[test]
    fn body_label_is_the_fallback() {
        assert_eq!(resolve_evaluate_window(None, Some("term-3")), "term-3");
        assert_eq!(resolve_evaluate_window(Some(""), Some("term-3")), "term-3");
    }
}

/// Evaluate a JavaScript expression in the webview.
pub async fn ui_bridge_page_evaluate_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<EvaluateQueryParams>,
    UiBridgeJson(request): UiBridgeJson<PageEvaluateRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let timeout = request.timeout_ms.map(|ms| ms.clamp(1000, 600_000));
    let allow_network_requests = request.allow_network_requests.unwrap_or(false);
    let window_label = resolve_evaluate_window(
        query.window_label.as_deref(),
        request.window_label.as_deref(),
    );
    page_evaluate_inner(
        state,
        request.expression,
        timeout,
        request.await_promise,
        allow_network_requests,
        &window_label,
    )
    .await
}

/// `POST /ui-bridge/control/page/evaluate-raw`
///
/// The whole body is the JS expression, so the only way to address a pop-out
/// window here is the `?windowLabel=` query param (the body can't carry a
/// routing field). Absent / empty → the main window.
pub async fn ui_bridge_page_evaluate_raw_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<EvaluateQueryParams>,
    body: String,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    if body.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("page/evaluate-raw: body is empty".to_string())),
        ));
    }
    let window_label = resolve_evaluate_window(query.window_label.as_deref(), None);
    page_evaluate_inner(state, body, None, false, false, &window_label).await
}

/// POST /ui-bridge/control/page/evaluate-safe
pub async fn ui_bridge_page_evaluate_safe_handler(
    State(state): State<Arc<ApiState>>,
    UiBridgeJson(request): UiBridgeJson<PageEvaluateRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let preview: String = request.expression.chars().take(80).collect();
    info!("UI Bridge API: Safe evaluate ({}...)", preview);

    // `safe_evaluate` embeds this string as a function BODY, so the expression
    // has to be spliced into a `return` position. Build that through
    // `return_expression_js` rather than `format!("return {}", …)`: the naive
    // concatenation lets automatic semicolon insertion swallow any expression
    // starting with a newline, answering `success: true` with an empty result.
    match safe_evaluate(&state, &return_expression_js(&request.expression)).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("JS evaluation error: {}", e))),
        )),
    }
}

/// Build the single combined JS expression that `evaluate-batch` sends over
/// the legacy `page_evaluate` IPC: each sub-expression runs inside its own
/// try/catch IIFE, and the whole set is returned as a JSON string.
///
/// Two splice hazards are handled here, both of which used to corrupt results
/// silently:
///
/// - Each sub-expression goes through [`return_expression_js`], so an
///   expression starting with a newline is no longer truncated by automatic
///   semicolon insertion (which reported `success: true, value: null` for it)
///   and one ending in a line comment can no longer swallow the rest of the
///   generated line — which, in a batch, meant taking EVERY LATER EXPRESSION
///   in the array down with it.
/// - Each `id` is emitted via `serde_json::to_string` as a proper JS string
///   literal instead of a hand-rolled `"` escape, so an id containing a
///   backslash, newline, or quote can't break out of the literal. JSON
///   strings are a strict subset of JS strings, so the output is always a
///   valid literal.
fn build_batch_expression_js(expressions: &[BatchExpression]) -> String {
    let js_parts: Vec<String> = expressions
        .iter()
        .map(|expr| {
            // `to_string` on a String only fails for non-UTF-8 / cyclic input,
            // neither of which a deserialized `String` can be — but avoid the
            // unwrap anyway and fall back to an empty-string literal.
            let id_literal =
                serde_json::to_string(&expr.id).unwrap_or_else(|_| "\"\"".to_string());
            format!(
                r#"(() => {{ try {{ var v = (function() {{ {} }})(); return {{ id: {}, success: true, value: v === undefined ? null : v }}; }} catch(e) {{ return {{ id: {}, success: false, error: e.message }}; }} }})()"#,
                return_expression_js(&expr.expression),
                id_literal,
                id_literal,
            )
        })
        .collect();

    let combined = format!("return JSON.stringify([{}])", js_parts.join(","));
    format!("(() => {{ {} }})()", combined)
}

/// POST /ui-bridge/control/page/evaluate-batch
pub async fn ui_bridge_page_evaluate_batch_handler(
    State(state): State<Arc<ApiState>>,
    UiBridgeJson(request): UiBridgeJson<BatchEvaluateRequest>,
) -> Result<Json<ApiResponse<Vec<BatchExpressionResult>>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: Batch evaluate ({} expressions)",
        request.expressions.len()
    );

    if request.expressions.is_empty() {
        return Ok(Json(ApiResponse::success(vec![])));
    }

    if request.expressions.len() > 50 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("Maximum 50 expressions per batch".to_string())),
        ));
    }

    let payload =
        serde_json::json!({ "expression": build_batch_expression_js(&request.expressions) });

    match ui_bridge_request_sync(&state, "page_evaluate", payload).await {
        Ok(data) => {
            // The batch expression returns a JSON STRING, so the envelope's
            // value is a string to be parsed. Reading it through the shared
            // `evaluate_ipc_value` keeps this in step with the single
            // `{value, type}` shape the frontend emits.
            //
            // A missing/unparseable value is reported per-expression rather
            // than as `[]`: an empty array under `success: true` is
            // indistinguishable from "every expression legitimately returned
            // nothing", which is the false-green this route must not produce.
            let results: Vec<BatchExpressionResult> = super::helpers::evaluate_ipc_value(&data)
                .and_then(|v| v.as_str())
                .and_then(|s| serde_json::from_str::<Vec<BatchExpressionResult>>(s).ok())
                .unwrap_or_else(|| {
                    let reason = format!(
                        "Failed to parse batch result from evaluate envelope (keys: {})",
                        data.as_object()
                            .map(|o| o.keys().cloned().collect::<Vec<_>>().join(","))
                            .unwrap_or_else(|| "<not an object>".to_string())
                    );
                    request
                        .expressions
                        .iter()
                        .map(|e| BatchExpressionResult {
                            id: e.id.clone(),
                            success: false,
                            value: serde_json::Value::Null,
                            error: Some(reason.clone()),
                        })
                        .collect()
                });

            Ok(Json(ApiResponse::success(results)))
        }
        Err(e) => {
            error!("UI Bridge API: Batch evaluate failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Tab switching (set-tab, activate-tab)
// ============================================================================

/// POST /ui-bridge/control/page/set-tab
pub async fn ui_bridge_page_set_tab_handler(
    State(state): State<Arc<ApiState>>,
    UiBridgeJson(request): UiBridgeJson<SetTabRequest>,
) -> Result<Json<ApiResponse<SetTabResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let tab = request.tab.trim().to_string();
    if tab.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "page/set-tab: `tab` is required (non-empty string)".to_string(),
            )),
        ));
    }

    if !VALID_TAB_IDS.contains(&tab.as_str()) {
        let preview: Vec<&str> = VALID_TAB_IDS.iter().take(12).copied().collect();
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "page/set-tab: unknown tab `{}`. Valid tabs include: {} (and {} more — see \
                 src/components/app/tab-types.ts for the full list).",
                tab,
                preview.join(", "),
                VALID_TAB_IDS.len() - preview.len()
            ))),
        ));
    }

    info!("UI Bridge API: page/set-tab → {}", tab);

    let escaped_tab = serde_json::to_string(&tab).unwrap_or_else(|_| "\"\"".to_string());
    let expression = format!(
        r#"(async () => {{
            window.dispatchEvent(new CustomEvent("ui-bridge-set-tab", {{ detail: {{ tab: {} }} }}));
            await new Promise(r => setTimeout(r, 100));
            var el = document.querySelector("[data-page-id]");
            var pageId = el && el.getAttribute ? el.getAttribute("data-page-id") : null;
            return JSON.stringify({{ pageId: pageId }});
        }})()"#,
        escaped_tab
    );

    match direct_webview_evaluate_with_result(&state, &expression, Some(5_000), false).await {
        Ok(result_str) => {
            let page_id = serde_json::from_str::<serde_json::Value>(&result_str)
                .ok()
                .and_then(|v| {
                    v.get("pageId")
                        .and_then(|p| p.as_str())
                        .map(|s| s.to_string())
                });
            Ok(Json(ApiResponse::success(SetTabResponse {
                success: true,
                tab,
                page_id,
            })))
        }
        Err(e) => {
            error!("UI Bridge API: page/set-tab direct eval failed: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "page/set-tab: failed to dispatch event: {}",
                    e
                ))),
            ))
        }
    }
}

/// POST /ui-bridge/control/activate-tab/{tab_id}
pub async fn ui_bridge_activate_tab_handler(
    State(state): State<Arc<ApiState>>,
    Path(tab_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let trimmed = tab_id.trim().to_string();
    if trimmed.is_empty() {
        // Iter-3 item 1 — empty tab id is structurally invalid; populate
        // `error_detail.code = "INVALID_TAB_ID"` for the same reason the
        // wrong-id branch below does, so both rejection paths share the
        // machine-readable envelope.
        let detail = UiBridgeError::invalid_tab_id("", VALID_TAB_IDS);
        let message = detail.message.clone();
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error_detailed(message, detail)),
        ));
    }

    if !VALID_TAB_IDS.contains(&trimmed.as_str()) {
        // Iter-3 item 1 — populate `error_detail.code = "INVALID_TAB_ID"`
        // so callers can match on the machine-readable code instead of
        // parsing the prose `error` field. `error_detail.context.knownTabs`
        // carries the full registry; the prose `error` keeps the
        // human-readable preview for terminal-friendly debugging.
        let detail = UiBridgeError::invalid_tab_id(&trimmed, VALID_TAB_IDS);
        let preview: Vec<&str> = VALID_TAB_IDS.iter().take(12).copied().collect();
        let message = format!(
            "activate-tab: unknown tab_id `{}`. Valid tab_ids include: {} (and {} more — \
             see src/components/app/tab-types.ts for the full list).",
            trimmed,
            preview.join(", "),
            VALID_TAB_IDS.len() - preview.len()
        );
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error_detailed(message, detail)),
        ));
    }

    info!("UI Bridge API: activate-tab -> {}", trimmed);

    state
        .app_handle
        .emit(
            "ui-bridge:activate-tab",
            serde_json::json!({ "tab_id": trimmed }),
        )
        .map_err(|e| {
            error!("UI Bridge API: activate-tab emit failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "activate-tab: failed to emit ui-bridge:activate-tab: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "success": true,
        "tab_id": trimmed,
    }))))
}

// ============================================================================
// F4 — First-class tab activation
// ============================================================================

/// Validate a user-supplied tab id. Returns `Ok(trimmed)` when the id is
/// known, or a sorted preview of `knownTabs` for the 400 body when it isn't.
///
/// Split out from the handler so it's unit-testable without spinning up a
/// full `axum::Router` — see `tab_activate_tests` below.
pub(crate) fn validate_tab_id(raw: &str) -> Result<String, Vec<&'static str>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || !VALID_TAB_IDS.contains(&trimmed) {
        return Err(VALID_TAB_IDS.to_vec());
    }
    Ok(trimmed.to_string())
}

/// `GET /ui-bridge/control/tabs`
///
/// Returns the full tab list with labels plus the currently active tab. Uses
/// the same IPC bridge as other `/control/*` endpoints — the authoritative
/// tab list lives in the frontend (`src/components/app/tab-types.ts` →
/// `TAB_LIST`) so there's no duplication between Rust and TS.
pub async fn ui_bridge_tabs_list_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: tabs_list");

    match ui_bridge_request_sync(&state, "tabs_list", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: tabs_list failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// `POST /ui-bridge/control/tab/activate`
///
/// Body: `{ "tabId": "<id>" }`. Fires the same code path a user click would
/// (the React handler dispatches the `ui-bridge-set-tab` window event, which
/// `useAppNavigation` listens for and fans out to `setActiveTab`). Returns
/// `{ activeTab, previousTab }` on success.
///
/// Rust-side validation returns HTTP 400 with
/// `{ error: "unknown_tab", knownTabs: [...] }` when the id isn't in the
/// static `VALID_TAB_IDS` registry so the caller gets the error without an
/// IPC round-trip; the React handler repeats the check as a defence-in-depth
/// guard in case the two lists ever diverge.
pub async fn ui_bridge_tab_activate_handler(
    State(state): State<Arc<ApiState>>,
    UiBridgeJson(request): UiBridgeJson<TabActivateRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<serde_json::Value>>)>
{
    let tab_id = match validate_tab_id(&request.tab_id) {
        Ok(id) => id,
        Err(known) => {
            // Iter-3 item 1 — populate `error_detail.code = "INVALID_TAB_ID"`
            // so callers can match on the machine-readable code instead of
            // parsing the prose `error` field. Keep the `data.knownTabs`
            // payload for backward compat with callers that read it from
            // there (the cheatsheet contract), and ALSO surface knownTabs
            // inside `error_detail.context` for callers using the structured
            // envelope. Two-tier presentation: legacy `data` shape preserved,
            // new `error_detail` carries the canonical machine-readable
            // payload.
            let data_payload = serde_json::json!({
                "knownTabs": known,
                "tabId": request.tab_id,
            });
            let detail = UiBridgeError::invalid_tab_id(&request.tab_id, &known);
            let message = detail.message.clone();
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ApiResponse {
                    success: false,
                    data: Some(data_payload),
                    error: Some(message),
                    error_detail: Some(detail),
                    hint: None,
                    code: None,
                    suggestions: None,
                }),
            ));
        }
    };

    info!("UI Bridge API: tab_activate -> {}", tab_id);

    let payload = serde_json::json!({ "tabId": tab_id });
    match ui_bridge_request_sync(&state, "tab_activate", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: tab_activate IPC failed: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse {
                    success: false,
                    data: None,
                    error: Some(e),
                    error_detail: None,
                    hint: None,
                    code: None,
                    suggestions: None,
                }),
            ))
        }
    }
}

// ============================================================================
// Page summary
// ============================================================================

/// POST /ui-bridge/control/page/summary
pub async fn ui_bridge_page_summary_handler(
    State(state): State<Arc<ApiState>>,
    _body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let js = r#"(() => {
        var vis = function(sel) { return Array.from(document.querySelectorAll(sel)).filter(function(el) { return el.offsetParent !== null; }); };
        return JSON.stringify({
            title: document.title,
            url: document.URL,
            // Whole-page visible text (capped at 8000 chars). Lets a caller
            // answer "did my plain-JSX panel render?" in one call without a
            // hand-rolled page/evaluate `document.body.innerText` — the
            // get_snapshot affordance surfaces only *registered* UI Bridge
            // components, so unregistered JSX is invisible there.
            bodyText: (document.body ? document.body.innerText : '').trim().slice(0, 8000),
            headings: vis('h1, h2, h3').map(function(el) { return { tag: el.tagName, text: el.textContent.trim().slice(0, 100) }; }).slice(0, 10),
            buttons: vis('button').filter(function(el) { return el.textContent.trim().length > 0; }).map(function(el) { return el.textContent.trim().slice(0, 40); }).filter(function(t) { return t.length < 30; }).slice(0, 20),
            inputs: vis('input, textarea, select').map(function(el) { return { tag: el.tagName, type: el.type || null, placeholder: (el.placeholder || '').slice(0, 40) || null, hasValue: (el.value || '').length > 0 }; }).slice(0, 15),
            links: vis('a[href]').filter(function(el) { return el.textContent.trim().length > 0; }).map(function(el) { return { text: el.textContent.trim().slice(0, 40), href: (el.getAttribute('href') || '').slice(0, 60) }; }).slice(0, 15),
            modals: vis('[role="dialog"]').map(function(el) { return el.textContent.trim().slice(0, 100); }).slice(0, 3),
            errors: vis('[role="alert"]').filter(function(el) { return el.textContent.trim().length > 0; }).map(function(el) { return el.textContent.trim().slice(0, 100); }).slice(0, 5),
            elementCounts: {
                buttons: document.querySelectorAll('button').length,
                inputs: document.querySelectorAll('input').length,
                textareas: document.querySelectorAll('textarea').length,
                selects: document.querySelectorAll('select').length,
                links: document.querySelectorAll('a').length,
                images: document.querySelectorAll('img').length
            }
        });
    })()"#;

    match evaluate_js_expression(&state, js).await {
        Ok(result) => Ok(Json(ApiResponse::success(parse_eval_result(
            "page/summary",
            "page summary",
            &result,
        )?))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

// ============================================================================
// Page playbook
// ============================================================================

/// `GET /ui-bridge/control/page/playbook`
///
/// Returns a single combined snapshot an external test harness or agent can
/// use to understand the current page's capabilities without trial-and-error
/// discovery. Dynamic data (current tab, registered components, registered
/// intents) is sourced from the frontend via `get_playbook` IPC; the static
/// `primaryActions` list of well-known capability endpoints is appended Rust-
/// side so it stays stable regardless of what the frontend has registered.
pub async fn ui_bridge_page_playbook_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: page_playbook");

    let mut data = match ui_bridge_request_sync(&state, "get_playbook", serde_json::json!({})).await
    {
        Ok(d) => d,
        Err(e) => {
            error!("UI Bridge API: page_playbook IPC failed: {}", e);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))));
        }
    };

    // Static "well-known capabilities" list. Order is stable; future entries
    // append. These are not derived from frontend registrations — they are
    // first-class HTTP endpoints the runner always exposes.
    let primary_actions = serde_json::json!([
        {
            "description": "Switch tabs",
            "method": "POST",
            "path": "/ui-bridge/control/tab/activate",
            "bodyExample": { "tabId": "<id>" }
        },
        {
            "description": "Find element by natural-language query",
            "method": "POST",
            "path": "/ui-bridge/ai/find",
            "bodyExample": { "query": "..." }
        },
        {
            "description": "Click an element",
            "method": "POST",
            "path": "/ui-bridge/control/element/{elementId}/action",
            "bodyExample": { "action": "click" }
        }
    ]);

    if let Some(obj) = data.as_object_mut() {
        obj.insert("primaryActions".to_string(), primary_actions);
    } else {
        // Defensive: if the frontend returned a non-object (shouldn't happen
        // per the IPC contract), wrap it so callers still see primaryActions.
        data = serde_json::json!({
            "raw": data,
            "primaryActions": primary_actions,
        });
    }

    Ok(Json(ApiResponse::success(data)))
}

// ============================================================================
// Route registration
// ============================================================================

/// Page navigation, evaluation, and tab-switching routes (includes /ai/*
/// alias for page-summary).
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/ui-bridge/control/tabs", get(ui_bridge_tabs_list_handler))
        .route(
            "/ui-bridge/control/tab/activate",
            post(ui_bridge_tab_activate_handler),
        )
        .route(
            "/ui-bridge/control/page/refresh",
            post(ui_bridge_page_refresh_handler),
        )
        .route(
            "/ui-bridge/control/page/hard-refresh",
            post(ui_bridge_page_hard_refresh_handler),
        )
        .route(
            "/ui-bridge/control/page/close-request",
            post(ui_bridge_page_close_request_handler),
        )
        .route(
            "/ui-bridge/control/page/force-close",
            post(ui_bridge_page_force_close_handler),
        )
        .route(
            "/ui-bridge/control/page/navigate",
            post(ui_bridge_page_navigate_handler),
        )
        .route(
            "/ui-bridge/control/page/navigate-to",
            post(ui_bridge_page_navigate_to_handler),
        )
        .route(
            "/ui-bridge/control/page/routes",
            get(ui_bridge_page_get_routes_handler),
        )
        .route(
            "/ui-bridge/control/viewport-constraints",
            post(ui_bridge_set_viewport_constraints_handler),
        )
        .route(
            "/ui-bridge/control/page/back",
            post(ui_bridge_page_go_back_handler),
        )
        .route(
            "/ui-bridge/control/page/forward",
            post(ui_bridge_page_go_forward_handler),
        )
        .route(
            "/ui-bridge/control/query-selector",
            post(ui_bridge_query_selector_handler),
        )
        .route(
            "/ui-bridge/control/page/evaluate",
            post(ui_bridge_page_evaluate_handler),
        )
        .route(
            "/ui-bridge/control/page/evaluate-raw",
            post(ui_bridge_page_evaluate_raw_handler),
        )
        .route(
            "/ui-bridge/control/page/evaluate-safe",
            post(ui_bridge_page_evaluate_safe_handler),
        )
        .route(
            "/ui-bridge/control/page/evaluate-batch",
            post(ui_bridge_page_evaluate_batch_handler),
        )
        .route(
            "/ui-bridge/control/page/set-tab",
            post(ui_bridge_page_set_tab_handler),
        )
        .route(
            "/ui-bridge/control/activate-tab/{tab_id}",
            post(ui_bridge_activate_tab_handler),
        )
        .route(
            "/ui-bridge/control/navigate-and-wait",
            post(ui_bridge_navigate_and_wait_handler),
        )
        .route(
            "/ui-bridge/control/page/summary",
            post(ui_bridge_page_summary_handler),
        )
        .route(
            "/ui-bridge/control/page/playbook",
            get(ui_bridge_page_playbook_handler),
        )
        .route(
            // GET + POST: page-summary is a pure read, so verification drivers
            // reach it with GET. Without the GET method axum returns a
            // framework 405 (no `text/plain` body), which slips past
            // `envelope_rewrite_middleware` and trips the debug-only
            // `envelope_audit` panic. See plan
            // 2026-06-05-ui-bridge-verification-read-freshness.
            "/ui-bridge/ai/page-summary",
            get(ui_bridge_page_summary_handler).post(ui_bridge_page_summary_handler),
        )
}

/// Normalize an optional `mode` string into `"hard"` / `"soft"`.
///
/// Returns `Err(other)` for any unrecognized mode (so the handler can return
/// a 400). Split out from the handler for unit-testing.
pub(crate) fn normalize_navigate_mode(mode: Option<&str>) -> Result<&'static str, String> {
    match mode {
        None | Some("hard") => Ok("hard"),
        Some("soft") => Ok("soft"),
        Some(other) => Err(other.to_string()),
    }
}

#[cfg(test)]
mod page_navigate_mode_tests {
    //! Unit tests for the soft/hard mode negotiation on
    //! `POST /ui-bridge/control/page/navigate`.

    use super::{normalize_navigate_mode, PageNavigateRequest};

    #[test]
    fn default_mode_is_hard() {
        assert_eq!(normalize_navigate_mode(None), Ok("hard"));
    }

    #[test]
    fn explicit_hard_mode_is_accepted() {
        assert_eq!(normalize_navigate_mode(Some("hard")), Ok("hard"));
    }

    #[test]
    fn soft_mode_is_accepted() {
        assert_eq!(normalize_navigate_mode(Some("soft")), Ok("soft"));
    }

    #[test]
    fn unknown_mode_is_rejected() {
        assert_eq!(normalize_navigate_mode(Some("spa")), Err("spa".to_string()));
        assert_eq!(normalize_navigate_mode(Some("")), Err("".to_string()));
    }

    #[test]
    fn request_without_mode_deserializes_to_none() {
        // Back-compat: legacy callers pass only { "url": "..." }.
        let req: PageNavigateRequest = serde_json::from_str(r#"{"url": "/fleet"}"#).expect("parse");
        assert_eq!(req.mode, None);
    }

    #[test]
    fn request_with_soft_mode_deserializes() {
        let req: PageNavigateRequest =
            serde_json::from_str(r#"{"url": "/fleet", "mode": "soft"}"#).expect("parse");
        assert_eq!(req.mode.as_deref(), Some("soft"));
    }
}

#[cfg(test)]
mod navigate_route_gate_tests {
    //! `page/navigate` must not report arriving somewhere it never went.
    //!
    //! The runner has no URL router. A path is turned into a page key and
    //! looked up in `PAGE_TO_TAB`; a key that is not there navigates nowhere
    //! — but `history.pushState` had already moved the address bar, so the
    //! snapshot's `route` (read by the SDK from `window.location.pathname`)
    //! echoed the caller's own unrouted request back at it, alongside
    //! `success: true`. Any automated check that navigated and then asserted
    //! on `route` was therefore self-confirming.
    //!
    //! These tests exercise `resolve_navigate_page`, the pure predicate the
    //! handler's gate is built on, in BOTH directions — a gate that rejected
    //! everything would end navigation entirely.
    use super::{
        resolve_navigate_page, same_origin_absolute_path, VALID_NAVIGATE_PAGES, VALID_TAB_IDS,
    };

    // -----------------------------------------------------------------------
    // Manual-test-loop iteration 21, item 2 — the same false PASS, one gate
    // over.
    //
    // Iteration 12 closed the hole for relative URLs. Absolute ones walked
    // straight past it: measured on a live runner,
    // `http://localhost:9881/terminal`, `http://127.0.0.1:9881/settings` AND
    // `http://localhost:9881/zzz-bogus-i20` all returned HTTP 200
    // `success: true` while the route stayed `/logs` — because the handler
    // gated only `url.starts_with('/')`, and the frontend discards absolute
    // URLs outright ("ignoring absolute URL navigation in runner") while still
    // answering success.
    //
    // The fix normalizes same-origin absolutes to their path BEFORE the gate,
    // so both spellings share ONE resolver rather than growing a second copy.
    // These tests pin the normalizer; composing it with
    // `resolve_navigate_page` below reproduces the handler's exact decision.
    // -----------------------------------------------------------------------

    /// The handler's decision, read off the SHARED resolver every navigate
    /// surface uses. `Err(None)` = rejected before the gate (not same-origin);
    /// `Err(Some(key))` = rejected BY the gate.
    fn navigate_decision(url: &str) -> Result<String, Option<String>> {
        match super::resolve_navigate_target(url) {
            Ok((_normalized, page)) => Ok(page),
            Err(super::NavigateRejection::NotNavigable) => Err(None),
            Err(super::NavigateRejection::UnroutedPage(key)) => Err(Some(key)),
        }
    }

    /// The resolver hands back the NORMALIZED url, not the caller's input —
    /// forwarding the original is what left the frontend discarding it.
    #[test]
    fn the_resolver_returns_the_normalized_url_for_callers_to_forward() {
        let (normalized, page) =
            super::resolve_navigate_target("http://localhost:9881/terminal").unwrap();
        assert_eq!(normalized, "/terminal");
        assert_eq!(page, "terminal");
        // A relative target passes through untouched.
        let (normalized, _) = super::resolve_navigate_target("/settings").unwrap();
        assert_eq!(normalized, "/settings");
    }

    #[test]
    fn a_same_origin_absolute_url_normalizes_to_its_path() {
        assert_eq!(
            same_origin_absolute_path("http://localhost:9881/terminal"),
            Some("/terminal".to_string())
        );
        assert_eq!(
            same_origin_absolute_path("http://127.0.0.1:9881/settings"),
            Some("/settings".to_string())
        );
        assert_eq!(
            same_origin_absolute_path("https://localhost/settings/world-state-verifier"),
            Some("/settings/world-state-verifier".to_string())
        );
        // No path at all is the root, not an empty key.
        assert_eq!(
            same_origin_absolute_path("http://localhost"),
            Some("/".to_string())
        );
        assert_eq!(
            same_origin_absolute_path("http://localhost:9881"),
            Some("/".to_string())
        );
    }

    /// A foreign host is NOT normalized — it must fall through to the
    /// handler's outright rejection rather than being rewritten into a
    /// same-origin path. `https://example.com/terminal` must never navigate
    /// the runner to `/terminal`.
    #[test]
    fn a_foreign_absolute_url_is_not_normalized() {
        assert_eq!(same_origin_absolute_path("https://example.com/x"), None);
        assert_eq!(
            same_origin_absolute_path("https://example.com/terminal"),
            None
        );
        assert_eq!(
            same_origin_absolute_path("http://localhost.evil.com/x"),
            None
        );
        // Userinfo must not smuggle a foreign authority past the host check.
        assert_eq!(same_origin_absolute_path("http://evil@localhost/x"), None);
        // Not an HTTP URL at all.
        assert_eq!(same_origin_absolute_path("/terminal"), None);
        assert_eq!(same_origin_absolute_path("ftp://localhost/x"), None);
    }

    /// The four measured cases, end to end through the handler's decision.
    #[test]
    fn absolute_and_relative_navigation_decide_identically() {
        // 1. absolute same-origin, valid → accepted (route moves)
        assert_eq!(
            navigate_decision("http://localhost:9881/terminal"),
            Ok("terminal".to_string())
        );
        assert_eq!(
            navigate_decision("http://127.0.0.1:9881/settings"),
            Ok("settings".to_string())
        );
        // 2. absolute same-origin, bogus → rejected BY the gate (400
        //    INVALID_REQUEST), not answered `success: true`.
        assert_eq!(
            navigate_decision("http://localhost:9881/zzz-bogus-i20"),
            Err(Some("zzz-bogus-i20".to_string()))
        );
        // 3. relative valid → unchanged
        assert_eq!(navigate_decision("/terminal"), Ok("terminal".to_string()));
        // 4. relative bogus → unchanged (iteration 12's fix still holds)
        assert_eq!(navigate_decision("/zzz"), Err(Some("zzz".to_string())));
        // …and the foreign absolute is rejected before the gate ever runs.
        assert_eq!(navigate_decision("https://example.com/x"), Err(None));
    }

    /// An absolute spelling must never be MORE permissive than its relative
    /// form — the gate predicts the frontend, which sees only the path.
    #[test]
    fn an_absolute_spelling_is_never_more_permissive_than_the_relative_one() {
        for path in ["/terminal", "/zzz", "/terminal?x=1", "/settings/nope", "/"] {
            for origin in [
                "http://localhost:9881",
                "http://127.0.0.1:9881",
                "https://localhost",
            ] {
                assert_eq!(
                    navigate_decision(&format!("{origin}{path}")),
                    navigate_decision(path),
                    "{origin}{path} must decide exactly as {path} does"
                );
            }
        }
    }

    #[test]
    fn a_real_page_resolves() {
        assert_eq!(resolve_navigate_page("/terminal"), Ok("terminal".into()));
        assert_eq!(resolve_navigate_page("/settings"), Ok("settings".into()));
        assert_eq!(resolve_navigate_page("/runs"), Ok("runs".into()));
    }

    #[test]
    fn a_nested_path_collapses_the_way_the_frontend_collapses_it() {
        // `usePageEvents.ts`: strip leading slashes, `/` -> `-`.
        assert_eq!(
            resolve_navigate_page("/settings/world-state-verifier"),
            Ok("settings-world-state-verifier".into())
        );
    }

    #[test]
    fn the_root_path_resolves_to_the_default_page() {
        assert_eq!(
            resolve_navigate_page("/"),
            Ok("gui-automation".into()),
            "the frontend defaults an empty key to gui-automation; the gate must agree"
        );
        assert_eq!(resolve_navigate_page("///"), Ok("gui-automation".into()));
    }

    /// The gate is `PAGE_TO_TAB`, not `VALID_TAB_IDS`.
    ///
    /// `PAGE_TO_TAB` carries navigation aliases that are NOT tab ids. Gating
    /// on the tab registry instead would 400 these perfectly real pages.
    #[test]
    fn navigation_aliases_that_are_not_tab_ids_still_resolve() {
        let aliases = ["home", "run", "ai", "history", "logs", "run-summary"];
        for alias in aliases {
            assert_eq!(
                resolve_navigate_page(&format!("/{alias}")),
                Ok(alias.to_string()),
                "`{alias}` is a PAGE_TO_TAB key and must remain navigable"
            );
        }
        // The two registries really are different sets — this is why the gate
        // could not just reuse `VALID_TAB_IDS`.
        assert!(
            aliases.iter().any(|a| !VALID_TAB_IDS.contains(a)),
            "at least one PAGE_TO_TAB alias must be absent from VALID_TAB_IDS, otherwise \
             this gate has no reason to exist separately"
        );
    }

    /// The regression itself.
    #[test]
    fn an_unrouted_path_is_rejected_rather_than_reported_as_success() {
        assert_eq!(resolve_navigate_page("/zzz"), Err("zzz".into()));
        assert_eq!(resolve_navigate_page("/page-zzz"), Err("page-zzz".into()));
        assert_eq!(
            resolve_navigate_page("/settings/nope"),
            Err("settings-nope".into())
        );
    }

    /// A path the frontend cannot handle is rejected even when its PREFIX is
    /// real: `usePageEvents.ts` does not strip query strings, so
    /// `/terminal?x=1` produces the key `terminal?x=1`, which resolves to
    /// nothing and navigates nowhere. Accepting it would put the false PASS
    /// straight back.
    #[test]
    fn a_query_string_is_not_quietly_forgiven() {
        assert_eq!(
            resolve_navigate_page("/terminal?x=1"),
            Err("terminal?x=1".into())
        );
    }

    #[test]
    fn the_generated_table_is_populated() {
        assert!(
            VALID_NAVIGATE_PAGES.len() > 50,
            "VALID_NAVIGATE_PAGES has only {} entries — build.rs codegen is stale or broken",
            VALID_NAVIGATE_PAGES.len()
        );
        assert!(!VALID_NAVIGATE_PAGES.contains(&"zzz"));
    }

    /// The generated slice must equal the TypeScript `PAGE_TO_TAB` keys
    /// verbatim (same keys, same order) — the same staleness proof
    /// `valid_tab_ids_match_typescript_union` gives the tab gate.
    #[test]
    fn valid_navigate_pages_match_the_typescript_map() {
        // cargo runs tests with CWD = crate root (src-tauri).
        let source = std::fs::read_to_string("../src/components/app/useAppNavigation.ts")
            .expect("useAppNavigation.ts must be readable from the crate root");
        let decl = source
            .find("const PAGE_TO_TAB")
            .expect("`const PAGE_TO_TAB` must exist in useAppNavigation.ts");
        let rest = &source[decl..];
        let open = rest.find("= {").expect("object literal") + 3;
        let close = rest[open..].find("\n};").expect("object terminator") + open;

        let ts_keys: Vec<String> = rest[open..close]
            .lines()
            .filter_map(|raw| {
                let line = raw.split("//").next().unwrap_or("").trim();
                let colon = line.find(':')?;
                let key = line[..colon].trim().trim_matches('"');
                (!key.is_empty()).then(|| key.to_string())
            })
            .collect();

        assert_eq!(
            ts_keys.len(),
            VALID_NAVIGATE_PAGES.len(),
            "Rust VALID_NAVIGATE_PAGES ({}) and TS PAGE_TO_TAB ({}) differ in length — build.rs \
             codegen is stale",
            VALID_NAVIGATE_PAGES.len(),
            ts_keys.len(),
        );
        assert_eq!(
            ts_keys, VALID_NAVIGATE_PAGES,
            "Rust VALID_NAVIGATE_PAGES must be generated verbatim from PAGE_TO_TAB",
        );
    }
}

#[cfg(test)]
mod tab_activate_tests {
    //! Unit tests for the F4 `POST /ui-bridge/control/tab/activate` handler's
    //! validation seam. Following the same pattern as
    //! `page_navigate_mode_tests` — exercise the pure helper so we don't need
    //! an axum router or a live frontend.
    //!
    //! The HTTP response shape (400 body with `error: "unknown_tab"` +
    //! `knownTabs`) is built in the handler; these tests lock down the
    //! accept/reject decision it relies on.
    use super::{validate_tab_id, TabActivateRequest, VALID_TAB_IDS};

    #[test]
    fn unknown_tab_id_returns_400() {
        // `validate_tab_id` returning `Err(known)` is the precondition the
        // handler uses to emit HTTP 400. The preview list must match the
        // authoritative `VALID_TAB_IDS` registry so callers can recover.
        let err = validate_tab_id("not-a-real-tab").expect_err("unknown tab should fail");
        assert!(!err.is_empty(), "known_tabs preview must not be empty");
        assert_eq!(err.len(), VALID_TAB_IDS.len());
        assert!(err.contains(&"specs"), "preview should contain common tabs");
    }

    #[test]
    fn empty_tab_id_is_rejected() {
        assert!(validate_tab_id("").is_err());
        assert!(validate_tab_id("   ").is_err());
    }

    #[test]
    fn known_tab_id_is_accepted() {
        assert_eq!(validate_tab_id("specs").expect("specs"), "specs");
        assert_eq!(
            validate_tab_id("state-machine").expect("state-machine"),
            "state-machine"
        );
        assert_eq!(
            validate_tab_id("  specs  ").expect("trimmed"),
            "specs",
            "validate_tab_id should trim whitespace"
        );
    }

    /// Regression: when a new `MainTabId` is added to
    /// `src/components/app/tab-types.ts`, the Rust gate must accept it too —
    /// otherwise `/control/tabs` advertises the tab while `tab/activate`
    /// rejects it as `unknown_tab`. (Surfaced 2026-05-02 for `"wrappers"`.)
    ///
    /// Since iter-2 R2 the slice is GENERATED from the TS union, so this can no
    /// longer drift — these ids are the historical misses, kept as a canary on
    /// the codegen itself. (`memory-federation` was the original live drift;
    /// it has since been retired from the union entirely, so it now lives in
    /// `retired_tab_ids_are_rejected` instead.)
    #[test]
    fn recently_added_tabs_are_accepted() {
        for id in &[
            "wrappers",
            "productivity",
            "settings-account",
            "settings-notifications",
        ] {
            validate_tab_id(id).unwrap_or_else(|_| {
                panic!(
                    "tab '{id}' missing from VALID_TAB_IDS — the slice is generated from \
                     src/components/app/tab-types.ts by build.rs; check the codegen",
                )
            });
        }
    }

    /// R1 companion: ids REMOVED from the `MainTabId` union (because nothing
    /// renders them) must also disappear from the Rust gate. Before iter-2,
    /// `tab/activate {"tabId":"monitor-issues"}` returned 200 and left the user
    /// staring at "This page could not be displayed".
    #[test]
    fn retired_tab_ids_are_rejected() {
        for id in &[
            "run-summary",
            "monitor-summary",
            "monitor-issues",
            "monitor-learnings",
            "monitor-discoveries",
            // Retired by 2026-07-26-claude-session-memory-cutover-to-coord
            // Phase 3a (memory-dir federation deleted).
            "memory-federation",
        ] {
            assert!(
                validate_tab_id(id).is_err(),
                "retired tab '{id}' must not be activatable — it has no renderer",
            );
        }
    }

    /// The generated slice must equal the TypeScript `VALID_TAB_IDS` array
    /// verbatim (same ids, same order). Proves the build-script codegen ran
    /// against the current source rather than a cached/stale OUT_DIR.
    #[test]
    fn valid_tab_ids_match_typescript_union() {
        // cargo runs tests with CWD = crate root (src-tauri).
        let source = std::fs::read_to_string("../src/components/app/tab-types.ts")
            .expect("tab-types.ts must be readable from the crate root");
        let decl = source
            .find("const VALID_TAB_IDS")
            .expect("`const VALID_TAB_IDS` must exist in tab-types.ts");
        let rest = &source[decl..];
        let open = rest.find("= [").expect("array literal") + 3;
        let close = rest[open..].find("];").expect("array terminator") + open;
        let ts_ids: Vec<&str> = rest[open..close]
            .split(',')
            .filter_map(|entry| {
                let t = entry.trim();
                t.strip_prefix('"').and_then(|t| t.strip_suffix('"'))
            })
            .collect();

        assert_eq!(
            ts_ids.len(),
            VALID_TAB_IDS.len(),
            "Rust VALID_TAB_IDS ({}) and TS VALID_TAB_IDS ({}) differ in length — build.rs codegen \
             is stale",
            VALID_TAB_IDS.len(),
            ts_ids.len(),
        );
        assert_eq!(
            ts_ids, VALID_TAB_IDS,
            "Rust VALID_TAB_IDS must be generated verbatim from tab-types.ts",
        );
    }

    #[test]
    fn tab_activate_request_deserializes_camel_case() {
        let req: TabActivateRequest =
            serde_json::from_str(r#"{"tabId": "specs"}"#).expect("parse camelCase");
        assert_eq!(req.tab_id, "specs");
    }

    /// Iter-3 item 1 — the `invalid_tab_id` factory must produce a
    /// `UiBridgeError` whose `code = INVALID_TAB_ID`, whose `message`
    /// echoes the rejected id, and whose `context` carries both the
    /// rejected id and the full `knownTabs` list. Locks down the
    /// envelope contract callers depend on: `error_detail.code` is the
    /// machine-readable surface, `error_detail.context.knownTabs` is
    /// the self-correction payload.
    #[test]
    fn invalid_tab_id_factory_envelope_shape() {
        use crate::mcp::ui_bridge::types::{UiBridgeError, UiBridgeErrorCode};
        let detail = UiBridgeError::invalid_tab_id("nonexistent", VALID_TAB_IDS);
        assert!(
            matches!(detail.code, UiBridgeErrorCode::InvalidTabId),
            "code must be InvalidTabId"
        );
        assert!(
            detail.message.contains("nonexistent"),
            "message echoes the rejected id"
        );
        let ctx = detail
            .context
            .as_ref()
            .expect("context must carry the structured payload");
        assert_eq!(
            ctx.get("code").and_then(|v| v.as_str()),
            Some("INVALID_TAB_ID")
        );
        assert_eq!(
            ctx.get("tabId").and_then(|v| v.as_str()),
            Some("nonexistent")
        );
        let known_arr = ctx
            .get("knownTabs")
            .and_then(|v| v.as_array())
            .expect("context.knownTabs must be an array");
        assert_eq!(
            known_arr.len(),
            VALID_TAB_IDS.len(),
            "knownTabs must include every entry from the registry"
        );
    }

    /// Regression: `VALID_TAB_IDS` must contain every id exactly once. A
    /// duplicate causes `/control/tabs` (and the `tabs_list` IPC response on
    /// the frontend side) to advertise the same id twice, which agents have
    /// no sane way to disambiguate. Surfaced 2026-05-20 for `"wrappers"`.
    #[test]
    fn valid_tab_ids_have_no_duplicates() {
        let unique: std::collections::BTreeSet<&str> = VALID_TAB_IDS.iter().copied().collect();
        assert_eq!(
            unique.len(),
            VALID_TAB_IDS.len(),
            "VALID_TAB_IDS contains duplicate entries (len {} vs unique {})",
            VALID_TAB_IDS.len(),
            unique.len(),
        );
    }
}

/// Static (method, path) tuples matching every route registered by `routes()`.
pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/control/tabs"),
        ("POST", "/ui-bridge/control/tab/activate"),
        ("POST", "/ui-bridge/control/page/refresh"),
        ("POST", "/ui-bridge/control/page/hard-refresh"),
        ("POST", "/ui-bridge/control/page/close-request"),
        ("POST", "/ui-bridge/control/page/force-close"),
        ("POST", "/ui-bridge/control/page/navigate"),
        ("POST", "/ui-bridge/control/page/navigate-to"),
        ("GET", "/ui-bridge/control/page/routes"),
        ("POST", "/ui-bridge/control/viewport-constraints"),
        ("POST", "/ui-bridge/control/page/back"),
        ("POST", "/ui-bridge/control/page/forward"),
        ("POST", "/ui-bridge/control/query-selector"),
        ("POST", "/ui-bridge/control/page/evaluate"),
        ("POST", "/ui-bridge/control/page/evaluate-raw"),
        ("POST", "/ui-bridge/control/page/evaluate-safe"),
        ("POST", "/ui-bridge/control/page/evaluate-batch"),
        ("POST", "/ui-bridge/control/page/set-tab"),
        ("POST", "/ui-bridge/control/activate-tab/{tab_id}"),
        ("POST", "/ui-bridge/control/navigate-and-wait"),
        ("POST", "/ui-bridge/control/page/summary"),
        ("GET", "/ui-bridge/control/page/playbook"),
        ("GET", "/ui-bridge/ai/page-summary"),
        ("POST", "/ui-bridge/ai/page-summary"),
    ]
}

#[cfg(test)]
mod batch_expression_js_tests {
    //! Regression tests for the JS `evaluate-batch` emits.
    //!
    //! The frontend fix for `/control/page/evaluate` (`compileEvaluateExpression`)
    //! could not reach this endpoint: `evaluate-batch` assembles its own JS
    //! program in Rust and sends the *assembled* program as the expression, so
    //! each sub-expression is spliced into a `return` position here, before the
    //! frontend ever sees it. These tests pin the two splice invariants.

    use super::{build_batch_expression_js, BatchExpression};

    fn expr(id: &str, expression: &str) -> BatchExpression {
        BatchExpression {
            id: id.to_string(),
            expression: expression.to_string(),
        }
    }

    /// THE REGRESSION, batch flavour. `return\n({a:1})` is ASI-truncated, so
    /// `v` is `undefined` and the batch entry reports `success: true` with a
    /// null value — a per-expression silent false green.
    #[test]
    fn leading_newline_sub_expression_stays_on_the_return_line() {
        let js = build_batch_expression_js(&[expr("a", "\n({a:1})")]);
        assert!(
            js.contains("return ({a:1})"),
            "sub-expression must not be pushed onto its own line: {js}"
        );
        assert!(
            !js.contains("return \n"),
            "no `return` may be left dangling before a newline: {js}"
        );
    }

    /// A sub-expression ending in a line comment used to comment out the rest
    /// of the emitted line — which holds the entry's own `};` AND every later
    /// expression in the array. One commented expression took the whole batch
    /// down with it.
    #[test]
    fn trailing_line_comment_does_not_swallow_the_rest_of_the_batch() {
        let js = build_batch_expression_js(&[expr("a", "1 // note"), expr("b", "2")]);
        // The second entry must still be reachable, i.e. it must not sit on
        // the same line as the first entry's trailing comment.
        let comment_line = js
            .lines()
            .find(|l| l.contains("// note"))
            .expect("comment line present");
        assert!(
            !comment_line.contains("id: \"b\""),
            "later batch entries must not be commented out: {comment_line}"
        );
        assert!(
            js.contains("id: \"b\""),
            "second entry must be emitted: {js}"
        );
    }

    /// Ids are emitted as JSON string literals, so a quote/backslash/newline in
    /// an id can't break out of the literal and corrupt the program.
    #[test]
    fn ids_are_emitted_as_json_string_literals() {
        let js = build_batch_expression_js(&[expr(r#"we"ird\one"#, "1")]);
        assert!(
            js.contains(r#"id: "we\"ird\\one""#),
            "id must be a properly escaped JS literal: {js}"
        );
        assert!(
            !js.contains(r#"id: "we"ird"#),
            "raw quote must not survive unescaped: {js}"
        );
    }

    /// The envelope the response parser expects is unchanged.
    #[test]
    fn emitted_program_shape_is_preserved() {
        let js = build_batch_expression_js(&[expr("a", "1"), expr("b", "2")]);
        assert!(js.starts_with("(() => { return JSON.stringify(["));
        assert!(js.ends_with("]) })()"));
        assert!(js.contains("success: true"));
        assert!(js.contains("error: e.message"));
    }

    #[test]
    fn empty_batch_emits_an_empty_array() {
        let js = build_batch_expression_js(&[]);
        assert_eq!(js, "(() => { return JSON.stringify([]) })()");
    }
}

#[cfg(test)]
mod page_evaluate_escaping_tests {
    //! Regression tests for the `/control/page/evaluate` direct-WebView path:
    //! the user's expression must be encoded as a JS string literal and
    //! `eval()`ed, so literal newlines / quotes / backslashes inside the
    //! expression survive the splice into the callback JS template.
    //!
    //! We can't spin up a real webview inside a unit test, so instead we
    //! validate the two invariants that make the fix correct:
    //!   1. `serde_json::to_string` produces a valid JS string literal.
    //!   2. Splicing that literal into `eval(...)` yields a JS source where
    //!      newlines in the original expression no longer appear as raw
    //!      line terminators.

    /// Helper mirroring the production encoding used in
    /// `direct_webview_evaluate_with_result`.
    fn build_eval_inner(expression: &str, await_promise: bool) -> String {
        let expr_literal = serde_json::to_string(expression).expect("encode expression");
        if await_promise {
            format!("await Promise.resolve(eval({}))", expr_literal)
        } else {
            format!("eval({})", expr_literal)
        }
    }

    #[test]
    fn expression_with_newline_in_string_literal_is_escaped() {
        // The original Phase 3J repro: `stack: "at A\n  at B"` decoded from
        // JSON produces a real newline inside the expression. Before the fix,
        // this newline appeared verbatim in the generated JS → SyntaxError.
        let expression = "invoke(\"report_ui_error\", {stack: \"at A\n  at B\"})";
        let emitted = build_eval_inner(expression, false);

        // The raw literal newline from the source expression must NOT appear
        // inside the emitted `eval(...)` argument — it should have been
        // escaped to the two-character sequence \n by serde_json.
        // (`emitted` itself still begins `eval("...` with no newlines.)
        assert!(
            !emitted.contains('\n'),
            "emitted JS still contains a raw newline: {:?}",
            emitted
        );
        // But the escaped sequence must be present.
        assert!(
            emitted.contains(r"\n"),
            "emitted JS missing escaped newline: {:?}",
            emitted
        );
        // Shape check: the wrapper is the eval form.
        assert!(
            emitted.starts_with("eval(\""),
            "unexpected prefix: {:?}",
            emitted
        );
        assert!(emitted.ends_with("\")"), "unexpected suffix: {:?}", emitted);
    }

    #[test]
    fn await_promise_emits_promise_resolve_wrapper() {
        let emitted = build_eval_inner("invoke(\"long_async_op\")", true);
        assert!(
            emitted.starts_with("await Promise.resolve(eval("),
            "awaitPromise=true wrapper missing: {:?}",
            emitted
        );
    }

    #[test]
    fn expression_with_quotes_and_backslashes_roundtrips() {
        // A nasty mix: embedded double-quote, single-quote, backslash, and
        // unicode. serde_json must escape all of these such that the emitted
        // string is a single JS string literal — no premature termination.
        let expression = r#"doc.querySelector('a[href="/x\\y"]').click() // "test""#;
        let emitted = build_eval_inner(expression, false);

        // Exactly one `eval("` opening and one `")` closing — no premature
        // quote in the middle would split the literal into multiple tokens.
        assert_eq!(emitted.matches("eval(\"").count(), 1);
        assert!(emitted.ends_with("\")"));
        // And the emitted blob must decode back to the original when parsed
        // as a JSON string (proving the JS eval() would see the same bytes).
        let inner = &emitted["eval(".len()..emitted.len() - 1];
        let decoded: String = serde_json::from_str(inner).expect("decode back");
        assert_eq!(decoded, expression);
    }
}

#[cfg(test)]
mod page_evaluate_tagging_tests {
    //! Plan item D regression tests for request-tagged `/control/page/evaluate`.
    //!
    //! Concurrent `/page/evaluate` callers against the same runner must never
    //! observe each other's responses. The HTTP handler path goes through
    //! `tagged_page_evaluate` → `EvaluateRequestStore` → the
    //! `ui-bridge:evaluate-response` listener installed in `mcp_api.rs`,
    //! with per-call uuid request_ids keying the pending oneshots.
    //!
    //! We can't spin up Tauri inside a unit test, so these tests exercise the
    //! store directly. The store is the sole correlation surface: every
    //! arriving response has to round-trip through `deliver(request_id, …)`,
    //! so showing the store's id-keyed routing is correct is equivalent to
    //! showing the handler path is correct.
    use super::{resolve_evaluate_budget, DEFAULT_PAGE_EVALUATE_TIMEOUT_MS};
    use crate::ui_bridge_evaluate::{EvaluateRequestStore, EvaluateResponse};
    use std::sync::Arc;
    use tokio::sync::oneshot;

    /// An omitted `timeoutMs` must reach the frontend as the default budget
    /// AND as a budget the caller did not choose.
    ///
    /// The second half is the one that regressed: `unwrap_or` alone produced
    /// the right NUMBER while erasing the provenance, so the frontend told a
    /// caller who sent nothing that the budget "came from the `timeoutMs` you
    /// sent". `page/evaluate-raw` always lands on this arm — it has no
    /// `timeoutMs` field for a caller to send.
    #[test]
    fn an_omitted_timeout_is_flagged_as_the_default() {
        let (timeout_ms, from_default) = resolve_evaluate_budget(None);
        assert_eq!(timeout_ms, DEFAULT_PAGE_EVALUATE_TIMEOUT_MS);
        assert!(
            from_default,
            "an omitted timeoutMs is OUR default, not the caller's choice"
        );
    }

    /// A caller-supplied budget is forwarded verbatim and attributed to the
    /// caller — including when it happens to equal our default, which is the
    /// case the number alone can never distinguish.
    #[test]
    fn a_supplied_timeout_is_forwarded_and_attributed_to_the_caller() {
        for requested in [1_000_u64, 10_000, 60_000, 600_000] {
            let (timeout_ms, from_default) = resolve_evaluate_budget(Some(requested));
            assert_eq!(
                timeout_ms, requested,
                "the caller's budget is forwarded verbatim"
            );
            assert!(
                !from_default,
                "timeoutMs={requested} was chosen by the caller, even when it equals our default"
            );
        }
    }

    /// Fire two concurrent "page_evaluate" calls through the store-level
    /// surface that `tagged_page_evaluate` relies on. Each call registers
    /// its own oneshot, deliveries land in the opposite order, and each
    /// caller observes exactly its own result. This is the plan's
    /// "unit test that fires two concurrent `/page/evaluate` calls and
    /// asserts they each get their own result" verification criterion.
    #[tokio::test]
    async fn concurrent_page_evaluate_calls_do_not_interleave() {
        let store = Arc::new(EvaluateRequestStore::new());

        // Caller A: evaluates `document.title` → "Runner".
        let request_id_a = "evaluate-call-a".to_string();
        let (tx_a, rx_a) = oneshot::channel::<EvaluateResponse>();
        store.register("main", &request_id_a, tx_a).await;

        // Caller B: evaluates `window.location.pathname` → "/dashboard".
        let request_id_b = "evaluate-call-b".to_string();
        let (tx_b, rx_b) = oneshot::channel::<EvaluateResponse>();
        store.register("main", &request_id_b, tx_b).await;

        // Run both HTTP-handler-equivalent awaits concurrently. Each
        // simulates `tagged_page_evaluate`'s `tokio::time::timeout(..., rx)`.
        let caller_a = tokio::spawn(async move {
            tokio::time::timeout(std::time::Duration::from_secs(2), rx_a)
                .await
                .expect("caller A should not time out")
                .expect("caller A sender should not drop")
        });
        let caller_b = tokio::spawn(async move {
            tokio::time::timeout(std::time::Duration::from_secs(2), rx_b)
                .await
                .expect("caller B should not time out")
                .expect("caller B sender should not drop")
        });

        // Frontend (simulated) delivers B's response first, then A's —
        // proving the store's id-keyed routing doesn't care about arrival
        // order and that concurrent pending entries don't shadow each other.
        let store_for_b = store.clone();
        let deliver_b = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            store_for_b
                .deliver(
                    "main",
                    "evaluate-call-b",
                    EvaluateResponse {
                        ok: true,
                        result: Some(serde_json::json!({
                            "success": true,
                            "result": { "value": "/dashboard" }
                        })),
                        error: None,
                    },
                )
                .await
        });
        let store_for_a = store.clone();
        let deliver_a = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            store_for_a
                .deliver(
                    "main",
                    "evaluate-call-a",
                    EvaluateResponse {
                        ok: true,
                        result: Some(serde_json::json!({
                            "success": true,
                            "result": { "value": "Runner" }
                        })),
                        error: None,
                    },
                )
                .await
        });

        assert!(deliver_a.await.expect("deliver_a join"));
        assert!(deliver_b.await.expect("deliver_b join"));

        let result_a = caller_a.await.expect("caller_a join");
        let result_b = caller_b.await.expect("caller_b join");

        // Each caller received its own expression's result, not the other's.
        assert!(result_a.ok);
        assert!(result_b.ok);
        assert_eq!(
            result_a
                .result
                .as_ref()
                .and_then(|v| v.pointer("/result/value"))
                .and_then(|v| v.as_str()),
            Some("Runner"),
            "caller A must see its own result, not caller B's"
        );
        assert_eq!(
            result_b
                .result
                .as_ref()
                .and_then(|v| v.pointer("/result/value"))
                .and_then(|v| v.as_str()),
            Some("/dashboard"),
            "caller B must see its own result, not caller A's"
        );

        // Store should be fully drained after both deliveries.
        assert_eq!(store.pending_len().await, 0);
    }

    /// Timing out one call must not strand the other call's pending slot.
    /// Simulates: A times out (cancelled), B still delivers successfully.
    #[tokio::test]
    async fn timeout_of_one_page_evaluate_does_not_affect_sibling() {
        let store = Arc::new(EvaluateRequestStore::new());

        let request_id_a = "evaluate-timeout".to_string();
        let (tx_a, rx_a) = oneshot::channel::<EvaluateResponse>();
        store.register("main", &request_id_a, tx_a).await;

        let request_id_b = "evaluate-sibling".to_string();
        let (tx_b, rx_b) = oneshot::channel::<EvaluateResponse>();
        store.register("main", &request_id_b, tx_b).await;

        // Caller A times out while waiting (mirrors the Elapsed branch of
        // tagged_page_evaluate). The handler cancels its slot.
        let wait_a = tokio::time::timeout(std::time::Duration::from_millis(50), rx_a).await;
        assert!(wait_a.is_err(), "caller A must time out");
        store.cancel("main", &request_id_a).await;

        // Caller B still gets a clean delivery afterwards.
        let store_for_b = store.clone();
        tokio::spawn(async move {
            store_for_b
                .deliver(
                    "main",
                    "evaluate-sibling",
                    EvaluateResponse {
                        ok: true,
                        result: Some(serde_json::json!({ "value": 7 })),
                        error: None,
                    },
                )
                .await
        });

        let result_b = tokio::time::timeout(std::time::Duration::from_secs(1), rx_b)
            .await
            .expect("caller B should not time out")
            .expect("caller B sender should not drop");

        assert!(result_b.ok);
        assert_eq!(
            result_b.result.as_ref().and_then(|v| v.get("value")),
            Some(&serde_json::json!(7))
        );
        assert_eq!(store.pending_len().await, 0);
    }
}

#[cfg(test)]
mod close_door_tests {
    //! The close doors' liveness policy and the force-close body parser.
    //!
    //! Plan `2026-08-19-runner-blocked-ui-thread-cannot-be-closed`, Phase 3.
    //! These cover the *decisions*, not the Win32 probe itself — a real
    //! `SendMessageTimeoutW` round-trip needs a window and a message loop, so
    //! it belongs to the temp-runner gate, not a unit test.

    use super::{
        force_close_header_present, parse_force_close_body, EventLoopVerdict,
        EVENT_LOOP_PROBE_CEILING, FORCE_CLOSE_HEADER,
    };

    /// A pumping loop must not be refused. This is the arm that keeps the
    /// pre-existing behaviour intact for every healthy runner — the change is
    /// meant to remove a false success, not add a false failure.
    #[test]
    fn a_pumping_loop_is_never_refused() {
        assert_eq!(EventLoopVerdict::Pumping.refusal(), None);
    }

    /// A confirmed wedge refuses, and says it is confirmed. Both discriminators
    /// the health monitor can produce are covered: the latched multi-sample
    /// verdict and a single failed round-trip.
    #[test]
    fn a_confirmed_wedge_refuses_as_confirmed() {
        assert_eq!(
            EventLoopVerdict::Wedged("wedge_detector_latched").refusal(),
            Some((true, "wedge_detector_latched"))
        );
        assert_eq!(
            EventLoopVerdict::Wedged("probe_no_round_trip").refusal(),
            Some((true, "probe_no_round_trip"))
        );
    }

    /// **UNKNOWN refuses on Windows.** This is the decision the whole step
    /// turns on: the route previously answered `200 {"success": true}` for a
    /// close it had no way to verify, and during the 2026-08-19 incident that
    /// answer was false. An unverifiable close is reported as a refusal with
    /// `confirmed: false` — never as a success.
    #[cfg(windows)]
    #[test]
    fn unknown_refuses_on_windows_but_does_not_claim_an_observation() {
        assert_eq!(
            EventLoopVerdict::Unknown("probe_unavailable").refusal(),
            Some((false, "probe_unavailable")),
            "UNKNOWN must refuse, and must flag itself unconfirmed"
        );
    }

    /// **UNKNOWN passes off Windows**, and the asymmetry is not an oversight.
    /// `SendMessageTimeoutW` asks a Win32 question; `ui_thread_pumping()` is a
    /// `None`-returning stub elsewhere. Refusing on that `None` would break
    /// this route on every macOS/Linux build for a condition those platforms
    /// detect by another route entirely (the webview shares the main thread
    /// there, so a blocked loop stops the HTTP pong and the shipped 90s
    /// `ui_stale` arm fires). Absence of a detector is not evidence of a hang.
    #[cfg(not(windows))]
    #[test]
    fn unknown_passes_off_windows_where_no_probe_exists() {
        assert_eq!(
            EventLoopVerdict::Unknown("probe_unavailable").refusal(),
            None
        );
    }

    /// The `spawn_blocking` join in [`event_loop_verdict`] must have a ceiling
    /// ABOVE the probe's own internal bound, or a healthy-but-slow sample is
    /// reported as UNKNOWN (a refusal, on Windows) and the door stops working
    /// for the case it was written for.
    #[test]
    fn the_event_loop_probe_ceiling_sits_above_the_probe_s_own_bound() {
        let probe_bound = std::time::Duration::from_millis(
            crate::health_monitor::UI_THREAD_PROBE_TIMEOUT_MS as u64,
        );
        assert!(
            EVENT_LOOP_PROBE_CEILING > probe_bound,
            "the async ceiling {EVENT_LOOP_PROBE_CEILING:?} must exceed the probe's own \
             {probe_bound:?} bound, or every slow-but-successful probe reads as UNKNOWN"
        );
        // …and it must still be a CEILING. An unbounded join is what this
        // replaces: `close-request` is the route an operator reaches for when
        // the window will not close, and it must answer.
        assert!(
            EVENT_LOOP_PROBE_CEILING <= std::time::Duration::from_secs(10),
            "close-request must answer promptly; {EVENT_LOOP_PROBE_CEILING:?} is not prompt"
        );
    }

    /// FINDING 1(b) — the probe join is BOUNDED.
    ///
    /// `spawn_blocking(...).await` has no deadline of its own. The Win32 call
    /// inside is capped, but that is a property of the probe, not of the
    /// await: a saturated blocking pool (which finding 3 shows a wedge can
    /// produce) means the task never starts and `close-request` parks with no
    /// ceiling at all — on the one route that exists for a runner whose window
    /// will not close.
    ///
    /// A SOURCE assertion because the failure needs a saturated tokio blocking
    /// pool to reproduce.
    #[test]
    fn the_close_request_probe_join_is_wrapped_in_a_timeout() {
        let src = include_str!("page.rs");
        let start = src
            .find("async fn event_loop_verdict()")
            .expect("event_loop_verdict must exist");
        let end = src[start..].find("\n}\n").expect("its body ends") + start;
        let body: String = src[start..end]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("spawn_blocking"),
            "negative control: the probe still goes to a blocking task"
        );
        assert!(
            body.contains("tokio::time::timeout"),
            "the spawn_blocking join must be bounded — an unbounded await here parks \
             close-request for the whole wedge"
        );
        assert!(
            body.contains("EVENT_LOOP_PROBE_CEILING"),
            "…and bounded by the named ceiling, not an inline literal"
        );
    }

    /// FINDING 6 — both readers of the wedge latch use the SAME guard.
    ///
    /// This route read `health_monitor::ui_thread_wedged()` raw while every
    /// `derived_status` sink read it through
    /// `ui_error::native_ui_probe_verdict()`, which additionally requires a
    /// cached HWND and a RUNNING monitor. The two disagreed exactly when it
    /// mattered: `stop_health_monitor()` cleared `MONITOR_RUNNING` without
    /// clearing the atom, so this door answered `Wedged` off a dead monitor —
    /// refusing every close forever — while `/health` correctly said UNKNOWN.
    #[test]
    fn the_close_door_reads_the_latch_through_the_same_guard_as_health() {
        let src = include_str!("page.rs");
        let start = src
            .find("async fn event_loop_verdict()")
            .expect("event_loop_verdict must exist");
        let end = src[start..].find("\n}\n").expect("its body ends") + start;
        let body: String = src[start..end]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("native_ui_probe_verdict()"),
            "the latch must be read through the shared guarded reader"
        );
        assert!(
            !body.contains("health_monitor::ui_thread_wedged()"),
            "the raw atomic read is back — this door will disagree with /health about a \
             stopped monitor and refuse every close forever"
        );
    }

    /// FINDING 11 — force-close is CSRF-gated by a mandatory custom header.
    ///
    /// The MCP API is `CorsLayer::new().allow_origin(Any)` by design, and the
    /// body parser never rejects, so a **simple** cross-origin `POST` (no
    /// custom header, no JSON `Content-Type`, no body) is exempt from the CORS
    /// preflight entirely — any page in the operator's browser could
    /// force-close the runner and `taskkill /F /T` every tracked agent tree.
    /// Requiring any custom header takes the request out of the simple set and
    /// forces a preflight a drive-by page cannot satisfy.
    #[test]
    fn force_close_requires_a_header_a_simple_cross_origin_post_cannot_send() {
        use axum::http::HeaderMap;

        // The drive-by shape: nothing but the method and the URL.
        assert!(
            !force_close_header_present(&HeaderMap::new()),
            "a bare cross-origin POST must not satisfy the gate"
        );

        // Headers a SIMPLE request is allowed to carry are not enough either —
        // none of them triggers a preflight.
        let mut simple = HeaderMap::new();
        simple.insert("content-type", "text/plain".parse().unwrap());
        simple.insert("accept", "*/*".parse().unwrap());
        simple.insert("origin", "https://evil.example".parse().unwrap());
        assert!(
            !force_close_header_present(&simple),
            "simple-request headers must not satisfy the gate"
        );

        // The operator's `curl -H 'X-Qontinui-Force-Close: 1'` does. Value is
        // irrelevant — the preflight is the gate, not the contents.
        for value in ["1", "0", "", "yes please"] {
            let mut ok = HeaderMap::new();
            ok.insert(FORCE_CLOSE_HEADER, value.parse().unwrap());
            assert!(
                force_close_header_present(&ok),
                "any value of {FORCE_CLOSE_HEADER} must pass (value {value:?})"
            );
        }

        // Header names are case-insensitive on the wire; the gate must be too.
        let mut mixed = HeaderMap::new();
        mixed.insert("x-qontinui-force-close", "1".parse().unwrap());
        assert!(force_close_header_present(&mixed));

        // The constant is the lowercase wire form (axum's `HeaderMap` keys are
        // lowercase), so a rename cannot silently disarm the gate.
        assert_eq!(FORCE_CLOSE_HEADER, FORCE_CLOSE_HEADER.to_ascii_lowercase());
    }

    /// The force-close body parser never fails the request. An operator whose
    /// runner is wedged must not be turned away over a missing BODY — the one
    /// thing it is turned away over is the header above.
    #[test]
    fn force_close_body_parsing_never_rejects() {
        assert_eq!(parse_force_close_body("").reason, None);
        assert_eq!(parse_force_close_body("   ").reason, None);
        assert_eq!(parse_force_close_body("not json at all").reason, None);
        assert_eq!(parse_force_close_body("{}").reason, None);
        assert_eq!(
            parse_force_close_body(r#"{"reason":"X button did nothing"}"#).reason,
            Some("X button did nothing".to_string())
        );
    }
}

#[cfg(test)]
mod navigate_response_honesty_tests {
    use super::augment_navigate_response;

    /// The reported defect: `mode: "hard"` (the DEFAULT) came back
    /// `{"hard": true}` for a navigation that performed no reload at all.
    /// Measured 2/2 with two witnesses a real reload would have reset — the
    /// SDK's 20-entry navigation ring kept its 43-minute-old oldest entry,
    /// and the single boot-time `[PROJECT_SELECTION]` console error stayed
    /// single across four `hard` navigations.
    #[test]
    fn hard_mode_does_not_claim_a_reload_it_did_not_perform() {
        let mut data = serde_json::json!({ "success": true });
        augment_navigate_response(&mut data, "/settings", "hard");
        assert_eq!(data["hard"], serde_json::json!(false));
        assert_eq!(data["reloaded"], serde_json::json!(false));
        // The requested mode is still echoed — the caller can audit what it
        // asked for; it just no longer doubles as a claim about what happened.
        assert_eq!(data["mode"], serde_json::json!("hard"));
        assert_eq!(data["url"], serde_json::json!("/settings"));
    }

    #[test]
    fn soft_mode_reports_the_same_absence_of_a_reload() {
        let mut data = serde_json::json!({ "success": true });
        augment_navigate_response(&mut data, "/terminal", "soft");
        assert_eq!(data["hard"], serde_json::json!(false));
        assert_eq!(data["reloaded"], serde_json::json!(false));
        assert_eq!(data["mode"], serde_json::json!("soft"));
    }

    /// A frontend that still answers `hard: true` must not be able to put
    /// that back on the wire — the route owns this claim, so the stamp
    /// OVERWRITES rather than deferring. The old `.or_insert` is exactly why
    /// the false claim survived.
    #[test]
    fn a_frontend_claiming_hard_true_is_overridden_not_deferred_to() {
        let mut data = serde_json::json!({ "success": true, "hard": true, "reloaded": true });
        augment_navigate_response(&mut data, "/settings", "hard");
        assert_eq!(data["hard"], serde_json::json!(false));
        assert_eq!(data["reloaded"], serde_json::json!(false));
    }

    /// `url`/`mode` are still deferred to the frontend when it supplies them
    /// (only the two outcome claims are seized).
    #[test]
    fn frontend_supplied_url_and_mode_are_preserved() {
        let mut data = serde_json::json!({ "url": "/from-frontend", "mode": "soft" });
        augment_navigate_response(&mut data, "/from-rust", "hard");
        assert_eq!(data["url"], serde_json::json!("/from-frontend"));
        assert_eq!(data["mode"], serde_json::json!("soft"));
    }
}
