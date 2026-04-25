//! Element, component, discovery, snapshot, wait-for-element and
//! DOM-convenience (find/click/read/type) HTTP handlers.
//!
//! Extracted from `mod.rs` as the final per-family extraction pass. This
//! file houses roughly the "elements surface" of the UI Bridge HTTP API:
//!
//!   - Primary element operations (`get_elements`, `get_element`,
//!     `assert_element`, `execute_action`, `batch_actions`)
//!   - Component operations (`get_components`, `get_component`,
//!     `execute_component_action`)
//!   - Discovery and snapshotting (`discover`, `get_snapshot`,
//!     `get_last_discovered`)
//!   - Wait-for-element polling with optional declarative assertions
//!   - DOM convenience helpers (`find`, `find_by_text`, `click_by_text`,
//!     `click_by_selector`, `read_value`, `type_into`)
//!
//! Shared helpers (snapshot signatures, field filtering, JS evaluation)
//! live in `helpers.rs` — this module only owns the tightly-coupled
//! `evaluate_wait_for_element_assertions` which nothing else needs.

use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use tracing::{debug, error, info, warn};

use crate::mcp::types::{api_error, api_error_detailed, ApiResponse, ApiState};

use super::helpers::{
    count_elements_in_discover_payload, direct_webview_evaluate_with_result,
    evaluate_js_expression, extract_ai_find_match, extract_get_element_match,
    filter_element_fields, snapshot_signature,
};
use super::request::{ui_bridge_request_sync, wrap_ipc_result};
use super::screenshots::capture_runner_window_base64;
use super::types::{
    classify_transport_error, UIBridgeActionRequest, UIBridgeComponentActionRequest,
    UIBridgeDiscoveryRequest, UiBridgeError,
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
            return Err((StatusCode::BAD_REQUEST, Json(ApiResponse::error(hint))));
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

    // Body-shape routing — three accepted shapes share `/ai/wait-for-element`:
    //
    //   1. `{ state: "...", elementId|selector, ... }` — M1 state-predicate.
    //      Forwarded to the SDK runtime which polls the registry and returns
    //      `{ found, durationMs, finalState | lastObservedState }`. The
    //      `state` field is a string predicate (visible|enabled|...), which
    //      no other shape uses, so this disambiguator is unambiguous.
    //   2. `{ predicate: {...}, requirement?, ... }` — `waitForElementRegistered`.
    //   3. `{ query|elementId, assertions?, ... }` — legacy poll-via-ai_find.
    if body.get("state").and_then(|v| v.as_str()).is_some() {
        return ui_bridge_wait_for_element_state_predicate_handler(state, body).await;
    }
    if body.get("predicate").is_some() {
        return ui_bridge_wait_for_element_registered_forward(state, body).await;
    }

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

/// Forward a `predicate`-shaped wait-for-element body to the SDK's
/// `waitForElementRegistered` dispatcher. We stamp a defensive timeout
/// clamp before forwarding so a runaway client can't park the webview
/// listener indefinitely, mirroring Phase 1's /wait-for-route-change
/// behaviour. The SDK itself re-clamps to the same window.
async fn ui_bridge_wait_for_element_registered_forward(
    state: Arc<ApiState>,
    mut body: serde_json::Value,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Clamp timeoutMs to [100, 60_000] (default 5000) before forwarding.
    let raw_timeout = body
        .get("timeoutMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(5000);
    let timeout_ms = raw_timeout.clamp(100, 60_000);
    if let serde_json::Value::Object(ref mut map) = body {
        map.insert(
            "timeoutMs".to_string(),
            serde_json::Value::from(timeout_ms),
        );
    }

    info!(
        "UI Bridge API: wait-for-element (predicate shape) predicate={:?} requirement={:?} timeoutMs={}",
        body.get("predicate"),
        body.get("requirement").and_then(|v| v.as_str()),
        timeout_ms,
    );

    let payload = serde_json::json!({ "params": body });
    match ui_bridge_request_sync(&state, "wait_for_element_registered", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!(
                "UI Bridge API: wait-for-element (predicate shape) failed: {}",
                e
            );
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// M1 wait-for-element state-predicate validation.
///
/// `Ok(WaitForElementStateRequest)` on success; `Err((status, msg))` with
/// HTTP status + descriptive error on failure. Pulled out as a pure helper
/// so unit tests can lock down the validation seam without spinning up axum.
///
/// Spec accepts:
///   - elementId XOR selector (at least one required)
///   - state ∈ {present|visible|enabled|disabled|value-not-empty|value-empty|checked|unchecked|absent}
///   - timeoutMs in [0, 30000] (default 5000)
///   - pollMs >= 10 (default 50)
///
/// `timeoutMs` larger than the ceiling is rejected (400) rather than
/// silently clamped, so callers don't unknowingly wait less than they
/// asked. `pollMs` smaller than the floor is similarly rejected.
#[derive(Debug, PartialEq, Eq)]
pub struct WaitForElementStateRequest {
    pub element_id: Option<String>,
    pub selector: Option<String>,
    pub state: String,
    pub timeout_ms: u64,
    pub poll_ms: u64,
}

const VALID_WAIT_FOR_ELEMENT_STATES: &[&str] = &[
    "present",
    "visible",
    "enabled",
    "disabled",
    "value-not-empty",
    "value-empty",
    "checked",
    "unchecked",
    "absent",
];

pub fn validate_wait_for_element_state_request(
    body: &serde_json::Value,
) -> Result<WaitForElementStateRequest, String> {
    let element_id = body
        .get("elementId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    let selector = body
        .get("selector")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    if element_id.is_none() && selector.is_none() {
        return Err("wait-for-element: 'elementId' or 'selector' is required".to_string());
    }

    let state = body
        .get("state")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wait-for-element: 'state' is required".to_string())?;
    if !VALID_WAIT_FOR_ELEMENT_STATES.contains(&state) {
        return Err(format!(
            "wait-for-element: invalid state '{}', expected one of {}",
            state,
            VALID_WAIT_FOR_ELEMENT_STATES.join("|")
        ));
    }

    let timeout_ms = match body.get("timeoutMs") {
        None => 5000_u64,
        Some(v) => {
            let n = v
                .as_u64()
                .or_else(|| v.as_i64().filter(|i| *i >= 0).map(|i| i as u64))
                .or_else(|| v.as_f64().filter(|f| *f >= 0.0 && f.is_finite()).map(|f| f as u64))
                .ok_or_else(|| "wait-for-element: 'timeoutMs' must be a non-negative number".to_string())?;
            if n > 30_000 {
                return Err(
                    "wait-for-element: 'timeoutMs' must be between 0 and 30000".to_string(),
                );
            }
            n
        }
    };

    let poll_ms = match body.get("pollMs") {
        None => 50_u64,
        Some(v) => {
            let n = v
                .as_u64()
                .or_else(|| v.as_f64().filter(|f| *f >= 0.0 && f.is_finite()).map(|f| f as u64))
                .ok_or_else(|| "wait-for-element: 'pollMs' must be a number".to_string())?;
            if n < 10 {
                return Err("wait-for-element: 'pollMs' must be >= 10".to_string());
            }
            n
        }
    };

    Ok(WaitForElementStateRequest {
        element_id,
        selector,
        state: state.to_string(),
        timeout_ms,
        poll_ms,
    })
}

/// Forwards a `state`-shape wait-for-element body to the SDK
/// `wait_for_element_state_predicate` runtime handler. Validates the body
/// up-front (returning HTTP 400 on bad shapes) so the SDK side only sees
/// well-formed payloads. The SDK handler returns the predicate outcome
/// verbatim; we surface it 1:1 inside the standard `ApiResponse::success`
/// envelope so `found:false` (timeout) is **not** an HTTP error.
async fn ui_bridge_wait_for_element_state_predicate_handler(
    state: Arc<ApiState>,
    body: serde_json::Value,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let req = validate_wait_for_element_state_request(&body)
        .map_err(|msg| (StatusCode::BAD_REQUEST, Json(api_error(msg))))?;

    info!(
        "UI Bridge API: wait-for-element (state shape) elementId={:?} selector={:?} state={} timeoutMs={} pollMs={}",
        req.element_id, req.selector, req.state, req.timeout_ms, req.poll_ms
    );

    let payload = serde_json::json!({
        "params": {
            "elementId": req.element_id,
            "selector": req.selector,
            "state": req.state,
            "timeoutMs": req.timeout_ms,
            "pollMs": req.poll_ms,
        }
    });

    match ui_bridge_request_sync(&state, "wait_for_element_state_predicate", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!(
                "UI Bridge API: wait-for-element (state shape) failed: {}",
                e
            );
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
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

/// Primary element operations, components, discovery/snapshot, wait-for-element,
/// and the DOM-convenience family (find / find-by-text / click-by-text /
/// click-by-selector / read-value / type-into).
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use super::routing::add_dual;
    use axum::routing::{get, post};
    let router = axum::Router::new()
        .route(
            "/ui-bridge/control/elements",
            get(ui_bridge_get_elements_handler),
        );
    let router = add_dual!(
        router,
        get,
        "elements/last-discovered",
        ui_bridge_get_last_discovered_handler
    );
    let router = router
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
        );
    // Wait-for-element: identical handler under /control + /ai.
    let router = add_dual!(
        router,
        post,
        "wait-for-element",
        ui_bridge_wait_for_element_handler
    );
    router
        .route("/ui-bridge/control/find", post(ui_bridge_find_handler))
        // DOM convenience helpers
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
}

/// Static (method, path) tuples corresponding to every route registered
/// by `routes()`. Concatenated into `route_manifest()` in `mod.rs`.
pub fn route_entries() -> &'static [(&'static str, &'static str)] {
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
        ("POST", "/ui-bridge/control/wait-for-element"),
        ("POST", "/ui-bridge/ai/wait-for-element"),
        ("POST", "/ui-bridge/control/find"),
        ("POST", "/ui-bridge/control/page/find-by-text"),
        ("POST", "/ui-bridge/control/page/click-by-text"),
        ("POST", "/ui-bridge/control/page/click-by-selector"),
        ("POST", "/ui-bridge/control/page/read-value"),
        ("POST", "/ui-bridge/control/page/type-into"),
    ]
}

#[cfg(test)]
mod wait_for_element_state_tests {
    //! Validation seam for the M1 `/ai/wait-for-element` state-predicate
    //! body shape. Exercises the pure helper so we don't need an axum
    //! router / live SDK.

    use super::{validate_wait_for_element_state_request, WaitForElementStateRequest};
    use serde_json::json;

    fn ok(body: serde_json::Value) -> WaitForElementStateRequest {
        validate_wait_for_element_state_request(&body)
            .expect("expected validation to succeed")
    }

    fn err(body: serde_json::Value) -> String {
        validate_wait_for_element_state_request(&body)
            .expect_err("expected validation to fail")
    }

    #[test]
    fn missing_both_element_id_and_selector_is_rejected() {
        let msg = err(json!({ "state": "visible" }));
        assert!(
            msg.contains("'elementId' or 'selector'"),
            "expected missing-id-or-selector error, got: {msg}"
        );
    }

    #[test]
    fn empty_strings_count_as_missing() {
        // The validator must treat "" as absent so callers can't slip past the
        // gate by sending a blank string.
        let msg = err(json!({ "elementId": "", "selector": "", "state": "visible" }));
        assert!(msg.contains("'elementId' or 'selector'"));
    }

    #[test]
    fn missing_state_is_rejected() {
        let msg = err(json!({ "elementId": "x" }));
        assert!(msg.contains("'state' is required"), "got: {msg}");
    }

    #[test]
    fn invalid_state_is_rejected() {
        let msg = err(json!({ "elementId": "x", "state": "bogus" }));
        assert!(msg.contains("invalid state 'bogus'"), "got: {msg}");
        assert!(msg.contains("present"), "should list valid states: {msg}");
        assert!(msg.contains("absent"), "should list valid states: {msg}");
    }

    #[test]
    fn timeout_ms_above_ceiling_is_rejected() {
        let msg = err(json!({ "elementId": "x", "state": "visible", "timeoutMs": 30_001 }));
        assert!(msg.contains("between 0 and 30000"), "got: {msg}");
    }

    #[test]
    fn timeout_ms_negative_is_rejected() {
        let msg = err(json!({ "elementId": "x", "state": "visible", "timeoutMs": -5 }));
        assert!(
            msg.contains("non-negative") || msg.contains("between 0 and 30000"),
            "got: {msg}"
        );
    }

    #[test]
    fn timeout_ms_at_ceiling_is_accepted() {
        let req = ok(json!({ "elementId": "x", "state": "visible", "timeoutMs": 30_000 }));
        assert_eq!(req.timeout_ms, 30_000);
    }

    #[test]
    fn poll_ms_below_floor_is_rejected() {
        let msg = err(json!({ "elementId": "x", "state": "visible", "pollMs": 5 }));
        assert!(msg.contains("'pollMs' must be >= 10"), "got: {msg}");
    }

    #[test]
    fn defaults_applied_when_omitted() {
        let req = ok(json!({ "elementId": "x", "state": "present" }));
        assert_eq!(req.timeout_ms, 5000);
        assert_eq!(req.poll_ms, 50);
        assert_eq!(req.element_id.as_deref(), Some("x"));
        assert_eq!(req.selector, None);
        assert_eq!(req.state, "present");
    }

    #[test]
    fn selector_only_is_accepted() {
        let req = ok(json!({ "selector": "input[name=foo]", "state": "value-not-empty" }));
        assert_eq!(req.element_id, None);
        assert_eq!(req.selector.as_deref(), Some("input[name=foo]"));
        assert_eq!(req.state, "value-not-empty");
    }

    #[test]
    fn all_documented_states_accepted() {
        for state in [
            "present",
            "visible",
            "enabled",
            "disabled",
            "value-not-empty",
            "value-empty",
            "checked",
            "unchecked",
            "absent",
        ] {
            let req = ok(json!({ "elementId": "x", "state": state }));
            assert_eq!(req.state, state, "state {state} should round-trip");
        }
    }
}
