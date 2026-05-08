//! Miscellaneous handlers: capabilities manifest, keyboard shortcuts,
//! interaction metrics, action history, structured assertions, expect-text,
//! route manifest, ping/pong, IPC response, batch execution, render log,
//! workflow runner, element state.
//!
//! Grouped here because each handler is a thin stand-alone endpoint that
//! doesn't fit any of the bigger thematic families (screenshots, ai,
//! intents). `routes_manifest_handler` still needs the static
//! `route_manifest()` in `mod.rs` so it's re-exported via `pub(super)`.

use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::helpers::compute_snapshot_diff;
use super::ipc_handler_post;
use super::request::{handle_ui_bridge_response, ui_bridge_request_sync};
use super::types::{classify_transport_error, UiBridgeError};

// ============================================================================
// Structured Assert (declarative assertions over DOM)
// ============================================================================

/// Request for a structured UI assertion.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredAssertRequest {
    /// CSS selector or text to search for
    pub query: String,
    /// How to search: "css" (CSS selector), "text" (text content match), "textContains" (substring)
    #[serde(default = "default_query_type")]
    pub query_type: String,
    /// What to assert: "exists", "notExists", "count", "hasText", "hasClass", "isVisible", "hasAttribute"
    pub assertion: String,
    /// Expected value (for count, hasText, hasClass, hasAttribute assertions)
    #[serde(default)]
    pub expected: Option<serde_json::Value>,
    /// Attribute name (for hasAttribute assertion)
    #[serde(default)]
    pub attribute: Option<String>,
}

fn default_query_type() -> String {
    "css".to_string()
}

/// Result of a structured assertion.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssertResult {
    pub passed: bool,
    pub assertion: String,
    pub query: String,
    pub actual: serde_json::Value,
    pub expected: serde_json::Value,
    pub message: String,
}

/// POST /ui-bridge/control/assert
///
/// Evaluate a structured UI assertion without writing JavaScript.
/// Supports CSS selectors and text search with common assertion types.
/// Returns a structured pass/fail result.
pub async fn ui_bridge_structured_assert_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StructuredAssertRequest>,
) -> Result<Json<ApiResponse<AssertResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: Structured assert ({} {} on '{}')",
        request.assertion, request.query_type, request.query
    );

    // Build the JS expression for this assertion
    let query_js = match request.query_type.as_str() {
        "css" => format!(
            "document.querySelectorAll('{}')",
            request.query.replace('\'', "\\'")
        ),
        "text" => format!(
            r#"Array.from(document.querySelectorAll('*')).filter(el => el.childNodes.length <= 3 && el.textContent.trim() === '{}')"#,
            request.query.replace('\'', "\\'")
        ),
        "textContains" => format!(
            r#"Array.from(document.querySelectorAll('*')).filter(el => el.childNodes.length <= 3 && el.textContent.includes('{}'))"#,
            request.query.replace('\'', "\\'")
        ),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!(
                    "Invalid query_type '{}'. Use: css, text, textContains",
                    request.query_type
                ))),
            ));
        }
    };

    let assertion_js = match request.assertion.as_str() {
        "exists" => format!(
            r#"return JSON.stringify({{ found: ({}).length, passed: ({}).length > 0 }})"#,
            query_js, query_js
        ),
        "notExists" => format!(
            r#"return JSON.stringify({{ found: ({}).length, passed: ({}).length === 0 }})"#,
            query_js, query_js
        ),
        "count" => {
            let expected = request
                .expected
                .as_ref()
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            format!(
                r#"return JSON.stringify({{ found: ({}).length, passed: ({}).length === {}, expected: {} }})"#,
                query_js, query_js, expected, expected
            )
        }
        "isVisible" => {
            format!(
                r#"var els = {}; var visible = Array.from(els).filter(function(el) {{ var r = el.getBoundingClientRect(); return r.width > 0 && r.height > 0 && getComputedStyle(el).display !== 'none'; }}); return JSON.stringify({{ found: els.length, visible: visible.length, passed: visible.length > 0 }})"#,
                query_js
            )
        }
        "hasText" => {
            let expected_text = request
                .expected
                .as_ref()
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!(
                r#"(() => {{ var els = {}; var texts = Array.from(els).map(el => el.textContent.trim()); var found = texts.some(t => t.includes('{}')); return JSON.stringify({{ found: els.length, texts: texts.slice(0, 3), passed: found }}); }})()"#,
                query_js,
                expected_text.replace('\'', "\\'")
            )
        }
        "hasClass" => {
            let expected_class = request
                .expected
                .as_ref()
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!(
                r#"(() => {{ var els = {}; var found = Array.from(els).some(el => el.classList.contains('{}')); var classes = Array.from(els).slice(0, 3).map(el => Array.from(el.classList).join(' ')); return JSON.stringify({{ found: els.length, classes: classes, passed: found }}); }})()"#,
                query_js,
                expected_class.replace('\'', "\\'")
            )
        }
        "hasAttribute" => {
            let attr = request.attribute.as_deref().unwrap_or("title");
            let expected_val = request.expected.as_ref().and_then(|v| v.as_str());
            let check = if let Some(val) = expected_val {
                format!(
                    "el.getAttribute('{}') === '{}'",
                    attr.replace('\'', "\\'"),
                    val.replace('\'', "\\'")
                )
            } else {
                format!("el.hasAttribute('{}')", attr.replace('\'', "\\'"))
            };
            format!(
                r#"(() => {{ var els = {}; var found = Array.from(els).some(el => {}); return JSON.stringify({{ found: els.length, passed: found }}); }})()"#,
                query_js, check
            )
        }
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!(
                    "Invalid assertion '{}'. Use: exists, notExists, count, isVisible, hasText, hasClass, hasAttribute",
                    request.assertion
                ))),
            ));
        }
    };

    // Use the safe evaluate path
    let full_expr = format!("(() => {{ {} }})()", assertion_js);
    let payload = serde_json::json!({ "expression": full_expr });

    match ui_bridge_request_sync(&state, "page_evaluate", payload).await {
        Ok(data) => {
            let result_str = data
                .get("result")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("{}");

            let parsed: serde_json::Value =
                serde_json::from_str(result_str).unwrap_or(serde_json::json!({"passed": false}));

            let passed = parsed
                .get("passed")
                .and_then(|p| p.as_bool())
                .unwrap_or(false);

            let result = AssertResult {
                passed,
                assertion: request.assertion.clone(),
                query: request.query.clone(),
                actual: parsed.clone(),
                expected: request.expected.clone().unwrap_or(serde_json::Value::Null),
                message: if passed {
                    format!(
                        "Assertion '{}' passed for '{}'",
                        request.assertion, request.query
                    )
                } else {
                    format!(
                        "Assertion '{}' failed for '{}': {:?}",
                        request.assertion, request.query, parsed
                    )
                },
            };

            Ok(Json(ApiResponse::success(result)))
        }
        Err(e) => {
            let result = AssertResult {
                passed: false,
                assertion: request.assertion,
                query: request.query,
                actual: serde_json::Value::Null,
                expected: request.expected.unwrap_or(serde_json::Value::Null),
                message: format!("Evaluation failed: {}", e),
            };
            Ok(Json(ApiResponse::success(result)))
        }
    }
}

