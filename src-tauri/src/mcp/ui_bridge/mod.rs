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

pub mod ai;
pub mod analytics;
pub mod bookmarks;
pub mod capabilities;
pub mod circuit_breaker;
pub mod design;
pub mod errors;
pub mod exploration;
pub mod forms;
pub mod helpers;
pub mod history;
pub mod intents;
pub mod network;
pub mod page;
pub mod request;
pub mod screenshots;
pub mod types;

// Re-export all public symbols so every currently-used path like
// `crate::mcp::ui_bridge::UiBridgeError`, `::ui_bridge_request_sync`,
// `::handle_ui_bridge_response`, `::UiBridgeCircuitBreaker` still resolves.
pub use ai::{
    ui_bridge_action_plan_cache_lookup_handler, ui_bridge_action_plan_cache_stats_handler,
    ui_bridge_ai_assert_batch_handler, ui_bridge_ai_assert_handler, ui_bridge_ai_execute_handler,
    ui_bridge_ai_find_handler, ui_bridge_ai_search_handler, ui_bridge_ai_snapshot_handler,
    ui_bridge_ai_summary_handler, ui_bridge_execute_action_plan_handler,
    ActionPlanCacheLookupQuery, ActionPlanElementTarget, ActionPlanRequest, ActionPlanResponse,
    AiSnapshotQuery, PlannedAction, PlannedActionResult,
};
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
pub use capabilities::{
    ui_bridge_append_render_log_handler, ui_bridge_batch_handler, ui_bridge_capabilities_handler,
    ui_bridge_control_batch_execute_handler, ui_bridge_control_batch_handler,
    ui_bridge_expect_text_handler, ui_bridge_get_action_history_handler,
    ui_bridge_get_element_state_handler, ui_bridge_get_interaction_metrics_handler,
    ui_bridge_get_keyboard_shortcuts_handler, ui_bridge_get_render_log_handler,
    ui_bridge_get_workflow_status_handler, ui_bridge_get_workflows_handler,
    ui_bridge_ipc_response_handler, ui_bridge_pong_handler, ui_bridge_routes_manifest_handler,
    ui_bridge_run_workflow_handler, ui_bridge_structured_assert_handler, AssertResult,
    BatchOperation, BatchOperationResult, BatchRequest, BatchResponse, StructuredAssertRequest,
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
pub use intents::{
    ui_bridge_wait_for_element_condition_handler, ui_bridge_wait_for_element_stable_handler,
    ui_bridge_wait_for_element_state_handler, ui_bridge_wait_for_idle_handler,
    ui_bridge_wait_for_idle_signal_handler, ui_bridge_wait_for_navigation_handler,
    ui_bridge_wait_for_route_handler, ui_bridge_wait_for_targets_handler,
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
pub use screenshots::{
    capture_runner_window_base64, ui_bridge_annotated_screenshot_handler,
    ui_bridge_annotations_coverage_handler, ui_bridge_annotations_create_handler,
    ui_bridge_annotations_delete_handler, ui_bridge_annotations_export_handler,
    ui_bridge_annotations_get_handler, ui_bridge_annotations_list_handler,
    ui_bridge_annotations_update_handler, ui_bridge_capture_element_images_handler,
    ui_bridge_diagnose_stuck_screen_handler, ui_bridge_element_screenshot_handler,
    ui_bridge_get_element_images_handler, ui_bridge_media_analyze_handler,
    ui_bridge_media_audit_handler, ui_bridge_media_find_handler,
    ui_bridge_media_snapshot_handler, ui_bridge_page_health_handler,
    AnnotatedScreenshotData, AnnotatedScreenshotQuery, PageHealthRequest,
};
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
// Exploration Handlers
// ============================================================================


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

/// Get render log entries.



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
pub(super) fn route_manifest() -> &'static [(&'static str, &'static str)] {
    use std::sync::OnceLock;
    static MANIFEST: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        let mut all: Vec<(&'static str, &'static str)> = Vec::new();
        all.extend_from_slice(local_route_entries());
        all.extend_from_slice(ai::route_entries());
        all.extend_from_slice(bookmarks::route_entries());
        all.extend_from_slice(capabilities::route_entries());
        all.extend_from_slice(design::route_entries());
        all.extend_from_slice(errors::route_entries());
        all.extend_from_slice(exploration::route_entries());
        all.extend_from_slice(forms::route_entries());
        all.extend_from_slice(intents::route_entries());
        all.extend_from_slice(network::route_entries());
        all.extend_from_slice(page::route_entries());
        all.extend_from_slice(screenshots::route_entries());
        all
    })
}

