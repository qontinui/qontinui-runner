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

use super::helpers::{
    direct_webview_evaluate_with_result, evaluate_js_expression, safe_evaluate,
};
use super::request::ui_bridge_request_sync;

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

/// Request for page navigation
#[derive(Debug, Deserialize)]
pub struct PageNavigateRequest {
    url: String,
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
    "settings-account",
    "settings-ai",
    "settings-agentic",
    "settings-self-healing",
    "settings-world-state-verifier",
    "settings-playwright",
    "settings-mobile",
    "settings-cloud-relay",
    "settings-web-integration",
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

    match ui_bridge_request_sync(&state, "page_refresh", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
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
pub async fn ui_bridge_page_navigate_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<PageNavigateRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Page navigate to {}", request.url);

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

    let payload = serde_json::json!({ "url": url });

    match ui_bridge_request_sync(&state, "page_navigate", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Go back in browser history.
pub async fn ui_bridge_page_go_back_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Page go back");

    match ui_bridge_request_sync(&state, "page_go_back", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Go forward in browser history.
pub async fn ui_bridge_page_go_forward_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Page go forward");

    match ui_bridge_request_sync(&state, "page_go_forward", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
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

    match ui_bridge_request_sync(&state, "query_selector", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
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
                let data = if result_value.is_object() {
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
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let preview: String = expression.chars().take(80).collect();
    info!("UI Bridge API: Page evaluate ({}...)", preview);

    let ipc_result = tagged_page_evaluate(&state, &expression, await_promise, timeout_ms).await;
    match ipc_result {
        Ok(data) => {
            if let Some(false) = data.get("success").and_then(|v| v.as_bool()) {
                let error_msg = data
                    .get("error")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "JS evaluation failed".to_string());
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error(format!("JS evaluation error: {}", error_msg))),
                ));
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
                Ok(result) => Ok(Json(ApiResponse::success(serde_json::json!({
                    "result": { "value": result },
                    "source": "direct_eval"
                })))),
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

/// Evaluate a JavaScript expression in the webview.
pub async fn ui_bridge_page_evaluate_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<PageEvaluateRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let timeout = request.timeout_ms.map(|ms| ms.clamp(1000, 600_000));
    page_evaluate_inner(state, request.expression, timeout, request.await_promise).await
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
    page_evaluate_inner(state, body, None, false).await
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
// Route registration
// ============================================================================

/// Page navigation, evaluation, and tab-switching routes (includes /ai/*
/// alias for page-summary).
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::post;
    axum::Router::new()
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
            "/ui-bridge/ai/page-summary",
            post(ui_bridge_page_summary_handler),
        )
}

/// Static (method, path) tuples matching every route registered by `routes()`.
pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
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
        ("POST", "/ui-bridge/ai/page-summary"),
    ]
}