// ============================================================================
// Keyboard Shortcuts
// ============================================================================

/// Get discovered keyboard shortcuts.
pub async fn ui_bridge_get_keyboard_shortcuts_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting keyboard shortcuts");

    match ui_bridge_request_sync(&state, "get_keyboard_shortcuts", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Expect text
// ============================================================================

/// POST /ui-bridge/ai/expect
///
/// Wait for a substring to appear anywhere in the page text.
pub async fn ui_bridge_expect_text_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let body = body
        .map(|Json(v)| v)
        .unwrap_or_else(|| serde_json::json!({}));

    let text = match body.get("text").and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error("expect: 'text' is required")),
            ));
        }
    };
    let timeout_ms = body.get("timeout").and_then(|v| v.as_u64()).unwrap_or(5000);
    let poll_interval_ms = body
        .get("pollIntervalMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(250)
        .max(50);
    let case_insensitive = body
        .get("caseInsensitive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Truncate on a char boundary — &text[..60] panics on multi-byte UTF-8.
    let text_preview: String = text.chars().take(60).collect();

    info!(
        "UI Bridge API: expect text=\"{}\" timeout={}ms ci={}",
        text_preview, timeout_ms, case_insensitive
    );

    // Use serde_json::to_string to produce a fully RFC-compliant JS string
    // literal.
    let js_literal = serde_json::to_string(&text).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to encode search text: {}", e))),
        )
    })?;
    let js = if case_insensitive {
        format!(
            "document.body.innerText.toLowerCase().includes({}.toLowerCase())",
            js_literal
        )
    } else {
        format!("document.body.innerText.includes({})", js_literal)
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let start = std::time::Instant::now();
    let mut polls: u32 = 0;
    let mut consecutive_errors: u32 = 0;
    let mut last_error: Option<String> = None;

    loop {
        polls += 1;

        let payload = serde_json::json!({ "expression": js });
        match ui_bridge_request_sync(&state, "page_evaluate", payload).await {
            Ok(data) => {
                consecutive_errors = 0;
                last_error = None;
                let raw = data
                    .get("result")
                    .and_then(|r| r.get("value"))
                    .or_else(|| data.get("value"));
                let value = raw
                    .and_then(|v| v.as_bool())
                    .or_else(|| raw.and_then(|v| v.as_str()).map(|s| s == "true"))
                    .unwrap_or(false);
                if value {
                    let elapsed_ms = start.elapsed().as_millis() as u64;
                    return Ok(Json(ApiResponse::success(serde_json::json!({
                        "found": true,
                        "elapsed_ms": elapsed_ms,
                        "polls": polls,
                    }))));
                }
            }
            Err(e) => {
                consecutive_errors += 1;
                debug!("expect: poll {} transport error: {}", polls, e);
                last_error = Some(e);
                if consecutive_errors >= 3 {
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!(
                            "expect: {} consecutive transport errors, giving up. Last error: {}",
                            consecutive_errors,
                            last_error.as_deref().unwrap_or("unknown")
                        ))),
                    ));
                }
            }
        }

        let now = std::time::Instant::now();
        if now >= deadline {
            let elapsed_ms = start.elapsed().as_millis() as u64;
            let msg = format!(
                "expect: timeout after {}ms ({} polls) waiting for text",
                elapsed_ms, polls
            );
            info!("UI Bridge API: {}", msg);
            return Err((
                StatusCode::REQUEST_TIMEOUT,
                Json(api_error(format!("{} \"{}\"", msg, text_preview))),
            ));
        }

        let poll_sleep = std::time::Duration::from_millis(poll_interval_ms);
        let remaining = deadline.saturating_duration_since(now);
        tokio::time::sleep(poll_sleep.min(remaining)).await;
    }
}

