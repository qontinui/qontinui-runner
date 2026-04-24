//! UI Bridge handlers for MCP API
//!
//! Provides HTTP handlers for UI Bridge control (React UI automation)
//! and UI Bridge exploration (qontinui library via Python bridge).
//!
//! This module was extracted from a single 12.7kLOC file. Foundation
//! pieces (request types, error taxonomy, circuit breaker, core IPC
//! request machinery) live in dedicated submodules; the per-endpoint
//! HTTP handlers and `routes()` composer are still inline in this file
//! and will be extracted in subsequent passes.

pub mod analytics;
pub mod bookmarks;
pub mod circuit_breaker;
pub mod design;
pub mod errors;
pub mod exploration;
pub mod forms;
pub mod helpers;
pub mod history;
pub mod network;
pub mod page;
pub mod request;
pub mod types;

// Re-export all public symbols so every currently-used path like
// `crate::mcp::ui_bridge::UiBridgeError`, `::ui_bridge_request_sync`,
// `::handle_ui_bridge_response`, `::UiBridgeCircuitBreaker` still resolves.
pub use analytics::{
    analytics_action_baselines_handler, analytics_annotation_gaps_handler,
    analytics_decay_curve_handler, analytics_failure_taxonomy_handler,
    analytics_fragility_heatmap_handler, analytics_health_score_handler,
    analytics_intervention_handler, analytics_recommendations_handler,
    analytics_regressions_handler, analytics_stall_frequency_handler,
    analytics_state_coverage_handler, AnalyticsDaysQuery, AnnotationGapQuery, DecayCurveQuery,
    StateCoverageQuery,
};
pub use bookmarks::{
    ui_bridge_categorize_last_diff_handler, ui_bridge_delete_bookmark_handler,
    ui_bridge_diff_from_bookmark_handler, ui_bridge_disable_change_buffer_handler,
    ui_bridge_drain_change_buffer_handler, ui_bridge_enable_change_buffer_handler,
    ui_bridge_execute_with_diff_handler, ui_bridge_get_bookmark_handler,
    ui_bridge_get_change_buffer_size_handler, ui_bridge_list_bookmarks_handler,
    ui_bridge_save_bookmark_handler, ui_bridge_scoped_diff_handler,
    ui_bridge_structured_changes_handler, ui_bridge_summarize_diff_handler,
    ui_bridge_wait_for_change_handler, ui_bridge_with_diff_handler,
};
pub use circuit_breaker::{CircuitBreakerState, UiBridgeCircuitBreaker};
pub use design::{
    ui_bridge_design_audit_handler, ui_bridge_design_clear_style_guide_handler,
    ui_bridge_design_element_styles_handler, ui_bridge_design_get_style_guide_handler,
    ui_bridge_design_load_style_guide_handler, ui_bridge_design_responsive_handler,
    ui_bridge_design_snapshot_handler, ui_bridge_design_state_styles_handler,
};
pub use errors::{
    ui_bridge_capture_error_baseline_handler, ui_bridge_circuit_breaker_reset_handler,
    ui_bridge_compare_error_baseline_handler, ui_bridge_diagnostics_handler,
    ui_bridge_end_error_session_handler, ui_bridge_get_error_report_handler,
    ui_bridge_get_error_sessions_handler, ui_bridge_get_error_snapshots_handler,
    ui_bridge_get_health_report_handler, ui_bridge_get_idle_signal_handler,
    ui_bridge_get_idle_status_handler, ui_bridge_health_signals_handler,
    ui_bridge_readiness_handler, ui_bridge_start_error_session_handler, ErrorBaselineRequest,
    ErrorSessionStartRequest, ErrorSnapshotsQuery, UiBridgeHealthSignals,
};
pub use exploration::{
    discover_states_from_renders, get_ui_bridge_exploration_results,
    get_ui_bridge_exploration_status, start_ui_bridge_exploration, stop_ui_bridge_exploration,
    ui_bridge_list_windows_handler, WindowInfo,
};
pub use forms::{
    ui_bridge_clipboard_read_handler, ui_bridge_clipboard_write_handler,
    ui_bridge_diff_forms_handler, ui_bridge_fill_form_handler, ui_bridge_get_forms_handler,
    ui_bridge_snapshot_forms_handler,
};
pub use history::{
    ui_bridge_element_reliability_handler, ui_bridge_history_element_handler,
    ui_bridge_history_elements_handler, ui_bridge_history_flaky_handler, ElementReliabilityQuery,
    FlakyElementsQuery, HistoryElementQuery, HistoryElementsQuery,
};
pub use network::{
    ui_bridge_clear_console_errors_handler, ui_bridge_get_browser_events_handler,
    ui_bridge_get_console_errors_handler, ui_bridge_get_network_chains_handler,
    ui_bridge_get_network_request_handler, ui_bridge_get_network_requests_handler,
    ui_bridge_get_network_requests_in_flight_handler, ui_bridge_get_timeline_handler,
    ui_bridge_wait_for_network_request_handler, BrowserEventsQuery, ConsoleErrorsQuery,
    NetworkChainsQuery, NetworkRequestsQuery, TimelineQuery,
};
pub use page::{
    ui_bridge_activate_tab_handler, ui_bridge_navigate_and_wait_handler,
    ui_bridge_page_close_request_handler, ui_bridge_page_evaluate_batch_handler,
    ui_bridge_page_evaluate_handler, ui_bridge_page_evaluate_raw_handler,
    ui_bridge_page_evaluate_safe_handler, ui_bridge_page_go_back_handler,
    ui_bridge_page_go_forward_handler, ui_bridge_page_hard_refresh_handler,
    ui_bridge_page_navigate_handler, ui_bridge_page_refresh_handler,
    ui_bridge_page_set_tab_handler, ui_bridge_page_summary_handler,
    ui_bridge_query_selector_handler, BatchEvaluateRequest, BatchExpression, BatchExpressionResult,
    NavigateAndWaitRequest, PageEvaluateRequest, PageNavigateRequest, QuerySelectorRequest,
    SetTabRequest, SetTabResponse,
};
pub use request::{handle_ui_bridge_response, ui_bridge_request_sync};
pub use types::{
    classify_assertion_failure, classify_transport_error, recovery_hint_for, ClipboardWriteRequest,
    DiscoverStatesRequest, RecoveryHint, StartUIBridgeExplorationRequest, UIBridgeActionRequest,
    UIBridgeComponentActionRequest, UIBridgeDiscoveryRequest, UIBridgeExplorationStatusRequest,
    UiBridgeError, UiBridgeErrorCode,
};

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, error, info, warn};

use tauri::Emitter;

use crate::executor::with_default_bridge;
use crate::mcp::types::{api_error, api_error_detailed, ApiResponse, ApiState};
use crate::screen;

// Internal re-imports for inline handlers that still live in this file.
// `request::get_ui_bridge_timeout_ms` and `request::wrap_ipc_result` are
// `pub(super)` helpers the inline handlers depend on.
use request::{gather_readiness_diagnostics, get_ui_bridge_timeout_ms, wrap_ipc_result};

// Shared helpers (JS evaluation, snapshot diffing, response extractors)
// live in `helpers.rs` so per-family handler extractions can pull them in
// without dragging the rest of `mod.rs` along.
use helpers::{
    compute_snapshot_diff, count_elements_in_discover_payload, direct_webview_evaluate_with_result,
    evaluate_js_expression, extract_ai_find_match, extract_first_element_id,
    extract_get_element_match, filter_element_fields, glob_match, safe_evaluate,
    snapshot_signature,
};

/// Get all registered UI elements from the React UI Bridge.
pub async fn ui_bridge_get_elements_handler(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Result<(HeaderMap, Json<ApiResponse<serde_json::Value>>), (StatusCode, Json<ApiResponse<()>>)>
{
    let refresh = query
        .get("refresh")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    // P3.2: opt-in legacy shape via `?v=1` query or `X-Api-Version: 1` header.
    // Default (no opt-in) returns the v2 wrapped object; the deprecation header
    // is attached only when v1 is in effect so v2 callers don't see noise.
    let api_version_v1 = query.get("v").map(|v| v == "1").unwrap_or(false)
        || headers
            .get("x-api-version")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim() == "1")
            .unwrap_or(false);

    info!(
        "UI Bridge API: Getting all elements (refresh={}, v1={})",
        refresh, api_version_v1
    );

    // Optional pre-fetch discover to defeat the well-known
    // "stale registry after navigation" gotcha. Without `?refresh=true`,
    // this endpoint returns whatever the SDK registry has cached, which
    // can be a 1-element list if the user just switched tabs and the
    // current page hasn't auto-rediscovered. With `?refresh=true`, we
    // force a fresh discover before reading the registry. Defaults to
    // false to preserve the existing fast-path semantics.
    if refresh {
        if let Err(e) = ui_bridge_request_sync(
            &state,
            "discover",
            serde_json::json!({ "options": { "interactiveOnly": false } }),
        )
        .await
        {
            warn!(
                "UI Bridge API: get_elements refresh discover failed ({}); returning stale registry",
                e
            );
        }
    }

    // Extract Tier 1.2 text filter params for server-side post-filtering.
    // These are applied after the SDK returns the element list so callers can
    // narrow results without a full discover round-trip.
    let filter_title = query.get("title").map(|s| s.to_lowercase());
    let filter_aria_label = query.get("aria_label").map(|s| s.to_lowercase());
    let filter_text = query.get("text").map(|s| s.to_lowercase());

    match ui_bridge_request_sync(&state, "get_elements", serde_json::json!({})).await {
        Ok(data) => {
            let mut resp_headers = HeaderMap::new();
            // The IPC response is either a raw array or `{ elements: [...] }`.
            // Normalize to a Vec<Value> we can re-wrap in either shape.
            let mut elements_array: Vec<serde_json::Value> = if data.is_array() {
                data.as_array().cloned().unwrap_or_default()
            } else if let Some(arr) = data.get("elements").and_then(|v| v.as_array()) {
                arr.clone()
            } else {
                Vec::new()
            };

            // Apply Tier 1.2 filters: case-insensitive substring match on
            // title, aria-label, or label/id text fields.
            if filter_title.is_some() || filter_aria_label.is_some() || filter_text.is_some() {
                elements_array.retain(|el| {
                    let label = el
                        .get("label")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_lowercase();
                    let id = el
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_lowercase();

                    if let Some(ref needle) = filter_title {
                        if !label.contains(needle.as_str()) {
                            return false;
                        }
                    }
                    if let Some(ref needle) = filter_aria_label {
                        if !label.contains(needle.as_str()) {
                            return false;
                        }
                    }
                    if let Some(ref needle) = filter_text {
                        if !label.contains(needle.as_str()) && !id.contains(needle.as_str()) {
                            return false;
                        }
                    }
                    true
                });
            }

            let payload = if api_version_v1 {
                // Legacy shape: data: [...] (raw list).
                resp_headers.insert(
                    "X-Api-Deprecation",
                    "data shape changed in v2; use ?v=1 for old shape"
                        .parse()
                        .unwrap(),
                );
                serde_json::Value::Array(elements_array)
            } else {
                // v2: wrapped object with elements/count/timestamp.
                let count = elements_array.len();
                serde_json::json!({
                    "elements": elements_array,
                    "count": count,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                })
            };
            Ok((resp_headers, Json(ApiResponse::success(payload))))
        }
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a specific element by ID.
///
/// Optional query param `?fields=id,label,rect,...` returns only the listed
/// top-level fields (server-side filter — the IPC fetches the full element
/// either way). Unknown field names are silently dropped. See
/// `ELEMENT_ALLOWED_FIELDS` for the supported list.
pub async fn ui_bridge_get_element_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting element {}", id);

    let fields_csv = query.get("fields").cloned();

    let result = ui_bridge_request_sync(
        &state,
        "get_element",
        serde_json::json!({ "elementId": id }),
    )
    .await;

    // Apply field filtering up-front, then delegate to wrap_ipc_result so we
    // preserve the same success/failure envelope as before (including the
    // `success: false` detection for IPC-layer errors).
    let filtered_result = result.map(|mut data| {
        if let Some(csv) = fields_csv {
            // Frontend wraps the element either at top level or under
            // `data.element`. Filter both possible shapes in place so
            // callers get the same envelope they already use.
            if let Some(el) = data.get("element").cloned() {
                let filtered = filter_element_fields(&el, &csv);
                if let Some(obj) = data.as_object_mut() {
                    obj.insert("element".to_string(), filtered);
                }
            } else if data.is_object() {
                data = filter_element_fields(&data, &csv);
            }
        }
        data
    });

    wrap_ipc_result(filtered_result)
}

/// Declarative element assertion — check multiple predicates against a registered element.
///
/// POST /ui-bridge/control/element/{id}/assert
/// Body: `{ "visible": true, "enabled": true, "text": "Save", ... }`
/// Returns: `{ "passed": bool, "checked": N, "passedCount": M, "failures": [...] }`
pub async fn ui_bridge_assert_element_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(spec): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Assert element {}", id);

    let payload = serde_json::json!({
        "elementId": id,
        "spec": spec
    });

    match ui_bridge_request_sync(&state, "assert_element", payload).await {
        Ok(data) => {
            // Check for ELEMENT_NOT_FOUND from the frontend
            if data.get("error").and_then(|v| v.as_str()) == Some("ELEMENT_NOT_FOUND") {
                let msg = data
                    .get("errorMessage")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Element not found")
                    .to_string();
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(api_error_detailed(
                        msg,
                        UiBridgeError::element_not_found(&id),
                    )),
                ));
            }
            Ok(Json(ApiResponse::success(data)))
        }
        Err(e) => {
            error!("UI Bridge API: Assert element failed: {}", e);
            let detail = classify_transport_error(&e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error_detailed(e, detail)),
            ))
        }
    }
}

/// Execute an action on an element.
/// Optional query parameters for action execution (e.g., task_run_id for persistence).
#[derive(Debug, Deserialize, Default)]
pub struct ActionQueryParams {
    /// When provided, the action event is persisted to ui_bridge_events for cross-run analysis.
    #[serde(default)]
    pub task_run_id: Option<i64>,
}

/// Execute multiple element actions in a single HTTP round-trip.
///
/// Each step is forwarded to the frontend's `execute_action` handler
/// sequentially. Supports `stopOnFailure` (default true) and
/// `delayBetweenMs` (default 0). Returns per-step results + aggregate
/// counts.
pub async fn ui_bridge_batch_actions_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let steps = request
        .get("steps")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let stop_on_failure = request
        .get("stopOnFailure")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let delay_between_ms = request
        .get("delayBetweenMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    info!(
        "UI Bridge API: Batch actions ({} steps, stopOnFailure={}, delay={}ms)",
        steps.len(),
        stop_on_failure,
        delay_between_ms
    );

    let start = std::time::Instant::now();
    let mut results = Vec::new();
    let mut succeeded = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let mut stopped = false;

    for (i, step) in steps.iter().enumerate() {
        if stopped {
            skipped += 1;
            results.push(serde_json::json!({
                "index": i,
                "label": step.get("label"),
                "elementId": step.get("elementId"),
                "response": {"success": false, "error": "Skipped (previous step failed)"},
            }));
            continue;
        }

        if i > 0 && delay_between_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay_between_ms)).await;
        }

        let element_id = step.get("elementId").and_then(|v| v.as_str()).unwrap_or("");
        let action = step.get("action").cloned().unwrap_or(serde_json::json!({}));

        let payload = serde_json::json!({
            "id": element_id,
            "action": action.get("action").and_then(|v| v.as_str()).unwrap_or("click"),
            "params": action.get("params"),
        });

        let result = ui_bridge_request_sync(&state, "execute_action", payload).await;

        let (success, response) = match result {
            Ok(data) => (
                data.get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
                data,
            ),
            Err(e) => (false, serde_json::json!({"success": false, "error": e})),
        };

        results.push(serde_json::json!({
            "index": i,
            "label": step.get("label"),
            "elementId": element_id,
            "response": response,
        }));

        if success {
            succeeded += 1;
        } else {
            failed += 1;
            if stop_on_failure {
                stopped = true;
            }
        }
    }

    let duration_ms = start.elapsed().as_millis() as u64;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "success": failed == 0,
        "results": results,
        "succeededCount": succeeded,
        "failedCount": failed,
        "skippedCount": skipped,
        "durationMs": duration_ms,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    }))))
}

