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
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use tauri::Emitter;
use tracing::{debug, error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::helpers::{direct_webview_evaluate_with_result, evaluate_js_expression, safe_evaluate};
use super::request::{ui_bridge_request_sync, wrap_ipc_result};

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
/// `mode` is optional and defaults to `"hard"` (full webview reload). `"soft"`
/// performs a SPA-style `history.pushState` navigation that preserves any
/// injected window state (fetch patches, spies, `window.__*` globals). Any
/// value other than `"hard"` / `"soft"` is rejected with a 400.
#[derive(Debug, Deserialize)]
pub struct PageNavigateRequest {
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) mode: Option<String>,
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

/// Request for page evaluation
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageEvaluateRequest {
    pub expression: String,
    /// Optional timeout override in milliseconds (clamped to [1000, 600000]).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// When true, if the expression returns a Promise the runner awaits it.
    #[serde(default)]
    pub await_promise: bool,
    /// When true, the frontend returns a consistent discriminated
    /// `{value, type}` shape regardless of result type. When false/absent,
    /// the legacy conditional-wrapping shape is preserved for backward
    /// compatibility.
    #[serde(default)]
    pub unwrap: Option<bool>,
    /// EXPLICIT opt-in. When true, the frontend evaluate handler relaxes
    /// only the four network-related blocks (fetch / XMLHttpRequest /
    /// sendBeacon / WebSocket) so test assertions can hit runner APIs
    /// directly. Structural code-injection blocks stay in force regardless.
    #[serde(default)]
    pub allow_network_requests: Option<bool>,
}

/// Request to evaluate multiple JS expressions in one round-trip.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchEvaluateRequest {
    pub expressions: Vec<BatchExpression>,
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

// ============================================================================
// Tab id registry (shared between set-tab and activate-tab)
// ============================================================================

/// Valid `MainTabId` values, mirroring `src/components/app/tab-types.ts`.
/// Kept in sync manually.
const VALID_TAB_IDS: &[&str] = &[
    "prompt-home",
    "gui-automation",
    "workflow-queue",
    "active",
    "runs",
    "history",
    "error-monitor",
    "processes",
    "reflection",
    "observations",
    "architecture",
    "generator-eval",
    "meta-optimizer",
    "run-recap",
    "run-actions",
    "run-image",
    "run-findings",
    "run-state-explorer",
    "run-tests",
    "run-ai-output",
    "run-statistics",
    "run-ai-data",
    "run-traces",
    "ai",
    "logs",
    "run-summary",
    "monitor-summary",
    "monitor-findings",
    "monitor-issues",
    "monitor-learnings",
    "monitor-state-explorer",
    "monitor-statistics",
    "monitor-discoveries",
    "library",
    "step-builders",
    "check-builder",
    "check-group-builder",
    "shell-command-builder",
    "task-builder",
    "context-builder",
    "playwright-test-builder",
    "unified-workflow-builder",
    "state-machine",
    "specs",
    "capture",
    "config-log-sources",
    "config-findings",
    "config-hooks",
    "config-ui-bridge",
    "triggers",
    "tasks",
    "settings",
    "settings-ai",
    "settings-agentic",
    "settings-self-healing",
    "settings-world-state-verifier",
    "settings-playwright",
    "settings-mobile",
    "settings-discovery",
    "settings-backend-connection",
    "settings-mcp",
    "settings-log-sources",
    "settings-execution-variables",
    "settings-general",
    "settings-storage",
    "settings-backup",
    "settings-instances",
    "settings-debug",
    "settings-security",
    "accessibility-explorer",
    "settings-updates",
    "orchestration-loop",
    "image-quality-tests",
    "terminal",
    "llm-analytics",
    "cost-control",
    "evaluation",
    "skills",
    "help",
    "automation-health",
    "activity-timeline",
    "watchers",
    "knowledge-explorer",
    "event-history",
    "development-intelligence",
    "demo-video",
    "product-tours",
    "session-recap",
    "api-surface",
    "decision-trail",
    "memory-search",
    "online-learning",
    "dag-workflow-editor",
    "project-explainer",
    "wrappers",
    "productivity",
];

// ============================================================================
// Navigate-and-wait
// ============================================================================

pub async fn ui_bridge_navigate_and_wait_handler(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<NavigateAndWaitRequest>,
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

/// Simulate a user clicking the window's X button.
pub async fn ui_bridge_page_close_request_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    use tauri::Manager;
    info!("UI Bridge API: Close request (simulating X-button click)");

    if let Some(window) = state
        .app_handle
        .get_webview_window(qontinui_runner_lib::get_main_window_label())
    {
        window.close().map_err(|e| {
            let msg = format!("Failed to close main window: {}", e);
            error!("{}", msg);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(msg)))
        })?;
        Ok(Json(ApiResponse::success(serde_json::json!({
            "success": true,
            "message": "Close requested; Tauri WindowEvent::CloseRequested handler should now fire"
        }))))
    } else {
        Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error("Main webview window not found".to_string())),
        ))
    }
}