// ============================================================================
// Tier 3.2 — Mixed action/wait/snapshot batch execution
// ============================================================================

/// POST /ui-bridge/control/batch-execute
pub async fn ui_bridge_control_batch_execute_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let actions = request
        .get("actions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let stop_on_error = request
        .get("stop_on_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let total = actions.len();

    info!(
        "UI Bridge API: control/batch-execute {} steps, stop_on_error={}",
        total, stop_on_error
    );

    let mut results: Vec<serde_json::Value> = Vec::with_capacity(total);
    let mut completed: usize = 0;

    for (i, step) in actions.iter().enumerate() {
        let step_type = step.get("type").and_then(|v| v.as_str()).unwrap_or("");

        let step_result: serde_json::Value = match step_type {
            "wait" => {
                let ms = step.get("ms").and_then(|v| v.as_u64()).unwrap_or(0);
                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                serde_json::json!({ "index": i, "success": true, "data": { "waited_ms": ms } })
            }

            "snapshot" => {
                match ui_bridge_request_sync(&state, "get_snapshot", serde_json::json!({})).await {
                    Ok(snap) => {
                        serde_json::json!({ "index": i, "success": true, "data": snap })
                    }
                    Err(e) => {
                        serde_json::json!({ "index": i, "success": false, "error": e })
                    }
                }
            }

            "action" => {
                let element_id = step
                    .get("element_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let action = step
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("click");
                let params = step.get("params").cloned();

                let payload = serde_json::json!({
                    "id": element_id,
                    "action": action,
                    "params": params,
                });

                match ui_bridge_request_sync(&state, "execute_action", payload).await {
                    Ok(data) => {
                        let ok = data
                            .get("success")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true);
                        serde_json::json!({ "index": i, "success": ok, "data": data })
                    }
                    Err(e) => {
                        serde_json::json!({ "index": i, "success": false, "error": e })
                    }
                }
            }

            unknown => {
                serde_json::json!({
                    "index": i,
                    "success": false,
                    "error": format!("Unknown step type: {}", unknown)
                })
            }
        };

        let step_ok = step_result
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        results.push(step_result);
        completed += 1;

        if !step_ok && stop_on_error {
            break;
        }
    }

    Ok(Json(ApiResponse::success(serde_json::json!({
        "results": results,
        "completed": completed,
        "total": total,
    }))))
}

// ============================================================================
// Capabilities manifest
// ============================================================================

/// Capabilities endpoint — static listing of all supported features.
pub async fn ui_bridge_capabilities_handler() -> Json<ApiResponse<serde_json::Value>> {
    Json(ApiResponse::success(serde_json::json!({
        "version": "0.3.1",
        "appId": "qontinui-runner",
        "appType": "desktop",
        "framework": "tauri",
        "categories": {
            "control": {
                "description": "Element discovery, actions, navigation, forms",
                "endpoints": ["snapshot", "elements", "element/:id", "element/:id/state", "element/:id/action",
                    "discover", "find", "components", "component/:id/state", "navigate-and-wait",
                    "page/navigate", "page/refresh",
                    "page/back", "page/forward", "page/evaluate", "forms", "fill", "forms/snapshot", "forms/diff",
                    "workflows", "specs", "query-selector", "keyboard-shortcuts", "batch"],
                "batch": {
                    "method": "POST",
                    "path": "/ui-bridge/control/batch",
                    "description": "Execute a sequence of element actions with snapshot diff",
                    "maxBatchSize": 50,
                    "body": {
                        "steps": "[{ elementId, action, params? }]",
                        "stopOnError": "boolean (default: true)"
                    }
                },
                "navigateAndWait": {
                    "method": "POST",
                    "path": "/ui-bridge/control/navigate-and-wait",
                    "description": "Click an element, wait for the DOM to stabilize, return a fresh snapshot. Replaces the click → sleep → discover → snapshot pattern.",
                    "body": {
                        "elementId": "string (required)",
                        "action": "string (default: 'click')",
                        "params": "object (optional, passed to the action)",
                        "waitForStableMs": "number (default: 800, how long element count must be unchanged)",
                        "timeoutMs": "number (default: 8000, overall deadline)"
                    },
                    "response": "Same shape as GET /control/snapshot. On timeout, includes navigateAndWait.timedOut: true."
                }
            },
            "ai": {
                "description": "AI-powered search, assertions, semantic snapshots, NL actions",
                "endpoints": ["find", "search", "execute", "assert", "assert-batch", "snapshot", "summary",
                    "semantic-search", "diff", "execute-with-diff", "bookmarks", "scoped-diff",
                    "summarize-diff", "categorize-last-diff"]
            },
            "media": {
                "description": "Media element discovery, audits, and analysis",
                "endpoints": ["media/find", "media/audit/accessibility", "media/audit/performance",
                    "media/snapshot", "media/analyze", "media/analyze/batch", "media/analyze/page", "media/compare",
                    "image-diff"]
            },
            "stateMachine": {
                "description": "State discovery, activation, transitions, navigation",
                "endpoints": ["states", "states/active", "states/snapshot", "states/find-path",
                    "states/navigate", "state/:id", "state/:id/activate", "state/:id/deactivate",
                    "state-groups", "transitions", "transition/:id/can-execute", "transition/:id/execute"]
            },
            "navigation": {
                "description": "Deterministic navigation-complete signal with idle fallback",
                "endpoints": ["wait-for-navigation"]
            },
            "idle": {
                "description": "Composite and per-signal idle detection",
                "endpoints": ["idle-status", "idle-status/:signal", "wait-for-idle",
                    "wait-for-idle/:signal", "wait-for-targets"]
            },
            "design": {
                "description": "Design inspection, styles, audit, responsive, evaluation",
                "endpoints": ["design/element/{id}/styles", "design/snapshot", "design/audit", "design/responsive",
                    "design/evaluate", "design/evaluate/baseline", "design/evaluate/contexts", "design/evaluate/diff"]
            },
            "network": {
                "description": "Network request monitoring",
                "endpoints": ["network-requests", "network-requests/in-flight", "network-request/:id",
                    "network-requests/wait"]
            },
            "errorTracking": {
                "description": "Error sessions, baselines, reports, console error grouping",
                "endpoints": ["error-sessions/start", "error-sessions", "error-sessions/end",
                    "error-baselines/capture", "error-baselines/compare", "error-report", "error-snapshots",
                    "console-errors", "console-errors/clear"],
                "consoleErrors": {
                    "queryParams": {
                        "since": "number (epoch ms)",
                        "limit": "number (default 50)",
                        "group": "boolean (default false) — return grouped errors",
                        "groupBy": "'fingerprint' | 'message' | 'source' (default 'fingerprint')"
                    }
                }
            },
            "clipboard": {
                "description": "System clipboard read/write (OS-level via arboard)",
                "available": true
            },
            "jsEvaluation": {
                "description": "Arbitrary JavaScript evaluation in webview",
                "available": true
            },
            "annotations": {
                "description": "Element annotation CRUD with coverage tracking",
                "endpoints": ["annotations", "annotation/{id}", "annotations/coverage", "annotations/export", "annotations/import"]
            },
            "intents": {
                "description": "Intent registration, discovery, and execution",
                "endpoints": ["intents", "intents/find", "intents/execute", "intents/execute-from-query"]
            },
            "history": {
                "description": "Action history, interaction metrics, performance entries",
                "endpoints": ["action-history", "metrics", "performance-entries"]
            },
            "timeline": {
                "description": "Performance timeline, browser events, error snapshots",
                "endpoints": ["timeline", "browser-events", "error-snapshots"]
            },
            "debug": {
                "description": "Debug tools for element inspection and highlighting",
                "endpoints": ["element-tree", "highlight/{id}"]
            },
            "analysis": {
                "description": "AI-powered analysis: data extraction, regions, structured data, cross-app",
                "endpoints": ["analyze/data", "analyze/regions", "analyze/structured-data",
                    "analyze/cross-app-compare", "recovery/attempt"]
            },
            "batch": {
                "description": "Execute multiple UI Bridge operations in a single HTTP call",
                "endpoints": ["batch"],
                "maxBatchSize": 50,
                "parameters": {
                    "operations": "Array of { id, operation, params } objects",
                    "stopOnError": "boolean (default: false) — stop on first failure"
                }
            },
            "withDiff": {
                "description": "Execute action(s) with atomic change-buffer tracking (enable → act → drain → disable)",
                "endpoints": ["control/with-diff"],
                "parameters": {
                    "single": "{ operation, elementId, params } — returns { result, changes, changeCount }",
                    "batch": "{ operations: [{ operation, elementId, params }] } — returns { results, changes, changeCount }"
                }
            },
            "diagnostics": {
                "description": "WebView readiness diagnostics and proactive health checks",
                "endpoints": ["diagnostics", "diagnostics/readiness"],
                "readiness": {
                    "method": "GET",
                    "path": "/ui-bridge/diagnostics/readiness",
                    "description": "Returns 200 when frontend is ready, 503 with diagnostic details when not ready",
                    "response": "{ ready, sdk_connected, last_pong_age_ms, uptime_ms }"
                }
            },
            "stableRefs": {
                "description": "Stable element references that survive React re-renders via fingerprint + semantic path fallback",
                "includeInDiscover": true,
                "resolutionStrategies": ["primaryId", "data-ui-bridge-id", "fingerprint", "semanticPath"]
            }
        }
    })))
}

// ============================================================================
// Action history / interaction metrics
// ============================================================================

/// Get action history.
pub async fn ui_bridge_get_action_history_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get action history");
    match ui_bridge_request_sync(&state, "get_action_history", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Get action history failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get interaction metrics.
pub async fn ui_bridge_get_interaction_metrics_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get interaction metrics");
    match ui_bridge_request_sync(&state, "get_interaction_metrics", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Get interaction metrics failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Workflow run/status + element state
// ============================================================================

/// POST /ui-bridge/control/workflow/:id/run — Run a workflow via the unified workflow engine.
/// Proxies to the runner's existing `/unified-workflows/:id/run` endpoint via internal HTTP.
pub async fn ui_bridge_run_workflow_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Running workflow {}", id);

    let base_url = crate::mcp::types::get_self_base_url(&state.app_state);
    let url = format!("{}/unified-workflows/{}/run", base_url, id);

    let request_body = body
        .map(|Json(v)| v)
        .unwrap_or_else(|| serde_json::json!({ "force_fresh_start": false }));

    let client = reqwest::Client::new();
    match client.post(&url).json(&request_body).send().await {
        Ok(resp) => {
            let status = resp.status();
            match resp.json::<serde_json::Value>().await {
                Ok(result) => {
                    if status.is_success() {
                        Ok(Json(ApiResponse::success(result)))
                    } else {
                        let err_msg = result
                            .get("error")
                            .and_then(|e| e.as_str())
                            .unwrap_or("Workflow execution failed")
                            .to_string();
                        Err((
                            StatusCode::from_u16(status.as_u16())
                                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                            Json(api_error(err_msg)),
                        ))
                    }
                }
                Err(e) => {
                    error!("UI Bridge API: Failed to parse run response: {}", e);
                    Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!("Failed to parse response: {}", e))),
                    ))
                }
            }
        }
        Err(e) => {
            error!("UI Bridge API: Failed to call unified workflow run: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to run workflow: {}", e))),
            ))
        }
    }
}