pub async fn ui_bridge_execute_action_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(query): Query<ActionQueryParams>,
    headers: HeaderMap,
    body: Result<Json<UIBridgeActionRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Custom body extraction so JSON-parse errors come back with an actionable
    // hint instead of the raw serde_json message. The most common trip is a
    // Windows path in `params.value` (e.g. `D:\foo\bar`) where a single
    // backslash is an invalid JSON escape — agents keep hitting this and the
    // serde message alone ("invalid escape at line 1 column 42") doesn't
    // point at the fix.
    let request = match body {
        Ok(Json(req)) => req,
        Err(rej) => {
            let raw = format!("{}", rej);
            let lower = raw.to_lowercase();
            let hint = if lower.contains("invalid escape") || lower.contains("control character") {
                format!(
                    "{}. Hint: if a field value contains a Windows path, escape each backslash as `\\\\` in the JSON body — or send the body from a file with `curl --data-binary @file.json` to bypass shell re-escaping. Forward slashes also work: `D:/qontinui-root/...`.",
                    raw
                )
            } else {
                raw
            };
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::error(hint)),
            ));
        }
    };

    info!(
        "UI Bridge API: Executing action {} on element {}",
        request.action, id
    );

    let action_name = request.action.clone();
    let task_run_id = query.task_run_id;

    // Default expectChange=true for click/doubleClick on non-input elements
    // so callers get a state-change signal instead of bare {success: true}.
    // Explicit caller values always win.
    let expect_change = if request.expect_change.is_some() {
        request.expect_change.clone()
    } else if matches!(
        action_name.as_str(),
        "click" | "doubleClick" | "double_click"
    ) {
        // Check if target is an input-like element (clicks on inputs just
        // focus; the "change" signal would be misleading).
        let is_input = match ui_bridge_request_sync(
            &state,
            "get_element",
            serde_json::json!({ "elementId": id }),
        )
        .await
        {
            Ok(data) => {
                let tag = data
                    .get("element_type")
                    .or_else(|| data.get("tagName"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                matches!(tag.to_uppercase().as_str(), "INPUT" | "TEXTAREA" | "SELECT")
            }
            Err(_) => false, // can't tell — default to enabling detector
        };
        if is_input {
            None
        } else {
            Some(serde_json::Value::Bool(true))
        }
    } else {
        request.expect_change.clone()
    };

    let start = Instant::now();

    // Optional pre-action snapshot for the click-had-no-effect detector.
    // We take it BEFORE the action so the timing is fair (vs. counting
    // mutations the action itself produces in flight). The snapshot is a
    // discover call with `interactive_only: false` to catch every element
    // that could have changed.
    let pre_snapshot_signature: Option<(usize, u64)> = if expect_change.is_some() {
        match ui_bridge_request_sync(
            &state,
            "discover",
            serde_json::json!({ "options": { "interactiveOnly": false } }),
        )
        .await
        {
            Ok(data) => Some(snapshot_signature(&data)),
            Err(e) => {
                warn!(
                    "execute_action: pre-snapshot failed for expectChange ({}); skipping detector",
                    e
                );
                None
            }
        }
    } else {
        None
    };

    // Merge flat top-level fields into params so actions like drag work with
    // both {"action":"drag","params":{"targetPosition":{...}}} and
    // {"action":"drag","targetPosition":{...}} formats.
    let merged_params = if request.extra.is_empty() {
        request.params
    } else {
        let mut base = match request.params {
            Some(serde_json::Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        for (k, v) in request.extra {
            base.entry(k).or_insert(v);
        }
        Some(serde_json::Value::Object(base))
    };

    let action_obj = serde_json::json!({
        "action": action_name,
        "params": merged_params,
        "waitOptions": request.wait_options
    });

    let payload = serde_json::json!({
        "elementId": id,
        "action": action_obj.clone()
    });

    // ── Issue 2: Disabled-state detection ──────────────────────────────
    // Before dispatching the action, fetch the element state and reject
    // clicks on disabled elements early so callers get an explicit error
    // instead of a silent no-op.
    if let Ok(elem_data) = ui_bridge_request_sync(
        &state,
        "get_element",
        serde_json::json!({ "elementId": id }),
    )
    .await
    {
        let is_disabled = {
            let props = elem_data
                .get("properties")
                .or_else(|| elem_data.get("props"));
            let aria_disabled = elem_data
                .get("ariaDisabled")
                .and_then(|v| v.as_str())
                .map(|s| s == "true")
                .unwrap_or(false);
            let disabled_attr = elem_data
                .get("disabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let data_disabled = props
                .and_then(|p| p.get("data-disabled"))
                .and_then(|v| v.as_str())
                .map(|s| s == "true")
                .unwrap_or(false);
            // Also check nested properties for aria-disabled / disabled
            let prop_aria_disabled = props
                .and_then(|p| p.get("aria-disabled"))
                .and_then(|v| v.as_str())
                .map(|s| s == "true")
                .unwrap_or(false);
            let prop_disabled = props
                .and_then(|p| p.get("disabled"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            aria_disabled || disabled_attr || data_disabled || prop_aria_disabled || prop_disabled
        };

        if is_disabled {
            warn!(
                "execute_action: element {} is disabled, rejecting {} action early",
                id, action_name
            );
            return Ok(Json(ApiResponse {
                success: false,
                data: Some(serde_json::json!({
                    "error": "element is disabled",
                    "elementState": elem_data,
                })),
                error: Some("element is disabled".to_string()),
                error_detail: None,
            }));
        }
    }

    // ── Execute the action ─────────────────────────────────────────────
    let mut result =
        wrap_ipc_result(ui_bridge_request_sync(&state, "execute_action", payload.clone()).await);

    // ── Issue 1: Stale registry retry ──────────────────────────────────
    // When React unmount/remount creates a new DOM node the old registry
    // entry goes stale.  If the SDK reports "never registered or
    // discovered" or "not found", refresh via discover and retry once.
    {
        let should_retry = match &result {
            Ok(ref resp) if !resp.success => resp
                .error
                .as_deref()
                .map(|e| {
                    let lower = e.to_lowercase();
                    lower.contains("never registered or discovered") || lower.contains("not found")
                })
                .unwrap_or(false),
            Err((_, ref err_resp)) => err_resp
                .error
                .as_deref()
                .map(|e| {
                    let lower = e.to_lowercase();
                    lower.contains("never registered or discovered") || lower.contains("not found")
                })
                .unwrap_or(false),
            _ => false,
        };

        if should_retry {
            warn!(
                "execute_action: element {} not found in registry, refreshing via discover and retrying",
                id
            );
            // Refresh the element registry
            let _ = ui_bridge_request_sync(
                &state,
                "discover",
                serde_json::json!({ "options": { "interactiveOnly": false } }),
            )
            .await;

            // Retry the original action once
            let retry_result =
                wrap_ipc_result(ui_bridge_request_sync(&state, "execute_action", payload).await);

            match &retry_result {
                Ok(ref resp) if resp.success => {
                    info!(
                        "execute_action: retry succeeded for element {} after discover refresh",
                        id
                    );
                    result = retry_result;
                }
                _ => {
                    warn!(
                        "execute_action: retry also failed for element {}, returning original error",
                        id
                    );
                    // Keep the original `result` — the caller gets the first error
                }
            }
        }

        // ── Stable ref fallback ──────────────────────────────────────────
        // If the action still failed after discover-retry and the caller
        // provided a stable ref header, attempt to resolve the element
        // via the SDK's resolve_stable_ref IPC and retry with the new ID.
        let still_failed = match &result {
            Ok(ref resp) if !resp.success => resp
                .error
                .as_deref()
                .map(|e| {
                    let lower = e.to_lowercase();
                    lower.contains("never registered or discovered") || lower.contains("not found")
                })
                .unwrap_or(false),
            Err(_) => true,
            _ => false,
        };

        if still_failed {
            if let Some(stable_ref_b64) = headers
                .get("X-UI-Bridge-Stable-Ref")
                .and_then(|v| v.to_str().ok())
            {
                // Decode base64 → JSON → send resolve_stable_ref IPC
                use base64::Engine;
                if let Ok(decoded_bytes) =
                    base64::engine::general_purpose::STANDARD.decode(stable_ref_b64)
                {
                    if let Ok(stable_ref_json) =
                        serde_json::from_slice::<serde_json::Value>(&decoded_bytes)
                    {
                        info!(
                            "execute_action: attempting stable ref resolution for element {}",
                            id
                        );
                        let resolve_payload = serde_json::json!({
                            "stableRef": stable_ref_json
                        });
                        if let Ok(resolve_result) =
                            ui_bridge_request_sync(&state, "resolve_stable_ref", resolve_payload)
                                .await
                        {
                            if let Some(new_id) =
                                resolve_result.get("elementId").and_then(|v| v.as_str())
                            {
                                info!("execute_action: stable ref resolved {} -> {}", id, new_id);
                                // Retry with the resolved ID
                                let retry_payload = serde_json::json!({
                                    "elementId": new_id,
                                    "action": action_obj
                                });
                                let retry_result = wrap_ipc_result(
                                    ui_bridge_request_sync(&state, "execute_action", retry_payload)
                                        .await,
                                );
                                match &retry_result {
                                    Ok(ref resp) if resp.success => {
                                        info!(
                                            "execute_action: stable ref retry succeeded for {} (resolved to {})",
                                            id, new_id
                                        );
                                        // Merge the resolved ID into the response
                                        result = retry_result.map(|mut r| {
                                            let merged = match r.data.take() {
                                                Some(serde_json::Value::Object(mut m)) => {
                                                    m.insert(
                                                        "resolvedId".to_string(),
                                                        serde_json::Value::String(
                                                            new_id.to_string(),
                                                        ),
                                                    );
                                                    serde_json::Value::Object(m)
                                                }
                                                Some(other) => serde_json::json!({
                                                    "result": other,
                                                    "resolvedId": new_id,
                                                }),
                                                None => serde_json::json!({ "resolvedId": new_id }),
                                            };
                                            r.data = Some(merged);
                                            r
                                        });
                                    }
                                    _ => {
                                        warn!(
                                            "execute_action: stable ref retry also failed for element {}",
                                            id
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Click-had-no-effect detector. Re-snapshot after a short settle delay
    // and compare against the pre-snapshot signature. If the element graph
    // is byte-identical, the click was a silent no-op.
    if let Some(pre_sig) = pre_snapshot_signature {
        let settle_ms = expect_change
            .as_ref()
            .and_then(|v| v.get("settleMs"))
            .and_then(|v| v.as_u64())
            .unwrap_or(250);
        tokio::time::sleep(std::time::Duration::from_millis(settle_ms)).await;

        let post_sig = match ui_bridge_request_sync(
            &state,
            "discover",
            serde_json::json!({ "options": { "interactiveOnly": false } }),
        )
        .await
        {
            Ok(data) => Some(snapshot_signature(&data)),
            Err(e) => {
                warn!(
                    "execute_action: post-snapshot failed for expectChange ({})",
                    e
                );
                None
            }
        };

        let detector = match post_sig {
            Some(post) => {
                let changed = post != pre_sig;
                serde_json::json!({
                    "supported": true,
                    "effectChanged": changed,
                    "preElementCount": pre_sig.0,
                    "postElementCount": post.0,
                    "preSignature": pre_sig.1.to_string(),
                    "postSignature": post.1.to_string(),
                    "settleMs": settle_ms,
                })
            }
            None => serde_json::json!({
                "supported": false,
                "effectChanged": null,
                "reason": "post-snapshot failed",
            }),
        };

        // Merge the detector report into the result data without
        // disturbing the existing fields.
        if let Ok(ref mut json_resp) = result {
            let merged = match json_resp.data.take() {
                Some(serde_json::Value::Object(mut m)) => {
                    m.insert("expectChange".to_string(), detector);
                    serde_json::Value::Object(m)
                }
                Some(other) => serde_json::json!({
                    "result": other,
                    "expectChange": detector,
                }),
                None => serde_json::json!({ "expectChange": detector }),
            };
            json_resp.data = Some(merged);
        }
    }

    // Persist the action event when task_run_id is provided (non-blocking)
    if let Some(tr_id) = task_run_id {
        let duration_ms = start.elapsed().as_secs_f64() * 1000.0;
        let seq = state
            .ui_bridge_event_sequence
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let (success, error_msg, result_json) = match &result {
            Ok(json_resp) => {
                let s = json_resp.success;
                let err = json_resp.error.clone();
                // Truncate result to 1KB to respect memory budgets
                let res = json_resp.data.as_ref().map(|d| {
                    let s = d.to_string();
                    if s.len() > 1024 {
                        format!("{}...", &s[..1024])
                    } else {
                        s
                    }
                });
                (s, err, res)
            }
            Err(_) => (false, Some("transport error".to_string()), None),
        };

        let pg_db = state.app_state.pg_db.clone();
        let element_id = id.clone();
        let action_for_db = action_name.clone();

        // Async PG write — fire-and-forget, never blocks the response
        tokio::spawn(async move {
            match pg_db
                .insert_ui_bridge_event(
                    Some(tr_id),
                    seq,
                    "action_executed",
                    Some(&element_id),
                    None,
                    None,
                    Some(&action_for_db),
                    None,
                    result_json.as_deref(),
                    Some(duration_ms),
                    success,
                    error_msg.as_deref(),
                    None,
                )
                .await
            {
                Ok(row_id) => info!(
                    "UI Bridge event persisted: element={}, row_id={}",
                    element_id, row_id
                ),
                Err(e) => warn!("UI Bridge event persist failed: {}", e),
            }
        });
    }

    result
}

/// Get all registered components.
pub async fn ui_bridge_get_components_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting all components");

    match ui_bridge_request_sync(&state, "get_components", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a specific component by ID.
pub async fn ui_bridge_get_component_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting component {}", id);

    match ui_bridge_request_sync(
        &state,
        "get_component",
        serde_json::json!({ "componentId": id }),
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

/// Execute an action on a component.
pub async fn ui_bridge_execute_component_action_handler(
    State(state): State<Arc<ApiState>>,
    Path((id, action_id)): Path<(String, String)>,
    Json(request): Json<UIBridgeComponentActionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: Executing action {} on component {}",
        action_id, id
    );

    let payload = serde_json::json!({
        "componentId": id,
        "actionId": action_id,
        "params": request.params
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "execute_component_action", payload).await)
}

/// Discover controllable elements in the UI.
///
/// On success the result is cached on `ApiState::ui_bridge_last_discovered`
/// so agents can retrieve the same element set via
/// `GET /ui-bridge/control/elements/last-discovered`. This works around the
/// React registry occasionally pruning elements between calls.
pub async fn ui_bridge_discover_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<UIBridgeDiscoveryRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Discovering elements");

    let payload = serde_json::json!({
        "options": {
            "root": request.root,
            "interactiveOnly": request.interactive_only,
            "includeHidden": request.include_hidden,
            "limit": request.limit,
            "types": request.types,
            "selector": request.selector
        }
    });

    match ui_bridge_request_sync(&state, "discover", payload).await {
        Ok(mut data) => {
            // Populate the last-discovered cache. Best-effort — never
            // block the response on a write lock.
            let element_count = count_elements_in_discover_payload(&data);
            let cache_entry = crate::mcp::types::CachedDiscoverResult {
                data: data.clone(),
                captured_at: std::time::Instant::now(),
                element_count,
            };
            {
                let mut guard = state.ui_bridge_last_discovered.write().await;
                *guard = Some(cache_entry);
            }

            // When discovery returns 0 elements, attach a diagnostic block
            // so callers (especially manual testers) immediately know WHY.
            // Common causes: frontend not authenticated, registry not yet
            // populated, page on a guard/login screen.
            if element_count == 0 {
                if let Some(obj) = data.as_object_mut() {
                    let last_pong = state
                        .ui_bridge_last_pong
                        .load(std::sync::atomic::Ordering::Relaxed);

                    // direct_webview_evaluate_with_result returns the
                    // JSON-encoded string of the evaluated value. Wrap each
                    // call in a short timeout so a broken frontend doesn't
                    // stall the diagnostics block (which is emitted precisely
                    // when the frontend may be broken).
                    let diag_timeout = std::time::Duration::from_millis(500);

                    let body_text_present = tokio::time::timeout(
                        diag_timeout,
                        direct_webview_evaluate_with_result(
                            &state,
                            "document.body && document.body.innerText.length > 100",
                            None,
                            false,
                        ),
                    )
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .map(|s| s.trim() == "true");

                    let title = tokio::time::timeout(
                        diag_timeout,
                        direct_webview_evaluate_with_result(&state, "document.title", None, false),
                    )
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    // The result is JSON-encoded (e.g. `"Qontinui Runner"`),
                    // so parse it as a JSON string to get the raw text.
                    .and_then(|s| serde_json::from_str::<String>(&s).ok());

                    obj.insert(
                        "diagnostics".to_string(),
                        serde_json::json!({
                            "reason": "registry_empty",
                            "hint": "The UI Bridge element registry is empty. \
This usually means the frontend's AutoRegisterProvider has not yet populated \
the registry. Common causes: (1) the page is on a login/guard screen and the \
auth-gated providers have not mounted, (2) the React app has just loaded and \
the MutationObserver scan has not yet completed (try again in 200ms), \
(3) the page rendered before the UIBridgeProvider mounted. Use \
control/page/evaluate to inspect the DOM directly as a fallback.",
                            "frontend_pong_seen": last_pong > 0,
                            "page_has_content": body_text_present,
                            "page_title": title,
                        }),
                    );
                }
            }

            Ok(Json(ApiResponse::success(data)))
        }
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Maximum age of the last-discovered cache served to callers, in seconds.
/// Past this age the endpoint reports the entry as stale but still returns
/// it (callers can decide what to do based on `age_ms`).
const LAST_DISCOVERED_FRESH_SECS: u64 = 60;

/// Return the last discovered element set (cached from the most recent
/// `POST /ui-bridge/control/discover` call on this runner instance).
///
/// Unlike `/control/elements`, this endpoint does NOT round-trip to the
/// React frontend — it serves the exact payload captured at discover time,
/// so elements that the React registry has since pruned are still
/// available to the caller. Returns 404 if no discover has ever run on
/// this runner instance.
///
/// The response adds two diagnostic fields alongside the original payload:
/// - `cache_age_ms`: how long ago the cache was populated
/// - `stale`: true if the cache is older than the freshness window
pub async fn ui_bridge_get_last_discovered_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let guard = state.ui_bridge_last_discovered.read().await;
    let entry = match guard.as_ref() {
        Some(e) => e,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(
                    "No discover result cached yet — call POST /ui-bridge/control/discover first",
                )),
            ));
        }
    };

    let age_ms = entry.captured_at.elapsed().as_millis() as u64;
    let stale = entry.captured_at.elapsed().as_secs() > LAST_DISCOVERED_FRESH_SECS;

    // Wrap the cached payload with diagnostic metadata without mutating
    // the original. If the payload is already an object, merge in the
    // cache_* fields; otherwise wrap it under `data`.
    let response = match entry.data.clone() {
        serde_json::Value::Object(mut map) => {
            map.insert("cache_age_ms".to_string(), serde_json::Value::from(age_ms));
            map.insert(
                "cache_element_count".to_string(),
                serde_json::Value::from(entry.element_count),
            );
            map.insert("cache_stale".to_string(), serde_json::Value::from(stale));
            serde_json::Value::Object(map)
        }
        other => serde_json::json!({
            "data": other,
            "cache_age_ms": age_ms,
            "cache_element_count": entry.element_count,
            "cache_stale": stale,
        }),
    };

    Ok(Json(ApiResponse::success(response)))
}

/// Get a full snapshot of the UI Bridge state.
///
/// Supports optional query-string filters applied after the snapshot is
/// fetched from the SDK:
///   - `?visibleOnly=true` — drop elements whose `state.visible` is false.
///   - `?currentRouteOnly=true` — drop elements whose `page.pathname` (when
///     present) does not match the snapshot-level page pathname. In a
///     standard SPA every registered element belongs to the current route, so
///     this is usually a no-op, but we forward it consistently for callers
///     that rely on the filter existing end-to-end.
///
/// Filtering happens in this Rust handler (not via a special SDK code path),
/// because `get_snapshot` participates in the dedup cache keyed by type, and
/// filtering client-side here keeps the cached payload shared across callers.
pub async fn ui_bridge_get_snapshot_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let truthy = |v: &String| {
        let s = v.trim();
        s == "1" || s.eq_ignore_ascii_case("true")
    };
    let visible_only = query.get("visibleOnly").is_some_and(truthy);
    let current_route_only = query.get("currentRouteOnly").is_some_and(truthy);

    info!(
        "UI Bridge API: Getting snapshot (visibleOnly={}, currentRouteOnly={})",
        visible_only, current_route_only
    );

    // Try to get snapshot; if elements are empty, auto-discover once and retry.
    // This avoids the common cold-start pitfall where the first snapshot call
    // returns zero elements because the frontend hasn't discovered yet.
    let snapshot_result =
        match ui_bridge_request_sync(&state, "get_snapshot", serde_json::json!({})).await {
            Ok(data) => {
                let elements_empty = data
                    .get("elements")
                    .and_then(|e| e.as_array())
                    .is_none_or(|a| a.is_empty());
                if elements_empty {
                    info!("UI Bridge API: snapshot returned 0 elements — auto-discovering");
                    let _ = ui_bridge_request_sync(
                        &state,
                        "discover",
                        serde_json::json!({"interactive_only": false}),
                    )
                    .await;
                    // Retry snapshot after discovery
                    ui_bridge_request_sync(&state, "get_snapshot", serde_json::json!({})).await
                } else {
                    Ok(data)
                }
            }
            Err(e) => Err(e),
        };

    match snapshot_result {
        Ok(mut data) => {
            // Apply post-fetch filters. We do this before enrichment so the
            // architecture summary still attaches to the filtered response.
            //
            // `visibleOnly`: drop any element whose `state.visible` is
            // explicitly false. Elements missing `state.visible` are kept,
            // since older snapshot shapes and DOM-fallback entries may not
            // populate it — better to over-include than silently drop.
            //
            // `currentRouteOnly`: drop elements whose optional `page.pathname`
            // disagrees with the snapshot's top-level `page.pathname`. This is
            // a no-op in SPAs where the registry only holds current-route
            // elements, but we forward it so the filter works end-to-end.
            if visible_only || current_route_only {
                let snapshot_pathname = data
                    .get("page")
                    .and_then(|p| p.get("pathname"))
                    .and_then(|p| p.as_str())
                    .map(|s| s.to_string());

                if let Some(obj) = data.as_object_mut() {
                    if let Some(elements_val) = obj.get_mut("elements") {
                        if let Some(arr) = elements_val.as_array_mut() {
                            let before = arr.len();
                            arr.retain(|el| {
                                if visible_only {
                                    if let Some(visible) = el
                                        .get("state")
                                        .and_then(|s| s.get("visible"))
                                        .and_then(|v| v.as_bool())
                                    {
                                        if !visible {
                                            return false;
                                        }
                                    }
                                }
                                if current_route_only {
                                    if let (Some(el_path), Some(snap_path)) = (
                                        el.get("page")
                                            .and_then(|p| p.get("pathname"))
                                            .and_then(|p| p.as_str()),
                                        snapshot_pathname.as_deref(),
                                    ) {
                                        if el_path != snap_path {
                                            return false;
                                        }
                                    }
                                }
                                true
                            });
                            let after = arr.len();
                            debug!(
                                "UI Bridge snapshot filter: visibleOnly={} currentRouteOnly={} kept {}/{} elements",
                                visible_only, current_route_only, after, before
                            );
                        }
                    }
                }
            }

            // Enrich snapshot with architecture spec summaries from the database.
            // Wrapped in a timeout so a slow/stuck PG pool never blocks the snapshot response.
            let enrich_result = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                state.app_state.pg_db.get_all_cached_specs(),
            )
            .await;

            match enrich_result {
                Ok(Ok(specs)) => {
                    let summaries: Vec<serde_json::Value> = specs
                        .iter()
                        .filter(|s| crate::spec_utils::is_architecture_spec_str(&s.spec_json))
                        .filter_map(|s| {
                            let parsed: serde_json::Value =
                                serde_json::from_str(&s.spec_json).ok()?;
                            Some(crate::spec_utils::format_architecture_summary(
                                &parsed,
                                &s.app_name,
                            ))
                        })
                        .collect();

                    if !summaries.is_empty() {
                        if let Some(obj) = data.as_object_mut() {
                            obj.insert(
                                "architecture".to_string(),
                                serde_json::json!({ "projects": summaries }),
                            );
                        }
                    }
                }
                Ok(Err(e)) => {
                    warn!(
                        "Snapshot enrichment: failed to fetch cached specs from database: {}",
                        e
                    );
                }
                Err(_) => {
                    warn!(
                        "Snapshot enrichment: PG query timed out after 3s, returning snapshot without architecture data"
                    );
                }
            }

            // Fire-and-forget: enqueue a co-occurrence observation for this
            // snapshot. Never block the snapshot response on this — any
            // error downgrades to WARN inside `enqueue_observation`. See
            // `state-definition-observation-pipeline.md` Step 2 for why
            // capture sits after the response body is assembled.
            //
            // TODO: infer `spec_id` from snapshot/page context once the
            // spec→page binding is available. For now capture is
            // scope-agnostic and the derivation endpoint decides whether to
            // filter by spec.
            let pg_db_for_obs = state.app_state.pg_db.clone();
            let snapshot_for_obs = data.clone();
            let runner_instance = std::env::var("QONTINUI_RUNNER_ROLE")
                .ok()
                .unwrap_or_else(|| "primary".to_string());
            tokio::spawn(async move {
                crate::state_discovery::enqueue_observation(
                    pg_db_for_obs,
                    &snapshot_for_obs,
                    None,
                    runner_instance,
                )
                .await;
            });

            Ok(Json(ApiResponse::success(data)))
        }
        Err(e) => {
            // Fall back to native window capture when the SDK/frontend is not connected.
            // This gives agents a degraded-but-useful snapshot (screenshot + source tag)
            // instead of a blind error, which is critical for diagnosing webview issues
            // like ERR_CONNECTION_REFUSED or blank screens.
            warn!(
                "UI Bridge API: snapshot via SDK failed ({}), falling back to native capture",
                e
            );
            match capture_runner_window_base64(&state).await {
                Some((screenshot, width, height)) => {
                    let data = serde_json::json!({
                        "source": "native_capture",
                        "reason": e,
                        "screenshot": screenshot,
                        "width": width,
                        "height": height,
                        "elements": [],
                        "note": "SDK was not connected. This is a native window capture fallback — no element tree is available."
                    });
                    Ok(Json(ApiResponse::success(data)))
                }
                None => {
                    error!("UI Bridge API: native capture fallback also failed");
                    Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
                }
            }
        }
    }
}

/// Get undo/redo state from the UI Bridge.
pub async fn ui_bridge_get_undo_state_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting undo state");

    match ui_bridge_request_sync(&state, "get_undo_state", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute undo via the UI Bridge.
pub async fn ui_bridge_undo_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Undo");

    match ui_bridge_request_sync(&state, "undo", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Undo failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute redo via the UI Bridge.
pub async fn ui_bridge_redo_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Redo");

    match ui_bridge_request_sync(&state, "redo", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Redo failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get all loaded specs from the SpecStore.
pub async fn ui_bridge_get_specs_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting all specs");

    match ui_bridge_request_sync(&state, "get_specs", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a specific spec by ID from the SpecStore.
pub async fn ui_bridge_get_spec_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting spec {}", id);

    match ui_bridge_request_sync(&state, "get_spec", serde_json::json!({ "specId": id })).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Page Navigation Handlers
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

// ============================================================================
// Direct tab navigation moved to `page::routes()` — see `page.rs`.
// ============================================================================
#[cfg(any())]
const _REMOVED_TAB_LIST: &[&str] = &[
    // Superseded block below (kept inside cfg(any()) so it never compiles).
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
// Structured Assert (Task 18: declarative assertions)
// ============================================================================

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
// Exploration Handlers
// ============================================================================

// =============================================================================
// App-Specific Screenshots (xcap)
// Window listing moved to `exploration::routes()` — see `exploration.rs`.
// =============================================================================

/// Query parameters for annotated screenshot
#[derive(Debug, Deserialize)]
pub struct AnnotatedScreenshotQuery {
    /// Monitor index (0-based), None for primary monitor. Used for full-screen capture.
    #[serde(default)]
    monitor: Option<i32>,
    /// Capture a specific window by title (case-insensitive substring match)
    #[serde(default)]
    window_title: Option<String>,
    /// Capture a specific window by app name (case-insensitive substring match)
    #[serde(default)]
    app_name: Option<String>,
    /// Capture a specific window by its ID (HWND as u32)
    #[serde(default)]
    window_id: Option<u32>,
    /// Capture the runner's own window
    #[serde(default)]
    runner: Option<bool>,
    /// When true, overlay numbered element bounding boxes on the screenshot.
    #[serde(default)]
    annotate: Option<bool>,
    /// Maximum image width after resize (only used when annotate=true).
    /// Default: 1024. Set to 0 to skip resize.
    #[serde(default)]
    max_width: Option<u32>,
}

/// Annotated screenshot response
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotatedScreenshotData {
    screenshot: String,
    width: i32,
    height: i32,
    /// Device pixel ratio (physical pixels / CSS pixels).
    /// Use this to scale CSS element bounds to screenshot pixel coordinates.
    #[serde(skip_serializing_if = "Option::is_none")]
    scale_factor: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    monitor: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_app_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_id: Option<u32>,
    /// Text index of annotated elements (only present when annotate=true).
    /// Format: `[1] "Login" button at (0.12, 0.34) size (0.08, 0.04)`
    #[serde(skip_serializing_if = "Option::is_none")]
    element_index_text: Option<String>,
}

impl AnnotatedScreenshotData {
    /// Create from a `CapturedScreenshot` — width/height always come from the
    /// decoded image dimensions, preventing the DPI double-multiply bug.
    fn from_captured(captured: &screen::CapturedScreenshot) -> Result<Self, String> {
        Ok(Self {
            screenshot: captured.to_png_base64()?,
            width: captured.physical_width as i32,
            height: captured.physical_height as i32,
            scale_factor: Some(captured.scale_factor),
            monitor: captured.monitor_index.map(|i| i as i32),
            window_title: None,
            window_app_name: None,
            window_id: None,
            element_index_text: None,
        })
    }
}

/// Encode a DynamicImage as base64 PNG.
fn encode_image_to_base64(image: &image::DynamicImage) -> Result<String, String> {
    use base64::Engine;
    let mut png_bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut png_bytes, image::ImageFormat::Png)
        .map_err(|e| format!("Failed to encode PNG: {}", e))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(png_bytes.into_inner()))
}

/// Capture a specific window by matching criteria.
fn capture_window_screenshot(
    window_title: Option<String>,
    app_name: Option<String>,
    window_id: Option<u32>,
) -> Result<AnnotatedScreenshotData, String> {
    use xcap::Window;

    let windows = Window::all().map_err(|e| format!("Failed to enumerate windows: {}", e))?;

    let target = if let Some(id) = window_id {
        windows
            .iter()
            .find(|w| w.id().unwrap_or(0) == id)
            .ok_or_else(|| format!("No window found with id {}", id))?
    } else if let Some(ref title_query) = window_title {
        let query_lower = title_query.to_lowercase();
        windows
            .iter()
            .find(|w| {
                w.title()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query_lower)
            })
            .ok_or_else(|| {
                let available: Vec<String> = windows
                    .iter()
                    .filter_map(|w| {
                        let t = w.title().unwrap_or_default();
                        if t.is_empty() {
                            None
                        } else {
                            Some(t)
                        }
                    })
                    .take(10)
                    .collect();
                format!(
                    "No window found matching title '{}'. Available: {:?}",
                    title_query, available
                )
            })?
    } else if let Some(ref app_query) = app_name {
        let query_lower = app_query.to_lowercase();
        windows
            .iter()
            .find(|w| {
                w.app_name()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query_lower)
            })
            .ok_or_else(|| {
                let available: Vec<String> = windows
                    .iter()
                    .filter_map(|w| {
                        let a = w.app_name().unwrap_or_default();
                        if a.is_empty() {
                            None
                        } else {
                            Some(a)
                        }
                    })
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .take(10)
                    .collect();
                format!(
                    "No window found matching app_name '{}'. Available: {:?}",
                    app_query, available
                )
            })?
    } else {
        return Err("No window selection criteria provided".to_string());
    };

    let title = target.title().unwrap_or_default();
    let app = target.app_name().unwrap_or_default();
    let id = target.id().unwrap_or(0);

    if target.is_minimized().unwrap_or(false) {
        return Err(format!(
            "Window '{}' ({}) is minimized — cannot capture",
            title, app
        ));
    }

    let image = target
        .capture_image()
        .map_err(|e| format!("Failed to capture window '{}': {}", title, e))?;

    // Determine the scale factor from the monitor the window is on
    let scale = {
        let win_x = target.x().unwrap_or(0);
        let win_y = target.y().unwrap_or(0);
        screen::MonitorManager::detect()
            .ok()
            .and_then(|mgr| mgr.at_logical_point(win_x, win_y).map(|m| m.scale_factor))
            .unwrap_or(1.0)
    };

    let width = image.width() as i32;
    let height = image.height() as i32;
    let dynamic = image::DynamicImage::ImageRgba8(image);
    let b64 = encode_image_to_base64(&dynamic)?;

    Ok(AnnotatedScreenshotData {
        screenshot: b64,
        width,
        height,
        scale_factor: Some(scale),
        monitor: None,
        window_title: Some(title),
        window_app_name: Some(app),
        window_id: Some(id),
        element_index_text: None,
    })
}

/// Capture the runner's own window by cropping from a monitor screenshot.
/// xcap skips same-process windows, so we capture the monitor and crop.
///
/// DPI handling:
/// - Tauri `outer_position()` / `outer_size()` return physical pixels.
/// - xcap `Monitor::x()` / `y()` return logical coordinates (dmPosition).
/// - xcap `Monitor::width()` / `height()` return physical pixels (dmPelsWidth/Height).
/// - The captured image is at physical resolution.
///
/// To match monitors: convert Tauri physical position to logical using scale_factor.
/// To crop the image: work in physical pixels (image coords = physical).
fn capture_runner_window(
    phys_x: i32,
    phys_y: i32,
    phys_w: u32,
    phys_h: u32,
    scale: f64,
    title: &str,
) -> Result<AnnotatedScreenshotData, String> {
    let mgr = screen::MonitorManager::detect()?;

    // Convert Tauri physical position to logical for monitor matching.
    // Use the window center so partially-off-screen windows still find the right monitor.
    let logical_x = (phys_x as f64 / scale) as i32;
    let logical_y = (phys_y as f64 / scale) as i32;
    let logical_center_x = logical_x + (phys_w as f64 / scale / 2.0) as i32;
    let logical_center_y = logical_y + (phys_h as f64 / scale / 2.0) as i32;

    let monitor = mgr
        .at_logical_point(logical_center_x, logical_center_y)
        .ok_or_else(|| "Runner window not on any monitor".to_string())?;

    let captured = screen::CapturedScreenshot::from_monitor(&mgr, monitor.index)?;

    // Convert logical window position to physical pixel offset in the captured image.
    let (rel_local_x, rel_local_y) = monitor.to_monitor_local(logical_x, logical_y);
    let (rel_phys_x, rel_phys_y) = monitor.logical_to_physical(rel_local_x, rel_local_y);

    // Handle negative offsets (window partially off-screen)
    let crop_x = rel_phys_x.max(0) as u32;
    let crop_y = rel_phys_y.max(0) as u32;
    let crop_w = if rel_phys_x < 0 {
        phys_w.saturating_sub((-rel_phys_x) as u32)
    } else {
        phys_w
    }
    .min(captured.physical_width.saturating_sub(crop_x));
    let crop_h = if rel_phys_y < 0 {
        phys_h.saturating_sub((-rel_phys_y) as u32)
    } else {
        phys_h
    }
    .min(captured.physical_height.saturating_sub(crop_y));

    if crop_w == 0 || crop_h == 0 {
        return Err(format!(
            "Runner window has zero visible area (crop: {}x{} at ({}, {}), image: {}x{}, scale: {})",
            crop_w,
            crop_h,
            crop_x,
            crop_y,
            captured.physical_width,
            captured.physical_height,
            monitor.scale_factor
        ));
    }

    let cropped = captured.image.crop_imm(crop_x, crop_y, crop_w, crop_h);
    let b64 = encode_image_to_base64(&cropped)?;

    Ok(AnnotatedScreenshotData {
        screenshot: b64,
        width: crop_w as i32,
        height: crop_h as i32,
        scale_factor: Some(scale),
        monitor: None,
        window_title: Some(title.to_string()),
        window_app_name: Some("Qontinui Runner".to_string()),
        window_id: None,
        element_index_text: None,
    })
}

/// Capture the runner's own window as a base64 PNG string.
///
/// This is a convenience wrapper used by the snapshot and health fallback paths.
/// Returns `Some((base64_png, width, height))` on success, `None` on failure.
/// Does not require the UI Bridge SDK — uses native xcap capture.
pub async fn capture_runner_window_base64(state: &Arc<ApiState>) -> Option<(String, i32, i32)> {
    use tauri::Manager;

    let window = state
        .app_handle
        .get_webview_window(qontinui_runner_lib::get_main_window_label())?;
    let scale = window.scale_factor().unwrap_or(1.0);
    let pos = window.inner_position().unwrap_or_default();
    let size = window.inner_size().unwrap_or_default();
    let x = pos.x;
    let y = pos.y;
    let w = size.width;
    let h = size.height;
    let title = window
        .title()
        .unwrap_or_else(|_| "Qontinui Runner".to_string());

    match tokio::task::spawn_blocking(move || capture_runner_window(x, y, w, h, scale, &title))
        .await
    {
        Ok(Ok(data)) => Some((data.screenshot, data.width, data.height)),
        Ok(Err(e)) => {
            warn!("Native capture fallback failed: {}", e);
            None
        }
        Err(e) => {
            warn!("Native capture task join error: {}", e);
            None
        }
    }
}

/// Capture a full monitor screenshot.
fn capture_monitor_screenshot(
    monitor_index: Option<i32>,
) -> Result<AnnotatedScreenshotData, String> {
    if let Some(idx) = monitor_index {
        if idx < 0 {
            return Err(format!("Monitor index must be non-negative, got {}", idx));
        }
    }

    let mgr = screen::MonitorManager::detect()?;
    let idx = monitor_index
        .map(|i| i as usize)
        .unwrap_or_else(|| mgr.primary_index());
    let captured = screen::CapturedScreenshot::from_monitor(&mgr, idx)?;
    let mut data = AnnotatedScreenshotData::from_captured(&captured)?;
    data.monitor = monitor_index;
    Ok(data)
}

/// GET /ui-bridge/control/annotated-screenshot — Screenshot with metadata
///
/// Captures natively via xcap (Rust). No Python executor dependency.
///
/// Query params (all optional, first match wins):
/// - `runner=true` — capture the runner's own Tauri window
/// - `window_title=...` — case-insensitive substring match on window title
/// - `app_name=...` — case-insensitive substring match on app name
/// - `window_id=N` — exact window ID (HWND)
/// - `monitor=N` — full monitor capture (0-based index, default: primary)
/// - (none) — captures primary monitor
pub async fn ui_bridge_annotated_screenshot_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AnnotatedScreenshotQuery>,
) -> Json<ApiResponse<AnnotatedScreenshotData>> {
    let want_annotate = query.annotate.unwrap_or(false);
    let max_width = query.max_width.unwrap_or(1024);

    let is_window_capture = query.runner.unwrap_or(false)
        || query.window_title.is_some()
        || query.app_name.is_some()
        || query.window_id.is_some();

    // --- Phase 1: Capture the raw screenshot ---
    let capture_result: Result<AnnotatedScreenshotData, String> = if is_window_capture {
        info!(
            runner = ?query.runner,
            window_title = ?query.window_title,
            app_name = ?query.app_name,
            window_id = ?query.window_id,
            "UI Bridge API: Capturing window screenshot (native)"
        );

        // For runner's own window, xcap skips same-process windows,
        // so we capture the monitor and crop to the window bounds.
        if query.runner.unwrap_or(false) {
            use tauri::Manager;
            let window = state
                .app_handle
                .get_webview_window(qontinui_runner_lib::get_main_window_label());
            if let Some(win) = window {
                let scale = win.scale_factor().unwrap_or(1.0);
                // Use inner_position/inner_size for the content area (viewport).
                // Element bounds from the UI Bridge SDK are relative to the viewport,
                // not the outer window frame (which includes title bar).
                let pos = win.inner_position().unwrap_or_default();
                let size = win.inner_size().unwrap_or_default();
                let x = pos.x;
                let y = pos.y;
                let w = size.width;
                let h = size.height;
                let title = win
                    .title()
                    .unwrap_or_else(|_| "Qontinui Runner".to_string());

                match tokio::task::spawn_blocking(move || {
                    capture_runner_window(x, y, w, h, scale, &title)
                })
                .await
                {
                    Ok(Ok(data)) => {
                        info!(
                            "UI Bridge screenshot: Captured runner window ({}x{})",
                            data.width, data.height
                        );
                        Ok(data)
                    }
                    Ok(Err(e)) => {
                        error!("UI Bridge screenshot: Runner capture failed: {}", e);
                        Err(format!("Runner screenshot failed: {}", e))
                    }
                    Err(e) => {
                        error!("UI Bridge screenshot: Task join error: {}", e);
                        Err(format!("Screenshot capture task failed: {}", e))
                    }
                }
            } else {
                Err("Runner window not found".to_string())
            }
        } else {
            let window_title = query.window_title;
            let app_name = query.app_name;
            let window_id = query.window_id;

            match tokio::task::spawn_blocking(move || {
                capture_window_screenshot(window_title, app_name, window_id)
            })
            .await
            {
                Ok(Ok(data)) => {
                    info!(
                        "UI Bridge screenshot: Captured window '{}' ({}x{}, id={})",
                        data.window_title.as_deref().unwrap_or("?"),
                        data.width,
                        data.height,
                        data.window_id.unwrap_or(0),
                    );
                    Ok(data)
                }
                Ok(Err(e)) => {
                    error!("UI Bridge screenshot: Window capture failed: {}", e);
                    Err(format!("Window screenshot failed: {}", e))
                }
                Err(e) => {
                    error!("UI Bridge screenshot: Task join error: {}", e);
                    Err(format!("Screenshot capture task failed: {}", e))
                }
            }
        }
    } else {
        // Full monitor capture (existing behavior)
        info!(
            monitor = ?query.monitor,
            "UI Bridge API: Capturing monitor screenshot (native)"
        );

        let monitor = query.monitor;
        match tokio::task::spawn_blocking(move || capture_monitor_screenshot(monitor)).await {
            Ok(Ok(data)) => {
                info!(
                    "UI Bridge screenshot: Captured {}x{} from monitor {:?}",
                    data.width, data.height, data.monitor
                );
                Ok(data)
            }
            Ok(Err(e)) => {
                error!("UI Bridge screenshot: Monitor capture failed: {}", e);
                Err(format!("Screenshot capture failed: {}", e))
            }
            Err(e) => {
                error!("UI Bridge screenshot: Task join error: {}", e);
                Err(format!("Screenshot capture task failed: {}", e))
            }
        }
    };

    // --- Phase 2: If annotate=true, overlay element bounding boxes ---
    let mut data = match capture_result {
        Ok(d) => d,
        Err(e) => return Json(ApiResponse::error(e)),
    };

    if want_annotate {
        match apply_annotation(&state, &mut data, max_width).await {
            Ok(()) => {
                info!(
                    "UI Bridge screenshot: Annotation applied (max_width={})",
                    max_width
                );
            }
            Err(e) => {
                warn!(
                    "UI Bridge screenshot: Annotation failed, returning raw screenshot: {}",
                    e
                );
                // Fall through — return the unannotated screenshot
            }
        }
    }

    Json(ApiResponse::success(data))
}

/// Apply element annotation overlay to a captured screenshot.
///
/// Calls UI Bridge discover to get elements, converts them to `AnnotatedElement`,
/// and runs the annotation engine to overlay numbered bounding boxes.
async fn apply_annotation(
    state: &Arc<ApiState>,
    data: &mut AnnotatedScreenshotData,
    max_width: u32,
) -> Result<(), String> {
    use crate::vision::annotator::{annotate_screenshot, AnnotatedElement};
    use crate::vision::types::NormalizedRect;

    // Discover elements from the UI Bridge SDK
    let discover_payload = serde_json::json!({ "interactive_only": false });
    let discover_data = ui_bridge_request_sync(state, "discover", discover_payload)
        .await
        .map_err(|e| format!("Discover failed: {}", e))?;

    // Extract elements array — may be at top-level or nested under "data"
    let elements_arr = discover_data
        .get("elements")
        .and_then(|v| v.as_array())
        .or_else(|| {
            discover_data
                .get("data")
                .and_then(|d| d.get("elements"))
                .and_then(|v| v.as_array())
        });

    let elements_arr = match elements_arr {
        Some(arr) => arr,
        None => {
            // No elements found — annotate with empty list (adds "No interactive elements" text)
            let result = annotate_screenshot(&data.screenshot, &[], max_width)
                .map_err(|e| format!("Annotation engine failed: {}", e))?;
            data.screenshot = result.annotated_base64;
            data.element_index_text = Some(result.element_index_text);
            return Ok(());
        }
    };

    // Convert discover elements to AnnotatedElement
    let interactive_types = [
        "button", "input", "select", "textarea", "link", "checkbox", "radio", "a",
    ];

    let mut annotated_elements = Vec::new();
    let mut index = 1_u32;

    for el in elements_arr {
        let el_type = el.get("type").and_then(|v| v.as_str()).unwrap_or("");

        // Filter to interactive elements only
        if !interactive_types.contains(&el_type) {
            continue;
        }

        // Skip hidden elements
        if let Some(state_val) = el.get("state") {
            let visible = state_val
                .get("visible")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if !visible {
                continue;
            }
        }

        // Extract normalized rect — try normalizedRect first, then rect
        let normalized_rect = extract_normalized_rect_from_element(el);
        let Some(normalized_rect) = normalized_rect else {
            continue;
        };

        let label = el
            .get("label")
            .and_then(|v| v.as_str())
            .or_else(|| {
                el.get("state")
                    .and_then(|s| s.get("textContent"))
                    .and_then(|v| v.as_str())
            })
            .or_else(|| el.get("accessibleName").and_then(|v| v.as_str()))
            .unwrap_or("(unlabeled)")
            .to_string();

        // Truncate long labels
        let label = if label.chars().count() > 40 {
            let truncated: String = label.chars().take(37).collect();
            format!("{}...", truncated)
        } else {
            label
        };

        annotated_elements.push(AnnotatedElement {
            index,
            label,
            element_type: el_type.to_string(),
            normalized_rect,
        });

        index += 1;
        if index > 50 {
            debug!("Annotated screenshot: capped at 50 elements");
            break;
        }
    }

    info!(
        "Annotated screenshot: {} elements to overlay",
        annotated_elements.len()
    );

    // Run the annotation engine
    let result = annotate_screenshot(&data.screenshot, &annotated_elements, max_width)
        .map_err(|e| format!("Annotation engine failed: {}", e))?;

    data.screenshot = result.annotated_base64;
    data.element_index_text = Some(result.element_index_text);

    Ok(())
}

/// Extract a NormalizedRect from a discover element JSON value.
///
/// Tries normalizedRect first (already 0-1), then falls back to rect
/// with viewport heuristic normalization.
fn extract_normalized_rect_from_element(
    el: &serde_json::Value,
) -> Option<crate::vision::types::NormalizedRect> {
    use crate::vision::types::NormalizedRect;

    // Try normalizedRect (UI Bridge provides this directly)
    if let Some(nr) = el
        .get("normalizedRect")
        .or_else(|| el.get("state").and_then(|s| s.get("normalizedRect")))
    {
        let x = nr.get("x").and_then(|v| v.as_f64())? as f32;
        let y = nr.get("y").and_then(|v| v.as_f64())? as f32;
        let width = nr.get("width").and_then(|v| v.as_f64())? as f32;
        let height = nr.get("height").and_then(|v| v.as_f64())? as f32;
        return Some(NormalizedRect {
            x,
            y,
            width,
            height,
        });
    }

    // Fallback: use rect with absolute pixel coords
    let rect = el
        .get("rect")
        .or_else(|| el.get("state").and_then(|s| s.get("rect")))?;

    let x = rect.get("x").and_then(|v| v.as_f64())? as f32;
    let y = rect.get("y").and_then(|v| v.as_f64())? as f32;
    let w = rect.get("width").and_then(|v| v.as_f64())? as f32;
    let h = rect.get("height").and_then(|v| v.as_f64())? as f32;

    if w > 0.0 && h > 0.0 {
        // If coords look already normalized (all < 1.5), pass through
        if x < 1.5 && y < 1.5 && w < 1.5 && h < 1.5 {
            return Some(NormalizedRect {
                x,
                y,
                width: w,
                height: h,
            });
        }
        // Otherwise assume pixel coords with 1920x1080 default
        return Some(NormalizedRect {
            x: x / 1920.0,
            y: y / 1080.0,
            width: w / 1920.0,
            height: h / 1080.0,
        });
    }

    None
}

// ============================================================================
// Keyboard Shortcuts Handler
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
// Idle Detection Handlers
// ============================================================================

/// Wait for an element to appear in the UI.
///
/// Polls via one of two backends until an element matches:
/// - `query` → polls `ai_find` (natural language + optional type filter)
/// - `elementId` → polls `get_element` (direct id lookup)
///
/// Exactly one of `query` or `elementId` must be provided. On match, returns
/// `{found: true, element, elapsed_ms, polls}`. On timeout, returns
/// `408 Request Timeout` with a descriptive error and the poll count.
///
/// Body fields (all optional unless noted):
/// - `query` (string) — natural language query for `ai_find`
/// - `type` (string) — element type filter passed to `ai_find`
/// - `elementId` (string) — direct element id to look up
/// - `timeout` (number, default 5000) — max wait in ms
/// - `pollIntervalMs` (number, default 200) — ms between polls
/// - `minConfidence` (number, default 0.5) — only for query mode;
///   ai_find results below this confidence are treated as "not found"
/// - `assertions` (array, optional) — additional conditions the element
///   must satisfy after it has been found. Each assertion is evaluated on
///   every poll; if any fail, the find is treated as a miss and polling
///   continues until the timeout expires. On timeout the last failing
///   assertion is reported, distinguishing "element never appeared" from
///   "element appeared but assertion failed".
///
///   Supported assertion `kind`s:
///   - `{"kind":"visible"}` — element bounds have non-zero area.
///   - `{"kind":"text_contains","value":"...","caseInsensitive":bool}`
///   - `{"kind":"text_equals","value":"...","caseInsensitive":bool}`
///   - `{"kind":"attribute_equals","name":"aria-pressed","value":"true"}`
pub async fn ui_bridge_wait_for_element_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let body = body
        .map(|Json(v)| v)
        .unwrap_or_else(|| serde_json::json!({}));

    let query = body.get("query").and_then(|v| v.as_str()).map(String::from);
    let element_id = body
        .get("elementId")
        .and_then(|v| v.as_str())
        .map(String::from);
    let element_type = body.get("type").and_then(|v| v.as_str()).map(String::from);
    let timeout_ms = body.get("timeout").and_then(|v| v.as_u64()).unwrap_or(5000);
    let poll_interval_ms = body
        .get("pollIntervalMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(200)
        .max(50); // floor at 50ms to avoid hammering the frontend
    let min_confidence = body
        .get("minConfidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.5);
    let assertions: Vec<serde_json::Value> = body
        .get("assertions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Exactly one of query / elementId must be provided.
    match (query.as_deref(), element_id.as_deref()) {
        (None, None) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(
                    "wait-for-element: provide either 'query' or 'elementId'",
                )),
            ));
        }
        (Some(_), Some(_)) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(
                    "wait-for-element: 'query' and 'elementId' are mutually exclusive",
                )),
            ));
        }
        _ => {}
    }

    info!(
        "UI Bridge API: wait-for-element query={:?} elementId={:?} timeout={}ms",
        query, element_id, timeout_ms
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let start = std::time::Instant::now();
    let mut polls: u32 = 0;
    // Track the most recent assertion failure so we can surface it on timeout.
    // None = no assertions evaluated yet OR last poll's element was missing.
    let mut last_assertion_failure: Option<String> = None;

    loop {
        polls += 1;

        // Attempt a single match.
        let match_result: Result<Option<serde_json::Value>, String> = if let Some(q) = &query {
            // Natural-language query via ai_find.
            let mut params = serde_json::json!({ "query": q });
            if let Some(t) = &element_type {
                params["type"] = serde_json::Value::from(t.clone());
            }
            let payload = serde_json::json!({ "params": params });
            match ui_bridge_request_sync(&state, "ai_find", payload).await {
                Ok(data) => Ok(extract_ai_find_match(&data, min_confidence)),
                Err(e) => Err(e),
            }
        } else {
            // Direct id lookup via get_element. A successful lookup with a
            // non-null `element` field counts as a match.
            let id = element_id.as_deref().unwrap_or("");
            match ui_bridge_request_sync(
                &state,
                "get_element",
                serde_json::json!({ "elementId": id }),
            )
            .await
            {
                Ok(data) => Ok(extract_get_element_match(&data)),
                Err(e) => {
                    // Treat "element not found" as a miss rather than a
                    // transport error: the frontend may return an error
                    // payload when the element doesn't exist yet.
                    let msg = e.to_lowercase();
                    if msg.contains("not found") || msg.contains("does not exist") {
                        Ok(None)
                    } else {
                        Err(e)
                    }
                }
            }
        };

        match match_result {
            Ok(Some(element)) => {
                // Element exists. If the caller supplied assertions,
                // evaluate them now and treat any failure as a continuing
                // miss (so we keep polling until timeout).
                if !assertions.is_empty() {
                    match evaluate_wait_for_element_assertions(&element, &assertions) {
                        Ok(()) => {
                            let elapsed_ms = start.elapsed().as_millis() as u64;
                            return Ok(Json(ApiResponse::success(serde_json::json!({
                                "found": true,
                                "element": element,
                                "elapsed_ms": elapsed_ms,
                                "polls": polls,
                                "assertions_passed": assertions.len(),
                            }))));
                        }
                        Err(reason) => {
                            last_assertion_failure = Some(reason);
                            // Fall through to timeout check + sleep.
                        }
                    }
                } else {
                    let elapsed_ms = start.elapsed().as_millis() as u64;
                    return Ok(Json(ApiResponse::success(serde_json::json!({
                        "found": true,
                        "element": element,
                        "elapsed_ms": elapsed_ms,
                        "polls": polls,
                    }))));
                }
            }
            Ok(None) => {
                // Not found yet — fall through to timeout check + sleep.
                last_assertion_failure = None;
            }
            Err(e) => {
                error!("UI Bridge API: wait-for-element transport error: {}", e);
                return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))));
            }
        }

        // Timeout check: if the next poll would overshoot the deadline,
        // give up now rather than sleep-then-check.
        if std::time::Instant::now() + std::time::Duration::from_millis(poll_interval_ms) > deadline
        {
            let elapsed_ms = start.elapsed().as_millis() as u64;
            let descriptor = if let Some(q) = &query {
                format!("query='{}'", q)
            } else if let Some(id) = &element_id {
                format!("elementId='{}'", id)
            } else {
                String::from("(no selector)")
            };
            let msg = if let Some(reason) = &last_assertion_failure {
                format!(
                    "wait-for-element: timeout after {}ms ({} polls, {}) — element appeared but assertion failed: {}",
                    elapsed_ms, polls, descriptor, reason
                )
            } else {
                format!(
                    "wait-for-element: timeout after {}ms ({} polls, {})",
                    elapsed_ms, polls, descriptor
                )
            };
            info!("UI Bridge API: {}", msg);
            return Err((StatusCode::REQUEST_TIMEOUT, Json(api_error(msg))));
        }

        tokio::time::sleep(std::time::Duration::from_millis(poll_interval_ms)).await;
    }
}

/// Evaluate the `assertions[]` array for the wait-for-element handler.
///
/// Returns `Ok(())` if all assertions pass; `Err(reason)` with a
/// human-readable message describing the first failing assertion. Used by
/// `ui_bridge_wait_for_element_handler` to keep polling until either the
/// element matches every assertion or the deadline expires.
///
/// Each assertion entry is a JSON object with a required `kind` field.
/// See the wait-for-element handler doc-comment for the supported kinds.
fn evaluate_wait_for_element_assertions(
    element: &serde_json::Value,
    assertions: &[serde_json::Value],
) -> Result<(), String> {
    fn text_of(element: &serde_json::Value) -> String {
        // Prefer live DOM reads (state.textContent / state.value) over the
        // cached top-level `label`/`text`. The registered element's label is
        // captured once at registration time, so a wait-for-element assertion
        // polling for in-place text changes (e.g., an overlay transitioning
        // from "Understanding your request..." to "Done") would never see the
        // update if we consulted `label` first. `state.*` fields are re-read
        // from the DOM on every get_element IPC call, so they reflect the
        // current render.
        let state = element.get("state");
        for key in ["textContent", "value", "innerText"] {
            if let Some(s) = state.and_then(|v| v.get(key)).and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
        // Fall back to cached / top-level fields. These are still useful for
        // ai_find results and for elements whose semantic label differs from
        // their visible text (e.g., icon buttons with aria-label).
        for key in [
            "text",
            "label",
            "accessibleName",
            "innerText",
            "textContent",
        ] {
            if let Some(s) = element.get(key).and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
        String::new()
    }

    fn attr_of(element: &serde_json::Value, name: &str) -> Option<String> {
        // Try multiple shapes — different IPC results expose attributes
        // differently. We accept either a flat `attributes` map or a
        // nested `state.attributes` object.
        for prefix in ["attributes", "state.attributes"] {
            let mut node = element;
            for key in prefix.split('.') {
                node = match node.get(key) {
                    Some(n) => n,
                    None => break,
                };
            }
            if let Some(v) = node.get(name).and_then(|v| v.as_str()) {
                return Some(v.to_string());
            }
        }
        None
    }

    for (idx, assertion) in assertions.iter().enumerate() {
        let kind = assertion
            .get("kind")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("assertion #{idx} missing 'kind'"))?;

        match kind {
            "visible" => {
                // Look for a non-zero bounding rect anywhere in the element.
                let bounds = element
                    .get("bounds")
                    .or_else(|| element.get("rect"))
                    .or_else(|| element.get("state").and_then(|s| s.get("rect")));
                let (w, h) = match bounds {
                    Some(b) => (
                        b.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        b.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    ),
                    None => (0.0, 0.0),
                };
                if w <= 0.0 || h <= 0.0 {
                    return Err(format!("visible: element bounds are {}x{}", w, h));
                }
            }
            "text_contains" | "text_equals" => {
                let needle = assertion
                    .get("value")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| format!("{kind}: 'value' is required"))?;
                let case_insensitive = assertion
                    .get("caseInsensitive")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let actual = text_of(element);
                let (actual_cmp, needle_cmp) = if case_insensitive {
                    (actual.to_lowercase(), needle.to_lowercase())
                } else {
                    (actual.clone(), needle.to_string())
                };
                let pass = if kind == "text_contains" {
                    actual_cmp.contains(&needle_cmp)
                } else {
                    actual_cmp == needle_cmp
                };
                if !pass {
                    return Err(format!(
                        "{kind}: expected '{needle}' got '{}' (caseInsensitive={case_insensitive})",
                        actual
                    ));
                }
            }
            "attribute_equals" => {
                let name = assertion
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| format!("attribute_equals: 'name' is required"))?;
                let expected = assertion
                    .get("value")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| format!("attribute_equals: 'value' is required"))?;
                let actual = attr_of(element, name);
                if actual.as_deref() != Some(expected) {
                    return Err(format!(
                        "attribute_equals: {name}='{expected}' but actual='{}'",
                        actual.unwrap_or_else(|| "<missing>".to_string())
                    ));
                }
            }
            other => {
                return Err(format!("unsupported assertion kind '{other}'"));
            }
        }
    }
    Ok(())
}