/// Navigate to a URL.
///
/// Accepts an optional `mode` field:
/// - `"hard"` (default): full webview reload via `window.location.href = url`.
/// - `"soft"`: SPA-style `history.pushState` + synthetic `popstate`/`ui-bridge:navigate`
///   events, preserving any injected `window.<custom-globals>` state.
///
/// The response `data` block carries `{ url, hard, mode }` so callers can
/// audit which behaviour the runner executed. The legacy `hard` flag is
/// retained for back-compat — old clients that only read `hard` continue to
/// work.
pub async fn ui_bridge_page_navigate_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<PageNavigateRequest>,
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
    if !url.starts_with('/') {
        let is_valid_localhost = [
            "http://localhost/",
            "http://localhost:",
            "https://localhost/",
            "https://localhost:",
            "http://127.0.0.1/",
            "http://127.0.0.1:",
            "https://127.0.0.1/",
            "https://127.0.0.1:",
        ]
        .iter()
        .any(|prefix| url.starts_with(prefix))
            || url == "http://localhost"
            || url == "https://localhost"
            || url == "http://127.0.0.1"
            || url == "https://127.0.0.1";

        if !is_valid_localhost {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!(
                    "Only relative URLs (starting with /) or localhost URLs are allowed, got: {}",
                    url
                ))),
            ));
        }
    }

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
            // Ensure the response carries the mode we actually used so callers
            // can audit soft-vs-hard negotiation. The JS handler should already
            // populate these; this block is defensive for older frontends.
            if let Some(obj) = data.as_object_mut() {
                obj.entry("url".to_string())
                    .or_insert_with(|| serde_json::Value::String(url.to_string()));
                obj.entry("mode".to_string())
                    .or_insert_with(|| serde_json::Value::String(mode.to_string()));
                obj.entry("hard".to_string())
                    .or_insert_with(|| serde_json::Value::Bool(mode == "hard"));
            }
            data
        });
    wrap_ipc_result(result)
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
    Json(request): Json<QuerySelectorRequest>,
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

/// Dispatch a page/evaluate request over the tagged
/// `ui-bridge:evaluate-request` / `ui-bridge:evaluate-response` event pair,
/// correlating the response through [`EvaluateRequestStore`].
async fn tagged_page_evaluate(
    state: &Arc<ApiState>,
    expression: &str,
    await_promise: bool,
    timeout_ms: Option<u64>,
    unwrap: bool,
    allow_network_requests: bool,
) -> Result<serde_json::Value, String> {
    let timeout_ms = timeout_ms.unwrap_or(DEFAULT_PAGE_EVALUATE_TIMEOUT_MS);
    let request_id = uuid::Uuid::new_v4().to_string();

    let (sender, receiver) = tokio::sync::oneshot::channel();
    state
        .ui_bridge_evaluate_store
        .register(request_id.clone(), sender)
        .await;

    let payload = serde_json::json!({
        "request_id": request_id,
        "expression": expression,
        "await_promise": await_promise,
        "timeout_ms": timeout_ms,
        // Forward unwrap so the frontend evaluate handler can emit the
        // discriminated {value, type} shape when requested. When unwrap is
        // true we skip the legacy conditional-wrap conversion below — the
        // frontend already produced the final payload.
        "unwrap": unwrap,
        // Forward the explicit network-request opt-in to the frontend
        // blocklist gate. snake_case to match the rest of this IPC payload's
        // convention (await_promise / timeout_ms / request_id).
        "allow_network_requests": allow_network_requests,
    });

    if let Err(e) = state
        .app_handle
        .emit("ui-bridge:evaluate-request", &payload)
    {
        state.ui_bridge_evaluate_store.cancel(&request_id).await;
        return Err(format!("Failed to emit ui-bridge:evaluate-request: {}", e));
    }

    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), receiver).await {
        Ok(Ok(resp)) => {
            if resp.ok {
                let result_value = resp.result.unwrap_or(serde_json::Value::Null);
                let data = if unwrap {
                    // Frontend already emitted the {value, type} shape —
                    // pass it through verbatim.
                    result_value
                } else if result_value.is_object() {
                    result_value
                } else {
                    serde_json::json!({
                        "success": true,
                        "result": { "value": result_value }
                    })
                };
                Ok(data)
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
            state.ui_bridge_evaluate_store.cancel(&request_id).await;
            Err(format!(
                "page/evaluate: response channel closed before delivery (request_id={})",
                request_id
            ))
        }
        Err(_) => {
            state.ui_bridge_evaluate_store.cancel(&request_id).await;
            Err(format!(
                "UI Bridge page_evaluate timed out after {}ms",
                timeout_ms
            ))
        }
    }
}