/// GET /ui-bridge/control/workflow/:run_id/status
pub async fn ui_bridge_get_workflow_status_handler(
    State(state): State<Arc<ApiState>>,
    Path(run_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting workflow status for run {}", run_id);

    let task_run_result = state.app_state.pg_db.get_task_run(&run_id).await;
    match task_run_result {
        Ok(Some(task_run)) => {
            let status = match task_run.status.as_str() {
                "running" | "in_progress" => "running",
                "complete" | "completed" | "success" => "completed",
                "failed" | "error" => "failed",
                "stopped" | "cancelled" => "cancelled",
                _ => "pending",
            };
            let is_terminal = matches!(status, "completed" | "failed" | "cancelled");
            Ok(Json(ApiResponse::success(serde_json::json!({
                "workflowId": task_run.workflow_id.unwrap_or_default(),
                "runId": run_id,
                "status": status,
                "steps": [],
                "totalSteps": 0,
                "success": if is_terminal { Some(status == "completed") } else { None::<bool> },
                "taskName": task_run.task_name,
                "sessionsCount": task_run.sessions_count,
                "startedAt": task_run.created_at,
                "completedAt": task_run.completed_at,
                "error": task_run.error_message,
            }))))
        }
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Task run not found: {}", run_id))),
        )),
        Err(e) => {
            error!("UI Bridge API: Failed to get task run: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get task run: {}", e))),
            ))
        }
    }
}