/// POST /ui-bridge/ai/expect
///
/// Wait for a substring to appear anywhere in the page text. Polls
/// `document.body.innerText` via page_evaluate until the text is found
/// or the timeout elapses. Useful for assertions during manual/auto
/// testing without writing custom JS expressions.
///
/// Body fields:
/// - `text` (string, required) — substring to wait for (case-sensitive
///   unless `caseInsensitive: true`)
/// - `timeout` (number, default 5000) — max wait in ms
/// - `pollIntervalMs` (number, default 250) — ms between polls (min 50)
/// - `caseInsensitive` (bool, default false) — match without regard to case
///
/// Returns 200 with `{ found: true, elapsed_ms, polls }` on success or
/// 408 Request Timeout with a descriptive error message on timeout.
/// Returns 500 if transport errors persist across 3 consecutive polls.
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
    // literal. This handles every edge case (newlines, U+2028/U+2029,
    // backticks, backslashes, quotes) correctly — unlike ad-hoc escaping.
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
                // Extract the boolean result. The SDK stringifies non-string
                // values, so `result.value` may be either a JSON bool or the
                // string "true"/"false" depending on the code path.
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
                // Transport error (frontend not ready, timeout, JS syntax).
                // Tolerate a few transient failures, but bail out after 3
                // consecutive errors so callers get a useful message instead
                // of a misleading 408.
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

        // Deadline check: if we're already past the deadline, give up.
        // Otherwise sleep, but clamp the sleep to (deadline - now) so the
        // next poll fires right at the deadline rather than one full poll
        // interval early.
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

    match ui_bridge_request_sync(&state, "wait_for_idle", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Wait for idle failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
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

// ============================================================================
// Tier 3.1 — Wait-for-element with structured selector and condition
// ============================================================================

/// POST /ui-bridge/ai/wait-for-element-condition
///
/// Forwards to the JS SDK's `waitForElementByCondition` handler which uses
/// registry-based polling and supports structured selectors (id, title,
/// aria_label, text, type) and conditions (present / visible / clickable /
/// text-matches).
///
/// Request body mirrors `WaitForElementByConditionRequest` in the SDK types.
/// Returns 200 `{ matched: bool, element?: ..., waited_ms: number }` on
/// success, or 408 when the SDK reports matched=false (timeout).
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

// ============================================================================
// Tier 3.2 — Mixed action/wait/snapshot batch execution
// ============================================================================

/// POST /ui-bridge/control/batch-execute
///
/// Executes a heterogeneous sequence of steps serially:
///   `{ type: "action", element_id, action, params? }`
///   `{ type: "wait", ms }`
///   `{ type: "snapshot" }`
///
/// Forwards each step to the appropriate IPC handler. `stop_on_error` (default
/// true) aborts on the first failure.
///
/// Response: `{ results: [...], completed: N, total: N }`
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
// Stuck Screen Diagnosis Handler
// ============================================================================

/// Internal capture result for diagnosis.
struct DiagnosisCapture {
    image: image::DynamicImage,
    base64: String,
    width: i32,
    height: i32,
    source: String,
}

/// Capture the runner window for diagnosis, falling back to primary monitor.
fn capture_for_diagnosis(app_handle: &tauri::AppHandle) -> Result<DiagnosisCapture, String> {
    use base64::Engine;
    use tauri::Manager;

    // Try runner window first
    if let Some(win) = app_handle.get_webview_window(qontinui_runner_lib::get_main_window_label()) {
        let scale = win.scale_factor().unwrap_or(1.0);
        let pos = win.outer_position().unwrap_or_default();
        let size = win.outer_size().unwrap_or_default();

        if size.width > 0 && size.height > 0 {
            match capture_runner_window(
                pos.x,
                pos.y,
                size.width,
                size.height,
                scale,
                "Qontinui Runner",
            ) {
                Ok(data) => {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(&data.screenshot)
                        .map_err(|e| format!("Base64 decode failed: {}", e))?;
                    let img = image::load_from_memory(&bytes)
                        .map_err(|e| format!("Image decode failed: {}", e))?;
                    return Ok(DiagnosisCapture {
                        image: img,
                        base64: data.screenshot,
                        width: data.width,
                        height: data.height,
                        source: "runner_window".to_string(),
                    });
                }
                Err(e) => {
                    warn!(
                        "Runner window capture failed, falling back to monitor: {}",
                        e
                    );
                }
            }
        }
    }

    // Fallback: primary monitor
    let data = capture_monitor_screenshot(None)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&data.screenshot)
        .map_err(|e| format!("Base64 decode failed: {}", e))?;
    let img = image::load_from_memory(&bytes).map_err(|e| format!("Image decode failed: {}", e))?;
    Ok(DiagnosisCapture {
        image: img,
        base64: data.screenshot,
        width: data.width,
        height: data.height,
        source: "primary_monitor".to_string(),
    })
}