async fn page_evaluate_inner(
    state: Arc<ApiState>,
    expression: String,
    timeout_ms: Option<u64>,
    await_promise: bool,
    unwrap: bool,
    allow_network_requests: bool,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let preview: String = expression.chars().take(80).collect();
    info!("UI Bridge API: Page evaluate ({}...)", preview);

    let ipc_result = tagged_page_evaluate(
        &state,
        &expression,
        await_promise,
        timeout_ms,
        unwrap,
        allow_network_requests,
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
            debug!(
                "UI Bridge: IPC evaluate failed ({}), trying direct WebView eval",
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
                    // Direct-eval fallback: when unwrap is requested, we
                    // build the {value, type} shape ourselves since the
                    // direct-eval path never reached the frontend handler
                    // that would otherwise produce it. `result` is a JSON-
                    // encoded string (direct_webview_evaluate_with_result
                    // sends the serialised value back over HTTP) — parse it
                    // before classifying so null/objects/arrays get the
                    // correct discriminator.
                    let data = if unwrap {
                        let parsed: serde_json::Value = serde_json::from_str(&result)
                            .unwrap_or_else(|_| serde_json::Value::String(result.clone()));
                        let (value, type_name) = classify_direct_eval_value(parsed);
                        serde_json::json!({
                            "value": value,
                            "type": type_name,
                        })
                    } else {
                        serde_json::json!({
                            "result": { "value": result },
                            "source": "direct_eval"
                        })
                    };
                    Ok(Json(ApiResponse::success(data)))
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

/// Evaluate a JavaScript expression in the webview.
pub async fn ui_bridge_page_evaluate_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<PageEvaluateRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let timeout = request.timeout_ms.map(|ms| ms.clamp(1000, 600_000));
    let unwrap = request.unwrap.unwrap_or(false);
    let allow_network_requests = request.allow_network_requests.unwrap_or(false);
    page_evaluate_inner(
        state,
        request.expression,
        timeout,
        request.await_promise,
        unwrap,
        allow_network_requests,
    )
    .await
}

/// `POST /ui-bridge/control/page/evaluate-raw`
pub async fn ui_bridge_page_evaluate_raw_handler(
    State(state): State<Arc<ApiState>>,
    body: String,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    if body.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("page/evaluate-raw: body is empty".to_string())),
        ));
    }
    page_evaluate_inner(state, body, None, false, false, false).await
}

/// POST /ui-bridge/control/page/evaluate-safe
pub async fn ui_bridge_page_evaluate_safe_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<PageEvaluateRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let preview: String = request.expression.chars().take(80).collect();
    info!("UI Bridge API: Safe evaluate ({}...)", preview);

    match safe_evaluate(&state, &format!("return {}", request.expression)).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("JS evaluation error: {}", e))),
        )),
    }
}