/// Get all workflows.
pub async fn ui_bridge_get_workflows_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting workflows");

    match ui_bridge_request_sync(&state, "get_workflows", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get element state by ID.
pub async fn ui_bridge_get_element_state_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting element state for {}", id);

    match ui_bridge_request_sync(
        &state,
        "get_element_state",
        serde_json::json!({ "elementId": id }),
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
// Render log
// ============================================================================

/// Get render log entries.
pub async fn ui_bridge_get_render_log_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting render log");

    let log = state.ui_bridge_render_log.lock().await;
    Ok(Json(ApiResponse::success(serde_json::json!({
        "entries": *log,
        "count": log.len()
    }))))
}

/// Append an entry to the render log.
pub async fn ui_bridge_append_render_log_handler(
    State(state): State<Arc<ApiState>>,
    Json(entry): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mut log = state.ui_bridge_render_log.lock().await;
    // Keep render log bounded to prevent memory leaks
    if log.len() >= 1000 {
        log.drain(0..100);
    }
    log.push(entry);
    Ok(Json(ApiResponse::success(serde_json::json!({
        "count": log.len()
    }))))
}

// ============================================================================
// Pong / IPC response
// ============================================================================

/// Handle UI Bridge pong from frontend.
pub async fn ui_bridge_pong_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    state
        .ui_bridge_last_pong
        .store(now, std::sync::atomic::Ordering::Relaxed);
    // Unblock any requests waiting for frontend readiness
    state.ui_bridge_ready.notify_waiters();
    Ok(Json(ApiResponse::success(
        serde_json::json!({ "pong": true }),
    )))
}