/// Native pixel-sampling similarity comparator (no WebView required).
///
/// Samples ~10,000 pixels evenly across the image pair and returns a
/// 0.0–1.0 similarity score. Used exclusively by `diagnose-stuck` which
/// runs when the React frontend hasn't loaded and the browser-side
/// `compareVisualRegression` (canvas-based, accurate) is unavailable.
///
/// This is intentionally a rough approximation — it exists to detect
/// "screen is frozen" vs "screen is changing", not for pixel-accurate
/// regression testing. Do not replace with the IPC-based image-diff
/// endpoint, which requires a functioning WebView.
fn sampled_pixel_similarity_native(img1: &image::DynamicImage, img2: &image::DynamicImage) -> f64 {
    let rgba1 = img1.to_rgba8();
    let rgba2 = img2.to_rgba8();

    if rgba1.dimensions() != rgba2.dimensions() {
        return 0.0;
    }

    let (w, h) = rgba1.dimensions();
    let total = w as u64 * h as u64;
    if total == 0 {
        return 1.0;
    }

    let pixels1 = rgba1.as_raw();
    let pixels2 = rgba2.as_raw();

    // Sample ~10,000 pixels evenly for speed
    let step = (total / 10_000u64).max(1) as usize;
    let mut matching = 0u64;
    let mut sampled = 0u64;

    for i in (0..total as usize).step_by(step) {
        let offset = i * 4;
        if offset + 3 >= pixels1.len() {
            break;
        }

        let diff: u32 = (0..4)
            .map(|c| (pixels1[offset + c] as i32 - pixels2[offset + c] as i32).unsigned_abs())
            .sum();

        // Tolerance for rendering/compression artifacts
        if diff <= 20 {
            matching += 1;
        }
        sampled += 1;
    }

    if sampled == 0 {
        1.0
    } else {
        matching as f64 / sampled as f64
    }
}

/// Try to get DOM-based idle signals from the React frontend.
/// Returns None if React hasn't mounted or doesn't respond within timeout.
async fn try_get_dom_signals(state: &Arc<ApiState>, timeout_ms: u64) -> Option<serde_json::Value> {
    match tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        ui_bridge_request_sync(state, "get_idle_status", serde_json::json!({})),
    )
    .await
    {
        Ok(Ok(data)) => Some(data),
        _ => None,
    }
}

/// Diagnose whether the app is stuck on a loading screen.
///
/// Uses native screenshot capture (xcap) to compare visual state across an
/// observation window. Optionally enriches with DOM signals from the React
/// UI Bridge if it's responsive. Works even if React hasn't mounted.
pub async fn ui_bridge_diagnose_stuck_screen_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<ApiResponse<serde_json::Value>> {
    info!("UI Bridge API: Diagnose stuck screen (native)");

    let observation_ms = body
        .get("observationWindowMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(3000);

    // Phase 1: Capture initial screenshot
    let app_handle1 = state.app_handle.clone();
    let cap1 = match tokio::task::spawn_blocking(move || capture_for_diagnosis(&app_handle1)).await
    {
        Ok(Ok(cap)) => cap,
        Ok(Err(e)) => {
            error!("Diagnosis: initial screenshot failed: {}", e);
            return Json(ApiResponse::error(format!(
                "Screenshot capture failed: {}",
                e
            )));
        }
        Err(e) => {
            return Json(ApiResponse::error(format!("Capture task failed: {}", e)));
        }
    };

    // Phase 2: Try to get DOM signals (short timeout — don't block if React hasn't mounted)
    let dom_status1 = try_get_dom_signals(&state, 2000).await;

    // Phase 3: Wait observation window
    tokio::time::sleep(std::time::Duration::from_millis(observation_ms)).await;

    // Phase 4: Capture second screenshot
    let app_handle2 = state.app_handle.clone();
    let cap2 = match tokio::task::spawn_blocking(move || capture_for_diagnosis(&app_handle2)).await
    {
        Ok(Ok(cap)) => cap,
        Ok(Err(e)) => {
            error!("Diagnosis: second screenshot failed: {}", e);
            return Json(ApiResponse::error(format!(
                "Second screenshot capture failed: {}",
                e
            )));
        }
        Err(e) => {
            return Json(ApiResponse::error(format!("Capture task failed: {}", e)));
        }
    };

    // Phase 5: Try DOM signals again
    let dom_status2 = try_get_dom_signals(&state, 2000).await;

    // Phase 6: Compare screenshots
    let img1 = cap1.image;
    let img2 = cap2.image;
    let similarity =
        tokio::task::spawn_blocking(move || sampled_pixel_similarity_native(&img1, &img2))
            .await
            .unwrap_or(0.5); // Couldn't compare — inconclusive
    let screenshot_changed = similarity < 0.95;

    // Phase 7: Extract DOM signal details
    let ui_bridge_responsive = dom_status1.is_some() || dom_status2.is_some();

    let dom_ref = dom_status2.as_ref().or(dom_status1.as_ref());
    let signals = dom_ref.and_then(|d| d.get("signals"));

    let has_loading_indicators = signals
        .and_then(|s| s.get("loading-indicators"))
        .and_then(|li| li.get("idle"))
        .and_then(|v| v.as_bool())
        .map(|idle| !idle)
        .unwrap_or(false);

    let loading_indicators_list = signals
        .and_then(|s| s.get("loading-indicators"))
        .and_then(|li| li.get("status"))
        .and_then(|s| s.get("indicators"))
        .cloned()
        .unwrap_or(serde_json::json!([]));

    let network_busy = signals
        .and_then(|s| s.get("network"))
        .and_then(|net| net.get("idle"))
        .and_then(|v| v.as_bool())
        .map(|idle| !idle)
        .unwrap_or(false);

    let pending_requests = signals
        .and_then(|s| s.get("network"))
        .and_then(|net| net.get("status"))
        .and_then(|s| s.get("pendingCount"))
        .and_then(|c| c.as_u64())
        .unwrap_or(0);

    // Phase 8: Determine verdict
    let obs_secs = observation_ms / 1000;

    let (verdict, confidence, summary, suggestions) =
        if !screenshot_changed && !ui_bridge_responsive {
            (
                "stuck",
                0.95f64,
                format!(
                    "The app appears stuck. The screen has not changed during the \
                     {obs_secs}s observation window and the UI Bridge is not responding \
                     (React may not have mounted)."
                ),
                vec![
                    "Check if the Tauri webview loaded successfully.",
                    "Check the browser console for JavaScript errors.",
                    "Check if the API server started (ports 9876-9878).",
                    "Try restarting the runner.",
                ],
            )
        } else if !screenshot_changed && has_loading_indicators {
            (
                "stuck",
                0.95,
                format!(
                    "The app appears stuck on a loading screen. Loading indicators \
                     are visible, the screen has not changed during the {obs_secs}s \
                     observation window, and no content is being rendered."
                ),
                vec![
                    "Check if a required backend service is running.",
                    "Check the browser console for JavaScript errors.",
                    "Try refreshing the page.",
                ],
            )
        } else if !screenshot_changed && network_busy {
            (
                "stuck",
                0.7,
                format!(
                    "The app appears stuck. The screen has not changed during the \
                     {obs_secs}s observation window but {pending_requests} network \
                     request(s) are still in flight. A request may be hanging."
                ),
                vec![
                    "Check if a network request is hanging.",
                    "Verify the API server is reachable.",
                ],
            )
        } else if !screenshot_changed && ui_bridge_responsive && !has_loading_indicators {
            (
                "idle",
                0.9,
                "The app appears to be in a normal resting state. No loading \
                 indicators detected and the screen is stable."
                    .to_string(),
                vec![],
            )
        } else if screenshot_changed && has_loading_indicators {
            (
                "loading",
                0.85,
                format!(
                    "The app is loading. The screen changed during the {obs_secs}s \
                     observation window and loading indicators are visible, indicating \
                     content is being rendered."
                ),
                vec![],
            )
        } else if screenshot_changed && !ui_bridge_responsive {
            (
                "unknown",
                0.5,
                "The screen is changing but the UI Bridge is not responding. \
                 The app may be loading or recovering."
                    .to_string(),
                vec!["Wait a few seconds and try again."],
            )
        } else {
            (
                "idle",
                0.7,
                "The app appears to be in a normal state. The screen changed \
                 slightly during observation but no loading indicators are present."
                    .to_string(),
                vec![],
            )
        };

    let evidence = serde_json::json!({
        "screenshotSimilarity": similarity,
        "screenshotChanged": screenshot_changed,
        "uiBridgeResponsive": ui_bridge_responsive,
        "loadingIndicators": loading_indicators_list,
        "networkBusy": network_busy,
        "pendingNetworkRequests": pending_requests,
    });

    let diagnosis = serde_json::json!({
        "verdict": verdict,
        "confidence": confidence,
        "summary": summary,
        "evidence": evidence,
        "observationWindowMs": observation_ms,
        "suggestions": suggestions,
        "screenshot": cap2.base64,
        "screenshotWidth": cap2.width,
        "screenshotHeight": cap2.height,
        "captureSource": cap2.source,
        "timestamp": chrono::Utc::now().timestamp_millis(),
    });

    Json(ApiResponse::success(diagnosis))
}

// ============================================================================
// Page Health Analysis
// ============================================================================

/// Optional request body for page-health endpoint.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PageHealthRequest {
    /// Reserved for future per-check toggles.
    #[serde(default)]
    pub options: Option<serde_json::Value>,
}