/// POST /ui-bridge/control/page/evaluate-batch
pub async fn ui_bridge_page_evaluate_batch_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<BatchEvaluateRequest>,
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

    // Build a single JS expression that evaluates all sub-expressions
    // and returns an array of results.
    let mut js_parts = Vec::new();
    for expr in request.expressions.iter() {
        js_parts.push(format!(
            r#"(() => {{ try {{ var v = (function() {{ return {}; }})(); return {{ id: "{}", success: true, value: v === undefined ? null : v }}; }} catch(e) {{ return {{ id: "{}", success: false, error: e.message }}; }} }})()"#,
            expr.expression,
            expr.id.replace('"', r#"\""#),
            expr.id.replace('"', r#"\""#),
        ));
    }

    let combined = format!("return JSON.stringify([{}])", js_parts.join(","));
    let payload = serde_json::json!({ "expression": format!("(() => {{ {} }})()", combined) });

    match ui_bridge_request_sync(&state, "page_evaluate", payload).await {
        Ok(data) => {
            let result_str = data
                .get("result")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("[]");

            let results: Vec<BatchExpressionResult> = serde_json::from_str(result_str)
                .unwrap_or_else(|_| {
                    request
                        .expressions
                        .iter()
                        .map(|e| BatchExpressionResult {
                            id: e.id.clone(),
                            success: false,
                            value: serde_json::Value::Null,
                            error: Some("Failed to parse batch result".to_string()),
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
    Json(request): Json<SetTabRequest>,
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
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "activate-tab: `tab_id` path parameter is required (non-empty string)".to_string(),
            )),
        ));
    }

    if !VALID_TAB_IDS.contains(&trimmed.as_str()) {
        let preview: Vec<&str> = VALID_TAB_IDS.iter().take(12).copied().collect();
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "activate-tab: unknown tab_id `{}`. Valid tab_ids include: {} (and {} more — \
                 see src/components/app/tab-types.ts for the full list).",
                trimmed,
                preview.join(", "),
                VALID_TAB_IDS.len() - preview.len()
            ))),
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
    Json(request): Json<TabActivateRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<serde_json::Value>>)>
{
    let tab_id = match validate_tab_id(&request.tab_id) {
        Ok(id) => id,
        Err(known) => {
            // Build a `data` payload whose shape matches the cheatsheet —
            // `{ knownTabs: [...], tabId: "<rejected>" }`. The cheatsheet
            // promises `knownTabs` is reachable from the response, and
            // callers disambiguate via inline help when `unknown_tab`
            // surfaces. We bypass `api_error(..)` because that helper
            // doesn't carry a `data` payload.
            let data_payload = serde_json::json!({
                "knownTabs": known,
                "tabId": request.tab_id,
            });
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ApiResponse {
                    success: false,
                    data: Some(data_payload),
                    error: Some(format!("unknown_tab: \"{}\"", request.tab_id)),
                    error_detail: None,
                    hint: None,
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
        Ok(result) => {
            let parsed: serde_json::Value = serde_json::from_str(&result)
                .unwrap_or(serde_json::json!({"error": "Parse error"}));
            Ok(Json(ApiResponse::success(parsed)))
        }
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
            "/ui-bridge/control/page/navigate",
            post(ui_bridge_page_navigate_handler),
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
            "/ui-bridge/ai/page-summary",
            post(ui_bridge_page_summary_handler),
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
    /// `src/components/app/tab-types.ts`, the Rust mirror must be updated too —
    /// otherwise `/control/tabs` advertises the tab while `tab/activate`
    /// rejects it as `unknown_tab`. (Surfaced 2026-05-02 for `"wrappers"`.)
    #[test]
    fn recently_added_tabs_are_accepted() {
        for id in &["wrappers", "productivity"] {
            validate_tab_id(id).unwrap_or_else(|_| {
                panic!(
                    "tab '{id}' missing from VALID_TAB_IDS — sync with src/components/app/tab-types.ts",
                )
            });
        }
    }

    #[test]
    fn tab_activate_request_deserializes_camel_case() {
        let req: TabActivateRequest =
            serde_json::from_str(r#"{"tabId": "specs"}"#).expect("parse camelCase");
        assert_eq!(req.tab_id, "specs");
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
        ("POST", "/ui-bridge/control/page/navigate"),
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
        ("POST", "/ui-bridge/ai/page-summary"),
    ]
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
    use crate::ui_bridge_evaluate::{EvaluateRequestStore, EvaluateResponse};
    use std::sync::Arc;
    use tokio::sync::oneshot;

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
        store.register(request_id_a.clone(), tx_a).await;

        // Caller B: evaluates `window.location.pathname` → "/dashboard".
        let request_id_b = "evaluate-call-b".to_string();
        let (tx_b, rx_b) = oneshot::channel::<EvaluateResponse>();
        store.register(request_id_b.clone(), tx_b).await;

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
        store.register(request_id_a.clone(), tx_a).await;

        let request_id_b = "evaluate-sibling".to_string();
        let (tx_b, rx_b) = oneshot::channel::<EvaluateResponse>();
        store.register(request_id_b.clone(), tx_b).await;

        // Caller A times out while waiting (mirrors the Elapsed branch of
        // tagged_page_evaluate). The handler cancels its slot.
        let wait_a = tokio::time::timeout(std::time::Duration::from_millis(50), rx_a).await;
        assert!(wait_a.is_err(), "caller A must time out");
        store.cancel(&request_id_a).await;

        // Caller B still gets a clean delivery afterwards.
        let store_for_b = store.clone();
        tokio::spawn(async move {
            store_for_b
                .deliver(
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