// SDK relay liveness ping. Distinct from `/ui-bridge/pong` (which is the
// runner's response to its own ping). The relay-side SDK contract returns
// `{ received: true }`; the React-side `receive_heartbeat` IPC handler is not
// yet implemented — until then, callers will get the standard
// "no IPC handler" error from `ui_bridge_request_sync`.
ipc_handler_post!(ui_bridge_heartbeat_handler, "receive_heartbeat");

/// Accept an IPC response via HTTP (fallback when Tauri event system is unavailable).
pub async fn ui_bridge_ipc_response_handler(
    State(state): State<Arc<ApiState>>,
    Json(response): Json<serde_json::Value>,
) -> Json<ApiResponse<serde_json::Value>> {
    let pending = state.ui_bridge_pending.clone();
    let pending_count = state.ui_bridge_pending_count.clone();
    handle_ui_bridge_response(pending, pending_count, response).await;
    // Also update pong timestamp since this proves the frontend is alive
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    state
        .ui_bridge_last_pong
        .store(now, std::sync::atomic::Ordering::Relaxed);
    Json(ApiResponse::success(
        serde_json::json!({ "received": true }),
    ))
}

// ============================================================================
// Batch execution
// ============================================================================

/// Request to execute multiple UI Bridge operations in a single HTTP call.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchRequest {
    pub operations: Vec<BatchOperation>,
    /// If true, stop executing on first error. Default: false (execute all).
    #[serde(default)]
    pub stop_on_error: bool,
}

/// A single operation within a batch request.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchOperation {
    /// Unique ID for this operation within the batch (for result correlation).
    pub id: String,
    /// The operation type (e.g., "get_elements", "execute_action", "discover").
    pub operation: String,
    /// Operation-specific parameters (merged into the IPC payload).
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Response from a batch execution.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchResponse {
    /// True only if all operations succeeded.
    pub success: bool,
    /// Per-operation results in request order.
    pub results: Vec<BatchOperationResult>,
    /// Total wall-clock time for the entire batch.
    pub total_duration_ms: u64,
}

/// Result of a single operation within a batch.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchOperationResult {
    pub id: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_detail: Option<UiBridgeError>,
    pub duration_ms: u64,
}

/// Execute multiple UI Bridge operations in a single HTTP call.
pub async fn ui_bridge_batch_handler(
    State(state): State<Arc<ApiState>>,
    Json(batch): Json<BatchRequest>,
) -> Result<Json<ApiResponse<BatchResponse>>, (StatusCode, Json<ApiResponse<serde_json::Value>>)> {
    const MAX_BATCH_SIZE: usize = 50;

    if batch.operations.len() > MAX_BATCH_SIZE {
        let error_body = serde_json::json!({
            "error": "batch_size_exceeded",
            "max": MAX_BATCH_SIZE,
            "received": batch.operations.len()
        });
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiResponse {
                success: false,
                data: Some(error_body),
                error: Some(format!(
                    "Batch size {} exceeds maximum of {}",
                    batch.operations.len(),
                    MAX_BATCH_SIZE
                )),
                error_detail: None,
                hint: None,
            }),
        ));
    }

    if batch.operations.is_empty() {
        return Ok(Json(ApiResponse::success(BatchResponse {
            success: true,
            results: vec![],
            total_duration_ms: 0,
        })));
    }

    info!(
        "UI Bridge API: Batch executing {} operations (stop_on_error={})",
        batch.operations.len(),
        batch.stop_on_error
    );

    let start = Instant::now();
    let mut results = Vec::with_capacity(batch.operations.len());

    for op in &batch.operations {
        let op_start = Instant::now();
        let result = ui_bridge_request_sync(&state, &op.operation, op.params.clone()).await;

        let (success, data, error, error_detail) = match result {
            Ok(data) => {
                if data.get("success").and_then(|v| v.as_bool()) == Some(false) {
                    let error_msg = data
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Operation failed")
                        .to_string();
                    let detail = classify_transport_error(&error_msg);
                    (false, Some(data), Some(error_msg), Some(detail))
                } else {
                    (true, Some(data), None, None)
                }
            }
            Err(e) => {
                let detail = classify_transport_error(&e);
                (false, None, Some(e), Some(detail))
            }
        };

        let op_duration = op_start.elapsed().as_millis() as u64;

        results.push(BatchOperationResult {
            id: op.id.clone(),
            success,
            data,
            error,
            error_detail,
            duration_ms: op_duration,
        });

        if !success && batch.stop_on_error {
            info!(
                "UI Bridge batch: stopping on error at operation '{}' ({}ms)",
                op.id, op_duration
            );
            break;
        }
    }

    let total_duration = start.elapsed().as_millis() as u64;
    let all_success = results.iter().all(|r| r.success);

    info!(
        "UI Bridge batch: {}/{} succeeded in {}ms",
        results.iter().filter(|r| r.success).count(),
        results.len(),
        total_duration
    );

    let response = BatchResponse {
        success: all_success,
        results,
        total_duration_ms: total_duration,
    };

    Ok(Json(ApiResponse::success(response)))
}