/// Analyse the current page by running discover internally and returning a
/// structured `PageHealthReport` with spatial coverage, layout regions,
/// element diversity, text signal scanning, interactive readiness, visual
/// anomalies and an ASCII heatmap.
pub async fn ui_bridge_page_health_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<PageHealthRequest>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Page health analysis");

    // --- Step 1: run discover to get all elements -------------------------
    let _body = body.map(|b| b.0).unwrap_or_default();

    let discover_payload = serde_json::json!({
        "options": {
            "includeHidden": true
        }
    });

    let discover_data = match ui_bridge_request_sync(&state, "discover", discover_payload).await {
        Ok(d) => d,
        Err(e) => {
            error!("UI Bridge API: page-health discover failed: {}", e);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))));
        }
    };

    // Elements live under "elements" key returned by discover.
    let elements = discover_data
        .get("elements")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let element_count = elements.len();

    // Visible elements: state.visible == true and state.normalizedRect present.
    let visible: Vec<&serde_json::Value> = elements
        .iter()
        .filter(|el| {
            let state = el.get("state");
            let is_visible = state
                .and_then(|s| s.get("visible"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let has_rect = state.and_then(|s| s.get("normalizedRect")).is_some();
            is_visible && has_rect
        })
        .collect();
    let visible_count = visible.len();

    let mut findings: Vec<serde_json::Value> = Vec::new();

    // --- Step 2: Spatial coverage (20x20 grid) ----------------------------
    const GRID: usize = 20;
    let mut grid = [[false; GRID]; GRID];

    for el in &visible {
        if let Some(rect) = el.get("state").and_then(|s| s.get("normalizedRect")) {
            let x = rect.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = rect.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let w = rect.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let h = rect.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0);

            let col_start = (x * GRID as f64).floor().max(0.0) as usize;
            let col_end = ((x + w) * GRID as f64).ceil().min(GRID as f64) as usize;
            let row_start = (y * GRID as f64).floor().max(0.0) as usize;
            let row_end = ((y + h) * GRID as f64).ceil().min(GRID as f64) as usize;

            for row in grid.iter_mut().take(row_end.min(GRID)).skip(row_start) {
                for cell in row.iter_mut().take(col_end.min(GRID)).skip(col_start) {
                    *cell = true;
                }
            }
        }
    }

    let total_cells = (GRID * GRID) as f64;
    let filled_cells = grid.iter().flatten().filter(|&&v| v).count() as f64;
    let coverage_pct = (filled_cells / total_cells * 100.0).round();

    // Left half = columns 0..10, right half = columns 10..20
    let left_filled = grid
        .iter()
        .flat_map(|row| row[..GRID / 2].iter())
        .filter(|&&v| v)
        .count() as f64;
    let right_filled = grid
        .iter()
        .flat_map(|row| row[GRID / 2..].iter())
        .filter(|&&v| v)
        .count() as f64;
    let half_cells = (GRID * GRID / 2) as f64;
    let left_half_pct = (left_filled / half_cells * 100.0).round();
    let right_half_pct = (right_filled / half_cells * 100.0).round();

    let spatial_severity = if coverage_pct < 15.0 {
        "CRITICAL"
    } else if coverage_pct < 30.0 {
        "WARNING"
    } else {
        "OK"
    };

    // Sidebar-only: right < 5% and left > 20%
    let spatial_severity = if right_half_pct < 5.0 && left_half_pct > 20.0 {
        "CRITICAL"
    } else {
        spatial_severity
    };

    findings.push(serde_json::json!({
        "check": "spatial_coverage",
        "severity": spatial_severity,
        "detail": format!(
            "Elements cover {}% of viewport. Left={}%, Right={}%",
            coverage_pct, left_half_pct, right_half_pct
        ),
        "data": {
            "coverage_pct": coverage_pct,
            "left_half_pct": left_half_pct,
            "right_half_pct": right_half_pct
        }
    }));

    // --- Step 3: Layout regions -------------------------------------------
    let mut sidebar_count: usize = 0;
    let mut header_count: usize = 0;
    let mut content_count: usize = 0;

    for el in &visible {
        if let Some(rect) = el.get("state").and_then(|s| s.get("normalizedRect")) {
            let x = rect.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let w = rect.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = rect.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let h = rect.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0);

            let cx = x + w / 2.0;
            let cy = y + h / 2.0;

            if cx < 0.2 {
                sidebar_count += 1;
            } else if cy < 0.08 {
                header_count += 1;
            } else {
                content_count += 1;
            }
        }
    }

    let layout_severity = if content_count == 0 {
        "CRITICAL"
    } else if content_count < 3 {
        "WARNING"
    } else {
        "OK"
    };

    findings.push(serde_json::json!({
        "check": "layout_regions",
        "severity": layout_severity,
        "detail": format!(
            "sidebar={}, header={}, content={}",
            sidebar_count, header_count, content_count
        ),
        "data": {
            "sidebar": sidebar_count,
            "header": header_count,
            "content": content_count
        }
    }));

    // --- Step 4: Element diversity ----------------------------------------
    let nav_types: &[&str] = &["button", "heading", "badge", "status-message"];
    let mut type_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for el in &elements {
        let t = el.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
        *type_counts.entry(t.to_string()).or_insert(0) += 1;
    }

    let all_nav = element_count > 5 && type_counts.keys().all(|k| nav_types.contains(&k.as_str()));

    let diversity_severity = if all_nav { "WARNING" } else { "OK" };

    findings.push(serde_json::json!({
        "check": "element_diversity",
        "severity": diversity_severity,
        "detail": format!(
            "{} type(s) across {} elements{}",
            type_counts.len(),
            element_count,
            if all_nav { " (navigation-only)" } else { "" }
        ),
        "data": {
            "types": type_counts
        }
    }));

    // --- Step 5: Text signal scanning -------------------------------------
    let skip_types: &[&str] = &["button", "link", "tab", "menuitem"];

    let error_phrases: &[&str] = &[
        "error occurred",
        "failed to",
        "exception",
        "crash",
        "unavailable",
        "something went wrong",
        "could not",
    ];
    let loading_phrases: &[&str] = &[
        "loading",
        "starting",
        "connecting",
        "please wait",
        "initializing",
        "fetching",
    ];
    let empty_phrases: &[&str] = &[
        "no data",
        "no results",
        "nothing here",
        "empty",
        "no items",
        "get started",
    ];
    let css_signals: &[&str] = &["spin", "pulse", "skeleton", "loading", "shimmer"];

    let mut detected_errors: Vec<String> = Vec::new();
    let mut detected_loading: Vec<String> = Vec::new();
    let mut detected_empty: Vec<String> = Vec::new();
    let mut detected_css: Vec<String> = Vec::new();

    for el in &elements {
        let el_type = el.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let text = el
            .get("state")
            .and_then(|s| s.get("textContent"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // "classes" is a top-level array of strings
        let classes_arr = el.get("classes").and_then(|v| v.as_array());
        let classes_str: String = classes_arr
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();

        // Check CSS class signals on all elements
        let classes_lower = classes_str.to_lowercase();
        for sig in css_signals {
            if classes_lower.contains(sig) {
                detected_css.push(format!("class contains '{}' on {}", sig, el_type));
                break;
            }
        }

        // Skip navigation types for text scanning
        if skip_types.contains(&el_type) {
            continue;
        }

        let text_lower = text.to_lowercase();
        for phrase in error_phrases {
            if text_lower.contains(phrase) {
                detected_errors.push(text.chars().take(120).collect());
                break;
            }
        }
        for phrase in loading_phrases {
            if text_lower.contains(phrase) {
                detected_loading.push(text.chars().take(120).collect());
                break;
            }
        }
        for phrase in empty_phrases {
            if text_lower.contains(phrase) {
                detected_empty.push(text.chars().take(120).collect());
                break;
            }
        }
    }

    let text_severity = if !detected_errors.is_empty() {
        "CRITICAL"
    } else if !detected_loading.is_empty() || !detected_css.is_empty() || !detected_empty.is_empty()
    {
        "WARNING"
    } else {
        "OK"
    };

    findings.push(serde_json::json!({
        "check": "text_signals",
        "severity": text_severity,
        "detail": format!(
            "errors={}, loading={}, empty={}, css_signals={}",
            detected_errors.len(),
            detected_loading.len(),
            detected_empty.len(),
            detected_css.len()
        ),
        "data": {
            "errors": detected_errors,
            "loading": detected_loading,
            "empty": detected_empty,
            "css_signals": detected_css
        }
    }));

    // --- Step 6: Interactive readiness ------------------------------------
    let mut interactive_total: usize = 0;
    let mut interactive_disabled: usize = 0;

    for el in &elements {
        let cat = el.get("category").and_then(|v| v.as_str()).unwrap_or("");
        if cat == "interactive" {
            interactive_total += 1;
            let enabled = el
                .get("state")
                .and_then(|s| s.get("enabled"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if !enabled {
                interactive_disabled += 1;
            }
        }
    }

    let interactive_severity = if interactive_total > 0
        && (interactive_disabled as f64 / interactive_total as f64) > 0.5
    {
        "WARNING"
    } else {
        "OK"
    };

    findings.push(serde_json::json!({
        "check": "interactive_readiness",
        "severity": interactive_severity,
        "detail": format!(
            "{} interactive elements, {} disabled",
            interactive_total, interactive_disabled
        ),
        "data": {
            "total": interactive_total,
            "disabled": interactive_disabled
        }
    }));

    // --- Step 7: Visual anomalies -----------------------------------------
    let mut zero_size: usize = 0;
    let mut outside_viewport: usize = 0;

    for el in &visible {
        if let Some(rect) = el.get("state").and_then(|s| s.get("normalizedRect")) {
            let x = rect.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = rect.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let w = rect.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let h = rect.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0);

            if w == 0.0 || h == 0.0 {
                zero_size += 1;
            }
            if x + w < 0.0 || y + h < 0.0 || x > 1.0 || y > 1.0 {
                outside_viewport += 1;
            }
        }
    }

    let anomaly_severity = if zero_size > 0 || outside_viewport > 0 {
        "WARNING"
    } else {
        "OK"
    };

    findings.push(serde_json::json!({
        "check": "visual_anomalies",
        "severity": anomaly_severity,
        "detail": format!(
            "zero_size={}, outside_viewport={}",
            zero_size, outside_viewport
        ),
        "data": {
            "zero_size": zero_size,
            "outside_viewport": outside_viewport
        }
    }));

    // --- Step 8: ASCII heatmap --------------------------------------------
    let heatmap: Vec<String> = grid
        .iter()
        .map(|row| {
            row.iter()
                .map(|&filled| if filled { '#' } else { '.' })
                .collect()
        })
        .collect();

    // --- Step 9: Determine worst severity ---------------------------------
    let severity_rank = |s: &str| -> u8 {
        match s {
            "CRITICAL" => 3,
            "WARNING" => 2,
            "OK" => 1,
            _ => 0,
        }
    };

    let worst = findings
        .iter()
        .filter_map(|f| f.get("severity").and_then(|s| s.as_str()))
        .max_by_key(|s| severity_rank(s))
        .unwrap_or("OK");

    let report = serde_json::json!({
        "summary": worst,
        "findings": findings,
        "heatmap": heatmap,
        "element_count": element_count,
        "visible_count": visible_count
    });

    Ok(Json(ApiResponse::success(report)))
}

// ============================================================================
// AI Search & Find Handlers
// ============================================================================

/// AI-powered element search.
pub async fn ui_bridge_ai_search_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI search");

    let payload = serde_json::json!({ "params": body });

    match ui_bridge_request_sync(&state, "ai_search", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: AI search failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Natural language element find.
///
/// Callers can pass `"minConfidence": 0.3` in the request body to override
/// the default threshold. The default (0.5) balances precision and recall
/// for common queries like "text input" or "search box".
pub async fn ui_bridge_ai_find_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI find");

    let payload = serde_json::json!({ "params": body });

    match ui_bridge_request_sync(&state, "ai_find", payload).await {
        Ok(mut data) => {
            // Confidence gate: if the best match is below threshold, return
            // element: null so callers don't act on a wrong match.  The raw
            // confidence and alternatives are preserved for debugging.
            //
            // Default 0.5 was chosen empirically: exact-match queries score
            // 1.0, good partial matches score 0.7-0.9, and reasonable fuzzy
            // matches (e.g., "search box" → input element) score ~0.5.
            // Callers can override via "minConfidence" in the request body.
            let min_confidence = body
                .get("minConfidence")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.5)
                .clamp(0.0, 1.0);
            let min_confidence = if min_confidence == 0.0 {
                0.5
            } else {
                min_confidence
            };
            let inner = data.get("data").unwrap_or(&data);
            let conf = inner
                .get("confidence")
                .and_then(|c| c.as_f64())
                .unwrap_or(0.0);
            if conf < min_confidence && conf > 0.0 {
                info!(
                    "UI Bridge API: ai/find below confidence threshold ({:.2} < {:.2}) — returning null element",
                    conf, min_confidence
                );
                if let Some(obj) = data.as_object_mut() {
                    obj.insert("element".to_string(), serde_json::Value::Null);
                    obj.insert(
                        "belowThreshold".to_string(),
                        serde_json::json!({
                            "originalConfidence": conf,
                            "threshold": min_confidence,
                        }),
                    );
                }
                if let Some(inner_obj) = data.get_mut("data").and_then(|d| d.as_object_mut()) {
                    inner_obj.insert("element".to_string(), serde_json::Value::Null);
                    inner_obj.insert(
                        "belowThreshold".to_string(),
                        serde_json::json!({
                            "originalConfidence": conf,
                            "threshold": min_confidence,
                        }),
                    );
                }
            }
            Ok(Json(ApiResponse::success(data)))
        }
        Err(e) => {
            error!("UI Bridge API: AI find failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Natural language action execution.
pub async fn ui_bridge_ai_execute_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI execute");
    let payload = serde_json::json!({ "params": body });
    match ui_bridge_request_sync(&state, "ai_execute", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: AI execute failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// AI assertion evaluation.
pub async fn ui_bridge_ai_assert_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI assert");
    let payload = serde_json::json!({ "params": body });
    match ui_bridge_request_sync(&state, "ai_assert", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: AI assert failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Batch AI assertion evaluation.
pub async fn ui_bridge_ai_assert_batch_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI assert batch");
    let payload = serde_json::json!({ "params": body });
    match ui_bridge_request_sync(&state, "ai_assert_batch", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: AI assert batch failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Query parameters for the AI snapshot endpoint.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AiSnapshotQuery {
    /// Maximum token budget for the snapshot (0 = unlimited).
    /// When set, elements are pruned by region priority.
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

/// Semantic AI snapshot of the page.
pub async fn ui_bridge_ai_snapshot_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AiSnapshotQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: AI snapshot (maxTokens={:?})",
        query.max_tokens
    );
    let mut payload = serde_json::json!({});
    if let Some(max_tokens) = query.max_tokens {
        payload["maxTokens"] = serde_json::json!(max_tokens);
    }
    match ui_bridge_request_sync(&state, "ai_snapshot", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: AI snapshot failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Natural language page summary.
pub async fn ui_bridge_ai_summary_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI summary");
    match ui_bridge_request_sync(&state, "ai_summary", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: AI summary failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

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

/// Wait for a specific idle signal.
pub async fn ui_bridge_wait_for_idle_signal_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(signal): axum::extract::Path<String>,
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
    match ui_bridge_request_sync(&state, "wait_for_idle_signal", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Wait for idle signal failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Wait for multiple idle targets.
pub async fn ui_bridge_wait_for_targets_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Wait for targets");
    let payload = serde_json::json!({ "params": body });
    match ui_bridge_request_sync(&state, "wait_for_targets", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Wait for targets failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

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

/// List all annotations.
pub async fn ui_bridge_annotations_list_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: List annotations");
    match ui_bridge_request_sync(&state, "annotations_list", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: List annotations failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Create annotation.
pub async fn ui_bridge_annotations_create_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Create annotation");
    let payload = serde_json::json!({ "params": body });
    match ui_bridge_request_sync(&state, "annotations_create", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Create annotation failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get single annotation.
pub async fn ui_bridge_annotations_get_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get annotation '{}'", id);
    let payload = serde_json::json!({ "params": { "id": id } });
    match ui_bridge_request_sync(&state, "annotations_get", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Get annotation failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Update annotation.
pub async fn ui_bridge_annotations_update_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Update annotation '{}'", id);
    let payload = serde_json::json!({ "params": { "id": id, "updates": body } });
    match ui_bridge_request_sync(&state, "annotations_update", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Update annotation failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Delete annotation.
pub async fn ui_bridge_annotations_delete_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Delete annotation '{}'", id);
    let payload = serde_json::json!({ "params": { "id": id } });
    match ui_bridge_request_sync(&state, "annotations_delete", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Delete annotation failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get annotation coverage metrics.
pub async fn ui_bridge_annotations_coverage_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Annotation coverage");
    match ui_bridge_request_sync(&state, "annotations_coverage", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Annotation coverage failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Export annotations.
pub async fn ui_bridge_annotations_export_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Export annotations");
    match ui_bridge_request_sync(&state, "annotations_export", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Export annotations failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Media routes (IPC to webview SDK)
// ============================================================================

/// Find media elements.
pub async fn ui_bridge_media_find_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let payload = serde_json::json!({ "params": body });
    match ui_bridge_request_sync(&state, "find_media", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Media audit (accessibility or performance).
pub async fn ui_bridge_media_audit_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(audit_type): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let payload = serde_json::json!({ "params": { "auditType": audit_type } });
    match ui_bridge_request_sync(&state, "media_audit", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Capture media snapshot.
pub async fn ui_bridge_media_snapshot_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let payload = serde_json::json!({ "params": body });
    match ui_bridge_request_sync(&state, "capture_media_snapshot", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Analyze media elements.
pub async fn ui_bridge_media_analyze_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let payload = serde_json::json!({ "params": body });
    match ui_bridge_request_sync(&state, "analyze_media", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

// ============================================================================
// State machine routes (IPC to webview SDK)
// ============================================================================

macro_rules! ipc_handler_get {
    ($fn_name:ident, $ipc_type:expr) => {
        pub async fn $fn_name(
            State(state): State<Arc<ApiState>>,
        ) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
            match ui_bridge_request_sync(&state, $ipc_type, serde_json::json!({})).await {
                Ok(data) => Ok(Json(ApiResponse::success(data))),
                Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
            }
        }
    };
}

macro_rules! ipc_handler_post {
    ($fn_name:ident, $ipc_type:expr) => {
        pub async fn $fn_name(
            State(state): State<Arc<ApiState>>,
            Json(body): Json<serde_json::Value>,
        ) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
            let payload = serde_json::json!({ "params": body });
            match ui_bridge_request_sync(&state, $ipc_type, payload).await {
                Ok(data) => Ok(Json(ApiResponse::success(data))),
                Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
            }
        }
    };
}

macro_rules! ipc_handler_path_get {
    ($fn_name:ident, $ipc_type:expr, $param_name:expr) => {
        pub async fn $fn_name(
            State(state): State<Arc<ApiState>>,
            axum::extract::Path(id): axum::extract::Path<String>,
        ) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
            let payload = serde_json::json!({ "params": { $param_name: id } });
            match ui_bridge_request_sync(&state, $ipc_type, payload).await {
                Ok(data) => Ok(Json(ApiResponse::success(data))),
                Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
            }
        }
    };
}

macro_rules! ipc_handler_path_post {
    ($fn_name:ident, $ipc_type:expr, $param_name:expr) => {
        pub async fn $fn_name(
            State(state): State<Arc<ApiState>>,
            axum::extract::Path(id): axum::extract::Path<String>,
            Json(body): Json<serde_json::Value>,
        ) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
            let mut params = body;
            if let Some(obj) = params.as_object_mut() {
                obj.insert($param_name.to_string(), serde_json::json!(id));
            } else {
                // Body wasn't an object; create one with just the path param
                params = serde_json::json!({ $param_name: id });
            }
            let payload = serde_json::json!({ "params": params });
            match ui_bridge_request_sync(&state, $ipc_type, payload).await {
                Ok(data) => Ok(Json(ApiResponse::success(data))),
                Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
            }
        }
    };
}

// State machine
ipc_handler_get!(ui_bridge_get_states_handler, "get_states");
ipc_handler_get!(ui_bridge_get_active_states_handler, "get_active_states");
ipc_handler_get!(ui_bridge_get_state_snapshot_handler, "get_state_snapshot");
ipc_handler_path_get!(ui_bridge_get_state_handler, "get_state", "stateId");
ipc_handler_path_post!(
    ui_bridge_activate_state_handler,
    "activate_state",
    "stateId"
);
ipc_handler_path_post!(
    ui_bridge_deactivate_state_handler,
    "deactivate_state",
    "stateId"
);
ipc_handler_get!(ui_bridge_get_state_groups_handler, "get_state_groups");
ipc_handler_path_post!(
    ui_bridge_activate_state_group_handler,
    "activate_state_group",
    "groupId"
);
ipc_handler_path_post!(
    ui_bridge_deactivate_state_group_handler,
    "deactivate_state_group",
    "groupId"
);
ipc_handler_get!(ui_bridge_get_transitions_handler, "get_transitions");
ipc_handler_path_get!(
    ui_bridge_can_execute_transition_handler,
    "can_execute_transition",
    "transitionId"
);
ipc_handler_path_post!(
    ui_bridge_execute_transition_handler,
    "execute_transition",
    "transitionId"
);
ipc_handler_post!(ui_bridge_find_state_path_handler, "find_state_path");
ipc_handler_post!(ui_bridge_navigate_to_state_handler, "navigate_to_state");

// Runner-specific: tab navigation and storage management
ipc_handler_post!(ui_bridge_navigate_tab_handler, "navigate_tab");
ipc_handler_post!(ui_bridge_clear_storage_handler, "clear_storage");

// AI semantic search & diff
ipc_handler_post!(ui_bridge_ai_semantic_search_handler, "ai_semantic_search");
ipc_handler_get!(ui_bridge_ai_diff_handler, "ai_diff");

// Intents
ipc_handler_get!(ui_bridge_get_intents_handler, "get_intents");
ipc_handler_post!(ui_bridge_register_intent_handler, "register_intent");
ipc_handler_post!(ui_bridge_find_intent_handler, "find_intent");
ipc_handler_post!(ui_bridge_execute_intent_handler, "execute_intent");

// Component state
ipc_handler_path_get!(
    ui_bridge_get_component_state_handler,
    "get_component_state",
    "componentId"
);

// Page scroll
ipc_handler_post!(ui_bridge_scroll_page_handler, "scroll_page");

// Performance entries
ipc_handler_get!(
    ui_bridge_get_performance_entries_handler,
    "get_performance_entries"
);
ipc_handler_get!(
    ui_bridge_clear_performance_entries_handler,
    "clear_performance_entries"
);

// AI analysis
ipc_handler_post!(ui_bridge_ai_analyze_data_handler, "ai_analyze_data");
ipc_handler_post!(ui_bridge_ai_analyze_regions_handler, "ai_analyze_regions");
ipc_handler_post!(
    ui_bridge_ai_analyze_structured_handler,
    "ai_analyze_structured_data"
);
ipc_handler_post!(
    ui_bridge_ai_analyze_cross_app_handler,
    "ai_analyze_cross_app"
);
ipc_handler_post!(ui_bridge_ai_recovery_attempt_handler, "ai_recovery_attempt");

// Design evaluation
ipc_handler_post!(ui_bridge_design_evaluate_handler, "design_evaluate");
ipc_handler_post!(
    ui_bridge_design_evaluate_baseline_handler,
    "design_evaluate_baseline"
);
ipc_handler_get!(
    ui_bridge_design_evaluate_contexts_handler,
    "design_evaluate_contexts"
);
ipc_handler_post!(
    ui_bridge_design_evaluate_diff_handler,
    "design_evaluate_diff"
);

// Media compare
ipc_handler_post!(ui_bridge_media_compare_handler, "media_compare");

// Pixel-accurate image diff (compareVisualRegression alias)
ipc_handler_post!(ui_bridge_image_diff_handler, "image_diff");

/// `GET /ui-bridge/control/element-screenshot?id=...`
///
/// Returns a single element's PNG capture. Wraps the existing
/// `capture_element_images` batch IPC type — we ask the webview to capture
/// just one element and unwrap the single-entry result.
///
/// Response shape:
/// ```json
/// { "success": true, "data": {
///     "id": "btn-save",
///     "base64_png": "iVBOR...",
///     "width": 80, "height": 30,
///     "device_pixel_ratio": 1.5
/// } }
/// ```
pub async fn ui_bridge_element_screenshot_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let id = match query.get("id") {
        Some(s) if !s.is_empty() => s.clone(),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(
                    "element-screenshot requires `id` query param".to_string(),
                )),
            ));
        }
    };

    // Use the dedicated capture_single_element IPC type which has
    // multiple DOM lookup fallbacks (registry → getElementById →
    // data-ui-bridge-id → title → aria-label). The old batch path
    // (capture_element_images) only looked in the bridge registry
    // and returned 0 captures for elements that weren't persistently
    // registered.
    let payload = serde_json::json!({
        "params": { "elementId": id.clone() }
    });

    match ui_bridge_request_sync(&state, "capture_single_element", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            if e.to_lowercase().contains("not found") {
                Err((
                    StatusCode::NOT_FOUND,
                    Json(api_error(format!(
                        "element '{}' not found or not capturable",
                        id
                    ))),
                ))
            } else {
                Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
            }
        }
    }
}

// Annotations import
ipc_handler_post!(ui_bridge_annotations_import_handler, "annotations_import");

// Intents from NL query
ipc_handler_post!(
    ui_bridge_execute_intent_from_query_handler,
    "execute_intent_from_query"
);

// Debug
ipc_handler_get!(ui_bridge_get_element_tree_handler, "get_element_tree");
ipc_handler_path_get!(
    ui_bridge_highlight_element_handler,
    "highlight_element",
    "elementId"
);

/// Find elements matching criteria.
pub async fn ui_bridge_find_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Find elements");

    match ui_bridge_request_sync(&state, "find", request).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// POST /ui-bridge/control/capture-element-images — Capture element images directly from the DOM.
///
/// Uses html2canvas in the frontend to render each element to a canvas, bypassing
/// screen capture entirely. This produces correct images even when other windows
/// cover the runner.
///
/// Body: `{ "element_ids": ["btn-save", "input-name"] }` (optional — null captures all visible)
/// Returns: `{ "captures": { "btn-save": { "base64_png": "...", "width": 80, "height": 30 }, ... } }`
pub async fn ui_bridge_capture_element_images_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Capture element images");

    // Use a longer timeout for element capture — html2canvas rendering 30+ elements
    // can take 30-60 seconds. The default 10s UI Bridge IPC timeout is too short.
    let request_id = uuid::Uuid::new_v4().to_string();
    let event_payload = serde_json::json!({
        "requestId": request_id,
        "type": "capture_element_images",
        "params": body,
    });

    let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();
    {
        let mut pending = state.ui_bridge_pending.lock().await;
        pending.insert(request_id.clone(), tx);
        state
            .ui_bridge_pending_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    if let Err(e) = state.app_handle.emit("ui-bridge-request", &event_payload) {
        let mut pending = state.ui_bridge_pending.lock().await;
        if pending.remove(&request_id).is_some() {
            state
                .ui_bridge_pending_count
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to emit request: {}", e))),
        ));
    }

    // 120 second timeout for element capture (vs 10s default)
    let timeout = std::time::Duration::from_secs(120);
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(data)) => Ok(Json(ApiResponse::success(data))),
        Ok(Err(_)) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error("Request channel closed".to_string())),
        )),
        Err(_) => {
            let mut pending = state.ui_bridge_pending.lock().await;
            if pending.remove(&request_id).is_some() {
                state
                    .ui_bridge_pending_count
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }
            error!("UI Bridge API: Capture element images timed out after 120s");
            Err((
                StatusCode::GATEWAY_TIMEOUT,
                Json(api_error(
                    "Element capture timed out after 120s".to_string(),
                )),
            ))
        }
    }
}

/// POST /ui-bridge/control/get-element-images — Read <img> src attributes from the DOM.
///
/// A lightweight alternative to capture-element-images that reads image metadata
/// (src, alt, dimensions) without rendering via html2canvas. Useful for verifying
/// which images are displayed (e.g., thumbnail URLs on state cards).
///
/// Body: `{ "element_id": "some-container", "max_images": 50, "full_src": false, "image_index": 0 }`
/// - `element_id` (optional): Scope search to a specific UI Bridge element
/// - `max_images` (optional, default 50): Maximum number of images to return
/// - `full_src` (optional, default false): Return full data: URIs instead of truncated
/// - `image_index` (optional): When `full_src` is true, only return full src for this index
///
/// Returns: `{ "images": [...], "total": N }`
pub async fn ui_bridge_get_element_images_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get element images");

    let payload = serde_json::json!({ "params": body });
    match ui_bridge_request_sync(&state, "get_element_images", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: get_element_images failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

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

/// GET /ui-bridge/control/workflow/:run_id/status — Get workflow run status.
/// Reads the task run directly from the checkpoint database.
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

/// Accept an IPC response via HTTP (fallback when Tauri event system is unavailable).
/// The frontend can POST responses here instead of using emit("ui-bridge-response").
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
// Structured Action Plan Execution
// ============================================================================

/// A target specification for finding an element.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionPlanElementTarget {
    /// Direct element ID from a prior snapshot
    #[serde(default)]
    pub element_id: Option<String>,
    /// data-testid attribute value
    #[serde(default)]
    pub test_id: Option<String>,
    /// Natural language description for fuzzy search
    #[serde(default)]
    pub search_text: Option<String>,
    /// Element type hint (e.g., "button", "input")
    #[serde(default)]
    pub element_type: Option<String>,
    /// CSS selector
    #[serde(default)]
    pub selector: Option<String>,
}

/// A single planned action in an action plan.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedAction {
    /// Action type to execute (click, type, select, etc.)
    pub action: String,
    /// How to find the target element
    pub target: ActionPlanElementTarget,
    /// LLM's reasoning for this action (audit trail)
    #[serde(default)]
    pub reasoning: Option<String>,
    /// LLM's confidence (0.0–1.0)
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    /// Action-specific parameters (text, value, direction, etc.)
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    /// Separates generic intent from specific data (for caching).
    /// Example: "What email should be entered?"
    #[serde(default)]
    pub user_detail_query: Option<String>,
    /// The specific data for the query.
    /// Example: "test@example.com"
    #[serde(default)]
    pub user_detail_answer: Option<String>,
}

fn default_confidence() -> f64 {
    1.0
}

/// Request to execute a structured action plan.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionPlanRequest {
    /// Ordered list of actions to execute
    pub actions: Vec<PlannedAction>,
    /// High-level goal this plan achieves
    #[serde(default)]
    pub goal: Option<String>,
    /// Minimum confidence threshold — actions below are skipped (default: 0.5)
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f64,
    /// Whether to stop on first action failure (default: true)
    #[serde(default = "default_stop_on_failure")]
    pub stop_on_failure: bool,
    /// Page URL for cache keying (when provided, enables plan caching)
    #[serde(default)]
    pub page_url: Option<String>,
    /// Element snapshot for cache fingerprinting (array of {id, type, role, label})
    #[serde(default)]
    pub element_snapshot: Option<serde_json::Value>,
}

fn default_confidence_threshold() -> f64 {
    0.5
}

fn default_stop_on_failure() -> bool {
    true
}

/// Result of a single planned action execution.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedActionResult {
    pub index: usize,
    pub success: bool,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_element_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub skipped_low_confidence: bool,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element_state: Option<serde_json::Value>,
}

/// Aggregated result of executing a full action plan.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionPlanResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    pub results: Vec<PlannedActionResult>,
    pub executed_count: usize,
    pub skipped_count: usize,
    pub failed_count: usize,
    pub total_duration_ms: u64,
    /// Whether this plan was stored in the cache for future reuse
    #[serde(default)]
    pub cached: bool,
}

/// Execute a structured action plan: an ordered sequence of typed UI actions.
///
/// Each action is resolved (element lookup) then executed via the standard
/// IPC path. This avoids a second LLM call for instruction interpretation —
/// the agentic-phase LLM directly specifies element targets and action types.
///
/// Element resolution priority:
///   1. element_id (direct registry lookup)
///   2. test_id (data-testid attribute)
///   3. selector (CSS selector)
///   4. search_text + element_type (fuzzy AI search)
pub async fn ui_bridge_execute_action_plan_handler(
    State(state): State<Arc<ApiState>>,
    Json(plan): Json<ActionPlanRequest>,
) -> Result<Json<ApiResponse<ActionPlanResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let total_start = Instant::now();
    let action_count = plan.actions.len();

    if action_count == 0 {
        return Ok(Json(ApiResponse::success(ActionPlanResponse {
            success: true,
            goal: plan.goal,
            results: vec![],
            executed_count: 0,
            skipped_count: 0,
            failed_count: 0,
            total_duration_ms: 0,
            cached: false,
        })));
    }

    if action_count > 50 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "Action plan has {} actions, maximum is 50",
                action_count
            ))),
        ));
    }

    info!(
        "UI Bridge API: Executing action plan with {} actions (goal: {:?})",
        action_count, plan.goal
    );

    let mut results = Vec::with_capacity(action_count);
    let mut executed_count = 0usize;
    let mut skipped_count = 0usize;
    let mut failed_count = 0usize;
    let mut all_success = true;

    for (i, planned) in plan.actions.iter().enumerate() {
        let action_start = Instant::now();

        // Skip low-confidence actions
        if planned.confidence < plan.confidence_threshold {
            info!(
                "UI Bridge action plan: Skipping action {} ({}) — confidence {:.2} < threshold {:.2}",
                i, planned.action, planned.confidence, plan.confidence_threshold
            );
            skipped_count += 1;
            results.push(PlannedActionResult {
                index: i,
                success: true,
                action: planned.action.clone(),
                resolved_element_id: None,
                error: None,
                skipped_low_confidence: true,
                duration_ms: 0,
                element_state: None,
            });
            continue;
        }

        // Handle non-element actions (navigate, wait)
        if planned.action == "navigate" {
            let url = planned
                .params
                .as_ref()
                .and_then(|p| p.get("url"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let result =
                ui_bridge_request_sync(&state, "navigate", serde_json::json!({ "url": url })).await;
            let duration = action_start.elapsed().as_millis() as u64;
            let (success, error) = match result {
                Ok(_) => (true, None),
                Err(e) => (false, Some(e)),
            };
            if !success {
                all_success = false;
                failed_count += 1;
            } else {
                executed_count += 1;
            }
            results.push(PlannedActionResult {
                index: i,
                success,
                action: "navigate".into(),
                resolved_element_id: None,
                error,
                skipped_low_confidence: false,
                duration_ms: duration,
                element_state: None,
            });
            if !success && plan.stop_on_failure {
                break;
            }
            continue;
        }

        if planned.action == "wait" {
            let ms = planned
                .params
                .as_ref()
                .and_then(|p| p.get("ms"))
                .and_then(|v| v.as_u64())
                .unwrap_or(1000)
                .min(30000); // Cap at 30s
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            executed_count += 1;
            results.push(PlannedActionResult {
                index: i,
                success: true,
                action: "wait".into(),
                resolved_element_id: None,
                error: None,
                skipped_low_confidence: false,
                duration_ms: ms,
                element_state: None,
            });
            continue;
        }

        // Resolve element ID from target specification
        let resolved_id = resolve_action_plan_target(&state, &planned.target).await;

        let resolved_id = match resolved_id {
            Ok(id) => id,
            Err(e) => {
                let duration = action_start.elapsed().as_millis() as u64;
                all_success = false;
                failed_count += 1;
                results.push(PlannedActionResult {
                    index: i,
                    success: false,
                    action: planned.action.clone(),
                    resolved_element_id: None,
                    error: Some(format!("Element resolution failed: {}", e)),
                    skipped_low_confidence: false,
                    duration_ms: duration,
                    element_state: None,
                });
                if plan.stop_on_failure {
                    break;
                }
                continue;
            }
        };

        // Execute the action via standard IPC
        let action_payload = serde_json::json!({
            "elementId": resolved_id,
            "action": {
                "action": planned.action,
                "params": planned.params,
            }
        });

        let result = ui_bridge_request_sync(&state, "execute_action", action_payload).await;
        let duration = action_start.elapsed().as_millis() as u64;

        match result {
            Ok(data) => {
                let action_success = data
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let element_state = data.get("elementState").cloned();
                let action_error = if action_success {
                    None
                } else {
                    data.get("error")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                };

                if !action_success {
                    all_success = false;
                    failed_count += 1;
                } else {
                    executed_count += 1;
                }

                results.push(PlannedActionResult {
                    index: i,
                    success: action_success,
                    action: planned.action.clone(),
                    resolved_element_id: Some(resolved_id.clone()),
                    error: action_error,
                    skipped_low_confidence: false,
                    duration_ms: duration,
                    element_state,
                });

                if !action_success && plan.stop_on_failure {
                    break;
                }
            }
            Err(e) => {
                all_success = false;
                failed_count += 1;
                results.push(PlannedActionResult {
                    index: i,
                    success: false,
                    action: planned.action.clone(),
                    resolved_element_id: Some(resolved_id.clone()),
                    error: Some(e),
                    skipped_low_confidence: false,
                    duration_ms: duration,
                    element_state: None,
                });
                if plan.stop_on_failure {
                    break;
                }
            }
        }
    }

    let total_duration = total_start.elapsed().as_millis() as u64;
    info!(
        "UI Bridge action plan complete: {}/{} succeeded, {} skipped, {} failed ({}ms)",
        executed_count, action_count, skipped_count, failed_count, total_duration
    );

    // Cache successful plans for future reuse
    let mut was_cached = false;
    if all_success {
        if let (Some(ref url), Some(ref snapshot)) = (&plan.page_url, &plan.element_snapshot) {
            if let Some((norm_url, fingerprint)) =
                super::action_plan_cache::ActionPlanCache::build_key(url, snapshot)
            {
                let plan_json = serde_json::to_value(&plan.actions).unwrap_or_default();
                super::action_plan_cache::global_action_plan_cache().put(
                    &norm_url,
                    &fingerprint,
                    plan_json,
                    plan.goal.clone(),
                );
                was_cached = true;
            }
        }
    } else if let (Some(ref url), Some(ref snapshot)) = (&plan.page_url, &plan.element_snapshot) {
        // Mark failed plans so they're not reused
        if let Some((norm_url, fingerprint)) =
            super::action_plan_cache::ActionPlanCache::build_key(url, snapshot)
        {
            super::action_plan_cache::global_action_plan_cache()
                .mark_failed(&norm_url, &fingerprint);
        }
    }

    Ok(Json(ApiResponse::success(ActionPlanResponse {
        success: all_success,
        goal: plan.goal,
        results,
        executed_count,
        skipped_count,
        failed_count,
        total_duration_ms: total_duration,
        cached: was_cached,
    })))
}

/// Resolve an element target to a concrete element ID.
///
/// Tries resolution strategies in order of specificity:
/// 1. Direct element_id
/// 2. test_id → find by data-testid
/// 3. CSS selector → find by selector
/// 4. search_text → fuzzy AI search
async fn resolve_action_plan_target(
    state: &Arc<ApiState>,
    target: &ActionPlanElementTarget,
) -> Result<String, String> {
    // 1. Direct element ID
    if let Some(ref id) = target.element_id {
        return Ok(id.clone());
    }

    // 2. Find by data-testid attribute
    if let Some(ref test_id) = target.test_id {
        let find_payload = serde_json::json!({
            "testId": test_id
        });
        if let Ok(data) = ui_bridge_request_sync(state, "find", find_payload).await {
            if let Some(id) = extract_first_element_id(&data) {
                return Ok(id);
            }
        }
    }

    // 3. Find by CSS selector
    if let Some(ref selector) = target.selector {
        let find_payload = serde_json::json!({
            "selector": selector
        });
        if let Ok(data) = ui_bridge_request_sync(state, "find", find_payload).await {
            if let Some(id) = extract_first_element_id(&data) {
                return Ok(id);
            }
        }
    }

    // 4. Fuzzy search by text + element type
    if let Some(ref text) = target.search_text {
        let mut find_payload = serde_json::json!({
            "text": text,
            "fuzzy": true
        });
        if let Some(ref el_type) = target.element_type {
            find_payload["element_type"] = serde_json::json!(el_type);
        }
        if let Ok(data) = ui_bridge_request_sync(state, "find", find_payload).await {
            if let Some(id) = extract_first_element_id(&data) {
                return Ok(id);
            }
        }
    }

    Err("No element target specified or element not found".to_string())
}

// ============================================================================
// Action Plan Cache Endpoints
// ============================================================================

/// Query params for action plan cache lookup.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionPlanCacheLookupQuery {
    /// Page URL to look up
    pub url: String,
    /// Element snapshot JSON (array of {id, type, role, label})
    pub elements: String,
}