/// Routes that are still defined inline in `mod.rs::routes()`.
/// Per-family extractions append their own `route_entries()` via
/// `route_manifest()` above; do not list those entries here.
fn local_route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/control/elements"),
        ("GET", "/ui-bridge/control/elements/last-discovered"),
        ("GET", "/ui-bridge/ai/elements/last-discovered"),
        ("GET", "/ui-bridge/control/element/{id}"),
        ("POST", "/ui-bridge/control/element/{id}/assert"),
        ("POST", "/ui-bridge/control/element/{id}/action"),
        ("POST", "/ui-bridge/control/batch-actions"),
        ("GET", "/ui-bridge/control/components"),
        ("GET", "/ui-bridge/control/component/{id}"),
        (
            "POST",
            "/ui-bridge/control/component/{id}/action/{action_id}",
        ),
        ("POST", "/ui-bridge/control/discover"),
        ("GET", "/ui-bridge/control/snapshot"),
        ("GET", "/ui-bridge/control/specs"),
        ("GET", "/ui-bridge/control/spec/{id}"),
        ("POST", "/ui-bridge/control/page/find-by-text"),
        ("POST", "/ui-bridge/control/page/click-by-text"),
        ("POST", "/ui-bridge/control/page/click-by-selector"),
        ("POST", "/ui-bridge/control/page/read-value"),
        ("POST", "/ui-bridge/control/page/type-into"),
        ("POST", "/ui-bridge/control/wait-for-element"),
        ("POST", "/ui-bridge/ai/wait-for-element"),
        ("POST", "/ui-bridge/control/find"),
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
        // Endpoint discovery (`/_routes`, `/_help`) lives in
        // `capabilities::routes()` — see `capabilities.rs`.
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
        // `/control/batch` (step-level diff batch) lives in
        // `capabilities::routes()`.
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
        // `/control/annotated-screenshot` lives in `screenshots::routes()`.
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
        // `/control/assert` (structured assert) lives in
        // `capabilities::routes()`.
        // Design Review (manual handlers + /ai/design-audit alias) live in
        // `design::routes()` — see `design.rs`. Macro-generated
        // `design/evaluate*` handlers stay in this file.
        // Change tracking + bookmarks + with-diff (control + ai aliases) live
        // in `bookmarks::routes()` — see `bookmarks.rs`.
        // Keyboard shortcuts live in `capabilities::routes()`.
        // Wait-for handlers (navigation, idle, element-stable / condition /
        // state, route, targets, idle signal) live in `intents::routes()`.
        // Wait for an element to appear (polls ai_find or get_element) —
        // this family stays here alongside the elements surface.
        .route(
            "/ui-bridge/control/wait-for-element",
            post(ui_bridge_wait_for_element_handler),
        )
        .route(
            "/ui-bridge/ai/wait-for-element",
            post(ui_bridge_wait_for_element_handler),
        )
        // `/control/batch-execute` (mixed action/wait/snapshot) lives in
        // `capabilities::routes()`.
        // `/ai/expect` lives in `capabilities::routes()`.
        // `/control/diagnose-stuck`, `/control/page-health`,
        // `/control/capture-element-images`, `/control/get-element-images`
        // live in `screenshots::routes()`.
        // `/control/ai/search`, `/control/ai/find` live in `ai::routes()`.
        // Find
        .route("/ui-bridge/control/find", post(ui_bridge_find_handler))
        // Workflows, element-state, render log, pong, ipc-response, batch
        // execution and action plan cache live in `capabilities::routes()`
        // and `ai::routes()`.
        // Exploration + window listing live in `exploration::routes()` —
        // see `exploration.rs`.
        // /ai/* bookmark aliases live in `bookmarks::routes()` —
        // see `bookmarks.rs`.
        // AI search/find/execute/assert/snapshot/summary live in
        // `ai::routes()` — see `ai.rs`.
        // `/capabilities`, `/control/action-history`, `/control/metrics` live
        // in `capabilities::routes()`.
        // Phase 4: Idle sub-signal wait (get moved to errors::routes(),
        // post moved to intents::routes()).
        // Annotations CRUD and media routes live in `screenshots::routes()`.
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
        // Element-screenshot routes live in `screenshots::routes()`.
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
        // Wait-for / intent polling handlers (extracted to intents.rs)
        .merge(intents::routes())
        // Console errors, browser events, timeline, network requests
        // (extracted to network.rs)
        .merge(network::routes())
        // UI Bridge exploration + window listing (extracted to exploration.rs)
        .merge(exploration::routes())
        // Page navigation / evaluation / tab switching (extracted to page.rs)
        .merge(page::routes())
        // Screenshot capture, annotations CRUD, media routes, page-health,
        // diagnose-stuck (extracted to screenshots.rs)
        .merge(screenshots::routes())
        // AI search/find/execute/assert + action plan execution
        // (extracted to ai.rs)
        .merge(ai::routes())
        // Capabilities, keyboard shortcuts, action history, metrics,
        // structured assert, expect text, render log, pong, IPC response,
        // batch execution, control batch, routes manifest, workflow run/
        // status, element state (extracted to capabilities.rs)
        .merge(capabilities::routes())
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