// ============================================================================
// /control/batch (step-level, distinct from /batch which is operation-level)
// ============================================================================

/// POST /ui-bridge/control/batch
///
/// Execute a sequence of element actions and report per-step timing plus a
/// snapshot diff (element ids added / removed) between pre- and post-batch.
pub async fn ui_bridge_control_batch_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    const MAX_BATCH_SIZE: usize = 50;

    let steps = request
        .get("steps")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if steps.len() > MAX_BATCH_SIZE {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiResponse {
                success: false,
                data: None,
                error: Some(
                    serde_json::to_string(&serde_json::json!({
                        "error": "batch_size_exceeded",
                        "max": MAX_BATCH_SIZE,
                        "received": steps.len()
                    }))
                    .unwrap_or_default(),
                ),
                error_detail: None,
                hint: None,
            }),
        ));
    }

    let stop_on_error = request
        .get("stopOnError")
        .or_else(|| request.get("stopOnFailure"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    info!(
        "UI Bridge API: control batch ({} steps, stopOnError={})",
        steps.len(),
        stop_on_error
    );

    // Pre-snapshot for diffing. Best-effort — if it fails we report a null
    // diff but still execute the batch.
    let pre_snapshot = ui_bridge_request_sync(&state, "get_snapshot", serde_json::json!({}))
        .await
        .ok();

    let total_start = std::time::Instant::now();
    let mut results: Vec<serde_json::Value> = Vec::with_capacity(steps.len());
    let mut all_succeeded = true;
    let mut stopped_early = false;

    for (i, step) in steps.iter().enumerate() {
        let element_id = step.get("elementId").and_then(|v| v.as_str()).unwrap_or("");
        let action = step
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("click");
        let params = step.get("params").cloned();

        let payload = serde_json::json!({
            "elementId": element_id,
            "action": {
                "action": action,
                "params": params,
            },
        });

        let step_start = std::time::Instant::now();
        let res = ui_bridge_request_sync(&state, "execute_action", payload).await;
        let duration_ms = step_start.elapsed().as_secs_f64() * 1000.0;

        let (ok, response_value) = match res {
            Ok(data) => {
                let success = data
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                (success, data)
            }
            Err(e) => (false, serde_json::json!({"success": false, "error": e})),
        };

        results.push(serde_json::json!({
            "step": i,
            "success": ok,
            "durationMs": duration_ms,
            "elementId": element_id,
            "action": action,
            "response": response_value,
        }));

        if !ok {
            all_succeeded = false;
            if stop_on_error {
                stopped_early = true;
                break;
            }
        }
    }

    let total_ms = total_start.elapsed().as_secs_f64() * 1000.0;

    // Post-snapshot for diffing.
    let post_snapshot = ui_bridge_request_sync(&state, "get_snapshot", serde_json::json!({}))
        .await
        .ok();
    let snapshot_diff = compute_snapshot_diff(pre_snapshot.as_ref(), post_snapshot.as_ref());

    let payload = serde_json::json!({
        "results": results,
        "totalMs": total_ms,
        "snapshotDiff": snapshot_diff,
        "stoppedEarly": stopped_early,
    });

    if all_succeeded {
        Ok(Json(ApiResponse::success(payload)))
    } else {
        Ok(Json(ApiResponse {
            success: false,
            data: Some(payload),
            error: Some("One or more batch steps failed".to_string()),
            error_detail: None,
            hint: None,
        }))
    }
}

// ============================================================================
// Routes manifest
// ============================================================================

/// GET /ui-bridge/_routes
///
/// Returns the list of registered UI Bridge routes (method + path) sorted by
/// path. Used by SDKs and tests to discover what the runner exposes without
/// having to read the source.
pub async fn ui_bridge_routes_manifest_handler(
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mut entries: Vec<(String, String)> = super::route_manifest()
        .iter()
        .map(|(m, p)| (m.to_string(), p.to_string()))
        .collect();
    // Sort by path first, then method, so paths with multiple methods stay
    // adjacent in the output.
    entries.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));

    let routes_json: Vec<serde_json::Value> = entries
        .into_iter()
        .map(|(method, path)| serde_json::json!({"method": method, "path": path}))
        .collect();

    Ok(Json(ApiResponse::success(serde_json::json!({
        "routes": routes_json,
        "count": routes_json.len(),
    }))))
}