/// Look up a cached action plan by page URL and element fingerprint.
async fn ui_bridge_action_plan_cache_lookup_handler(
    Query(query): Query<ActionPlanCacheLookupQuery>,
) -> Json<ApiResponse<serde_json::Value>> {
    let elements: serde_json::Value = match serde_json::from_str(&query.elements) {
        Ok(v) => v,
        Err(_) => {
            return Json(ApiResponse::error("Invalid elements JSON".to_string()));
        }
    };

    if let Some((norm_url, fingerprint)) =
        super::action_plan_cache::ActionPlanCache::build_key(&query.url, &elements)
    {
        if let Some(cached) =
            super::action_plan_cache::global_action_plan_cache().get(&norm_url, &fingerprint)
        {
            return Json(ApiResponse::success(serde_json::json!({
                "hit": true,
                "plan": cached.plan,
                "goal": cached.goal,
                "hitCount": cached.hit_count,
                "lastSuccess": cached.last_success,
            })));
        }
    }

    Json(ApiResponse::success(serde_json::json!({ "hit": false })))
}

/// Get action plan cache statistics.
async fn ui_bridge_action_plan_cache_stats_handler() -> Json<ApiResponse<serde_json::Value>> {
    let stats = super::action_plan_cache::global_action_plan_cache().stats();
    Json(ApiResponse::success(stats))
}

// ============================================================================
// Batch Execution
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
///
/// Each operation is executed sequentially via the existing `ui_bridge_request_sync`
/// path, reusing all existing concurrency, circuit breaker, and timeout logic.
/// Max 50 operations per batch.
async fn ui_bridge_batch_handler(
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
                // Check for nested frontend failure (same logic as wrap_ipc_result)
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

// Create routes for this module.

// =========================================================================
// Convenience endpoints — app-agnostic DOM interaction helpers
// =========================================================================

/// Find elements by visible text content.
/// POST /ui-bridge/control/page/find-by-text
/// Body: { "text": "Submit", "tag": "button", "exact": true }
pub async fn ui_bridge_find_by_text_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let tag = body.get("tag").and_then(|v| v.as_str()).unwrap_or("*");
    let exact = body.get("exact").and_then(|v| v.as_bool()).unwrap_or(false);

    if text.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("'text' field is required")),
        ));
    }

    let escaped_text = text.replace('\\', "\\\\").replace('\'', "\\'");
    let match_expr = if exact {
        format!("el.textContent.trim() === '{}'", escaped_text)
    } else {
        format!(
            "el.textContent.trim().toLowerCase().includes('{}')",
            escaped_text.to_lowercase()
        )
    };

    let js = format!(
        r#"JSON.stringify(Array.from(document.querySelectorAll('{tag}'))
            .filter(el => el.offsetParent !== null && {match_expr})
            .map((el, i) => ({{
                index: i,
                tag: el.tagName,
                text: el.textContent.trim().slice(0, 100),
                id: el.id || null,
                disabled: el.disabled || false,
                visible: el.offsetParent !== null,
                rect: (() => {{ const r = el.getBoundingClientRect(); return {{ x: Math.round(r.x), y: Math.round(r.y), width: Math.round(r.width), height: Math.round(r.height) }}; }})()
            }})))"#,
        tag = tag,
        match_expr = match_expr
    );

    match evaluate_js_expression(&state, &js).await {
        Ok(result) => {
            let parsed: serde_json::Value =
                serde_json::from_str(&result).unwrap_or(serde_json::Value::Array(vec![]));
            Ok(Json(ApiResponse::success(serde_json::json!({
                "matches": parsed,
                "count": parsed.as_array().map(|a| a.len()).unwrap_or(0),
                "query": { "text": text, "tag": tag, "exact": exact }
            }))))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Click an element by its visible text content.
/// POST /ui-bridge/control/page/click-by-text
/// Body: { "text": "Submit", "tag": "button", "exact": true, "index": 0 }
pub async fn ui_bridge_click_by_text_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let tag = body.get("tag").and_then(|v| v.as_str()).unwrap_or("*");
    let exact = body.get("exact").and_then(|v| v.as_bool()).unwrap_or(false);
    let index = body.get("index").and_then(|v| v.as_u64()).unwrap_or(0);

    if text.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("'text' field is required")),
        ));
    }

    let escaped_text = text.replace('\\', "\\\\").replace('\'', "\\'");
    let match_expr = if exact {
        format!("el.textContent.trim() === '{}'", escaped_text)
    } else {
        format!(
            "el.textContent.trim().toLowerCase().includes('{}')",
            escaped_text.to_lowercase()
        )
    };

    let js = format!(
        r#"(() => {{
            const matches = Array.from(document.querySelectorAll('{tag}'))
                .filter(el => el.offsetParent !== null && {match_expr});
            if (matches.length === 0) return JSON.stringify({{ clicked: false, error: 'No matching elements found' }});
            const idx = {index};
            if (idx >= matches.length) return JSON.stringify({{ clicked: false, error: 'Index ' + idx + ' out of range (found ' + matches.length + ')' }});
            const el = matches[idx];
            el.scrollIntoView({{ block: 'center' }});
            el.click();
            return JSON.stringify({{
                clicked: true,
                tag: el.tagName,
                text: el.textContent.trim().slice(0, 100),
                index: idx,
                totalMatches: matches.length
            }});
        }})()"#,
        tag = tag,
        match_expr = match_expr,
        index = index
    );

    match evaluate_js_expression(&state, &js).await {
        Ok(result) => {
            let parsed: serde_json::Value = serde_json::from_str(&result)
                .unwrap_or(serde_json::json!({"clicked": false, "error": "Parse error"}));
            Ok(Json(ApiResponse::success(parsed)))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Click an element by CSS selector.
/// POST /ui-bridge/control/page/click-by-selector
/// Body: { "selector": "button[type='submit']", "index": 0 }
pub async fn ui_bridge_click_by_selector_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let selector = body.get("selector").and_then(|v| v.as_str()).unwrap_or("");
    let index = body.get("index").and_then(|v| v.as_u64()).unwrap_or(0);

    if selector.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("'selector' field is required")),
        ));
    }

    let escaped_selector = selector.replace('\\', "\\\\").replace('\'', "\\'");

    let js = format!(
        r#"(() => {{
            const matches = Array.from(document.querySelectorAll('{selector}'))
                .filter(el => el.offsetParent !== null);
            if (matches.length === 0) return JSON.stringify({{ clicked: false, error: 'No elements match selector' }});
            const idx = {index};
            if (idx >= matches.length) return JSON.stringify({{ clicked: false, error: 'Index ' + idx + ' out of range (found ' + matches.length + ')' }});
            const el = matches[idx];
            el.scrollIntoView({{ block: 'center' }});
            el.click();
            return JSON.stringify({{
                clicked: true,
                tag: el.tagName,
                text: el.textContent.trim().slice(0, 100),
                index: idx,
                totalMatches: matches.length
            }});
        }})()"#,
        selector = escaped_selector,
        index = index
    );

    match evaluate_js_expression(&state, &js).await {
        Ok(result) => {
            let parsed: serde_json::Value = serde_json::from_str(&result)
                .unwrap_or(serde_json::json!({"clicked": false, "error": "Parse error"}));
            Ok(Json(ApiResponse::success(parsed)))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Read the value of a form element by CSS selector.
/// POST /ui-bridge/control/page/read-value
/// Body: { "selector": "textarea", "index": 0 }
pub async fn ui_bridge_read_value_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let selector = body.get("selector").and_then(|v| v.as_str()).unwrap_or("");
    let index = body.get("index").and_then(|v| v.as_u64()).unwrap_or(0);

    if selector.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("'selector' field is required")),
        ));
    }

    let escaped_selector = selector.replace('\\', "\\\\").replace('\'', "\\'");

    let js = format!(
        r#"(() => {{
            const matches = Array.from(document.querySelectorAll('{selector}'));
            if (matches.length === 0) return JSON.stringify({{ found: false, error: 'No elements match selector' }});
            const idx = {index};
            if (idx >= matches.length) return JSON.stringify({{ found: false, error: 'Index out of range' }});
            const el = matches[idx];
            return JSON.stringify({{
                found: true,
                tag: el.tagName,
                type: el.type || null,
                value: el.value || el.textContent?.trim() || '',
                length: (el.value || el.textContent || '').length,
                placeholder: el.placeholder || null,
                disabled: el.disabled || false,
                readOnly: el.readOnly || false,
                index: idx,
                totalMatches: matches.length
            }});
        }})()"#,
        selector = escaped_selector,
        index = index
    );

    match evaluate_js_expression(&state, &js).await {
        Ok(result) => {
            let parsed: serde_json::Value = serde_json::from_str(&result)
                .unwrap_or(serde_json::json!({"found": false, "error": "Parse error"}));
            Ok(Json(ApiResponse::success(parsed)))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Type text into an element by CSS selector or label.
/// POST /ui-bridge/control/page/type-into
/// Body: { "selector": "textarea", "text": "hello", "clear": true, "index": 0 }
/// Or:   { "label": "Email", "text": "user@example.com" }
pub async fn ui_bridge_type_into_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let selector = body.get("selector").and_then(|v| v.as_str());
    let label = body.get("label").and_then(|v| v.as_str());
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let clear = body.get("clear").and_then(|v| v.as_bool()).unwrap_or(false);
    let index = body.get("index").and_then(|v| v.as_u64()).unwrap_or(0);

    let find_expr = if let Some(sel) = selector {
        let escaped = sel.replace('\\', "\\\\").replace('\'', "\\'");
        format!("document.querySelectorAll('{}')", escaped)
    } else if let Some(lbl) = label {
        let escaped = lbl.replace('\\', "\\\\").replace('\'', "\\'");
        format!(
            "(() => {{ const lb = Array.from(document.querySelectorAll('label')).find(l => l.textContent.trim().toLowerCase().includes('{}')); return lb && lb.htmlFor ? [document.getElementById(lb.htmlFor)] : []; }})()",
            escaped.to_lowercase()
        )
    } else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("'selector' or 'label' field is required")),
        ));
    };

    let escaped_text = text.replace('\\', "\\\\").replace('\'', "\\'");

    let js = format!(
        r#"(() => {{
            const matches = Array.from({find_expr}).filter(el => el);
            if (matches.length === 0) return JSON.stringify({{ typed: false, error: 'No elements found' }});
            const idx = {index};
            if (idx >= matches.length) return JSON.stringify({{ typed: false, error: 'Index out of range' }});
            const el = matches[idx];
            el.scrollIntoView({{ block: 'center' }});
            el.focus();
            if ({clear}) {{
                el.value = '';
                el.dispatchEvent(new Event('input', {{ bubbles: true }}));
            }}
            const nativeInputValueSetter = Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype, 'value')?.set
                || Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value')?.set;
            if (nativeInputValueSetter) {{
                nativeInputValueSetter.call(el, {clear} ? '{text}' : el.value + '{text}');
            }} else {{
                el.value = {clear} ? '{text}' : el.value + '{text}';
            }}
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
            return JSON.stringify({{
                typed: true,
                tag: el.tagName,
                valueLength: el.value.length,
                index: idx
            }});
        }})()"#,
        find_expr = find_expr,
        index = index,
        clear = clear,
        text = escaped_text
    );

    match evaluate_js_expression(&state, &js).await {
        Ok(result) => {
            let parsed: serde_json::Value = serde_json::from_str(&result)
                .unwrap_or(serde_json::json!({"typed": false, "error": "Parse error"}));
            Ok(Json(ApiResponse::success(parsed)))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// POST /ui-bridge/control/wait-for-element-state
///
/// Convenience wrapper around `wait_for_element` for the common case of
/// "wait until element <id> is visible / enabled / focused". Polls the
/// element registry every ~100ms until the state matches or the timeout
/// elapses.
///
/// Body: `{ "id": "button-x", "state": "visible" | "enabled" | "focused",
///          "timeout_ms": 5000 }`
///
/// Returns 200 with `{found: true, elapsed_ms, state: {...}}` on match or
/// `{found: false, elapsed_ms}` on timeout.
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
///
/// Body: `{ "pattern": "/settings*", "timeout_ms": 5000 }`
///
/// Returns 200 with `{matched: true, route, elapsed_ms}` on match or
/// `{matched: false, route, elapsed_ms}` on timeout.
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

/// POST /ui-bridge/control/batch
///
/// Execute a sequence of element actions and report per-step timing plus a
/// snapshot diff (element ids added / removed) between pre- and post-batch.
///
/// Body: `{ "steps": [{"action": "click", "elementId": "button-x"},
///                    {"action": "type", "elementId": "input-y",
///                     "params": {"text": "hello"}}],
///          "stopOnError": true }`
///
/// Returns:
/// ```json
/// {
///   "success": true,
///   "data": {
///     "results": [{"step": 0, "success": true, "durationMs": 68.5}, ...],
///     "totalMs": 142.3,
///     "snapshotDiff": {"addedIds": [...], "removedIds": [...], ...}
///   }
/// }
/// ```
///
/// On first failure with `stopOnError: true`, stops and returns the partial
/// results with `success: false` at the top level. With `stopOnError: false`,
/// continues through all steps regardless of failures.
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

    // Top-level success bit reflects whether every executed step passed —
    // matches the contract in the task spec.
    if all_succeeded {
        Ok(Json(ApiResponse::success(payload)))
    } else {
        Ok(Json(ApiResponse {
            success: false,
            data: Some(payload),
            error: Some("One or more batch steps failed".to_string()),
            error_detail: None,
        }))
    }
}

/// GET /ui-bridge/_routes
///
/// Returns the list of registered UI Bridge routes (method + path) sorted by
/// path. Used by SDKs and tests to discover what the runner exposes without
/// having to read the source.
///
/// The list is generated by walking the static manifest in `route_manifest()`
/// — kept in sync with the actual `routes()` registrations by hand. A unit
/// test in this module asserts that every entry resolves to a known handler.
pub async fn ui_bridge_routes_manifest_handler(
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mut entries: Vec<(String, String)> = route_manifest()
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

/// Static manifest of every UI Bridge route registered by `routes()`. Kept
/// in sync by hand — adding a new `.route(...)` call below should be paired
/// with a new `(method, path)` entry here. The `_routes` endpoint reads from
/// this list.
///
/// When a path supports multiple methods (e.g. GET+POST on the same URL),
/// list each method as a separate tuple.
///
/// Returns the in-file entries concatenated with `bookmarks::route_entries()`
/// (and any future per-family extraction). The OnceLock keeps the resulting
/// slice `'static` so existing callers that pass it around without cloning
/// continue to compile.
fn route_manifest() -> &'static [(&'static str, &'static str)] {
    use std::sync::OnceLock;
    static MANIFEST: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        let mut all: Vec<(&'static str, &'static str)> = Vec::new();
        all.extend_from_slice(local_route_entries());
        all.extend_from_slice(bookmarks::route_entries());
        all.extend_from_slice(design::route_entries());
        all.extend_from_slice(errors::route_entries());
        all.extend_from_slice(exploration::route_entries());
        all.extend_from_slice(forms::route_entries());
        all.extend_from_slice(network::route_entries());
        all.extend_from_slice(page::route_entries());
        all
    })
}

/// Routes that are still defined inline in `mod.rs::routes()`.
/// Per-family extractions append their own `route_entries()` via
/// `route_manifest()` above; do not list those entries here.
fn local_route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/_help"),
        ("GET", "/ui-bridge/_routes"),
        ("GET", "/ui-bridge/control/elements"),
        ("GET", "/ui-bridge/control/elements/last-discovered"),
        ("GET", "/ui-bridge/ai/elements/last-discovered"),
        ("GET", "/ui-bridge/control/element/{id}"),
        ("POST", "/ui-bridge/control/element/{id}/assert"),
        ("POST", "/ui-bridge/control/element/{id}/action"),
        ("POST", "/ui-bridge/control/batch-actions"),
        ("POST", "/ui-bridge/control/batch"),
        ("GET", "/ui-bridge/control/components"),
        ("GET", "/ui-bridge/control/component/{id}"),
        (
            "POST",
            "/ui-bridge/control/component/{id}/action/{action_id}",
        ),
        ("POST", "/ui-bridge/control/discover"),
        ("GET", "/ui-bridge/control/snapshot"),
        ("GET", "/ui-bridge/control/annotated-screenshot"),
        ("GET", "/ui-bridge/control/specs"),
        ("GET", "/ui-bridge/control/spec/{id}"),
        ("POST", "/ui-bridge/control/page/find-by-text"),
        ("POST", "/ui-bridge/control/page/click-by-text"),
        ("POST", "/ui-bridge/control/page/click-by-selector"),
        ("POST", "/ui-bridge/control/page/read-value"),
        ("POST", "/ui-bridge/control/page/type-into"),
        ("POST", "/ui-bridge/control/assert"),
        ("GET", "/ui-bridge/control/keyboard-shortcuts"),
        ("POST", "/ui-bridge/control/wait-for-navigation"),
        ("POST", "/ui-bridge/ai/wait-for-navigation"),
        ("POST", "/ui-bridge/control/wait-for-idle"),
        ("POST", "/ui-bridge/ai/wait-for-idle"),
        ("POST", "/ui-bridge/control/wait-for-element"),
        ("POST", "/ui-bridge/ai/wait-for-element"),
        ("POST", "/ui-bridge/ai/wait-for-element-condition"),
        ("POST", "/ui-bridge/control/batch-execute"),
        ("POST", "/ui-bridge/control/wait-for-element-state"),
        ("POST", "/ui-bridge/control/wait-for-element-stable"),
        ("POST", "/ui-bridge/control/wait-for-route"),
        ("POST", "/ui-bridge/ai/wait-for-route"),
        ("POST", "/ui-bridge/ai/expect"),
        ("POST", "/ui-bridge/control/diagnose-stuck"),
        ("POST", "/ui-bridge/control/page-health"),
        ("POST", "/ui-bridge/control/ai/search"),
        ("POST", "/ui-bridge/control/ai/find"),
        ("POST", "/ui-bridge/control/capture-element-images"),
        ("POST", "/ui-bridge/control/get-element-images"),
        ("POST", "/ui-bridge/control/find"),
        ("GET", "/ui-bridge/control/workflows"),
        ("POST", "/ui-bridge/control/workflow/{id}/run"),
        ("GET", "/ui-bridge/control/workflow/{run_id}/status"),
        ("GET", "/ui-bridge/control/element/{id}/state"),
        ("GET", "/ui-bridge/control/render-log"),
        ("POST", "/ui-bridge/control/render-log"),
        ("GET", "/ui-bridge/render-log"),
        ("POST", "/ui-bridge/render-log"),
        ("POST", "/ui-bridge/control/action-plan"),
        ("GET", "/ui-bridge/control/action-plan/cache"),
        ("GET", "/ui-bridge/control/action-plan/cache/stats"),
        ("POST", "/ui-bridge/batch"),
        ("POST", "/ui-bridge/pong"),
        ("POST", "/ui-bridge/ipc-response"),
        ("POST", "/ui-bridge/ai/search"),
        ("POST", "/ui-bridge/ai/find"),
        ("POST", "/ui-bridge/ai/execute"),
        ("POST", "/ui-bridge/ai/assert"),
        ("POST", "/ui-bridge/ai/assert-batch"),
        ("GET", "/ui-bridge/ai/snapshot"),
        ("GET", "/ui-bridge/ai/summary"),
        ("GET", "/ui-bridge/capabilities"),
        ("POST", "/ui-bridge/control/wait-for-idle/{signal}"),
        ("POST", "/ui-bridge/control/wait-for-targets"),
        ("GET", "/ui-bridge/control/action-history"),
        ("GET", "/ui-bridge/control/metrics"),
        ("GET", "/ui-bridge/control/annotations"),
        ("POST", "/ui-bridge/control/annotations"),
        ("GET", "/ui-bridge/control/annotation/{id}"),
        ("PUT", "/ui-bridge/control/annotation/{id}"),
        ("DELETE", "/ui-bridge/control/annotation/{id}"),
        ("GET", "/ui-bridge/control/annotations/coverage"),
        ("GET", "/ui-bridge/control/annotations/export"),
        ("POST", "/ui-bridge/ai/media/find"),
        ("POST", "/ui-bridge/ai/media/audit/{audit_type}"),
        ("POST", "/ui-bridge/ai/media/snapshot"),
        ("POST", "/ui-bridge/ai/media/analyze"),
        ("POST", "/ui-bridge/ai/media/analyze/batch"),
        ("POST", "/ui-bridge/ai/media/analyze/page"),
        ("GET", "/ui-bridge/control/states"),
        ("GET", "/ui-bridge/control/states/active"),
        ("GET", "/ui-bridge/control/states/snapshot"),
        ("POST", "/ui-bridge/control/states/find-path"),
        ("POST", "/ui-bridge/control/states/navigate"),
        ("GET", "/ui-bridge/control/state/{id}"),
        ("POST", "/ui-bridge/control/state/{id}/activate"),
        ("POST", "/ui-bridge/control/state/{id}/deactivate"),
        ("GET", "/ui-bridge/control/state-groups"),
        ("POST", "/ui-bridge/control/state-group/{id}/activate"),
        ("POST", "/ui-bridge/control/state-group/{id}/deactivate"),
        ("GET", "/ui-bridge/control/transitions"),
        ("GET", "/ui-bridge/control/transition/{id}/can-execute"),
        ("POST", "/ui-bridge/control/transition/{id}/execute"),
        ("POST", "/ui-bridge/ai/semantic-search"),
        ("GET", "/ui-bridge/ai/diff"),
        ("GET", "/ui-bridge/control/intents"),
        ("POST", "/ui-bridge/control/intents"),
        ("POST", "/ui-bridge/control/intents/find"),
        ("POST", "/ui-bridge/control/intents/execute"),
        ("GET", "/ui-bridge/control/component/{id}/state"),
        ("POST", "/ui-bridge/control/page/scroll"),
        ("GET", "/ui-bridge/control/performance-entries"),
        ("POST", "/ui-bridge/control/performance-entries/clear"),
        ("POST", "/ui-bridge/ai/analyze/data"),
        ("POST", "/ui-bridge/ai/analyze/regions"),
        ("POST", "/ui-bridge/ai/analyze/structured-data"),
        ("POST", "/ui-bridge/ai/analyze/cross-app-compare"),
        ("POST", "/ui-bridge/ai/recovery/attempt"),
        ("POST", "/ui-bridge/control/design/evaluate"),
        ("POST", "/ui-bridge/control/design/evaluate/baseline"),
        ("GET", "/ui-bridge/control/design/evaluate/contexts"),
        ("POST", "/ui-bridge/control/design/evaluate/diff"),
        ("POST", "/ui-bridge/ai/media/compare"),
        ("POST", "/ui-bridge/ai/image-diff"),
        ("POST", "/ui-bridge/control/ai/image-diff"),
        ("GET", "/ui-bridge/control/element-screenshot"),
        ("GET", "/ui-bridge/ai/element-screenshot"),
        ("POST", "/ui-bridge/control/annotations/import"),
        ("POST", "/ui-bridge/control/intents/execute-from-query"),
        ("GET", "/ui-bridge/debug/element-tree"),
        ("POST", "/ui-bridge/debug/highlight/{id}"),
        ("GET", "/ui-bridge/history/elements"),
        ("GET", "/ui-bridge/history/element/{id}"),
        ("GET", "/ui-bridge/history/flaky"),
        ("GET", "/ui-bridge/graph/element-reliability"),
        ("GET", "/ui-bridge/analytics/decay-curve"),
        ("GET", "/ui-bridge/analytics/action-baselines"),
        ("GET", "/ui-bridge/analytics/failure-taxonomy"),
        ("GET", "/ui-bridge/analytics/fragility-heatmap"),
        ("GET", "/ui-bridge/analytics/regressions"),
        ("GET", "/ui-bridge/analytics/stall-frequency"),
        ("GET", "/ui-bridge/analytics/intervention-effectiveness"),
        ("GET", "/ui-bridge/analytics/state-coverage"),
        ("GET", "/ui-bridge/analytics/annotation-gaps"),
        ("GET", "/ui-bridge/analytics/health-score"),
        ("GET", "/ui-bridge/analytics/recommendations"),
        ("POST", "/ui-bridge/control/navigate-tab"),
        ("POST", "/ui-bridge/control/clear-storage"),
        ("POST", "/ui-bridge/control/undo"),
        ("POST", "/ui-bridge/control/redo"),
        ("GET", "/ui-bridge/control/undo-state"),
        // Phase 3I.1 + 3I.2 — UI Bridge invoke proxy
        ("GET", "/ui-bridge/commands"),
        ("POST", "/ui-bridge/invoke/{command_name}"),
    ]
}

pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        // P2.2 — Endpoint discovery. `_help` is a cross-platform alias that
        // works on both runner and mobile UI Bridge; returns the same payload
        // here so one URL covers both surfaces.
        .route("/ui-bridge/_routes", get(ui_bridge_routes_manifest_handler))
        .route("/ui-bridge/_help", get(ui_bridge_routes_manifest_handler))
        .route(
            "/ui-bridge/control/elements",
            get(ui_bridge_get_elements_handler),
        )
        .route(
            "/ui-bridge/control/elements/last-discovered",
            get(ui_bridge_get_last_discovered_handler),
        )
        .route(
            "/ui-bridge/ai/elements/last-discovered",
            get(ui_bridge_get_last_discovered_handler),
        )
        .route(
            "/ui-bridge/control/element/{id}",
            get(ui_bridge_get_element_handler),
        )
        .route(
            "/ui-bridge/control/element/{id}/assert",
            post(ui_bridge_assert_element_handler),
        )
        .route(
            "/ui-bridge/control/element/{id}/action",
            post(ui_bridge_execute_action_handler),
        )
        .route(
            "/ui-bridge/control/batch-actions",
            post(ui_bridge_batch_actions_handler),
        )
        // P3.4 — Sequential step execution with snapshot diffing
        .route(
            "/ui-bridge/control/batch",
            post(ui_bridge_control_batch_handler),
        )
        .route(
            "/ui-bridge/control/components",
            get(ui_bridge_get_components_handler),
        )
        .route(
            "/ui-bridge/control/component/{id}",
            get(ui_bridge_get_component_handler),
        )
        .route(
            "/ui-bridge/control/component/{id}/action/{action_id}",
            post(ui_bridge_execute_component_action_handler),
        )
        .route(
            "/ui-bridge/control/discover",
            post(ui_bridge_discover_handler),
        )
        .route(
            "/ui-bridge/control/snapshot",
            get(ui_bridge_get_snapshot_handler),
        )
        .route(
            "/ui-bridge/control/annotated-screenshot",
            get(ui_bridge_annotated_screenshot_handler),
        )
        // Undo/Redo awareness
        .route(
            "/ui-bridge/control/undo-state",
            get(ui_bridge_get_undo_state_handler),
        )
        .route("/ui-bridge/control/undo", post(ui_bridge_undo_handler))
        .route("/ui-bridge/control/redo", post(ui_bridge_redo_handler))
        // Form state awareness, /ai/* form aliases, and clipboard live in
        // `forms::routes()` — see `forms.rs`.
        // /ai/design-audit lives in `design::routes()` — see `design.rs`.
        // Network request monitoring, console errors, browser events, and
        // timeline live in `network::routes()` — see `network.rs`.
        .route("/ui-bridge/control/specs", get(ui_bridge_get_specs_handler))
        .route(
            "/ui-bridge/control/spec/{id}",
            get(ui_bridge_get_spec_handler),
        )
        // Page navigation, evaluation, tab switching, navigate-and-wait, and
        // page summary live in `page::routes()` — see `page.rs`.
        // Convenience DOM interaction endpoints
        .route(
            "/ui-bridge/control/page/find-by-text",
            post(ui_bridge_find_by_text_handler),
        )
        .route(
            "/ui-bridge/control/page/click-by-text",
            post(ui_bridge_click_by_text_handler),
        )
        .route(
            "/ui-bridge/control/page/click-by-selector",
            post(ui_bridge_click_by_selector_handler),
        )
        .route(
            "/ui-bridge/control/page/read-value",
            post(ui_bridge_read_value_handler),
        )
        .route(
            "/ui-bridge/control/page/type-into",
            post(ui_bridge_type_into_handler),
        )
        .route(
            "/ui-bridge/control/assert",
            post(ui_bridge_structured_assert_handler),
        )
        // Design Review (manual handlers + /ai/design-audit alias) live in
        // `design::routes()` — see `design.rs`. Macro-generated
        // `design/evaluate*` handlers stay in this file.
        // Change tracking + bookmarks + with-diff (control + ai aliases) live
        // in `bookmarks::routes()` — see `bookmarks.rs`.
        // Keyboard shortcuts
        .route(
            "/ui-bridge/control/keyboard-shortcuts",
            get(ui_bridge_get_keyboard_shortcuts_handler),
        )
        .route(
            "/ui-bridge/control/wait-for-navigation",
            post(ui_bridge_wait_for_navigation_handler),
        )
        .route(
            "/ui-bridge/ai/wait-for-navigation",
            post(ui_bridge_wait_for_navigation_handler),
        )
        .route(
            "/ui-bridge/control/wait-for-idle",
            post(ui_bridge_wait_for_idle_handler),
        )
        // /ai/idle-status alias moved to `errors::routes()` for namespace
        // consistency with ai/page-summary, ai/find, ai/execute, etc.
        .route(
            "/ui-bridge/ai/wait-for-idle",
            post(ui_bridge_wait_for_idle_handler),
        )
        // Wait for an element to appear (polls ai_find or get_element)
        .route(
            "/ui-bridge/control/wait-for-element",
            post(ui_bridge_wait_for_element_handler),
        )
        .route(
            "/ui-bridge/ai/wait-for-element",
            post(ui_bridge_wait_for_element_handler),
        )
        // Tier 3.1 — Registry-based element condition polling
        .route(
            "/ui-bridge/ai/wait-for-element-condition",
            post(ui_bridge_wait_for_element_condition_handler),
        )
        // Tier 3.2 — Mixed action/wait/snapshot batch
        .route(
            "/ui-bridge/control/batch-execute",
            post(ui_bridge_control_batch_execute_handler),
        )
        // P3.3 — Convenience wait-for-element-state (simple id+state form)
        .route(
            "/ui-bridge/control/wait-for-element-state",
            post(ui_bridge_wait_for_element_state_handler),
        )
        // P3.3 — Wait for the page route to match a glob pattern
        .route(
            "/ui-bridge/control/wait-for-route",
            post(ui_bridge_wait_for_route_handler),
        )
        .route(
            "/ui-bridge/ai/wait-for-route",
            post(ui_bridge_wait_for_route_handler),
        )
        // Wait for an element to become visually/structurally stable
        .route(
            "/ui-bridge/control/wait-for-element-stable",
            post(ui_bridge_wait_for_element_stable_handler),
        )
        // Wait for a substring to appear in the page text
        .route("/ui-bridge/ai/expect", post(ui_bridge_expect_text_handler))
        .route(
            "/ui-bridge/control/diagnose-stuck",
            post(ui_bridge_diagnose_stuck_screen_handler),
        )
        .route(
            "/ui-bridge/control/page-health",
            post(ui_bridge_page_health_handler),
        )
        // AI search & find
        .route(
            "/ui-bridge/control/ai/search",
            post(ui_bridge_ai_search_handler),
        )
        .route(
            "/ui-bridge/control/ai/find",
            post(ui_bridge_ai_find_handler),
        )
        // Element image capture (DOM-based, no screen capture)
        .route(
            "/ui-bridge/control/capture-element-images",
            post(ui_bridge_capture_element_images_handler),
        )
        // Element image metadata (reads <img> src attributes, no rendering)
        .route(
            "/ui-bridge/control/get-element-images",
            post(ui_bridge_get_element_images_handler),
        )
        // Find, workflows, element state, render log
        .route("/ui-bridge/control/find", post(ui_bridge_find_handler))
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
        .route(
            "/ui-bridge/control/render-log",
            get(ui_bridge_get_render_log_handler).post(ui_bridge_append_render_log_handler),
        )
        // Render log alias (matches web's /render-log path for cross-app consistency)
        .route(
            "/ui-bridge/render-log",
            get(ui_bridge_get_render_log_handler).post(ui_bridge_append_render_log_handler),
        )
        // Action plan execution & caching
        .route(
            "/ui-bridge/control/action-plan",
            post(ui_bridge_execute_action_plan_handler),
        )
        .route(
            "/ui-bridge/control/action-plan/cache",
            get(ui_bridge_action_plan_cache_lookup_handler),
        )
        .route(
            "/ui-bridge/control/action-plan/cache/stats",
            get(ui_bridge_action_plan_cache_stats_handler),
        )
        // Batch execution
        .route("/ui-bridge/batch", post(ui_bridge_batch_handler))
        .route("/ui-bridge/pong", post(ui_bridge_pong_handler))
        .route(
            "/ui-bridge/ipc-response",
            post(ui_bridge_ipc_response_handler),
        )
        // Exploration + window listing live in `exploration::routes()` —
        // see `exploration.rs`.
        // /ai/* bookmark aliases live in `bookmarks::routes()` —
        // see `bookmarks.rs`.
        .route("/ui-bridge/ai/search", post(ui_bridge_ai_search_handler))
        .route("/ui-bridge/ai/find", post(ui_bridge_ai_find_handler))
        // Phase 2: AI endpoints
        .route("/ui-bridge/ai/execute", post(ui_bridge_ai_execute_handler))
        .route("/ui-bridge/ai/assert", post(ui_bridge_ai_assert_handler))
        .route(
            "/ui-bridge/ai/assert-batch",
            post(ui_bridge_ai_assert_batch_handler),
        )
        .route("/ui-bridge/ai/snapshot", get(ui_bridge_ai_snapshot_handler))
        .route("/ui-bridge/ai/summary", get(ui_bridge_ai_summary_handler))
        // Phase 3: Capabilities
        .route(
            "/ui-bridge/capabilities",
            get(ui_bridge_capabilities_handler),
        )
        // Phase 4: Idle sub-signal wait (get moved to errors::routes())
        .route(
            "/ui-bridge/control/wait-for-idle/{signal}",
            post(ui_bridge_wait_for_idle_signal_handler),
        )
        .route(
            "/ui-bridge/control/wait-for-targets",
            post(ui_bridge_wait_for_targets_handler),
        )
        // Phase 4: Action history & metrics
        .route(
            "/ui-bridge/control/action-history",
            get(ui_bridge_get_action_history_handler),
        )
        .route(
            "/ui-bridge/control/metrics",
            get(ui_bridge_get_interaction_metrics_handler),
        )
        // Phase 5: Annotations
        .route(
            "/ui-bridge/control/annotations",
            get(ui_bridge_annotations_list_handler).post(ui_bridge_annotations_create_handler),
        )
        .route(
            "/ui-bridge/control/annotation/{id}",
            get(ui_bridge_annotations_get_handler)
                .put(ui_bridge_annotations_update_handler)
                .delete(ui_bridge_annotations_delete_handler),
        )
        .route(
            "/ui-bridge/control/annotations/coverage",
            get(ui_bridge_annotations_coverage_handler),
        )
        .route(
            "/ui-bridge/control/annotations/export",
            get(ui_bridge_annotations_export_handler),
        )
        // Media routes
        .route(
            "/ui-bridge/ai/media/find",
            post(ui_bridge_media_find_handler),
        )
        .route(
            "/ui-bridge/ai/media/audit/{audit_type}",
            post(ui_bridge_media_audit_handler),
        )
        .route(
            "/ui-bridge/ai/media/snapshot",
            post(ui_bridge_media_snapshot_handler),
        )
        .route(
            "/ui-bridge/ai/media/analyze",
            post(ui_bridge_media_analyze_handler),
        )
        .route(
            "/ui-bridge/ai/media/analyze/batch",
            post(ui_bridge_media_analyze_handler),
        )
        .route(
            "/ui-bridge/ai/media/analyze/page",
            post(ui_bridge_media_analyze_handler),
        )
        // State machine routes
        .route(
            "/ui-bridge/control/states",
            get(ui_bridge_get_states_handler),
        )
        .route(
            "/ui-bridge/control/states/active",
            get(ui_bridge_get_active_states_handler),
        )
        .route(
            "/ui-bridge/control/states/snapshot",
            get(ui_bridge_get_state_snapshot_handler),
        )
        .route(
            "/ui-bridge/control/states/find-path",
            post(ui_bridge_find_state_path_handler),
        )
        .route(
            "/ui-bridge/control/states/navigate",
            post(ui_bridge_navigate_to_state_handler),
        )
        .route(
            "/ui-bridge/control/state/{id}",
            get(ui_bridge_get_state_handler),
        )
        .route(
            "/ui-bridge/control/state/{id}/activate",
            post(ui_bridge_activate_state_handler),
        )
        .route(
            "/ui-bridge/control/state/{id}/deactivate",
            post(ui_bridge_deactivate_state_handler),
        )
        .route(
            "/ui-bridge/control/state-groups",
            get(ui_bridge_get_state_groups_handler),
        )
        .route(
            "/ui-bridge/control/state-group/{id}/activate",
            post(ui_bridge_activate_state_group_handler),
        )
        .route(
            "/ui-bridge/control/state-group/{id}/deactivate",
            post(ui_bridge_deactivate_state_group_handler),
        )
        .route(
            "/ui-bridge/control/transitions",
            get(ui_bridge_get_transitions_handler),
        )
        .route(
            "/ui-bridge/control/transition/{id}/can-execute",
            get(ui_bridge_can_execute_transition_handler),
        )
        .route(
            "/ui-bridge/control/transition/{id}/execute",
            post(ui_bridge_execute_transition_handler),
        )
        // AI semantic search & diff
        .route(
            "/ui-bridge/ai/semantic-search",
            post(ui_bridge_ai_semantic_search_handler),
        )
        .route("/ui-bridge/ai/diff", get(ui_bridge_ai_diff_handler))
        // Intents
        .route(
            "/ui-bridge/control/intents",
            get(ui_bridge_get_intents_handler).post(ui_bridge_register_intent_handler),
        )
        .route(
            "/ui-bridge/control/intents/find",
            post(ui_bridge_find_intent_handler),
        )
        .route(
            "/ui-bridge/control/intents/execute",
            post(ui_bridge_execute_intent_handler),
        )
        // Component state
        .route(
            "/ui-bridge/control/component/{id}/state",
            get(ui_bridge_get_component_state_handler),
        )
        // Page scroll
        .route(
            "/ui-bridge/control/page/scroll",
            post(ui_bridge_scroll_page_handler),
        )
        // Performance entries
        .route(
            "/ui-bridge/control/performance-entries",
            get(ui_bridge_get_performance_entries_handler),
        )
        .route(
            "/ui-bridge/control/performance-entries/clear",
            post(ui_bridge_clear_performance_entries_handler),
        )
        // AI analysis
        .route(
            "/ui-bridge/ai/analyze/data",
            post(ui_bridge_ai_analyze_data_handler),
        )
        .route(
            "/ui-bridge/ai/analyze/regions",
            post(ui_bridge_ai_analyze_regions_handler),
        )
        .route(
            "/ui-bridge/ai/analyze/structured-data",
            post(ui_bridge_ai_analyze_structured_handler),
        )
        .route(
            "/ui-bridge/ai/analyze/cross-app-compare",
            post(ui_bridge_ai_analyze_cross_app_handler),
        )
        .route(
            "/ui-bridge/ai/recovery/attempt",
            post(ui_bridge_ai_recovery_attempt_handler),
        )
        // Design evaluation
        .route(
            "/ui-bridge/control/design/evaluate",
            post(ui_bridge_design_evaluate_handler),
        )
        .route(
            "/ui-bridge/control/design/evaluate/baseline",
            post(ui_bridge_design_evaluate_baseline_handler),
        )
        .route(
            "/ui-bridge/control/design/evaluate/contexts",
            get(ui_bridge_design_evaluate_contexts_handler),
        )
        .route(
            "/ui-bridge/control/design/evaluate/diff",
            post(ui_bridge_design_evaluate_diff_handler),
        )
        // Media compare
        .route(
            "/ui-bridge/ai/media/compare",
            post(ui_bridge_media_compare_handler),
        )
        // Pixel-accurate image diff (canonical visual regression)
        .route(
            "/ui-bridge/ai/image-diff",
            post(ui_bridge_image_diff_handler),
        )
        .route(
            "/ui-bridge/control/ai/image-diff",
            post(ui_bridge_image_diff_handler),
        )
        // Single-element screenshot (thin wrapper over capture_element_images)
        .route(
            "/ui-bridge/control/element-screenshot",
            get(ui_bridge_element_screenshot_handler),
        )
        .route(
            "/ui-bridge/ai/element-screenshot",
            get(ui_bridge_element_screenshot_handler),
        )
        // Annotations import
        .route(
            "/ui-bridge/control/annotations/import",
            post(ui_bridge_annotations_import_handler),
        )
        // Intents from NL query
        .route(
            "/ui-bridge/control/intents/execute-from-query",
            post(ui_bridge_execute_intent_from_query_handler),
        )
        // Debug
        .route(
            "/ui-bridge/debug/element-tree",
            get(ui_bridge_get_element_tree_handler),
        )
        .route(
            "/ui-bridge/debug/highlight/{id}",
            post(ui_bridge_highlight_element_handler),
        )
        // Persisted interaction history (cross-run analysis)
        .route(
            "/ui-bridge/history/elements",
            get(ui_bridge_history_elements_handler),
        )
        .route(
            "/ui-bridge/history/element/{id}",
            get(ui_bridge_history_element_handler),
        )
        .route(
            "/ui-bridge/history/flaky",
            get(ui_bridge_history_flaky_handler),
        )
        .route(
            "/ui-bridge/graph/element-reliability",
            get(ui_bridge_element_reliability_handler),
        )
        // Analytics endpoints (Phase 1 + 2)
        .route(
            "/ui-bridge/analytics/decay-curve",
            get(analytics_decay_curve_handler),
        )
        .route(
            "/ui-bridge/analytics/action-baselines",
            get(analytics_action_baselines_handler),
        )
        .route(
            "/ui-bridge/analytics/failure-taxonomy",
            get(analytics_failure_taxonomy_handler),
        )
        .route(
            "/ui-bridge/analytics/fragility-heatmap",
            get(analytics_fragility_heatmap_handler),
        )
        .route(
            "/ui-bridge/analytics/regressions",
            get(analytics_regressions_handler),
        )
        .route(
            "/ui-bridge/analytics/stall-frequency",
            get(analytics_stall_frequency_handler),
        )
        .route(
            "/ui-bridge/analytics/intervention-effectiveness",
            get(analytics_intervention_handler),
        )
        .route(
            "/ui-bridge/analytics/state-coverage",
            get(analytics_state_coverage_handler),
        )
        .route(
            "/ui-bridge/analytics/annotation-gaps",
            get(analytics_annotation_gaps_handler),
        )
        .route(
            "/ui-bridge/analytics/health-score",
            get(analytics_health_score_handler),
        )
        .route(
            "/ui-bridge/analytics/recommendations",
            get(analytics_recommendations_handler),
        )
        // Runner-specific: tab navigation and storage management
        .route(
            "/ui-bridge/control/navigate-tab",
            post(ui_bridge_navigate_tab_handler),
        )
        .route(
            "/ui-bridge/control/clear-storage",
            post(ui_bridge_clear_storage_handler),
        )
        // Bookmark, change-tracking and with-diff handlers (extracted to bookmarks.rs)
        .merge(bookmarks::routes())
        // Design Review handlers (extracted to design.rs)
        .merge(design::routes())
        // Health, error snapshots/sessions/baselines, diagnostics, idle status
        // (extracted to errors.rs)
        .merge(errors::routes())
        // Form state + clipboard handlers (extracted to forms.rs)
        .merge(forms::routes())
        // Console errors, browser events, timeline, network requests
        // (extracted to network.rs)
        .merge(network::routes())
        // UI Bridge exploration + window listing (extracted to exploration.rs)
        .merge(exploration::routes())
        // Page navigation / evaluation / tab switching (extracted to page.rs)
        .merge(page::routes())
        // Tier 2.1 — safelisted Tauri command proxy
        .merge(crate::mcp::tauri_proxy::routes())
        // Phase 3I.1 + 3I.2 — UI Bridge invoke proxy (HTTP → Tauri invoke round-trip)
        .route(
            "/ui-bridge/commands",
            get(crate::mcp::ui_bridge_invoke_handlers::ui_bridge_commands_handler),
        )
        .route(
            "/ui-bridge/invoke/{command_name}",
            post(crate::mcp::ui_bridge_invoke_handlers::ui_bridge_invoke_handler),
        )
}

#[cfg(test)]
mod manifest_drift_tests {
    use super::route_manifest;
    use std::collections::HashSet;
    use std::path::PathBuf;

    /// Verify `route_manifest()` matches the actual `.route(...)` calls in
    /// this file. Catches the common drift bug where someone adds a new
    /// endpoint but forgets to register it in the manifest (or the reverse).
    ///
    /// This is a stop-gap for the manual maintenance burden — axum 0.8
    /// doesn't expose `Router::routes()`, so we can't introspect the live
    /// router at runtime. Re-evaluate if axum adds router introspection.
    #[test]
    fn manifest_matches_route_calls() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let src_path = PathBuf::from(manifest_dir).join("src/mcp/ui_bridge.rs");
        let src = std::fs::read_to_string(&src_path)
            .unwrap_or_else(|e| panic!("read {}: {}", src_path.display(), e));

        // For each route registration, extract every HTTP method present
        // (axum allows chaining like get(h).delete(h2) on the same route).
        // Two passes: find each .route call, then within its body grep for
        // method-constructor calls.
        let route_open_re =
            regex::Regex::new(r#"(?s)\.route\(\s*"(/ui-bridge/[^"]+)"\s*,"#).unwrap();
        let method_re = regex::Regex::new(r#"\b(get|post|put|delete|patch)\("#).unwrap();

        let mut source_routes: HashSet<(String, String)> = HashSet::new();
        for cap in route_open_re.captures_iter(&src) {
            let path = cap[1].to_string();
            // Body starts at end of the matched prefix; scan forward up to
            // 400 bytes (largest known route body has 3 chained methods).
            let body_start = cap.get(0).unwrap().end();
            let body_end = (body_start + 400).min(src.len());
            let body = &src[body_start..body_end];
            // Stop at first balanced ")" — for our purposes, the next ".route("
            // delimiter is a safe upper bound, and we only care about methods
            // before the first newline followed by ".route(" or "}\n".
            let scan_end = body
                .find("\n        .route(")
                .or_else(|| body.find("\n    }"))
                .unwrap_or(body.len());
            let scan = &body[..scan_end];

            for m in method_re.captures_iter(scan) {
                source_routes.insert((m[1].to_uppercase(), path.clone()));
            }
        }

        let manifest_routes: HashSet<(String, String)> = route_manifest()
            .iter()
            .map(|(m, p)| ((*m).to_string(), (*p).to_string()))
            .collect();

        let registered_but_missing: Vec<&(String, String)> =
            source_routes.difference(&manifest_routes).collect();
        let in_manifest_but_unregistered: Vec<&(String, String)> =
            manifest_routes.difference(&source_routes).collect();

        if !registered_but_missing.is_empty() || !in_manifest_but_unregistered.is_empty() {
            panic!(
                "route_manifest() drift detected.\n\
                 Add the missing entries to route_manifest() (or remove unregistered ones).\n\n\
                 Registered via .route() but missing from manifest ({}):\n  {}\n\n\
                 In manifest but not actually registered ({}):\n  {}",
                registered_but_missing.len(),
                registered_but_missing
                    .iter()
                    .map(|(m, p)| format!("{} {}", m, p))
                    .collect::<Vec<_>>()
                    .join("\n  "),
                in_manifest_but_unregistered.len(),
                in_manifest_but_unregistered
                    .iter()
                    .map(|(m, p)| format!("{} {}", m, p))
                    .collect::<Vec<_>>()
                    .join("\n  "),
            );
        }

        // Sanity floor: catch a regex regression that silently matches nothing.
        assert!(
            source_routes.len() > 100,
            "regex extracted only {} routes — likely broken",
            source_routes.len()
        );
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