// ============================================================================
// Routes + manifest
// ============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use crate::mcp::sdk_client::{
        handle_clear_render_log, handle_render_log_path, handle_render_log_snapshot,
    };
    use axum::routing::{delete, get, post};
    axum::Router::new()
        // Endpoint discovery. `_help` is a cross-platform alias that works on
        // both runner and mobile UI Bridge.
        .route("/ui-bridge/_routes", get(ui_bridge_routes_manifest_handler))
        .route("/ui-bridge/_help", get(ui_bridge_routes_manifest_handler))
        // Structured assert
        .route(
            "/ui-bridge/control/assert",
            post(ui_bridge_structured_assert_handler),
        )
        // Keyboard shortcuts
        .route(
            "/ui-bridge/control/keyboard-shortcuts",
            get(ui_bridge_get_keyboard_shortcuts_handler),
        )
        // Expect text
        .route("/ui-bridge/ai/expect", post(ui_bridge_expect_text_handler))
        // Mixed action/wait/snapshot batch
        .route(
            "/ui-bridge/control/batch-execute",
            post(ui_bridge_control_batch_execute_handler),
        )
        // Capabilities
        .route(
            "/ui-bridge/capabilities",
            get(ui_bridge_capabilities_handler),
        )
        // Action history / metrics
        .route(
            "/ui-bridge/control/action-history",
            get(ui_bridge_get_action_history_handler),
        )
        .route(
            "/ui-bridge/control/metrics",
            get(ui_bridge_get_interaction_metrics_handler),
        )
        // SDK aliases — `/control/history` and `/control/interaction-metrics`
        // resolve to the same getActionHistory / getMetrics handlers as the
        // canonical paths above. `/debug/*` mirrors round out the cross-app
        // debug namespace declared by UI_BRIDGE_ROUTES.
        .route(
            "/ui-bridge/control/history",
            get(ui_bridge_get_action_history_handler),
        )
        .route(
            "/ui-bridge/control/interaction-metrics",
            get(ui_bridge_get_interaction_metrics_handler),
        )
        .route(
            "/ui-bridge/debug/action-history",
            get(ui_bridge_get_action_history_handler),
        )
        .route(
            "/ui-bridge/debug/metrics",
            get(ui_bridge_get_interaction_metrics_handler),
        )
        // Workflows
        .route(
            "/ui-bridge/control/workflows",
            get(ui_bridge_get_workflows_handler),
        )
        .route(
            "/ui-bridge/control/workflow/{id}/run",
            post(ui_bridge_run_workflow_handler),
        )
        .route(
            "/ui-bridge/control/workflow/{run_id}/status",
            get(ui_bridge_get_workflow_status_handler),
        )
        .route(
            "/ui-bridge/control/element/{id}/state",
            get(ui_bridge_get_element_state_handler),
        )
        // Render log (control + cross-app alias)
        .route(
            "/ui-bridge/control/render-log",
            get(ui_bridge_get_render_log_handler).post(ui_bridge_append_render_log_handler),
        )
        .route(
            "/ui-bridge/render-log",
            get(ui_bridge_get_render_log_handler)
                .post(ui_bridge_append_render_log_handler)
                .delete(handle_clear_render_log),
        )
        .route(
            "/ui-bridge/render-log/snapshot",
            post(handle_render_log_snapshot),
        )
        .route("/ui-bridge/render-log/path", get(handle_render_log_path))
        // SDK relay liveness ping (distinct from `/ui-bridge/pong`)
        .route("/ui-bridge/heartbeat", post(ui_bridge_heartbeat_handler))
        // Pong + IPC response
        .route("/ui-bridge/pong", post(ui_bridge_pong_handler))
        .route(
            "/ui-bridge/ipc-response",
            post(ui_bridge_ipc_response_handler),
        )
        // Batch execution (operation-level)
        .route("/ui-bridge/batch", post(ui_bridge_batch_handler))
        // Step-level control batch
        .route(
            "/ui-bridge/control/batch",
            post(ui_bridge_control_batch_handler),
        )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/_routes"),
        ("GET", "/ui-bridge/_help"),
        ("POST", "/ui-bridge/control/assert"),
        ("GET", "/ui-bridge/control/keyboard-shortcuts"),
        ("POST", "/ui-bridge/ai/expect"),
        ("POST", "/ui-bridge/control/batch-execute"),
        ("GET", "/ui-bridge/capabilities"),
        ("GET", "/ui-bridge/control/action-history"),
        ("GET", "/ui-bridge/control/metrics"),
        // SDK aliases for the same handlers.
        ("GET", "/ui-bridge/control/history"),
        ("GET", "/ui-bridge/control/interaction-metrics"),
        ("GET", "/ui-bridge/debug/action-history"),
        ("GET", "/ui-bridge/debug/metrics"),
        ("GET", "/ui-bridge/control/workflows"),
        ("POST", "/ui-bridge/control/workflow/{id}/run"),
        ("GET", "/ui-bridge/control/workflow/{run_id}/status"),
        ("GET", "/ui-bridge/control/element/{id}/state"),
        ("GET", "/ui-bridge/control/render-log"),
        ("POST", "/ui-bridge/control/render-log"),
        ("GET", "/ui-bridge/render-log"),
        ("POST", "/ui-bridge/render-log"),
        ("DELETE", "/ui-bridge/render-log"),
        ("POST", "/ui-bridge/render-log/snapshot"),
        ("GET", "/ui-bridge/render-log/path"),
        ("POST", "/ui-bridge/heartbeat"),
        ("POST", "/ui-bridge/pong"),
        ("POST", "/ui-bridge/ipc-response"),
        ("POST", "/ui-bridge/batch"),
        ("POST", "/ui-bridge/control/batch"),
    ]
}
