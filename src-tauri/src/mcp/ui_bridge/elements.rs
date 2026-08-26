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
use reqwest::Method;
use serde::Deserialize;
use tracing::{debug, error, info, warn};

use crate::mcp::app_dispatch::AppDispatcher;
use crate::mcp::app_registry::{AppRegistry, AppTransport};
use crate::mcp::envelope::UiBridgeJson;
use crate::mcp::types::{api_error, api_error_detailed, ApiResponse, ApiState};

use super::diagnostics::{extract_error_code, CanonicalCode};
use super::helpers::{
    count_elements_in_discover_payload, direct_webview_evaluate_with_result,
    evaluate_js_expression_in_window, extract_ai_find_match, extract_get_element_match,
    filter_element_fields, read_window_label, snapshot_signature,
};
use super::recovery_executor::attempt_recovery;
use super::request::{
    ui_bridge_request_sync, ui_bridge_request_sync_in_window, wrap_ipc_result,
};
use super::screenshots::capture_runner_window_base64;
use super::types::{
    classify_transport_error, UIBridgeActionRequest, UIBridgeComponentActionRequest,
    UIBridgeDiscoveryRequest, UiBridgeError,
};
use super::{ipc_handler_path_get, ipc_handler_post};
use crate::str_utils::truncate_str_ellipsis;

/// Phase 3.2b (plan 2026-05-03) — bidirectional simple-glob match between
/// a query value and a single `reveals` entry. Mirrors the canonical SDK
/// matcher (`selector-match.ts::revealsEntryMatches`):
///
///   - Exact equality short-circuits both regex paths.
///   - If the query contains `*`, treat it as a glob and match against the entry.
///   - If the entry contains `*`, treat IT as a glob and match against the query.
///
/// `*` maps to `.*`; other regex metachars are escaped. The compiled regex
/// is anchored (`^…$`) so `session-card` does NOT match `term-session-card-1`.
fn reveals_entry_matches(query: &str, entry: &str) -> bool {
    if query == entry {
        return true;
    }
    if query.contains('*') {
        if let Ok(re) = build_simple_glob_regex(query) {
            if re.is_match(entry) {
                return true;
            }
        }
    }
    if entry.contains('*') {
        if let Ok(re) = build_simple_glob_regex(entry) {
            if re.is_match(query) {
                return true;
            }
        }
    }
    false
}

/// Convert a simple `*`-wildcard glob into an anchored regex. Escapes
/// regex metachars in the rest of the string so callers can pass values
/// containing `.` or `+` without surprise.
fn build_simple_glob_regex(pattern: &str) -> Result<regex::Regex, regex::Error> {
    let mut out = String::with_capacity(pattern.len() + 2);
    out.push('^');
    for ch in pattern.chars() {
        match ch {
            '*' => out.push_str(".*"),
            // Escape every regex metachar EXCEPT `*`, which we just handled.
            '.' | '+' | '?' | '^' | '$' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out.push('$');
    regex::Regex::new(&out)
}

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

    // Optional `?windowLabel=term-1` scopes discovery to one pop-out terminal
    // window (discoverable via `GET /ui-bridge/control/runner-windows`). Omitted
    // → the main window, byte-identical to single-window behavior.
    let window_label = query.get("windowLabel").map(String::as_str);

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
            super::request::target_window_payload(
                serde_json::json!({ "options": { "interactiveOnly": false } }),
                window_label,
            ),
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
    // Phase 3.2b (plan 2026-05-03): per-element `reveals` filter. Bidirectional
    // simple-glob match — see `reveals_entry_matches` for semantics.
    let filter_reveals_any = query.get("revealsAny").cloned();

    match ui_bridge_request_sync(
        &state,
        "get_elements",
        super::request::target_window_payload(serde_json::json!({}), window_label),
    )
    .await
    {
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

            // Phase 3.2b (plan 2026-05-03): `revealsAny=<id-or-glob>` keeps
            // only elements whose `reveals: string[]` array intersects the
            // query under bidirectional simple-glob match. Mirrors the
            // canonical SDK matcher in selector-match.ts.
            if let Some(ref query_str) = filter_reveals_any {
                if !query_str.is_empty() {
                    elements_array.retain(|el| {
                        let reveals = match el.get("reveals").and_then(|v| v.as_array()) {
                            Some(arr) if !arr.is_empty() => arr,
                            _ => return false,
                        };
                        reveals.iter().any(|entry| {
                            entry
                                .as_str()
                                .map(|e| reveals_entry_matches(query_str, e))
                                .unwrap_or(false)
                        })
                    });
                }
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

    // Iter-3 item 3 — detect the "unknown element id" IPC failure shape
    // BEFORE handing off to `wrap_ipc_result`. The frontend's `get_element`
    // IPC handler returns `{ success: false, error: "" }` (or a generic
    // "element not found" prose) for ids that aren't in the registry; the
    // generic `wrap_ipc_result` path then forwards an empty `error` and
    // `error_detail.code = INTERNAL_ERROR` (the default of
    // `classify_transport_error` for unknown patterns), making the rejection
    // indistinguishable from a true transport failure. Detect the shape
    // here and override with the canonical `ELEMENT_NOT_FOUND` envelope so
    // callers see `error_detail.code = "ELEMENT_NOT_FOUND"` and
    // `context.elementId` for self-correction.
    if let Ok(ref data) = filtered_result {
        if data.get("success").and_then(|v| v.as_bool()) == Some(false) {
            let error_msg = data.get("error").and_then(|v| v.as_str()).unwrap_or("");
            let is_not_found_shape = error_msg.is_empty()
                || error_msg.to_lowercase().contains("not found")
                || error_msg.to_lowercase().contains("no element");
            if is_not_found_shape {
                let detail = super::types::UiBridgeError::element_not_found_by_id(&id);
                let message = detail.message.clone();
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(crate::mcp::types::api_error_detailed(message, detail)),
                ));
            }
        }
    }

    wrap_ipc_result(filtered_result)
}

/// Optional query params for `/ui-bridge/control/element/{id}/tree`.
#[derive(Debug, Deserialize, Default)]
pub struct ElementTreeQueryParams {
    /// DOM-subtree depth to walk; clamped to [1, 6]. Defaults to 3.
    #[serde(default)]
    pub depth: Option<i64>,
}

/// Return the rendered DOM subtree of a registered element so callers can
/// understand component structure without composing multiple ad-hoc queries.
///
/// `GET /ui-bridge/control/element/{elementId}/tree?depth=N`
///
/// Response shape (on success):
/// ```json
/// {
///   "success": true,
///   "data": {
///     "elementId": "<id>",
///     "depth": <int — actual depth returned, may be < requested>,
///     "tree": { "tagName": "DIV", "attrs": {...}, "childCount": N,
///               "children": [...] },
///     "truncated": true   // optional, set when the 200-node cap was hit
///   }
/// }
/// ```
///
/// The frontend filters attrs to navigation-useful ones only: class
/// (truncated to 80 chars), data-*, title, role, aria-label, name, type.
/// Unknown element ids return HTTP 404 with a flat error envelope.
pub async fn ui_bridge_get_element_tree_subtree_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(query): Query<ElementTreeQueryParams>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let depth = query.depth.unwrap_or(3).clamp(1, 6);

    info!(
        "UI Bridge API: Get element DOM tree id={} depth={}",
        id, depth
    );

    let result = ui_bridge_request_sync(
        &state,
        "get_element_dom_tree",
        serde_json::json!({ "elementId": id, "depth": depth }),
    )
    .await;

    match result {
        Ok(data) => {
            // Frontend signals "not found" via the canonical Wave-1
            // diagnostic code. Phase 5: discriminate on the enum mirror
            // instead of string-matching `error == "element_not_found"`.
            // Map UB-ELEM-NOT-FOUND → HTTP 404 with a structured
            // error_detail and forward the frontend's `hint.closestMatches`
            // payload so a typoed id gets the same typo suggestions it
            // would on `GET /control/element/<id>`.
            if data.get("success").and_then(|v| v.as_bool()) == Some(false) {
                let err_str = data
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("UI bridge call failed");
                if extract_error_code(&data) == Some(CanonicalCode::UbElemNotFound) {
                    let mut body = api_error_detailed(
                        format!("Element '{}' not found", id),
                        UiBridgeError::element_not_found(&id),
                    );
                    if let Some(hint) = data.get("hint") {
                        body.hint = Some(hint.clone());
                    }
                    return Err((StatusCode::NOT_FOUND, Json(body)));
                }
                // Other inner failures: flatten to 400 (matches wrap_ipc_result).
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error(err_str.to_string())),
                ));
            }
            Ok(Json(ApiResponse::success(data)))
        }
        Err(e) => {
            error!("UI Bridge API: get_element_dom_tree failed: {}", e);
            let detail = classify_transport_error(&e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error_detailed(e, detail)),
            ))
        }
    }
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
            // Phase 5: discriminate on the canonical Wave-1 diagnostic-code
            // mirror (`failureDetails.errorCode` / `code` / `errorCode`,
            // canonical `UB-` first; runner-frontend legacy bare strings
            // bridged) instead of string-matching the prose `error` field.
            match extract_error_code(&data) {
                Some(CanonicalCode::UbElemNotFound) => {
                    let msg = data
                        .get("errorMessage")
                        .and_then(|v| v.as_str())
                        .or_else(|| data.get("error").and_then(|v| v.as_str()))
                        .unwrap_or("Element not found")
                        .to_string();
                    Err((
                        StatusCode::NOT_FOUND,
                        Json(api_error_detailed(
                            msg,
                            UiBridgeError::element_not_found(&id),
                        )),
                    ))
                }
                _ => Ok(Json(ApiResponse::success(data))),
            }
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
    /// Optional `?windowLabel=term-1` — target a pop-out terminal window
    /// (discoverable via `GET /ui-bridge/control/runner-windows`). Omitted →
    /// the main window. Scopes both the action and its pre/post detector
    /// queries so the element id resolves in the right window's registry.
    #[serde(default, rename = "windowLabel")]
    pub window_label: Option<String>,
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

        // Forward the step's action envelope WHOLE (either grammar). This used to emit
        // `{"id": ..., "action": "<name>", "params": ...}`, which failed
        // twice over: the frontend reads `elementId` (so every step died on
        // "elementId and action are required"), and it reads `params` off the
        // action envelope, never as a top-level sibling — so the parameters
        // were unreachable even once the id was fixed. See
        // `request::element_action_payload` for the full contract.
        let payload = super::request::step_action_payload(step);

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

/// Closed set of action names accepted by `POST /control/element/{id}/action`.
///
/// Mirrors the dispatcher vocabulary owned by the wrapper SDK
/// (`ui-bridge/packages/ui-bridge/src/server/relay-handlers.ts`) — the runner
/// only forwards the action name to the frontend, so an unknown name *must*
/// be rejected here. Without this gate the request reached
/// `execute_action` → IPC failure → the Phase 5 `RecoveryExecutor`, whose LLM
/// fallback then synthesized a click on an unrelated "Active" element and
/// surfaced HTTP 200 `success:true` `via:"llm_fallback"` — a false-success
/// for autonomous loops (verified empirically 2026-05-21 with
/// `action:"press"`).
///
/// Includes both `camelCase` and `snake_case` aliases for the multi-token
/// names because the SDK accepts either (e.g. `scrollIntoView` vs
/// `scroll_into_view`, see `recovery_executor::execute_recovery_command`).
const SUPPORTED_ACTION_NAMES: &[&str] = &[
    "click",
    // Reveal a hover-gated control (`pointer-events:none` until `:hover`/
    // `group-hover`) then click it, in one dispatch. Handled SDK-side by
    // `@qontinui/ui-bridge` >= 0.16.0; the runner just forwards it.
    "hoverClick",
    "doubleClick",
    "double_click",
    "double",
    "right",
    "middle",
    "type",
    "sendKeys",
    "clear",
    "select",
    "focus",
    "blur",
    "hover",
    "scroll",
    "scrollIntoView",
    "scroll_into_view",
    "check",
    "uncheck",
    "toggle",
    "drag",
    "setValue",
    "submit",
    "reset",
    "autocomplete",
];

/// Validate a user-supplied action name against [`SUPPORTED_ACTION_NAMES`].
///
/// Returns the canonical action name on success (currently the input
/// unchanged — case-sensitive match — but the indirection keeps the
/// future option of accepting a kebab-case alias open without churning
/// every caller).
///
/// On failure returns a structured [`UiBridgeError`] with
/// `code = INVALID_REQUEST` and the full supported list in
/// `context.supportedActions`, so the handler can build a 400 envelope
/// that mirrors the `tab/activate` -> `knownTabs` prior art.
///
/// Split out from the handler as a pure helper so it's unit-testable
/// without spinning up an `axum::Router` — see `validate_action_name_tests`
/// below. This matches the seam established for
/// `validate_wait_for_element_state_request` / `validate_tab_id`.
pub(crate) fn validate_action_name(raw: &str) -> Result<&str, UiBridgeError> {
    let trimmed = raw.trim();
    if !trimmed.is_empty() && SUPPORTED_ACTION_NAMES.contains(&trimmed) {
        return Ok(trimmed);
    }
    Err(UiBridgeError::invalid_action(raw, SUPPORTED_ACTION_NAMES))
}

/// True when a `get_element` IPC reply is the frontend's "there is no such
/// element" shape rather than a real element.
///
/// The frontend's `get_element` handler answers a miss with a SUCCESSFUL IPC
/// carrying `{ success: false, error: "" }` (or a "not found" / "no element"
/// prose) — it does NOT fail the transport. So a caller that only matches on
/// `Err(_)` sees a miss as "probe worked, element has no metadata", which is
/// exactly how a typoed element id used to walk past both action gates, get
/// dispatched to IPC, and come back re-coded `ACTION_FAILED`.
///
/// This is the same predicate `ui_bridge_get_element_handler` already applies
/// before `wrap_ipc_result`, lifted out so `execute_action` resolves the
/// element with identical semantics instead of a second, divergent copy.
pub(crate) fn is_element_miss_reply(data: &serde_json::Value) -> bool {
    if data.get("success").and_then(|v| v.as_bool()) != Some(false) {
        return false;
    }
    let error_msg = data.get("error").and_then(|v| v.as_str()).unwrap_or("");
    let lower = error_msg.to_lowercase();
    error_msg.is_empty()
        || lower.contains("not found")
        || lower.contains("no element")
        || lower.contains("never registered or discovered")
}

/// Extract the element's advertised `actions` list from a `get_element` IPC
/// payload. The frontend snapshots elements with an `actions` array whose
/// entries are either plain strings (action names) or objects with a `name` /
/// `action` field — accept both shapes. Returns `None` when the field is
/// absent OR present but not an array, so the caller can distinguish "we
/// don't know which actions this element supports" from "we know it supports
/// nothing." Empty array → `Some(vec![])` (= element advertised an explicit
/// empty list).
///
/// Loop B Item A: feeds the `ACTION_NOT_SUPPORTED` discriminator that
/// invalidates the misleading `success:true, recovery.commandChosen:null`
/// envelope when the LLM fallback "recovered" against an element that
/// doesn't actually accept the requested action.
pub(crate) fn extract_supported_actions(elem_data: &serde_json::Value) -> Option<Vec<String>> {
    let arr = elem_data.get("actions").and_then(|v| v.as_array())?;
    let names: Vec<String> = arr
        .iter()
        .filter_map(|entry| {
            if let Some(s) = entry.as_str() {
                Some(s.to_string())
            } else if let Some(obj) = entry.as_object() {
                obj.get("name")
                    .or_else(|| obj.get("action"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect();
    Some(names)
}

/// Extract the element's registered **custom-action** names from a
/// `get_element` / discover payload.
///
/// The SDK serializes registered elements with a separate `customActions`
/// field (see `@qontinui/ui-bridge`'s `serializeRegisteredElement` —
/// `customActions: el.customActions ? Object.keys(el.customActions) : void 0`),
/// distinct from the built-in `actions` array. These are arbitrary
/// per-element handlers (e.g. `pasteText`, `getScrollback`, `sendKeys` on a
/// `terminal-input-<id>` element) that the frontend's control-event handler
/// dispatches via `registered.customActions[action].handler(params)` — they
/// are NOT in `SUPPORTED_ACTION_NAMES` and never will be (the runner can't
/// enumerate app-defined actions ahead of time).
///
/// Returns `None` when the field is absent OR present but not an array
/// (mirrors `extract_supported_actions` — `None` = "we don't know"); an
/// explicit empty array → `Some(vec![])`. Entries are accepted as bare
/// strings (the canonical SDK shape) or `{ name | action }` objects for
/// parity with `extract_supported_actions`.
///
/// Used by the `/control/element/{id}/action` handler so a registered custom
/// action (a) passes the action-name gate instead of a blanket 400
/// "Unknown action", and (b) is folded into the advertised-actions list so
/// `is_action_advertised` accepts it — matching the SDK route, whose frontend
/// dispatcher already merges `customActions` into its allowed-actions check
/// (`useControlEvents.ts`).
pub(crate) fn extract_custom_actions(elem_data: &serde_json::Value) -> Option<Vec<String>> {
    let arr = elem_data.get("customActions").and_then(|v| v.as_array())?;
    let names: Vec<String> = arr
        .iter()
        .filter_map(|entry| {
            if let Some(s) = entry.as_str() {
                Some(s.to_string())
            } else if let Some(obj) = entry.as_object() {
                obj.get("name")
                    .or_else(|| obj.get("action"))
                    .or_else(|| obj.get("id"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect();
    Some(names)
}

/// Fold an element's registered `customActions` into its advertised `actions`
/// array, IN PLACE, so a single element JSON object advertises both built-in
/// and custom actions in the one list that discover/snapshot consumers read.
///
/// Background: the SDK serializes an element with built-in actions in `actions`
/// (a string array) and registered custom actions in a SEPARATE `customActions`
/// string array (`Object.keys(el.customActions)` — see
/// `serializeRegisteredElement` in `@qontinui/ui-bridge`). An agent inspecting
/// the discover/snapshot output's `actions` list therefore could NOT see
/// `pasteText`/`getScrollback` on a `terminal-input-<id>` element even though
/// the `/control/element/{id}/action` route dispatches them. This mirrors the
/// frontend control-event dispatcher (`useControlEvents.ts`), which merges
/// `builtinActions + customActions` when deciding which actions are allowed.
///
/// Behavior:
///   - Custom-action names already present in `actions` are NOT duplicated.
///   - `customActions` itself is left untouched (additive — clients that read
///     the dedicated field keep working).
///   - An element with no `customActions`, or no `actions`/non-array `actions`,
///     is left unchanged (when `actions` is absent but custom actions exist, we
///     create an `actions` array from the custom names so the element advertises
///     them at all).
fn fold_custom_actions_into_element(elem: &mut serde_json::Value) {
    let custom = match extract_custom_actions(elem) {
        Some(c) if !c.is_empty() => c,
        _ => return,
    };
    let obj = match elem.as_object_mut() {
        Some(o) => o,
        None => return,
    };
    // Take the existing `actions` array (if it is one); else start empty.
    let mut actions: Vec<serde_json::Value> = obj
        .get("actions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    // Existing string entries — used to avoid duplicating a custom name the
    // SDK already enumerated in `actions`.
    let existing: std::collections::HashSet<String> = actions
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();
    for name in custom {
        if !existing.contains(&name) {
            actions.push(serde_json::Value::String(name));
        }
    }
    obj.insert("actions".to_string(), serde_json::Value::Array(actions));
}

/// Walk a discover/snapshot IPC payload and fold every element's
/// `customActions` into its advertised `actions` array (see
/// [`fold_custom_actions_into_element`]). Handles the three element-array
/// layouts the SDK emits — identical to `count_elements_in_discover_payload`:
///   - a bare top-level array,
///   - `{ elements: [...] }` (discover + snapshot),
///   - `{ data: { elements: [...] } }`.
///
/// Pure + idempotent: applying it twice yields the same result (the dedup in
/// `fold_custom_actions_into_element` makes the second pass a no-op).
fn advertise_custom_actions_in_payload(data: &mut serde_json::Value) {
    fn fold_array(arr: &mut [serde_json::Value]) {
        for elem in arr.iter_mut() {
            fold_custom_actions_into_element(elem);
        }
    }
    if let Some(arr) = data.as_array_mut() {
        fold_array(arr);
        return;
    }
    if let Some(arr) = data.get_mut("elements").and_then(|v| v.as_array_mut()) {
        fold_array(arr);
    }
    if let Some(arr) = data
        .get_mut("data")
        .and_then(|d| d.get_mut("elements"))
        .and_then(|v| v.as_array_mut())
    {
        fold_array(arr);
    }
}

/// True when the element advertises an explicit supported-actions list AND
/// the requested action is not in it. `None` (we don't know) → returns
/// `false` (don't accuse — degrade gracefully).
///
/// Match semantics mirror `SUPPORTED_ACTION_NAMES` membership in
/// `validate_action_name`: exact (case-sensitive) string equality. The SDK
/// emits canonical camelCase names in element metadata; aliases like
/// `scroll_into_view` -> `scrollIntoView` are normalized SDK-side before the
/// `actions` list is built, so an additional alias-fold here would be a
/// distortion not a feature.
pub(crate) fn is_action_advertised(action: &str, supported: &Option<Vec<String>>) -> bool {
    match supported {
        Some(list) => {
            list.iter().any(|s| s == action)
                // `hoverClick` is a click-variant — it reveals a hover-gated
                // control then clicks it. Any element that advertises `click`
                // can be hover-clicked, even when the SDK's inferActions didn't
                // enumerate `hoverClick` explicitly on this element. Without
                // this, hover-revealed toolbar buttons (which advertise `click`
                // but not `hoverClick`) would be rejected here pre-IPC.
                || (action == "hoverClick" && list.iter().any(|s| s == "click"))
        }
        // Unknown supported list (fetch failed or element didn't advertise
        // any) — assume yes, so we don't manufacture false ACTION_NOT_SUPPORTED.
        None => true,
    }
}

/// True only for a recovery that actually did something and worked: a command
/// was chosen AND the original action succeeded on the retry.
///
/// `recovered:true` with `command_chosen:None` is the shape the LLM fallback
/// used to produce — it asserted a recovery while admitting no recovery step
/// was selected. That is not a recovery, so the caller must see the ORIGINAL
/// failure, not a synthesized verdict about it.
pub(crate) fn recovery_is_genuine(recovery: &super::recovery_executor::RecoveryOutcome) -> bool {
    recovery.recovered && recovery.command_chosen.is_some()
}

/// Alias for the `execute_action` handler's own return type — the value that
/// travels from `wrap_ipc_result` through the retry/recovery ladder.
type ActionResult =
    Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)>;

/// Re-code a POST-DISPATCH failure as `ACTION_FAILED`, preserving the
/// handler's own error text verbatim and attaching the recovery audit trail.
///
/// Why this exists: the action was dispatched, so "not supported" is already
/// settled — the pre-IPC gate rejects unadvertised actions with the element's
/// supported-action list. What arrives here is a genuine failure, and the
/// typed body the handler returned (`TERMINAL_EXITED: …`, and its siblings) is
/// the entire diagnosis. Relabelling it destroyed that.
///
/// Only the UNCLASSIFIED case is re-coded. `classify_transport_error` falls
/// through to `INTERNAL_ERROR` for any message it doesn't recognise, which is
/// wrong for a dispatched action that failed — the runner is fine, the action
/// is not. A message that DID classify (element-not-found, not-visible,
/// timeout, …) keeps its more specific code; this only ever adds information.
///
/// Transport-level failures (HTTP 500 / 503 — the frontend never answered)
/// are left alone: those are not action outcomes at all.
pub(crate) fn as_action_failure(
    result: ActionResult,
    action_name: &str,
    element_id: &str,
    recovery: serde_json::Value,
) -> ActionResult {
    let attach = |context: Option<serde_json::Value>| -> serde_json::Value {
        let mut obj = match context {
            Some(serde_json::Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        obj.entry("requested_action")
            .or_insert_with(|| serde_json::Value::String(action_name.to_string()));
        obj.entry("element_id")
            .or_insert_with(|| serde_json::Value::String(element_id.to_string()));
        obj.insert("recovery".to_string(), recovery.clone());
        serde_json::Value::Object(obj)
    };

    match result {
        Err((status, Json(mut body))) => {
            if status == StatusCode::BAD_REQUEST {
                let message = body
                    .error
                    .clone()
                    .unwrap_or_else(|| format!("Action '{action_name}' failed"));
                let detail = match body.error_detail.take() {
                    // Unclassified — the classifier's fallthrough. A dispatched
                    // action that failed is an ACTION failure, not an internal
                    // runner error.
                    Some(d) if matches!(d.code, super::types::UiBridgeErrorCode::InternalError) => {
                        UiBridgeError {
                            code: super::types::UiBridgeErrorCode::ActionFailed,
                            message: d.message,
                            recovery: Some(super::types::RecoveryHint::Resnapshot),
                            context: Some(attach(d.context)),
                        }
                    }
                    Some(mut d) => {
                        d.context = Some(attach(d.context));
                        d
                    }
                    None => UiBridgeError {
                        code: super::types::UiBridgeErrorCode::ActionFailed,
                        message: message.clone(),
                        recovery: Some(super::types::RecoveryHint::Resnapshot),
                        context: Some(attach(None)),
                    },
                };
                // `error` is NOT rewritten: the handler's own text (e.g. the
                // terminal's `TERMINAL_EXITED: …` envelope) is what the caller
                // needs to read.
                body.error_detail = Some(detail);
            }
            Err((status, Json(body)))
        }
        // A `success:false` body that reached HTTP 200 (a later stage in the
        // ladder can produce one). Same treatment, in the envelope it has.
        Ok(Json(mut resp)) if !resp.success => {
            if resp.code.is_none() {
                resp.code = Some("ACTION_FAILED".to_string());
            }
            if let Some(serde_json::Value::Object(ref mut m)) = resp.data {
                m.entry("code")
                    .or_insert_with(|| serde_json::Value::String("ACTION_FAILED".to_string()));
                m.insert("recovery".to_string(), recovery.clone());
            }
            Ok(Json(resp))
        }
        ok => ok,
    }
}

/// Outcome of [`validate_type_action_params`] — the closed set of param-shape
/// rejections the `type` action recognises. Carried as the `Err` arm so the
/// handler can short-circuit with the matching HTTP-400 envelope before
/// dispatching to IPC.
///
/// `Ok` is the bare success path — `params.text` (or the flat-form `text`
/// field merged from `extra`) is present and is a string, so the request
/// is well-formed and dispatch continues as normal.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TypeParamError {
    /// `params.value` (or flat `value`) is present but `text` is not. Caller
    /// likely confused `type` with `setValue`/`select`. Returns HTTP 400
    /// with `code: INVALID_PARAM` and a `didYouMean: "text"` hint.
    WrongFieldValueInsteadOfText,
    /// Neither `text` nor `value` was provided — the action is missing its
    /// required input. Returns HTTP 400 with `code: MISSING_PARAM`.
    MissingTextField,
}

/// Pre-IPC validator for the `type` action's required `text` parameter.
///
/// Loop B iter 2 — runner-side analogue of ui-bridge PR #33's SDK-boundary
/// `WRONG_TYPE_PARAM` / `MISSING_PARAM` gate. The runner has its OWN
/// action-dispatch path (Tauri-internal callers / non-SDK clients) that does
/// NOT flow through `relay-handlers.ts::executeElementAction`, so the
/// SDK-side gate alone is insufficient. Without this helper a malformed
/// `{action:"type", params:{value:"foo"}}` request fell through the
/// `validate_action_name` whitelist (`"type"` is a known action), reached
/// `execute_action`, IPC-failed because the frontend's type handler needs
/// `params.text`, then hit the Phase 5 `RecoveryExecutor::llm_fallback`
/// which synthesized a non-null `commandChosen` and returned HTTP 200
/// `success:true` — silently typing the literal string `"text"` (or worse,
/// the element's `type` HTML attribute) instead of the caller's intent.
/// That violates the documented contract at
/// `ui-bridge/docs-site/docs/api/runner-features.md` § "Per-action param
/// names" which promises HTTP 400 with `type: 'value' is unknown; did you
/// mean 'text'?`.
///
/// Accepts both the canonical `params: { text: "..." }` shape AND the
/// flat-form `{ text: "..." }` shape that the handler's `extra`-flatten
/// merges into params (see the `merged_params` block in
/// `ui_bridge_execute_action_handler` — flat-form is a back-compat surface
/// for the `drag.targetPosition` family and must remain supported). Either
/// presence-as-string counts as a valid `text`.
///
/// Returns `Ok(())` for non-`type` actions (the gate is per-action by
/// design — `select`/`setValue` keep their own `value` semantics).
pub(crate) fn validate_type_action_params(
    action: &str,
    params: &Option<serde_json::Value>,
    extra: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), TypeParamError> {
    if action != "type" {
        return Ok(());
    }
    // Either nested-in-params OR top-level (flat-form) presence-as-string
    // satisfies the contract — mirrors the `merged_params` shape the
    // dispatcher actually sees downstream. Non-string values fall through
    // as "missing": `text: null` / `text: 123` would still confuse the
    // typing engine; treat them like absence so the structured 400 fires.
    let params_text_is_str = params
        .as_ref()
        .and_then(|p| p.get("text"))
        .map(|v| v.is_string())
        .unwrap_or(false);
    let extra_text_is_str = extra.get("text").map(|v| v.is_string()).unwrap_or(false);
    if params_text_is_str || extra_text_is_str {
        return Ok(());
    }
    let params_value_present = params.as_ref().and_then(|p| p.get("value")).is_some();
    let extra_value_present = extra.contains_key("value");
    if params_value_present || extra_value_present {
        Err(TypeParamError::WrongFieldValueInsteadOfText)
    } else {
        Err(TypeParamError::MissingTextField)
    }
}

pub async fn ui_bridge_execute_action_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(query): Query<ActionQueryParams>,
    headers: HeaderMap,
    body_bytes: axum::body::Bytes,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Custom body extraction so JSON-parse errors come back with an actionable
    // hint instead of the raw serde_json message. We sidestep axum's
    // `Json<UIBridgeActionRequest>` extractor and parse the bytes ourselves
    // for two reasons:
    //
    //   1. Windows paths in `params.value` (e.g. `D:\foo\bar`) where a single
    //      backslash is an invalid JSON escape — agents keep hitting this and
    //      the serde message alone ("invalid escape at line 1 column 42")
    //      doesn't point at the fix.
    //
    //   2. Bytes that are *almost* UTF-8 (e.g. a Windows-1252-encoded em-dash
    //      `0x97` slipping through a Git-Bash/cmd.exe shell pipeline). The
    //      raw `Json<>` extractor surfaces the cryptic
    //      `text: invalid unicode code point at line 1 column N` from
    //      `serde_json::Error::InvalidUnicodeCodePoint`. Manual parsing
    //      lets us fall back to `String::from_utf8_lossy` and retry so a
    //      stray byte doesn't break the whole "type" action.
    let mut request: UIBridgeActionRequest = match serde_json::from_slice(&body_bytes) {
        Ok(req) => req,
        Err(first_err) => {
            // First retry: bytes may be valid JSON whose string contents
            // contain non-UTF-8 (e.g. Windows-1252 em-dash 0x97). Lossy
            // decode, then re-parse from the re-encoded UTF-8 string.
            let lossy = String::from_utf8_lossy(&body_bytes);
            // Only retry when from_utf8_lossy actually substituted something
            // (i.e. the bytes weren't valid UTF-8 to begin with). If they
            // were valid UTF-8, the second parse would yield the same error.
            let did_substitute = std::str::from_utf8(&body_bytes).is_err();
            if did_substitute {
                if let Ok(req) = serde_json::from_str::<UIBridgeActionRequest>(&lossy) {
                    info!(
                        "UI Bridge API: execute_action body had non-UTF-8 bytes; \
                         recovered via lossy decode (substituted U+FFFD)"
                    );
                    req
                } else {
                    let hint = format!(
                        "Failed to parse the request body as JSON: {}. \
                         Hint: the body contains bytes that are not valid UTF-8 — \
                         set your shell to UTF-8 (e.g. `chcp 65001` on Windows cmd, \
                         or pipe the body via `curl --data-binary @file.json` from a \
                         UTF-8-encoded file).",
                        first_err
                    );
                    return Err((StatusCode::BAD_REQUEST, Json(ApiResponse::error(hint))));
                }
            } else {
                let raw = format!("Failed to parse the request body as JSON: {}", first_err);
                let lower = raw.to_lowercase();
                let hint = if lower.contains("invalid escape")
                    || lower.contains("control character")
                {
                    format!(
                        // The example path is a PLACEHOLDER, not a real
                        // location: an error hint shipped in an open-source
                        // binary must not name the author's machine layout
                        // (plan
                        // `2026-08-04-remove-hardcoded-machine-paths-from-product-code`,
                        // slice 5 Phase 8). `<PROJECT_ROOT>` matches the
                        // convention `context/builtins/shell_command_schema.md`
                        // already uses, and cannot be pasted into a working
                        // command by mistake.
                        "{}. Hint: if a field value contains a Windows path, escape each backslash as `\\\\` in the JSON body — or send the body from a file with `curl --data-binary @file.json` to bypass shell re-escaping. Forward slashes also work: `<PROJECT_ROOT>/some/file.txt`.",
                        raw
                    )
                } else {
                    raw
                };
                return Err((StatusCode::BAD_REQUEST, Json(ApiResponse::error(hint))));
            }
        }
    };

    // Target pop-out window (`?windowLabel=term-1`); scopes the action AND its
    // detector/validation queries below so the element id resolves in the right
    // window's registry. Owned so it survives the awaits without borrowing query.
    // Empty string is treated as "no target" (main).
    //
    // The `?windowLabel=` query wins; otherwise fall back to a `windowLabel`
    // body field. The SDK's `ControlActionRequest` and the `windowScope` facade
    // send the label in the body (like `page_evaluate` and the read/convenience
    // family), so without this fallback a body-only label would silently run in
    // the main window. Removed from `extra` so it never leaks into the merged
    // action params.
    let window_label = query.window_label.clone().or_else(|| {
        request
            .extra
            .remove("windowLabel")
            .and_then(|v| v.as_str().map(String::from))
    });
    let window_label = window_label.as_deref().filter(|s| !s.is_empty());

    // ── Unknown-window gate ─────────────────────────────────────────────
    // Fail fast with a clear error if a non-existent target window was named.
    // Without this, the pre-flight element probes below (is-input / supported-
    // actions `get_element`) hit the missing window, swallow the "no window"
    // error as an empty supported-actions list, and the action is rejected with
    // a misleading "action not supported ... advertises []" instead of naming
    // the real problem. Mirrors the guard in `request::ui_bridge_request_inner`.
    if let Some(target) = window_label {
        use tauri::Manager;
        if state.app_handle.get_webview_window(target).is_none() {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!(
                    "No runner window labeled '{target}'. Discover live windows via \
                     GET /ui-bridge/control/runner-windows."
                ))),
            ));
        }
    }

    // ── Resolve the ELEMENT before validating the ACTION ────────────────
    //
    // Order matters, and it used to be wrong. Both action gates below (the
    // deferred unknown-action gate and the pre-IPC `ACTION_NOT_SUPPORTED`
    // gate) key off the element's advertised action list, which is fetched by
    // a `get_element` probe. A MISSING element makes that probe come back
    // "no metadata", which both gates read permissively — so a request naming
    // a bogus id plus a custom action (`writeToTerminal`) sailed past them,
    // got dispatched to IPC, failed there for the obvious reason, and was
    // re-coded `ACTION_FAILED` by the post-dispatch branch. The caller was
    // told its ACTION failed when in truth its ELEMENT does not exist — the
    // one fact that would have let it self-correct.
    //
    // A miss is now settled here, first, with the same `ELEMENT_NOT_FOUND`
    // 404 that `GET /control/element/<id>` returns for the same id. Note the
    // asymmetry that caused the bug: the frontend answers a miss with a
    // SUCCESSFUL IPC carrying `{success:false, error:""}`, so `Err(_)` never
    // fired for it (see `is_element_miss_reply`).
    //
    // Settling it here must not DELETE the two recoveries the post-dispatch
    // path already had for a stale id, so this mirrors them rather than
    // pre-empting them:
    //
    //   1. a first miss triggers the same `discover` refresh + re-probe the
    //      stale-registry retry does (a React unmount/remount leaves an entry
    //      that a discover repairs), and
    //   2. a caller carrying `X-UI-Bridge-Stable-Ref` is never 404'd here —
    //      `resolve_stable_ref` downstream can still map it onto the element's
    //      new id, and refusing before that would turn a recoverable request
    //      into a hard failure.
    //
    // A probe that genuinely FAILS (transport error) yields `None` and falls
    // through permissively — we never manufacture a rejection out of a failed
    // probe, matching `is_action_advertised`'s `None` semantics. The surviving
    // reply is reused by both consumers below, so the common path costs one
    // FEWER IPC round-trip than the two probes it replaces.
    let probe_payload =
        super::request::target_window_payload(serde_json::json!({ "elementId": id }), window_label);
    let element_probe: Option<serde_json::Value> = {
        let first = ui_bridge_request_sync(&state, "get_element", probe_payload.clone()).await;
        match first {
            Ok(data) if !is_element_miss_reply(&data) => Some(data),
            Ok(_) => {
                warn!(
                    "execute_action: element {} missing on first probe — refreshing the registry \
                     via discover before deciding",
                    id
                );
                let _ = ui_bridge_request_sync(
                    &state,
                    "discover",
                    super::request::target_window_payload(
                        serde_json::json!({ "options": { "interactiveOnly": false } }),
                        window_label,
                    ),
                )
                .await;
                match ui_bridge_request_sync(&state, "get_element", probe_payload.clone()).await {
                    Ok(data) if !is_element_miss_reply(&data) => Some(data),
                    _ if headers.contains_key("X-UI-Bridge-Stable-Ref") => {
                        // Still missing, but the caller gave us something the
                        // stable-ref fallback can resolve. Leave that path its
                        // chance instead of answering 404 now.
                        warn!(
                            "execute_action: element {} still missing after discover; deferring to \
                             the stable-ref fallback",
                            id
                        );
                        None
                    }
                    _ => {
                        warn!(
                            "execute_action: element {} is not in the registry — returning \
                             ELEMENT_NOT_FOUND before the action-name gates",
                            id
                        );
                        let detail = UiBridgeError::element_not_found_by_id(&id);
                        let message = detail.message.clone();
                        return Err((
                            StatusCode::NOT_FOUND,
                            Json(api_error_detailed(message, detail)),
                        ));
                    }
                }
            }
            Err(e) => {
                warn!(
                    "execute_action: get_element probe for {} failed ({}); continuing without \
                     element metadata rather than inventing a rejection",
                    id, e
                );
                None
            }
        }
    };

    // ── Invalid-action gate (built-in vs. custom) ──────────────────────
    // Reject genuinely unknown action names BEFORE the registry lookup, the
    // stale-registry retry, and the Phase 5 RecoveryExecutor. Without this
    // gate a typo (e.g. `"press"`) reached `execute_action` -> IPC failure
    // -> RecoveryExecutor::llm_fallback, which synthesized a click on a
    // random "Active" button and returned HTTP 200 `success:true`
    // (verified empirically 2026-05-21). Mirrors the `tab/activate`
    // -> `knownTabs` 400 envelope shape; see `validate_action_name`.
    //
    // BUT: `SUPPORTED_ACTION_NAMES` only enumerates the built-in dispatcher
    // vocabulary — it cannot know about app-registered **custom actions**
    // (e.g. `pasteText`/`getScrollback`/`sendKeys` on a `terminal-input-<id>`
    // element). The SDK route (`/sdk/element/{id}/action`) skips this gate
    // entirely and forwards straight to the frontend custom-action
    // dispatcher, so custom actions worked there but were 400'd here. We now
    // mirror the SDK/frontend behavior: a non-built-in name is NOT rejected
    // outright; instead we defer to the per-element check below, which fetches
    // the element's `customActions` and rejects ("Unknown action") only when
    // the name is neither a built-in nor a registered custom action on THIS
    // element. Built-in behavior is unchanged.
    let action_is_builtin = validate_action_name(&request.action).is_ok();

    // ── Type-action param-shape gate ────────────────────────────────────
    // Reject `{action:"type", params:{value:"foo"}}` and `{action:"type",
    // params:{}}` BEFORE the IPC + Phase 5 RecoveryExecutor chain. Without
    // this gate the malformed request fell into `llm_fallback`, which
    // synthesized a non-null `commandChosen` against the `type` action and
    // returned HTTP 200 success — silently typing the literal string
    // `"text"` or the element's `type` HTML attribute instead of the
    // caller's intent. Docs at `runner-features.md` § "Per-action param
    // names" promise HTTP 400 + the literal `type: 'value' is unknown;
    // did you mean 'text'?` message; we honour that here. Loop A iter 3
    // (ui-bridge PR #33) added the analogous gate at the SDK boundary
    // (`relay-handlers.ts::executeElementAction`); this is the runner-side
    // mirror for non-SDK dispatch paths (Tauri-internal callers /
    // direct HTTP clients that bypass the SDK relay). See
    // `validate_type_action_params` for the shape contract.
    if let Err(kind) = validate_type_action_params(&request.action, &request.params, &request.extra)
    {
        let detail = match kind {
            TypeParamError::WrongFieldValueInsteadOfText => UiBridgeError::type_param_invalid(),
            TypeParamError::MissingTextField => UiBridgeError::type_param_missing(),
        };
        warn!(
            "execute_action: rejecting malformed type action on element {} ({:?}): {}",
            id, kind, detail.message
        );
        let msg = detail.message.clone();
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error_detailed(msg, detail)),
        ));
    }

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
        let is_input = match element_probe.as_ref() {
            Some(data) => {
                let tag = data
                    .get("element_type")
                    .or_else(|| data.get("tagName"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                matches!(tag.to_uppercase().as_str(), "INPUT" | "TEXTAREA" | "SELECT")
            }
            None => false, // probe failed — can't tell; default to enabling detector
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
    let pre_snapshot_signature: Option<super::helpers::SnapshotSignature> =
        if expect_change.is_some() {
            match ui_bridge_request_sync(
                &state,
                "discover",
                super::request::target_window_payload(
                    serde_json::json!({ "options": { "interactiveOnly": false } }),
                    window_label,
                ),
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

    let payload = super::request::target_window_payload(
        serde_json::json!({
            "elementId": id,
            "action": action_obj.clone()
        }),
        window_label,
    );

    // ── Issue 2: Disabled-state detection ──────────────────────────────
    // Before dispatching the action, fetch the element state and reject
    // clicks on disabled elements early so callers get an explicit error
    // instead of a silent no-op.
    //
    // Also capture the element's advertised `actions` list so the Loop B
    // Item A `ACTION_NOT_SUPPORTED` discriminator below can compare the
    // requested action against the element's metadata without an extra
    // IPC round-trip. None on fetch failure → the discriminator skips
    // (we never invent a supported list).
    //
    // We fold the element's registered **custom actions** (the separate
    // `customActions` field the SDK serializes — `pasteText`, `getScrollback`,
    // `sendKeys`, …) into the same supported list so `is_action_advertised`
    // treats them as first-class, exactly as the frontend's control-event
    // dispatcher does (`useControlEvents.ts` merges builtins + customActions).
    // `action_is_registered_custom` records whether the requested name is one
    // of THIS element's custom actions, which drives the deferred
    // "Unknown action" rejection below for non-built-in names.
    let mut action_is_registered_custom = false;
    let element_supported_actions: Option<Vec<String>> = match element_probe.as_ref() {
        Some(elem_data) => {
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
                aria_disabled
                    || disabled_attr
                    || data_disabled
                    || prop_aria_disabled
                    || prop_disabled
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
                    hint: None,
                    code: None,
                    suggestions: None,
                }));
            }

            // Merge built-in advertised actions with registered custom-action
            // names. `customActions` is the SDK's separate field; absent → no
            // custom actions on this element. Record whether the requested name
            // is one of them for the deferred unknown-action gate below.
            let custom = extract_custom_actions(elem_data);
            if let Some(ref custom_names) = custom {
                if custom_names.iter().any(|c| c == &action_name) {
                    action_is_registered_custom = true;
                }
            }
            match (extract_supported_actions(elem_data), custom) {
                (Some(mut builtin), Some(custom_names)) => {
                    builtin.extend(custom_names);
                    Some(builtin)
                }
                (Some(builtin), None) => Some(builtin),
                // No advertised `actions` but custom actions present: surface
                // the custom names so `is_action_advertised` can accept them
                // (otherwise a custom-only element would advertise nothing).
                (None, Some(custom_names)) => Some(custom_names),
                (None, None) => None,
            }
        }
        None => None,
    };

    // ── Deferred unknown-action gate ────────────────────────────────────
    // Now that we know the element's registered custom actions, reject a
    // non-built-in name only when it is ALSO not a custom action on this
    // element — the same "Unknown action" 400 the whitelist used to emit
    // eagerly, but no longer firing on legitimate custom actions that the
    // SDK route already accepts. A non-built-in name on an element whose
    // metadata we couldn't fetch (`element_supported_actions == None`) is
    // allowed through to IPC: we never manufacture a rejection from a failed
    // probe (mirrors the permissive `None` semantics of `is_action_advertised`).
    if !action_is_builtin && !action_is_registered_custom && element_supported_actions.is_some() {
        let detail = validate_action_name(&request.action)
            .err()
            .unwrap_or_else(|| {
                UiBridgeError::invalid_action(&request.action, SUPPORTED_ACTION_NAMES)
            });
        warn!(
            "execute_action: rejecting unknown action '{}' on element {} — \
             not a built-in and not a registered custom action (built-ins: {})",
            request.action,
            id,
            SUPPORTED_ACTION_NAMES.join(", ")
        );
        let msg = detail.message.clone();
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error_detailed(msg, detail)),
        ));
    }

    // ── Pre-IPC ACTION_NOT_SUPPORTED gate ──────────────────────────────
    //
    // When the element explicitly advertises a `actions: [...]` list AND the
    // requested action is not in it, short-circuit with a flat HTTP 400 envelope
    // BEFORE we hit IPC / the Phase 5 RecoveryExecutor / LLM fallback.
    //
    // Without this gate, e.g. `scrollIntoView` against a `<button>` whose
    // actions are `["click", "focus", "blur"]` would:
    //   1. pass the `validate_action_name` whitelist (scrollIntoView is a
    //      runner-known action name),
    //   2. dispatch to IPC,
    //   3. IPC fail with whatever shape the SDK emits for "this element
    //      doesn't handle that action" (often a PARSE_ERROR-flavoured envelope
    //      because the SDK tries to delegate to a non-existent handler),
    //   4. fall through to `attempt_recovery` → LLM fallback, which sometimes
    //      synthesizes a non-null `command_chosen` (a side-action like
    //      `focus`) and surfaces HTTP 200 `success:true, recovered:true`.
    //
    // That violates the contract: the runner declared the element supports
    // a closed action set; we should respect the element's declaration and
    // reject upstream of any recovery. Returns the same envelope shape used
    // by the post-recovery ACTION_NOT_SUPPORTED branch below so callers see
    // a consistent body in both paths. Loop iter-2 item 2.
    //
    // Permissive case (no advertised list / fetch failed) falls through —
    // `is_action_advertised` returns `true` for `None`, preserving
    // backward-compat for elements without an `actions` field. The
    // post-recovery gate at the bottom still catches the misleading
    // recovered-with-null-command case for those.
    if !is_action_advertised(&action_name, &element_supported_actions) {
        let supported: Vec<String> = element_supported_actions.clone().unwrap_or_default();
        warn!(
            "execute_action: rejecting {} on element {} pre-IPC — element advertises {:?}",
            action_name, id, supported
        );
        // Use a structured UiBridgeError so `error_detail.code` is
        // `ACTION_NOT_SUPPORTED` and `error_detail.context` carries the
        // supported-action list — same shape the post-recovery branch
        // serialises into `data.code` / `data.supported_actions`. The Err
        // arm here can't carry a free-form `data` payload (the handler's
        // Err type is `Json<ApiResponse<()>>`); the structured detail is
        // the canonical machine-readable surface in that envelope shape.
        let detail = UiBridgeError {
            code: super::types::UiBridgeErrorCode::ActionNotSupported,
            message: format!(
                "Action '{}' is not supported by element '{}'. \
                 Element advertises: {:?}",
                action_name, id, supported
            ),
            recovery: Some(super::types::RecoveryHint::Unrecoverable),
            context: Some(serde_json::json!({
                "code": "ACTION_NOT_SUPPORTED",
                "supported_actions": supported,
                "requested_action": action_name,
            })),
        };
        let msg = detail.message.clone();
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error_detailed(msg, detail)),
        ));
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
                super::request::target_window_payload(
                    serde_json::json!({ "options": { "interactiveOnly": false } }),
                    window_label,
                ),
            )
            .await;

            // Retry the original action once. Clone so `payload` stays
            // available for the Phase 5 RecoveryExecutor downstream.
            let retry_result = wrap_ipc_result(
                ui_bridge_request_sync(&state, "execute_action", payload.clone()).await,
            );

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
                        let resolve_payload = super::request::target_window_payload(
                            serde_json::json!({ "stableRef": stable_ref_json }),
                            window_label,
                        );
                        if let Ok(resolve_result) =
                            ui_bridge_request_sync(&state, "resolve_stable_ref", resolve_payload)
                                .await
                        {
                            if let Some(new_id) =
                                resolve_result.get("elementId").and_then(|v| v.as_str())
                            {
                                info!("execute_action: stable ref resolved {} -> {}", id, new_id);
                                // Retry with the resolved ID
                                let retry_payload = super::request::target_window_payload(
                                    serde_json::json!({
                                        "elementId": new_id,
                                        "action": action_obj
                                    }),
                                    window_label,
                                );
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

    // ── Distinct ELEMENT_NOT_FOUND for a true registry miss ─────────────
    //
    // If the action still failed *after* the discover-refresh retry and the
    // stable-ref fallback above, and the failure is a genuine registry miss
    // (the SDK reports the element was "never registered or discovered" /
    // "not found"), surface it as a distinct HTTP 404 with a structured
    // `error_detail.code == ElementNotFound` — the same envelope the
    // `get_element` DOM-tree / `assert` handlers already return for a typoed
    // id. Without this, `wrap_ipc_result` routes the prose through
    // `classify_transport_error`, which only maps the literal "No element
    // found" string and otherwise falls through to a generic
    // `INTERNAL_SERVER_ERROR` — making a missing-id miss indistinguishable
    // from a real action-execution failure.
    //
    // This fires ONLY on a true, unrecoverable registry miss: an element
    // that exists but whose action fails carries a different error and
    // keeps its existing `wrap_ipc_result` envelope untouched. We reuse the
    // file's own canonical miss predicate (the exact substrings the
    // stale-registry retry above keys on) so behavior stays consistent.
    {
        let is_registry_miss = |err: Option<&str>| -> bool {
            err.map(|e| {
                let lower = e.to_lowercase();
                lower.contains("never registered or discovered") || lower.contains("not found")
            })
            .unwrap_or(false)
        };
        let (miss, hint) = match &result {
            Ok(resp) if !resp.success => {
                (is_registry_miss(resp.error.as_deref()), resp.hint.clone())
            }
            Err((_, body)) => (is_registry_miss(body.error.as_deref()), body.hint.clone()),
            _ => (false, None),
        };
        if miss {
            warn!(
                "execute_action: element {} not found in registry after retry + \
                 stable-ref fallback; returning ELEMENT_NOT_FOUND",
                id
            );
            let mut body = api_error_detailed(
                format!("Element '{}' not found", id),
                UiBridgeError::element_not_found(&id),
            );
            // Forward the frontend's closest-match / typo-suggestion payload
            // (when present) so a mistyped id gets the same suggestions it
            // would on `GET /control/element/<id>`.
            if hint.is_some() {
                body.hint = hint;
            }
            return Err((StatusCode::NOT_FOUND, Json(body)));
        }
    }

    // ── Phase 5 pilot path: automatic structured recovery ───────────────
    //
    // This is the chosen runner pilot path (element-action / NL-click). If
    // the action still failed after the existing stale-retry + stable-ref
    // scaffolding, hand the failure body to the RecoveryExecutor. It:
    //   1. discriminates the canonical Wave-1 error code,
    //   2. parses `failureDetails.suggestedActions` (Wave-1 Phase 3),
    //   3. picks the best retryable command deterministically, executes it
    //      via the existing IPC action path, retries this action once,
    //   4. falls back to the runner's existing LLM-driven recovery IPC,
    //   5. emits `(errorCode, command, succeeded)` telemetry.
    // It never panics on missing/empty suggestedActions (degrades to LLM).
    {
        let failure_body: Option<serde_json::Value> = match &result {
            Ok(resp) if !resp.success => Some(serde_json::json!({
                "success": false,
                "error": resp.error,
                "failureDetails": resp
                    .data
                    .as_ref()
                    .and_then(|d| d.get("failureDetails"))
                    .cloned(),
                "code": resp
                    .data
                    .as_ref()
                    .and_then(|d| d.get("code"))
                    .cloned(),
            })),
            Err((_, body)) => Some(serde_json::json!({
                "success": false,
                "error": body.error,
                "failureDetails": serde_json::Value::Null,
            })),
            _ => None,
        };

        if let Some(failure) = failure_body {
            let recovery = attempt_recovery(&state, &failure, &id, &payload, task_run_id).await;
            // ── A post-dispatch failure is a FAILURE, never "not supported" ──
            //
            // "Not supported" is settled BEFORE IPC: the pre-IPC gate above
            // already rejects every action the element does not advertise,
            // with the supported-action list in the body. Anything that
            // reaches here was dispatched, so the only honest label for it is
            // an action FAILURE — and the handler's own typed body (e.g. the
            // terminal's `TERMINAL_EXITED: …` envelope) is the whole
            // diagnosis.
            //
            // This branch used to relabel such a failure `ACTION_NOT_SUPPORTED`
            // whenever the recovery outcome read `recovered:true` with no
            // command chosen. That claim contradicted the runner's OWN
            // advertised supported-action list — `writeToTerminal` IS
            // supported by a terminal element; the pty behind it had exited —
            // and it destroyed the typed error on its way to the wire, so
            // `TERMINAL_EXITED` existed at the handler and was unobservable
            // over HTTP.
            //
            // A recovery counts only when it chose and ran a command AND the
            // original action then succeeded. Everything else surfaces the
            // original failure, re-coded `ACTION_FAILED` (the variant that
            // already existed for exactly this) with the recovery audit trail
            // attached so callers still see what was attempted.
            if recovery_is_genuine(&recovery) {
                info!(
                    "execute_action: RecoveryExecutor recovered element {} via {:?} (command={:?})",
                    id, recovery.via, recovery.command_chosen
                );
                let body = recovery
                    .result
                    .clone()
                    .unwrap_or_else(|| serde_json::json!({ "recovered": true }));
                let mut ok = ApiResponse::success(body);
                // Attach the recovery audit trail so callers can see which
                // path/command made the action succeed.
                if let Some(serde_json::Value::Object(ref mut m)) = ok.data {
                    m.insert(
                        "recovery".to_string(),
                        serde_json::to_value(&recovery).unwrap_or(serde_json::Value::Null),
                    );
                }
                result = Ok(Json(ok));
            } else {
                warn!(
                    "execute_action: RecoveryExecutor could not recover element {} \
                     (code={:?}, recovered={}, command={:?}) — surfacing the original failure",
                    id, recovery.error_code, recovery.recovered, recovery.command_chosen
                );
                let recovery_json =
                    serde_json::to_value(&recovery).unwrap_or(serde_json::Value::Null);
                result = as_action_failure(result, &action_name, &id, recovery_json);
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
            super::request::target_window_payload(
                serde_json::json!({ "options": { "interactiveOnly": false } }),
                window_label,
            ),
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
                let changed = !post.unchanged_from(&pre_sig);
                // A same-shape remount: the element set and everything it
                // shows are identical, but the elements belong to a new mount.
                // Reported explicitly because it is the case a driver is most
                // likely to misread — the tree was destroyed and rebuilt, so
                // any local state in it (a wizard's current step, a form's
                // draft, scroll position) is GONE, even though the screen
                // looks the same.
                let remounted = post.remounted_from(&pre_sig);
                serde_json::json!({
                    "supported": true,
                    "effectChanged": changed,
                    "remounted": remounted,
                    "preElementCount": pre_sig.count,
                    "postElementCount": post.count,
                    "preSignature": pre_sig.content.to_string(),
                    "postSignature": post.content.to_string(),
                    "preGeneration": pre_sig.generation.to_string(),
                    "postGeneration": post.generation.to_string(),
                    "settleMs": settle_ms,
                })
            }
            None => serde_json::json!({
                "supported": false,
                "effectChanged": null,
                "remounted": null,
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
                    truncate_str_ellipsis(&s, 1024)
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

// ----------------------------------------------------------------------------
// Phase A1 (wrapper-Builder fix): WS-aware component handlers.
//
// `POST /ui-bridge/control/component/{id}/action/{action_id}` and the two
// companion read endpoints used to Tauri-IPC into the runner's React frontend
// unconditionally. That frontend only knows about its own React tree, so
// WebSocket-registered wrapper apps (Phase 1 wrapper framework) were
// unreachable through the canonical `/control/` surface — the wrapper plan's
// stated orchestration model claims this path works for wrappers.
//
// The helpers below let each handler consult `AppRegistry` first. When the
// caller's `id` matches a `Websocket`-transport registered app, we dispatch
// the command over its `/ui-bridge/ws` socket via `AppDispatcher`. Otherwise
// we fall through to the existing IPC path so runner-internal React
// components continue to behave exactly as before.
//
// Tie-break rule for ID collisions (extremely unlikely but possible — e.g. a
// wrapper named "terminal"): WS wins. Runner-internal IDs are kebab-cased
// React component names like `nav-button-terminal`; wrapper IDs are typically
// domain-named (`gmail`, `v0`, `test-wrapper`).
//
// On WS dispatch error we deliberately do NOT fall through to IPC. The
// wrapper has registered as the owner of that id; falling back would
// silently address the runner-frontend's React tree instead and produce
// confusing results.
// ----------------------------------------------------------------------------

/// Outcome of a WS-first lookup against `AppRegistry` and dispatch through
/// `AppDispatcher`.
///
/// - `None` — no WS-transport entry exists for this id; caller should fall
///   through to IPC.
/// - `Some(Ok(value))` — the registered wrapper handled the command.
/// - `Some(Err(message))` — the wrapper is registered as WS but dispatch
///   failed (disconnected, timeout, transport error). Caller MUST surface
///   the error and NOT fall through to IPC.
async fn try_ws_dispatch_for_app(
    registry: &AppRegistry,
    dispatcher: &AppDispatcher,
    app_id: &str,
    action: &str,
    http_method: Method,
    http_path: &str,
    payload: serde_json::Value,
) -> Option<Result<serde_json::Value, String>> {
    let entry = registry.get(app_id).await?;
    if entry.transport != AppTransport::Websocket {
        return None;
    }
    debug!(
        "[ui-bridge/elements] {} via WS → app_id='{}' (path={})",
        action, app_id, http_path
    );
    Some(
        dispatcher
            .dispatch(app_id, action, http_method, http_path, payload)
            .await
            .map_err(|e| e.to_user_message()),
    )
}

/// Iterate every live WS-transport app and ask each for its components list.
/// Errors from any individual app are logged but not propagated, so a single
/// flaky wrapper cannot abort the merged response. Used by the LIST endpoint
/// only.
async fn ws_collect_components(
    registry: &AppRegistry,
    dispatcher: &AppDispatcher,
) -> Vec<serde_json::Value> {
    let live = registry.list_live().await;
    let mut out = Vec::new();
    for entry in live {
        if entry.transport != AppTransport::Websocket {
            continue;
        }
        let app_id = entry.app.app_id.clone();
        match dispatcher
            .dispatch(
                &app_id,
                "getComponents",
                Method::GET,
                "/ui-bridge/control/components",
                serde_json::json!({}),
            )
            .await
        {
            Ok(value) => {
                // Wrappers may either return a bare array or wrap it in
                // `{success, data}` / `{components: [...]}`. Accept any of
                // those shapes and append the contained items.
                if let Some(arr) = value.as_array() {
                    out.extend(arr.iter().cloned());
                } else if let Some(arr) = value.get("data").and_then(|v| v.as_array()) {
                    out.extend(arr.iter().cloned());
                } else if let Some(arr) = value.get("components").and_then(|v| v.as_array()) {
                    out.extend(arr.iter().cloned());
                } else if value.is_object() {
                    // Single-component object — push as-is.
                    out.push(value);
                }
            }
            Err(e) => {
                warn!(
                    "[ui-bridge/elements] getComponents WS dispatch to '{}' failed: {}",
                    app_id, e
                );
            }
        }
    }
    out
}

/// Build a flat 400 error response from a WS dispatch failure. Mirrors the
/// shape `wrap_ipc_result` produces for inner-failure envelopes so callers
/// see a consistent error contract regardless of transport.
fn ws_dispatch_error_response(error_msg: String) -> (StatusCode, Json<ApiResponse<()>>) {
    let detail = classify_transport_error(&error_msg);
    (
        StatusCode::BAD_REQUEST,
        Json(api_error_detailed(error_msg, detail)),
    )
}

/// Get all registered components.
///
/// Phase A1 merge behavior: returns the IPC component list (runner-internal
/// React components) concatenated with the components reported by every live
/// WebSocket-transport registered app (wrappers). IPC entries come first;
/// per-wrapper errors during the WS fan-out are logged but never abort the
/// merged response, so a flaky wrapper degrades gracefully. Both bare-array
/// and `{data: [...]}` / `{components: [...]}` shapes from wrappers are
/// accepted and unwrapped.
///
/// F2 (Direction B) envelope shape:
/// - `data` is `{components: [...]}` (object-shaped, matches every other
///   rich endpoint convention and aligns with the SDK relay).
/// - A **deprecated** top-level `components` field mirrors the array so
///   pre-Direction-B callers keep working. Slated for removal one release
///   after Direction B lands — read from `data.components` instead.
pub async fn ui_bridge_get_components_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting all components");

    // Fan out to WS-transport wrappers in parallel with the IPC call.
    let registry = state.app_registry.clone();
    let dispatcher = state.app_dispatcher.clone();
    let ws_future = async move { ws_collect_components(&registry, &dispatcher).await };
    let ipc_future = ui_bridge_request_sync(&state, "get_components", serde_json::json!({}));
    let (ipc_result, mut ws_components) = tokio::join!(ipc_future, ws_future);

    // Surface IPC transport errors normally — but if IPC is broken AND we
    // do have WS components, prefer to return the WS list rather than 5xx,
    // since wrapper components are the more interesting payload for callers
    // that explicitly registered them.
    let mut merged: Vec<serde_json::Value> = match ipc_result {
        Ok(ipc_data) => {
            // The frontend can return either a bare array or a wrapped
            // `{components: [...]}` / `{data: {components: [...]}}` shape.
            // Defensive extraction mirrors `ws_collect_components` above.
            if let Some(arr) = ipc_data.as_array() {
                arr.iter().cloned().collect()
            } else if let Some(arr) = ipc_data.get("data").and_then(|v| v.as_array()) {
                arr.iter().cloned().collect()
            } else if let Some(arr) = ipc_data.get("components").and_then(|v| v.as_array()) {
                arr.iter().cloned().collect()
            } else if let Some(arr) = ipc_data
                .get("data")
                .and_then(|d| d.get("components"))
                .and_then(|v| v.as_array())
            {
                arr.iter().cloned().collect()
            } else if ipc_data.is_object() {
                vec![ipc_data]
            } else {
                Vec::new()
            }
        }
        Err(e) => {
            if ws_components.is_empty() {
                // Pure-IPC environment: surface the transport error.
                let detail = classify_transport_error(&e);
                let status = match detail.code {
                    super::types::UiBridgeErrorCode::FrontendNotReady => {
                        StatusCode::SERVICE_UNAVAILABLE
                    }
                    _ => StatusCode::INTERNAL_SERVER_ERROR,
                };
                error!("UI Bridge API: get_components IPC failed: {}", e);
                return Err((status, Json(api_error_detailed(e, detail))));
            } else {
                warn!(
                    "UI Bridge API: get_components IPC failed ({}); returning {} WS-only components",
                    e,
                    ws_components.len()
                );
                Vec::new()
            }
        }
    };

    merged.append(&mut ws_components);
    let components_array = serde_json::Value::Array(merged);
    // F2 (Direction B): emit `{success, data:{components:[...]}}` — the
    // canonical wrapper-SDK envelope shape.
    //
    // Loop B Item B: the previous emission also carried a top-level
    // `components` field as a deprecated back-compat mirror, but it
    // duplicated `data.components` byte-for-byte and confused callers
    // about which key was authoritative. The mirror has been dropped;
    // `data.components` is the single source of truth and the regression
    // test `components_envelope_omits_top_level_mirror` locks it in.
    Ok(Json(serde_json::json!({
        "success": true,
        "data": { "components": components_array },
    })))
}

/// Get a specific component by ID.
///
/// Phase A1: WS-transport registered apps win over the IPC path when their
/// `appId` matches the URL `id` segment.
///
/// Phase 1.1 (plan 2026-05-03): when the IPC returns the runner's bare
/// "Component not found: <id>" envelope, enrich the error with the SDK's
/// canonical `Available components: [...]` + cross-route hint shape (mirrors
/// the outer `/ui-bridge/sdk/component/:id` wrapper). The enrichment fetches
/// a snapshot opportunistically; if that fails, the bare error falls
/// through unchanged.
pub async fn ui_bridge_get_component_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting component {}", id);

    let ws_path = format!("/ui-bridge/control/component/{}", id);
    if let Some(ws_outcome) = try_ws_dispatch_for_app(
        &state.app_registry,
        &state.app_dispatcher,
        &id,
        "getComponent",
        Method::GET,
        &ws_path,
        serde_json::json!({ "componentId": id }),
    )
    .await
    {
        return match ws_outcome {
            Ok(value) => Ok(Json(ApiResponse::success(value))),
            Err(e) => Err(ws_dispatch_error_response(e)),
        };
    }

    let ipc_result = ui_bridge_request_sync(
        &state,
        "get_component",
        serde_json::json!({ "componentId": id }),
    )
    .await;

    // Detect the component-not-found inner-failure envelope BEFORE
    // wrap_ipc_result flattens it, so we can rewrite the error message
    // with the enriched SDK shape. Phase 5: discriminate on the canonical
    // Wave-1 code (the frontend `get_component` handler now emits
    // `code: "UB-ELEM-NOT-FOUND"`); the `"Component not found:"` prose
    // prefix is kept ONLY as a runner-internal bridge for an in-flight
    // frontend bundle (the `dist/` build can lag the Rust binary by one
    // rebuild). Anything else falls through to wrap_ipc_result unchanged.
    if let Ok(ref data) = ipc_result {
        if data.get("success").and_then(|v| v.as_bool()) == Some(false) {
            let bare_error = data.get("error").and_then(|v| v.as_str()).unwrap_or("");
            let is_not_found = extract_error_code(data) == Some(CanonicalCode::UbElemNotFound)
                || bare_error.starts_with("Component not found:");
            if is_not_found {
                // Best-effort snapshot fetch — if it fails, surface the
                // bare error rather than 5xx-ing the request.
                if let Ok(snapshot) =
                    ui_bridge_request_sync(&state, "get_snapshot", serde_json::json!({})).await
                {
                    let enriched =
                        super::component_errors::build_component_not_found_message(&id, &snapshot);
                    // Reuse `element_not_found` as the closest match in the
                    // structured-error taxonomy — the SDK's own error path
                    // does not differentiate component-vs-element, and the
                    // enriched human-readable message carries the
                    // component-specific guidance.
                    let mut body =
                        api_error_detailed(enriched, UiBridgeError::element_not_found(&id));
                    if let Some(hint) = data.get("hint") {
                        body.hint = Some(hint.clone());
                    }
                    return Err((StatusCode::NOT_FOUND, Json(body)));
                }
            }
        }
    }

    wrap_ipc_result(ipc_result)
}

/// Execute an action on a component.
///
/// Phase A1: WS-transport registered apps win over the IPC path when their
/// `appId` matches the URL `id` segment. WS dispatch failures are surfaced
/// directly (no IPC fallback), because the wrapper has registered as the
/// owner of that id and falling back would silently target the runner-
/// frontend's React tree instead.
pub async fn ui_bridge_execute_component_action_handler(
    State(state): State<Arc<ApiState>>,
    Path((id, action_id)): Path<(String, String)>,
    UiBridgeJson(request): UiBridgeJson<UIBridgeComponentActionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: Executing action {} on component {}",
        action_id, id
    );

    // Accept both `{"params":{...}}` and the bare `{"layoutId":"single"}`
    // shape the action's `paramSchema` advertises — merged_params() folds any
    // flat top-level keys into params (element-action route parity).
    let merged_params = request.merged_params();
    let payload = serde_json::json!({
        "componentId": id,
        "actionId": action_id,
        "params": merged_params
    });

    let ws_path = format!("/ui-bridge/control/component/{}/action/{}", id, action_id);
    if let Some(ws_outcome) = try_ws_dispatch_for_app(
        &state.app_registry,
        &state.app_dispatcher,
        &id,
        "executeComponentAction",
        Method::POST,
        &ws_path,
        payload.clone(),
    )
    .await
    {
        return match ws_outcome {
            Ok(value) => Ok(Json(ApiResponse::success(value))),
            Err(e) => Err(ws_dispatch_error_response(e)),
        };
    }

    // Iter-3 item 4 — detect "component not found" / "action not found"
    // inner-failure shapes BEFORE wrap_ipc_result flattens them to the
    // default INTERNAL_ERROR envelope. The frontend's
    // `execute_component_action` IPC handler returns
    // `{ success:false, error: "Component not found: ..." }` or
    // `{ success:false, error: "Action <X> not found on component <Y>" }`
    // for the two miss cases; both should map to the canonical
    // `ELEMENT_NOT_FOUND` taxonomy entry (the closest available — the
    // canonical schema has no `UB-COMPONENT-NOT-AVAILABLE` yet) instead
    // of being misclassified as a transport-level INTERNAL_ERROR.
    let ipc_result = ui_bridge_request_sync(&state, "execute_component_action", payload).await;
    if let Ok(ref data) = ipc_result {
        if data.get("success").and_then(|v| v.as_bool()) == Some(false) {
            let error_msg = data.get("error").and_then(|v| v.as_str()).unwrap_or("");
            let is_component_missing = extract_error_code(data)
                == Some(CanonicalCode::UbElemNotFound)
                || error_msg.to_lowercase().contains("component not found")
                || error_msg.to_lowercase().contains("not registered");
            let is_action_missing = error_msg.to_lowercase().contains("action")
                && (error_msg.to_lowercase().contains("not found")
                    || error_msg.to_lowercase().contains("unknown"));
            if is_component_missing {
                let detail = UiBridgeError::element_not_found(&id);
                let message = format!("Component not found: {}", id);
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(api_error_detailed(message, detail)),
                ));
            }
            if is_action_missing {
                let detail = UiBridgeError {
                    code: super::types::UiBridgeErrorCode::ActionNotSupported,
                    message: error_msg.to_string(),
                    recovery: Some(super::types::RecoveryHint::Unrecoverable),
                    context: Some(serde_json::json!({
                        "componentId": id,
                        "actionId": action_id,
                    })),
                };
                let message = detail.message.clone();
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error_detailed(message, detail)),
                ));
            }
        }
    }
    wrap_ipc_result(ipc_result)
}

/// Discover controllable elements in the UI.
///
/// On success the result is cached on `ApiState::ui_bridge_last_discovered`
/// so agents can retrieve the same element set via
/// `GET /ui-bridge/control/elements/last-discovered`. This works around the
/// React registry occasionally pruning elements between calls.
pub async fn ui_bridge_discover_handler(
    State(state): State<Arc<ApiState>>,
    request: Option<Json<UIBridgeDiscoveryRequest>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Discovering elements");

    // Empty / missing body → default to a force-rescan. Callers hitting this
    // endpoint with no payload almost always want a fresh discovery; the
    // axum default of 400'ing on `EOF while parsing a value` was needless
    // friction for manual testers and MCP clients sending bare POSTs.
    // Note: an explicit body (even `{"force": false}`) is preserved as-is.
    let request = match request {
        Some(Json(r)) => r,
        None => UIBridgeDiscoveryRequest {
            force: Some(true),
            ..Default::default()
        },
    };

    let payload = serde_json::json!({
        "options": {
            "root": request.root,
            "interactiveOnly": request.interactive_only,
            "includeHidden": request.include_hidden,
            "limit": request.limit,
            "types": request.types,
            "selector": request.selector
        },
        // Top-level (not inside options) — force is a meta-flag about
        // registry state rather than a discovery filter.
        "force": request.force.unwrap_or(false)
    });

    match ui_bridge_request_sync(&state, "discover", payload).await {
        Ok(mut data) => {
            // Fold each element's registered `customActions` into its
            // advertised `actions` array so a discover consumer sees e.g.
            // `pasteText`/`getScrollback` on a `terminal-input-<id>` element
            // (which the `/control/element/{id}/action` route already
            // dispatches). Mirrors the frontend dispatcher's
            // `builtinActions + customActions` merge in `useControlEvents.ts`.
            // Applied BEFORE caching so `/control/last-discovered` advertises
            // them too.
            advertise_custom_actions_in_payload(&mut data);

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
                        .app_state
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
///   - `?withDisabledOnly=true` (manual-test remediation 2026-05-10
///     Item 2) — keep only elements that report disabled by ANY signal in
///     `state`: `disabled === true`, `ariaDisabled === "true"`, or
///     `enabled === false`. Lets agents find a disabled target without
///     walking the full snapshot. Companion to the SDK-side filter in
///     `@qontinui/ui-bridge` (handlers.ts `getControlSnapshot`); both
///     paths apply the same predicate so the runner-direct HTTP route and
///     the SDK-WS route stay observationally identical.
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
    // Manual-test remediation 2026-05-10 (Item 2). Both camelCase and
    // snake_case spellings are accepted to match the SDK's alias surface.
    let with_disabled_only = query.get("withDisabledOnly").is_some_and(truthy)
        || query.get("with_disabled_only").is_some_and(truthy);

    info!(
        "UI Bridge API: Getting snapshot (visibleOnly={}, currentRouteOnly={}, withDisabledOnly={})",
        visible_only, current_route_only, with_disabled_only
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
            // Fold each element's registered `customActions` into its
            // advertised `actions` array (same as the discover handler) so a
            // snapshot consumer sees custom actions like `pasteText` /
            // `getScrollback` alongside the built-ins. Applied before the
            // post-fetch filters so the filtered output carries them too.
            advertise_custom_actions_in_payload(&mut data);

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
            if visible_only || current_route_only || with_disabled_only {
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
                                if with_disabled_only {
                                    // Manual-test remediation 2026-05-10
                                    // (Item 2). Mirror the SDK predicate in
                                    // `@qontinui/ui-bridge` handlers.ts:
                                    // keep elements that report disabled by
                                    // ANY of the three signals the SDK
                                    // might emit. Today's
                                    // `serializeRegisteredElement` writes
                                    // `state.enabled` (not `state.disabled`),
                                    // so checking only `disabled` would let
                                    // the filter return zero rows on every
                                    // current-shape snapshot.
                                    let state = el.get("state");
                                    let disabled_true = state
                                        .and_then(|s| s.get("disabled"))
                                        .and_then(|v| v.as_bool())
                                        == Some(true);
                                    let aria_disabled_true = state
                                        .and_then(|s| s.get("ariaDisabled"))
                                        .and_then(|v| v.as_str())
                                        == Some("true");
                                    let enabled_false = state
                                        .and_then(|s| s.get("enabled"))
                                        .and_then(|v| v.as_bool())
                                        == Some(false);
                                    if !(disabled_true || aria_disabled_true || enabled_false) {
                                        return false;
                                    }
                                }
                                true
                            });
                            let after = arr.len();
                            debug!(
                                "UI Bridge snapshot filter: visibleOnly={} currentRouteOnly={} withDisabledOnly={} kept {}/{} elements",
                                visible_only, current_route_only, with_disabled_only, after, before
                            );
                        }
                    }
                }
            }

            // Mirror the snapshot's top-level `activeTab` / `route` into the
            // `registration` sub-object so callers reading `registration.activeTab`
            // see the same value `/control/tabs` reports. Both top-level fields are
            // populated by the SDK from the runner's `getActiveTab` callback (which
            // reads `ACTIVE_TAB_STORAGE_KEY` — the source of truth `tabs_list` also
            // reads), so we just copy them across without a second IPC round-trip.
            // Purely additive: existing readers of `registration.{totalRegistered,
            // everHadRegistrations, byRoute}` are unaffected.
            {
                let active_tab_val = data.get("activeTab").cloned();
                let route_val = data.get("route").cloned();
                if let Some(obj) = data.as_object_mut() {
                    if let Some(reg_val) = obj.get_mut("registration") {
                        if let Some(reg_obj) = reg_val.as_object_mut() {
                            if let Some(v) = active_tab_val {
                                reg_obj.insert("activeTab".to_string(), v);
                            }
                            if let Some(v) = route_val {
                                reg_obj.insert("route".to_string(), v);
                            }
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
            // The page label is resolved inside `enqueue_observation` from the
            // snapshot's own `page.pageContext` / `page.pathname` — the caller
            // has no scope knowledge the snapshot doesn't already carry.
            let pg_db_for_obs = state.app_state.pg_db.clone();
            let snapshot_for_obs = data.clone();
            let runner_instance = std::env::var("QONTINUI_RUNNER_ROLE")
                .ok()
                .unwrap_or_else(|| "primary".to_string());
            tokio::spawn(async move {
                crate::state_discovery::enqueue_observation(
                    pg_db_for_obs,
                    &snapshot_for_obs,
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
    Query(query_params): Query<std::collections::HashMap<String, String>>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<serde_json::Value>>)>
{
    let body = body
        .map(|Json(v)| v)
        .unwrap_or_else(|| serde_json::json!({}));

    // B4 — `?strictTimeout=true` query parameter:
    // When set, the predicate/state SDK-forwarded helpers translate
    // `{found: false, durationMs, lastObservedState}` IPC payloads into
    // HTTP 408 + `api_error("wait_for_element_timeout")` with a `data:` body
    // carrying the original IPC fields. Default behaviour (back-compat)
    // preserves the existing `success:true` envelope on timeout. The NL/id
    // path already returns HTTP 408 on timeout (see the bottom of this
    // function) so `strictTimeout` is a no-op there.
    let strict_timeout = query_params
        .get("strictTimeout")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

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
        return ui_bridge_wait_for_element_state_predicate_handler(state, body, strict_timeout)
            .await;
    }
    if body.get("predicate").is_some() {
        return ui_bridge_wait_for_element_registered_forward(state, body, strict_timeout).await;
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
                Json(api_error_to_value(
                    "wait-for-element: provide either 'query' or 'elementId'",
                )),
            ));
        }
        (Some(_), Some(_)) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error_to_value(
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
                            // F1: emit canonical camelCase `durationMs` alongside
                            // legacy snake_case `elapsed_ms`. elapsed_ms emit
                            // retained for one release; remove after 2026-06.
                            return Ok(Json(ApiResponse::success(serde_json::json!({
                                "found": true,
                                "element": element,
                                "durationMs": elapsed_ms,
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
                    // F1: emit canonical camelCase `durationMs` alongside
                    // legacy snake_case `elapsed_ms`. elapsed_ms emit
                    // retained for one release; remove after 2026-06.
                    return Ok(Json(ApiResponse::success(serde_json::json!({
                        "found": true,
                        "element": element,
                        "durationMs": elapsed_ms,
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
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error_to_value(e)),
                ));
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
            // F1: attach structured `data` payload to the HTTP 408 envelope
            // so callers can read durationMs/polls without parsing the
            // error string. The shape mirrors the predicate/state strictTimeout
            // path's data payload.
            let mut body = api_error_to_value(msg);
            body.data = Some(serde_json::json!({
                "found": false,
                "durationMs": elapsed_ms,
                // TODO(remove 2026-06): deprecated snake_case alias kept for
                // pre-F1 callers still parsing `elapsed_ms`. New callers
                // must read `durationMs`.
                "elapsed_ms": elapsed_ms,
                "polls": polls,
            }));
            return Err((StatusCode::REQUEST_TIMEOUT, Json(body)));
        }

        tokio::time::sleep(std::time::Duration::from_millis(poll_interval_ms)).await;
    }
}

/// Local helper that produces an error `ApiResponse<serde_json::Value>` —
/// mirrors `api_error()` but with the `data: Option<serde_json::Value>`
/// field intact so callers can attach a structured payload alongside the
/// error string. Used by wait-for-element to emit `data:{durationMs,...}`
/// on timeout.
fn api_error_to_value(message: impl Into<String>) -> ApiResponse<serde_json::Value> {
    ApiResponse {
        success: false,
        data: None,
        error: Some(message.into()),
        error_detail: None,
        hint: None,
        code: None,
        suggestions: None,
    }
}

/// Forward a `predicate`-shaped wait-for-element body to the SDK's
/// `waitForElementRegistered` dispatcher. We stamp a defensive timeout
/// clamp before forwarding so a runaway client can't park the webview
/// listener indefinitely, mirroring Phase 1's /wait-for-route-change
/// behaviour. The SDK itself re-clamps to the same window.
///
/// B4 `strict_timeout`: when true, a successful IPC response carrying
/// `{found: false, ...}` is rewritten into an HTTP 408 + error envelope
/// so the predicate path matches the NL/id contract. When false (default),
/// the legacy `success:true` envelope is preserved for back-compat.
async fn ui_bridge_wait_for_element_registered_forward(
    state: Arc<ApiState>,
    mut body: serde_json::Value,
    strict_timeout: bool,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<serde_json::Value>>)>
{
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
        "UI Bridge API: wait-for-element (predicate shape) predicate={:?} requirement={:?} timeoutMs={} strictTimeout={}",
        body.get("predicate"),
        body.get("requirement").and_then(|v| v.as_str()),
        timeout_ms,
        strict_timeout,
    );

    let payload = serde_json::json!({ "params": body });
    let result = wrap_ipc_result(
        ui_bridge_request_sync(&state, "wait_for_element_registered", payload).await,
    );
    apply_strict_timeout_reshape(result, strict_timeout)
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
                .or_else(|| {
                    v.as_f64()
                        .filter(|f| *f >= 0.0 && f.is_finite())
                        .map(|f| f as u64)
                })
                .ok_or_else(|| {
                    "wait-for-element: 'timeoutMs' must be a non-negative number".to_string()
                })?;
            if n > 30_000 {
                return Err("wait-for-element: 'timeoutMs' must be between 0 and 30000".to_string());
            }
            n
        }
    };

    let poll_ms = match body.get("pollMs") {
        None => 50_u64,
        Some(v) => {
            let n = v
                .as_u64()
                .or_else(|| {
                    v.as_f64()
                        .filter(|f| *f >= 0.0 && f.is_finite())
                        .map(|f| f as u64)
                })
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
/// verbatim; without `strict_timeout` we surface it 1:1 inside the standard
/// `ApiResponse::success` envelope so `found:false` (timeout) is **not** an
/// HTTP error. With `strict_timeout`, `{found:false, durationMs, ...}` is
/// rewritten into HTTP 408 + `data:{...}` to match the NL/id path.
async fn ui_bridge_wait_for_element_state_predicate_handler(
    state: Arc<ApiState>,
    body: serde_json::Value,
    strict_timeout: bool,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<serde_json::Value>>)>
{
    let req = validate_wait_for_element_state_request(&body)
        .map_err(|msg| (StatusCode::BAD_REQUEST, Json(api_error_to_value(msg))))?;

    info!(
        "UI Bridge API: wait-for-element (state shape) elementId={:?} selector={:?} state={} timeoutMs={} pollMs={} strictTimeout={}",
        req.element_id, req.selector, req.state, req.timeout_ms, req.poll_ms, strict_timeout
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

    let result = wrap_ipc_result(
        ui_bridge_request_sync(&state, "wait_for_element_state_predicate", payload).await,
    );
    apply_strict_timeout_reshape(result, strict_timeout)
}

/// B4 — translate a successful IPC envelope carrying `{found: false, ...}`
/// into an HTTP 408 error response when the caller opted into
/// `?strictTimeout=true`. Otherwise pass the response through unchanged.
///
/// Also adapts the error tuple type from `ApiResponse<()>` (what
/// `wrap_ipc_result` returns) to `ApiResponse<serde_json::Value>` (what the
/// wait-for-element handlers need, so the timeout error path can carry a
/// `data` payload).
fn apply_strict_timeout_reshape(
    result: Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)>,
    strict_timeout: bool,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<serde_json::Value>>)>
{
    match result {
        Ok(Json(resp)) => {
            if strict_timeout {
                // Inspect the inner data for `found: false` — both predicate
                // (`waitForElementRegistered`) and state-predicate runtimes
                // return `{found: bool, durationMs?, lastObservedState?}`.
                let found_false = resp
                    .data
                    .as_ref()
                    .and_then(|d| d.get("found"))
                    .and_then(|v| v.as_bool())
                    == Some(false);
                if found_false {
                    let data_payload = resp.data.clone().unwrap_or(serde_json::json!({}));
                    let mut body = api_error_to_value("wait_for_element_timeout");
                    body.data = Some(data_payload);
                    return Err((StatusCode::REQUEST_TIMEOUT, Json(body)));
                }
            }
            Ok(Json(resp))
        }
        Err((status, Json(err))) => {
            // Widen the error type from ApiResponse<()> -> ApiResponse<serde_json::Value>.
            let widened = ApiResponse {
                success: err.success,
                data: None,
                error: err.error,
                error_detail: err.error_detail,
                hint: err.hint,
                code: err.code,
                suggestions: err.suggestions,
            };
            Err((status, Json(widened)))
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

    wrap_ipc_result(ui_bridge_request_sync(&state, "find", request).await)
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

    // Optional `windowLabel` scopes the read to a pop-out window (Phase 4 of
    // plan 2026-06-07-multi-window-sdk-automation); omit -> main window.
    let window_label = read_window_label(&body);
    match evaluate_js_expression_in_window(&state, &js, window_label).await {
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

    // Bust the vision cache: a click can change rendered pixels.
    crate::mcp::ui_bridge::vision_routes::bump_mutation_id(&state);

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

    // Optional `windowLabel` scopes the read to a pop-out window (Phase 4 of
    // plan 2026-06-07-multi-window-sdk-automation); omit -> main window.
    let window_label = read_window_label(&body);
    match evaluate_js_expression_in_window(&state, &js, window_label).await {
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

    // Bust the vision cache: a click can change rendered pixels.
    crate::mcp::ui_bridge::vision_routes::bump_mutation_id(&state);

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

    // Optional `windowLabel` scopes the read to a pop-out window (Phase 4 of
    // plan 2026-06-07-multi-window-sdk-automation); omit -> main window.
    let window_label = read_window_label(&body);
    match evaluate_js_expression_in_window(&state, &js, window_label).await {
        Ok(result) => {
            let parsed: serde_json::Value = serde_json::from_str(&result)
                .unwrap_or(serde_json::json!({"clicked": false, "error": "Parse error"}));
            Ok(Json(ApiResponse::success(parsed)))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Validate the `all` parameter of `read-value`.
///
/// Returns the effective boolean, or a 400 message naming the offending
/// parameter(s). Mirrors `readValuePrimitive` in `@qontinui/ui-bridge`
/// (`server/page-primitives.ts`) so the direct HTTP path and the SDK path
/// reject the same bodies with the same reasons — the whole point of this fix
/// is that the two paths stopped agreeing about `all`.
///
/// Pure + unit-tested: the validation is the part worth pinning, and it needs
/// neither a router nor a webview.
pub fn validate_read_value_all(body: &serde_json::Value) -> Result<bool, String> {
    let Some(raw) = body.get("all") else {
        return Ok(false);
    };
    if raw.is_null() {
        return Ok(false);
    }
    let Some(all) = raw.as_bool() else {
        return Err(format!(
            "'all' must be a boolean (got {})",
            json_type_name(raw)
        ));
    };
    if all && body.get("index").is_some_and(|v| !v.is_null()) {
        return Err(
            "'all' and 'index' are mutually exclusive — 'all' returns every match, 'index' selects one. Drop one of them.".to_string(),
        );
    }
    Ok(all)
}

/// JSON type name for a validation message ("string", "number", …).
fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Read the value of a form element by CSS selector.
/// POST /ui-bridge/control/page/read-value
/// Body: { "selector": "textarea", "index": 0 }
/// Or:   { "selector": "input.row", "all": true }
///
/// `all: true` returns EVERY match in `values[]` (document order). This handler
/// is an INDEPENDENT JS-injection implementation of the SDK's `readValue` — it
/// reads only `selector`/`index` — so honouring `all` in `@qontinui/ui-bridge`
/// (PR #156) did nothing for a caller reaching the runner over the direct HTTP
/// path: it asked for 14 values, got 1, and nothing in the response said the
/// parameter had been dropped. `all` and the singular `index` are mutually
/// exclusive and a body carrying both is REJECTED by name (matching the SDK's
/// wording) rather than one of them being silently reinterpreted; a non-boolean
/// `all` is likewise rejected by name.
///
/// `totalMatches` was already computed and returned in both modes, so a 1-of-N
/// read has always been able to say so.
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

    let all = match validate_read_value_all(&body) {
        Ok(v) => v,
        Err(msg) => return Err((StatusCode::BAD_REQUEST, Json(api_error(msg)))),
    };

    let escaped_selector = selector.replace('\\', "\\\\").replace('\'', "\\'");

    let js = format!(
        r#"(() => {{
            const readOne = (el, i) => ({{
                index: i,
                tag: el.tagName,
                type: el.type || null,
                value: el.value || el.textContent?.trim() || '',
                length: (el.value || el.textContent || '').length,
                placeholder: el.placeholder || null,
                disabled: el.disabled || false,
                readOnly: el.readOnly || false
            }});
            const matches = Array.from(document.querySelectorAll('{selector}'));
            if (matches.length === 0) return JSON.stringify({{ found: false, error: 'No elements match selector', totalMatches: 0 }});
            if ({all}) {{
                const values = matches.map(readOne);
                return JSON.stringify(Object.assign({{ found: true }}, values[0], {{
                    totalMatches: matches.length,
                    values: values
                }}));
            }}
            const idx = {index};
            if (idx >= matches.length) return JSON.stringify({{ found: false, error: 'Index out of range', totalMatches: matches.length }});
            return JSON.stringify(Object.assign({{ found: true }}, readOne(matches[idx], idx), {{
                totalMatches: matches.length
            }}));
        }})()"#,
        selector = escaped_selector,
        index = index,
        all = all
    );

    // Optional `windowLabel` scopes the read to a pop-out window (Phase 4 of
    // plan 2026-06-07-multi-window-sdk-automation); omit -> main window.
    let window_label = read_window_label(&body);
    match evaluate_js_expression_in_window(&state, &js, window_label).await {
        Ok(result) => {
            let parsed: serde_json::Value = serde_json::from_str(&result)
                .unwrap_or(serde_json::json!({"found": false, "error": "Parse error"}));
            Ok(Json(ApiResponse::success(parsed)))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Build the element-RESOLUTION expression for `type-into`.
///
/// The returned JS answers a JSON object and mutates nothing beyond scrolling
/// the hit into view:
///
///   - hit  → `{found: true, path, tag, id, testId, index}`
///   - miss → `{found: false, error, matchCount?}`
///
/// `path` is a selector that resolves to **exactly** this element (a unique
/// `#id` where one exists, otherwise a full `:nth-of-type` path from `html`).
/// That is what the SDK's `findElementByIdentifier` accepts in place of a
/// registered element id, so it is how a raw `<input>` — which never appears in
/// the UI Bridge element registry — becomes addressable by `execute_action`.
/// A synthesized path is used even when the caller's own selector would do,
/// because `findElementByIdentifier` tries `[data-testid=…]` and `#id` BEFORE
/// `querySelector`, so a bare selector like `email` could silently address a
/// different element than the one this step resolved.
fn type_into_resolve_js(find_expr: &str, index: u64) -> String {
    format!(
        r#"(() => {{
            const matches = Array.from({find_expr}).filter(el => el);
            if (matches.length === 0) return JSON.stringify({{ found: false, error: 'No elements found' }});
            const idx = {index};
            if (idx >= matches.length) return JSON.stringify({{ found: false, error: 'Index out of range', matchCount: matches.length }});
            const el = matches[idx];
            const isUnique = (sel) => {{
                try {{
                    const hits = document.querySelectorAll(sel);
                    return hits.length === 1 && hits[0] === el;
                }} catch (e) {{
                    return false;
                }}
            }};
            let path = null;
            if (el.id) {{
                const byId = '#' + CSS.escape(el.id);
                if (isUnique(byId)) path = byId;
            }}
            if (!path) {{
                const parts = [];
                let cur = el;
                while (cur && cur.nodeType === 1 && cur !== document.documentElement) {{
                    let seg = cur.nodeName.toLowerCase();
                    const parent = cur.parentElement;
                    if (parent) {{
                        const sibs = Array.from(parent.children).filter(c => c.nodeName === cur.nodeName);
                        if (sibs.length > 1) seg += ':nth-of-type(' + (sibs.indexOf(cur) + 1) + ')';
                    }}
                    parts.unshift(seg);
                    cur = parent;
                }}
                path = ['html'].concat(parts).join(' > ');
            }}
            el.scrollIntoView({{ block: 'center' }});
            return JSON.stringify({{
                found: true,
                path: path,
                tag: el.tagName,
                id: el.id || null,
                testId: el.getAttribute('data-testid'),
                index: idx
            }});
        }})()"#
    )
}

/// Build the `execute_action` IPC payload that hands the value mutation to the
/// SDK's `applyValueMutation` (native prototype setter chosen by element kind,
/// `_valueTracker` reset, `input`, the direct `__reactProps$.onChange` call,
/// then `change`).
///
/// Field names match what `useControlEvents.ts::execute_action` destructures
/// (`{ elementId, action }`), not the `{id, action, params}` spelling a couple
/// of older Rust call sites still use. `clear` maps to the SDK's
/// `clear-then-append` mode and its absence to `append`, which is exactly the
/// `clear ? text : el.value + text` semantics this endpoint has always had.
fn type_into_action_payload(element_id: &str, text: &str, clear: bool) -> serde_json::Value {
    serde_json::json!({
        "elementId": element_id,
        "action": {
            "action": "type",
            "params": { "text": text, "clear": clear },
        },
    })
}

/// Type text into an element by CSS selector or label.
/// POST /ui-bridge/control/page/type-into
/// Body: { "selector": "textarea", "text": "hello", "clear": true, "index": 0 }
/// Or:   { "label": "Email", "text": "user@example.com" }
///
/// **This handler resolves; the SDK mutates.** It used to carry its own value
/// mutation as injected JS, which was a third implementation of typing
/// alongside the SDK's `applyValueMutation` and the `type_into` relay case in
/// `usePageEvents.ts` — and it was broken for every `<input>` on the page:
///
///   1. The native setter was picked with
///      `descriptor(HTMLTextAreaElement.prototype, 'value')?.set || descriptor(HTMLInputElement.prototype, 'value')?.set`.
///      The textarea descriptor ALWAYS exists, so the `||` arm was dead code and
///      every element got the textarea IDL setter — which throws
///      `TypeError: Illegal invocation` in Blink when called with an `<input>`
///      receiver.
///   2. It never reset React's `_valueTracker` and never called
///      `__reactProps$*.onChange`, so a controlled React input reverted to its
///      state value on the next render even when the setter did fire.
///   3. `text` defaulted to `""`, so a body that omitted it typed nothing and
///      still answered `{typed: true}` — the silent success the SDK closed
///      deliberately in `performType`.
///
/// So the mutation is gone and the endpoint is now two steps: resolve the
/// selector/label/index to a uniquely-addressable path
/// ([`type_into_resolve_js`]), then dispatch `execute_action` `type`
/// ([`type_into_action_payload`]). All addressing modes survive — `index` and
/// `label` are resolved here because `findElementByIdentifier` only ever
/// returns the FIRST `querySelector` hit.
///
/// Response shape follows the SDK's declared `typeInto` contract
/// (`{typed, element?}`) plus `index`. The old `valueLength` field is gone:
/// `execute_action` reports no post-action value (`getElementState` carries no
/// `value`), and inventing a third round-trip to read one back is worse than
/// pointing callers at `POST /control/page/read-value`.
pub async fn ui_bridge_type_into_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let selector = body.get("selector").and_then(|v| v.as_str());
    let label = body.get("label").and_then(|v| v.as_str());
    let clear = body.get("clear").and_then(|v| v.as_bool()).unwrap_or(false);
    let index = body.get("index").and_then(|v| v.as_u64()).unwrap_or(0);

    // `text` is REQUIRED, exactly as the SDK's `typeInto` contract declares it.
    // Defaulting a missing field to `""` made "you forgot the text" and "I typed
    // your text" the same HTTP 200.
    let text = match body.get("text") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!(
                    "'text' must be a string (the characters to type); got {other}"
                ))),
            ));
        }
        None => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(
                    "'text' field is required (the characters to type). To blank a \
                     field, send 'clear': true with 'text': \"\".",
                )),
            ));
        }
    };

    // Bust the vision cache: typing changes rendered pixels (form values,
    // validation messages, etc).
    crate::mcp::ui_bridge::vision_routes::bump_mutation_id(&state);

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

    // Optional `windowLabel` scopes BOTH steps to a pop-out window (Phase 4 of
    // plan 2026-06-07-multi-window-sdk-automation); omit -> main window.
    let window_label = read_window_label(&body);

    // ── Step 1: resolve ──────────────────────────────────────────────────
    let resolve_js = type_into_resolve_js(&find_expr, index);
    let raw = evaluate_js_expression_in_window(&state, &resolve_js, window_label)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    // An unparseable eval result used to become
    // `ApiResponse::success({"typed": false, "error": "Parse error"})` — an
    // HTTP 200 for a call that demonstrably never ran. Report the runner-side
    // failure it actually is.
    let resolved: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "type-into: could not parse the element-resolution result ({e}); raw: {}",
                truncate_str_ellipsis(&raw, 200)
            ))),
        )
    })?;

    if resolved.get("found").and_then(|v| v.as_bool()) != Some(true) {
        let message = resolved
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("No elements found")
            .to_string();
        return Err((StatusCode::BAD_REQUEST, Json(api_error(message))));
    }
    let element_path = resolved
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(
                    "type-into: element resolution returned no addressable path",
                )),
            )
        })?;

    // ── Step 2: mutate, via the SDK ──────────────────────────────────────
    let action_result = ui_bridge_request_sync_in_window(
        &state,
        "execute_action",
        type_into_action_payload(element_path, &text, clear),
        window_label,
    )
    .await;
    // `wrap_ipc_result` flattens the SDK's own refusals (non-input element,
    // element vanished between the two steps) into an honest HTTP 400.
    let Json(action) = wrap_ipc_result(action_result)?;
    let action_data = action.data.unwrap_or(serde_json::Value::Null);

    Ok(Json(ApiResponse::success(serde_json::json!({
        "typed": true,
        "index": index,
        "element": {
            "tag": resolved.get("tag"),
            "id": resolved.get("id"),
            "testId": resolved.get("testId"),
            "selector": element_path,
        },
        "durationMs": action_data.get("durationMs"),
        "consoleErrors": action_data.get("consoleErrors"),
    }))))
}

// =========================================================================
// Phase 1.3 (plan 2026-05-03) — GET /control/state-summary
// =========================================================================

/// Compose the seven-field state digest: visibleElementCount, modalOpen,
/// hasErrors, idleSignals, registeredComponents, route, activeTab.
///
/// Synthesized Rust-side from a fresh `get_snapshot` (which already carries
/// elements + components + route + activeTab + modalStack) plus best-effort
/// `get_console_errors` and `get_idle_status` IPC calls. Fan-out runs in
/// parallel so the wall time is approximately the slowest single IPC, not
/// the sum of three. Each ancillary call falls back gracefully — a missing
/// console-errors response falls back to the Rust-side `AppState.ui_error`
/// crash state (so a dead React tree reports `hasErrors: true` rather than
/// reading as a clean page); missing idle status leaves `idleSignals=null`.
pub async fn ui_bridge_get_state_summary_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: GET /control/state-summary");

    let snapshot_future = ui_bridge_request_sync(&state, "get_snapshot", serde_json::json!({}));
    let console_future =
        ui_bridge_request_sync(&state, "get_console_errors", serde_json::json!({}));
    let idle_future = ui_bridge_request_sync(&state, "get_idle_status", serde_json::json!({}));

    let (snapshot_result, console_result, idle_result) =
        tokio::join!(snapshot_future, console_future, idle_future);

    let snapshot = match snapshot_result {
        Ok(v) => v,
        Err(e) => {
            error!("UI Bridge API: state-summary snapshot failed: {}", e);
            let detail = classify_transport_error(&e);
            let status = match detail.code {
                super::types::UiBridgeErrorCode::FrontendNotReady => {
                    StatusCode::SERVICE_UNAVAILABLE
                }
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            return Err((status, Json(api_error_detailed(e, detail))));
        }
    };

    // visibleElementCount: same predicate the SDK's `getStateSummary` uses —
    // element has layout (rect.width|height > 0) AND state.visible !== false.
    let visible_element_count = snapshot
        .get("elements")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|el| {
                    let rect = el.get("state").and_then(|s| s.get("rect"));
                    let has_layout = rect
                        .map(|r| {
                            let w = r.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            let h = r.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            w > 0.0 || h > 0.0
                        })
                        .unwrap_or(false);
                    let visible = el
                        .get("state")
                        .and_then(|s| s.get("visible"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    has_layout && visible
                })
                .count()
        })
        .unwrap_or(0);

    let modal_open = snapshot
        .get("modalStack")
        .and_then(|m| m.get("count"))
        .and_then(|v| v.as_u64())
        .map(|c| c > 0)
        .unwrap_or(false);

    let registered_components = snapshot
        .get("components")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let route = snapshot
        .get("route")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let active_tab = snapshot
        .get("activeTab")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let has_errors = match console_result {
        Ok(v) => {
            // The runner returns `{ errors: [...], count, ... }` shape.
            // Defensively also handle a bare array.
            if let Some(c) = v.get("count").and_then(|c| c.as_u64()) {
                c > 0
            } else if let Some(arr) = v.get("errors").and_then(|e| e.as_array()) {
                !arr.is_empty()
            } else if let Some(arr) = v.as_array() {
                !arr.is_empty()
            } else {
                false
            }
        }
        Err(e) => {
            // The console-errors buffer lives inside the React tree, so this
            // leg fails exactly when the tree is down — the case where
            // `hasErrors=false` is most misleading. Fall back to the Rust-side
            // crash state (`AppState.ui_error`, populated by the top-level
            // `ErrorBoundary`), which survives a dead tree. A crashed tree then
            // reports `hasErrors: true` instead of reading as a clean page.
            let has_ui_error = state.app_state.ui_error.get().await.is_some();
            warn!(
                "UI Bridge API: state-summary console-errors fetch failed ({}); falling back to Rust-side ui_error state (hasErrors={})",
                e, has_ui_error
            );
            has_ui_error
        }
    };

    let idle_signals = match idle_result {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "UI Bridge API: state-summary idle-status fetch failed ({}); defaulting idleSignals=null",
                e
            );
            serde_json::Value::Null
        }
    };

    let body = serde_json::json!({
        "visibleElementCount": visible_element_count,
        "modalOpen": modal_open,
        "hasErrors": has_errors,
        "idleSignals": idle_signals,
        "registeredComponents": registered_components,
        "route": route,
        "activeTab": active_tab,
    });
    Ok(Json(ApiResponse::success(body)))
}

// =========================================================================
// Phase 2.1 (plan 2026-05-03) — POST /control/element/:id/expect
// =========================================================================

/// Element predicate assertion endpoint. Returns HTTP 200 + `passed:true`
/// when the predicate holds within the timeout; HTTP 422 + `passed:false`
/// on timeout. Forwards to the SDK's `wait_for_element_state_predicate`
/// runtime (already wired through the runner-side `useAISearchEvents`
/// dispatcher) so the predicate semantics stay in lock-step with
/// `/ai/wait-for-element`.
pub async fn ui_bridge_expect_element_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let requested_state = body.get("state").and_then(|v| v.as_str());
    if requested_state.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("expect: 'state' is required".to_string())),
        ));
    }
    let requested_state = requested_state.unwrap();
    if !VALID_WAIT_FOR_ELEMENT_STATES.contains(&requested_state) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "expect: invalid state '{}', expected one of {}",
                requested_state,
                VALID_WAIT_FOR_ELEMENT_STATES.join("|")
            ))),
        ));
    }

    let timeout_ms = body
        .get("timeoutMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(5000)
        .min(30_000);
    let poll_ms = body
        .get("pollMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(100)
        .max(10);

    info!(
        "UI Bridge API: expect element id={} state={} timeoutMs={} pollMs={}",
        id, requested_state, timeout_ms, poll_ms
    );

    // Forward to the existing SDK predicate runtime — keeps semantics
    // aligned with /ai/wait-for-element. The SDK returns
    // `{found, durationMs, finalState | lastObservedState}`; we re-shape
    // into `{passed, observedState, durationMs}` per the plan contract.
    let payload = serde_json::json!({
        "params": {
            "elementId": id,
            "state": requested_state,
            "timeoutMs": timeout_ms,
            "pollMs": poll_ms,
        }
    });

    let started = Instant::now();
    let ipc_result =
        ui_bridge_request_sync(&state, "wait_for_element_state_predicate", payload).await;

    let result = match ipc_result {
        Ok(v) => v,
        Err(e) => {
            error!("UI Bridge API: expect IPC failed: {}", e);
            let detail = classify_transport_error(&e);
            let status = match detail.code {
                super::types::UiBridgeErrorCode::FrontendNotReady => {
                    StatusCode::SERVICE_UNAVAILABLE
                }
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            return Err((status, Json(api_error_detailed(e, detail))));
        }
    };

    // The SDK runtime returns `{success:false, error}` for validation
    // errors at that layer — flatten to HTTP 400.
    if result.get("success").and_then(|v| v.as_bool()) == Some(false) {
        let err = result
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("expect: predicate evaluation failed")
            .to_string();
        return Err((StatusCode::BAD_REQUEST, Json(api_error(err))));
    }

    // Project to {passed, observedState, durationMs}. The predicate runtime
    // returns either `finalState` (on success) or `lastObservedState` (on
    // timeout) — pick whichever is present.
    let found = result
        .get("found")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let observed_state = result
        .get("finalState")
        .cloned()
        .or_else(|| result.get("lastObservedState").cloned())
        .unwrap_or(serde_json::Value::Null);
    let duration_ms = result
        .get("durationMs")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| started.elapsed().as_millis() as u64);

    let body = serde_json::json!({
        "passed": found,
        "observedState": observed_state,
        "durationMs": duration_ms,
        // TODO(remove 2026-06): deprecated snake_case alias kept for pre-F1
        // callers still parsing `elapsed_ms` from wait-for-element-shaped
        // responses. New callers must read `durationMs`.
        "elapsed_ms": duration_ms,
    });

    if found {
        Ok(Json(ApiResponse::success(body)))
    } else {
        // Critical semantic — assertion miss surfaces as HTTP 422 so callers
        // can fail-fast on the status code rather than parsing the body.
        // Wrap in an error envelope alongside the raw payload so the body
        // stays predictable.
        let mut envelope = ApiResponse::<serde_json::Value>::success(body);
        envelope.success = false;
        Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ApiResponse::<()> {
                success: false,
                data: None,
                error: Some(format!(
                    "Element predicate '{}' did not hold within {}ms",
                    requested_state, timeout_ms
                )),
                error_detail: None,
                hint: Some(envelope.data.unwrap_or(serde_json::Value::Null)),
                code: None,
                suggestions: None,
            }),
        ))
    }
}

// =========================================================================
// Macro-generated IPC proxies — react-state, history, rank
// =========================================================================
ipc_handler_path_get!(
    ui_bridge_get_element_react_state_handler,
    "get_element_react_state",
    "id"
);
ipc_handler_path_get!(
    ui_bridge_get_element_history_handler,
    "get_element_history",
    "id"
);
ipc_handler_post!(ui_bridge_rank_elements_handler, "rank_elements");

/// `GET /ui-bridge/control/changes/since?since=<i64>&limit=<usize>` — proxy
/// for the SDK's mutation-buffer query. Custom (not macro-generated) because
/// the macros don't accept query params.
///
/// Forwards to the frontend's `get_changes_since` IPC handler with payload
/// `{ params: { since, limit } }`. Defaults: `since=0`, `limit=100`. Matches
/// the SDK shape from `relay-handlers.ts:1609-1614`.
pub async fn ui_bridge_get_changes_since_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let since: i64 = query.get("since").and_then(|s| s.parse().ok()).unwrap_or(0);
    let limit: usize = query
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);
    let payload = serde_json::json!({ "params": { "since": since, "limit": limit } });
    super::request::wrap_ipc_result(
        ui_bridge_request_sync(&state, "get_changes_since", payload).await,
    )
}

/// Primary element operations, components, discovery/snapshot, wait-for-element,
/// and the DOM-convenience family (find / find-by-text / click-by-text /
/// click-by-selector / read-value / type-into).
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use super::routing::add_dual;
    use axum::routing::{get, post};
    let router = axum::Router::new().route(
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
            "/ui-bridge/control/element/{id}/tree",
            get(ui_bridge_get_element_tree_subtree_handler),
        )
        .route(
            "/ui-bridge/control/element/{id}/assert",
            post(ui_bridge_assert_element_handler),
        )
        .route(
            // Phase 2.1 (plan 2026-05-03)
            "/ui-bridge/control/element/{id}/expect",
            post(ui_bridge_expect_element_handler),
        )
        .route(
            "/ui-bridge/control/element/{id}/action",
            post(ui_bridge_execute_action_handler),
        )
        .route(
            "/ui-bridge/control/element/{id}/react-state",
            get(ui_bridge_get_element_react_state_handler),
        )
        .route(
            "/ui-bridge/control/element/{id}/history",
            get(ui_bridge_get_element_history_handler),
        )
        // Alias for /control/element/{id}/history — same handler, debug path
        .route(
            "/ui-bridge/debug/element-history/{id}",
            get(ui_bridge_get_element_history_handler),
        )
        .route(
            "/ui-bridge/control/elements/rank",
            post(ui_bridge_rank_elements_handler),
        )
        .route(
            "/ui-bridge/control/changes/since",
            get(ui_bridge_get_changes_since_handler),
        )
        .route(
            "/ui-bridge/control/batch-actions",
            post(ui_bridge_batch_actions_handler),
        )
        // Alias for /control/batch-actions — same handler, plural path
        .route(
            "/ui-bridge/control/actions/batch",
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
        )
        .route(
            // Phase 1.3 (plan 2026-05-03)
            "/ui-bridge/control/state-summary",
            get(ui_bridge_get_state_summary_handler),
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
        ("GET", "/ui-bridge/control/element/{id}/tree"),
        ("POST", "/ui-bridge/control/element/{id}/assert"),
        ("POST", "/ui-bridge/control/element/{id}/expect"),
        ("POST", "/ui-bridge/control/element/{id}/action"),
        ("GET", "/ui-bridge/control/element/{id}/react-state"),
        ("GET", "/ui-bridge/control/element/{id}/history"),
        ("GET", "/ui-bridge/debug/element-history/{id}"),
        ("POST", "/ui-bridge/control/elements/rank"),
        ("GET", "/ui-bridge/control/changes/since"),
        ("POST", "/ui-bridge/control/batch-actions"),
        ("POST", "/ui-bridge/control/actions/batch"),
        ("GET", "/ui-bridge/control/components"),
        ("GET", "/ui-bridge/control/component/{id}"),
        (
            "POST",
            "/ui-bridge/control/component/{id}/action/{action_id}",
        ),
        ("POST", "/ui-bridge/control/discover"),
        ("GET", "/ui-bridge/control/snapshot"),
        ("GET", "/ui-bridge/control/state-summary"),
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
        validate_wait_for_element_state_request(&body).expect("expected validation to succeed")
    }

    fn err(body: serde_json::Value) -> String {
        validate_wait_for_element_state_request(&body).expect_err("expected validation to fail")
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

/// B4 — `?strictTimeout=true` re-shaping tests for the predicate/state
/// SDK-forwarded paths. These exercise the pure helper so we don't need an
/// axum router / live IPC. The helper translates a successful IPC envelope
/// carrying `{found: false, ...}` into an HTTP 408 + error body when the
/// caller opted into strict timeouts; otherwise the response passes through
/// unchanged for back-compat with existing SDK consumers.
#[cfg(test)]
mod strict_timeout_reshape_tests {
    use super::apply_strict_timeout_reshape;
    use crate::mcp::types::{api_error, ApiResponse};
    use axum::http::StatusCode;
    use axum::response::Json;
    use serde_json::json;
    use std::ops::Deref;

    fn ok_response(data: serde_json::Value) -> Json<ApiResponse<serde_json::Value>> {
        Json(ApiResponse::success(data))
    }

    #[test]
    fn strict_timeout_off_preserves_found_false_as_success() {
        // Default (strictTimeout=false) — `{found:false}` must pass through
        // as HTTP 200 + `success:true` for back-compat.
        let inner = json!({"found": false, "durationMs": 1234, "lastObservedState": "hidden"});
        let result = apply_strict_timeout_reshape(Ok(ok_response(inner.clone())), false);
        let resp = result.expect("should be Ok when strict_timeout=false");
        let body = resp.deref();
        assert!(body.success, "envelope must remain success=true");
        assert_eq!(body.data.as_ref().unwrap(), &inner);
    }

    #[test]
    fn strict_timeout_on_rewrites_found_false_to_408() {
        // strictTimeout=true — `{found:false}` becomes HTTP 408 + error.
        let inner = json!({"found": false, "durationMs": 1500, "lastObservedState": "absent"});
        let result = apply_strict_timeout_reshape(Ok(ok_response(inner.clone())), true);
        let (status, Json(err)) =
            result.expect_err("found:false with strict_timeout=true must return Err");
        assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
        assert!(!err.success, "error envelope must have success=false");
        assert_eq!(
            err.error.as_deref(),
            Some("wait_for_element_timeout"),
            "canonical error code per B4 spec"
        );
        // `data` payload carries the original IPC fields so callers can read
        // durationMs / lastObservedState without parsing the error string.
        let data = err.data.expect("strict_timeout 408 must include data");
        assert_eq!(data.get("found").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(data.get("durationMs").and_then(|v| v.as_u64()), Some(1500));
        assert_eq!(
            data.get("lastObservedState").and_then(|v| v.as_str()),
            Some("absent")
        );
    }

    #[test]
    fn strict_timeout_on_passes_found_true_through() {
        // Even with strictTimeout=true, a successful match `{found:true}`
        // must return HTTP 200 unchanged.
        let inner = json!({"found": true, "durationMs": 42, "element": {"id": "abc"}});
        let result = apply_strict_timeout_reshape(Ok(ok_response(inner.clone())), true);
        let resp = result.expect("found:true must remain Ok regardless of strict_timeout");
        let body = resp.deref();
        assert!(body.success);
        assert_eq!(body.data.as_ref().unwrap(), &inner);
    }

    #[test]
    fn strict_timeout_on_passes_through_when_found_field_missing() {
        // Defensive: if the IPC payload doesn't carry `found` at all, treat
        // it as "not a timeout" — don't reshape.
        let inner = json!({"durationMs": 100});
        let result = apply_strict_timeout_reshape(Ok(ok_response(inner.clone())), true);
        let resp = result.expect("missing `found` must not trigger reshape");
        assert_eq!(resp.deref().data.as_ref().unwrap(), &inner);
    }

    #[test]
    fn err_branch_widens_apiresponse_type() {
        // wrap_ipc_result yields ApiResponse<()> on Err; the reshape helper
        // must widen it to ApiResponse<serde_json::Value> so the outer
        // handler signature lines up. Verifies the widening preserves error
        // string + status code.
        let result = apply_strict_timeout_reshape(
            Err((StatusCode::SERVICE_UNAVAILABLE, Json(api_error("boom")))),
            true,
        );
        let (status, Json(err)) = result.expect_err("Err must propagate as Err");
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.error.as_deref(), Some("boom"));
        assert!(err.data.is_none());
    }
}

/// F1 — `durationMs` / `elapsed_ms` dual-emit verification for the NL/id
/// happy-path response shape. The handler is too IPC-coupled for a pure
/// unit test, so these tests construct the response body shape directly,
/// mirroring the literals at the two happy-path emit sites. If those
/// literals drift (e.g., someone drops `elapsed_ms` early), the dual-emit
/// contract here will catch it because the tests assert on both keys.
#[cfg(test)]
mod wait_for_element_dual_emit_tests {
    use serde_json::json;

    /// Mirrors the no-assertions happy path response shape literal in
    /// `ui_bridge_wait_for_element_handler` (`elements.rs:~2046`). Update in
    /// lock-step if the literal there changes.
    fn nl_happy_response(elapsed_ms: u64, polls: u32) -> serde_json::Value {
        json!({
            "found": true,
            "element": {"id": "x"},
            "durationMs": elapsed_ms,
            "elapsed_ms": elapsed_ms,
            "polls": polls,
        })
    }

    /// Mirrors the with-assertions happy path response shape literal.
    fn nl_happy_with_assertions(elapsed_ms: u64, polls: u32, asserts: usize) -> serde_json::Value {
        json!({
            "found": true,
            "element": {"id": "x"},
            "durationMs": elapsed_ms,
            "elapsed_ms": elapsed_ms,
            "polls": polls,
            "assertions_passed": asserts,
        })
    }

    #[test]
    fn nl_happy_path_emits_both_duration_keys() {
        let body = nl_happy_response(987, 3);
        // Canonical key per F1 (camelCase, matches SDK + docs).
        assert_eq!(body.get("durationMs").and_then(|v| v.as_u64()), Some(987));
        // Legacy key — retained for one release per the migration plan.
        assert_eq!(body.get("elapsed_ms").and_then(|v| v.as_u64()), Some(987));
        assert_eq!(body.get("polls").and_then(|v| v.as_u64()), Some(3));
    }

    #[test]
    fn nl_happy_with_assertions_emits_both_duration_keys() {
        let body = nl_happy_with_assertions(1234, 5, 2);
        assert_eq!(body.get("durationMs").and_then(|v| v.as_u64()), Some(1234));
        assert_eq!(body.get("elapsed_ms").and_then(|v| v.as_u64()), Some(1234));
        assert_eq!(
            body.get("assertions_passed").and_then(|v| v.as_u64()),
            Some(2)
        );
    }

    /// F1 — the NL/id timeout path attaches `data: {durationMs, elapsed_ms,
    /// polls}` to the HTTP 408 envelope. Mirrors the literal in the timeout
    /// branch of `ui_bridge_wait_for_element_handler` (`elements.rs:~2102`).
    /// The `elapsed_ms` mirror is the Item D back-compat field; new callers
    /// must read `durationMs`.
    fn nl_timeout_data(elapsed_ms: u64, polls: u32) -> serde_json::Value {
        json!({
            "found": false,
            "durationMs": elapsed_ms,
            "elapsed_ms": elapsed_ms,
            "polls": polls,
        })
    }

    #[test]
    fn nl_timeout_data_payload_has_canonical_duration_ms_key() {
        let data = nl_timeout_data(5000, 25);
        assert_eq!(data.get("found").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(data.get("durationMs").and_then(|v| v.as_u64()), Some(5000));
        assert_eq!(data.get("polls").and_then(|v| v.as_u64()), Some(25));
    }

    /// Item D — the timeout-data payload exposes both `durationMs` (canonical)
    /// and `elapsed_ms` (deprecated mirror) so pre-F1 callers parsing the
    /// snake_case form keep working through the 2026-06 sunset window.
    #[test]
    fn nl_timeout_data_payload_dual_emits_elapsed_ms_for_backcompat() {
        let data = nl_timeout_data(1234, 5);
        assert_eq!(data.get("durationMs").and_then(|v| v.as_u64()), Some(1234));
        assert_eq!(data.get("elapsed_ms").and_then(|v| v.as_u64()), Some(1234));
    }
}

/// `type-into` — the two pure builders behind the resolve-then-`execute_action`
/// split, and the UTF-8 passthrough (Item C) that used to depend on a
/// hand-written JS string literal.
#[cfg(test)]
mod type_into_tests {
    use super::{type_into_action_payload, type_into_resolve_js};
    use serde_json::json;

    // ── Item C: UTF-8 passthrough ───────────────────────────────────────
    //
    // The text no longer crosses a JS string-literal boundary at ALL — it
    // travels as a field of the `execute_action` IPC payload, so the encoding
    // is serde's and the guarantee is structural rather than a mirrored
    // template. These assert the value survives byte-for-byte.

    /// The `text` a caller sent must arrive at the SDK unchanged — no lossy
    /// `?` substitution, no literal-termination failure.
    fn typed_text(text: &str) -> String {
        let payload = type_into_action_payload("#email", text, false);
        payload["action"]["params"]["text"]
            .as_str()
            .expect("text must be a JSON string")
            .to_string()
    }

    #[test]
    fn cyrillic_survives_the_ipc_payload() {
        let text = "\u{0442}\u{0435}\u{0441}\u{0442}";
        assert_eq!(typed_text(text), text);
        // Crucially: no '?' substitution anywhere in the serialized payload.
        let wire = type_into_action_payload("#email", text, false).to_string();
        assert!(!wire.contains('?'), "lossy downgrade in: {wire}");
    }

    #[test]
    fn newlines_and_line_separators_survive() {
        assert_eq!(typed_text("line1\nline2"), "line1\nline2");
        // U+2028 / U+2029 are legal in JSON but NOT in a JS string literal —
        // the exact pair the old template had to escape by hand.
        assert_eq!(typed_text("a\u{2028}b\u{2029}c"), "a\u{2028}b\u{2029}c");
    }

    #[test]
    fn emoji_with_zwj_survives() {
        // Family emoji = woman + ZWJ + woman + ZWJ + girl.
        let text = "\u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(typed_text(text), text);
    }

    #[test]
    fn embedded_quotes_and_backslashes_survive() {
        assert_eq!(typed_text(r#"she said "hi" \ ok"#), r#"she said "hi" \ ok"#);
    }

    // ── The action payload the frontend actually destructures ───────────

    #[test]
    fn action_payload_matches_the_execute_action_contract() {
        // `useControlEvents.ts` destructures `{ elementId, action }` — the
        // `{id, action, params}` spelling some older Rust call sites use
        // reaches that handler as `elementId: undefined`.
        let payload = type_into_action_payload("html > body > input", "hello", true);
        assert_eq!(
            payload,
            json!({
                "elementId": "html > body > input",
                "action": {
                    "action": "type",
                    "params": { "text": "hello", "clear": true },
                },
            })
        );
    }

    #[test]
    fn action_payload_defaults_to_append_when_not_clearing() {
        let payload = type_into_action_payload("#note", "more", false);
        assert_eq!(payload["action"]["params"]["clear"], json!(false));
    }

    // ── The resolution expression ───────────────────────────────────────

    #[test]
    fn resolve_js_never_mutates_the_element_value() {
        // The whole point of the split: this step reads and scrolls, it does
        // not type. Any value assignment here would be the duplicate coming
        // back.
        let js = type_into_resolve_js("document.querySelectorAll('input')", 0);
        assert!(
            !js.contains(".value ="),
            "resolution step must not assign a value: {js}"
        );
        assert!(
            !js.contains("dispatchEvent"),
            "resolution step must not dispatch events: {js}"
        );
        assert!(
            !js.contains("HTMLTextAreaElement.prototype"),
            "the dead prototype-selection fallback must be gone: {js}"
        );
    }

    #[test]
    fn resolve_js_reports_a_miss_rather_than_a_typed_success() {
        let js = type_into_resolve_js("document.querySelectorAll('input')", 3);
        assert!(js.contains("found: false"));
        assert!(js.contains("'No elements found'"));
        assert!(js.contains("'Index out of range'"));
        // `typed: true` is now the HANDLER's answer, only ever reached after
        // `execute_action` succeeded — the eval can no longer claim it.
        assert!(!js.contains("typed:"), "eval must not claim a typed verdict");
    }

    #[test]
    fn resolve_js_honours_the_requested_index_and_find_expression() {
        let js = type_into_resolve_js("document.querySelectorAll('textarea')", 2);
        assert!(js.contains("document.querySelectorAll('textarea')"));
        assert!(js.contains("const idx = 2;"));
    }

    #[test]
    fn resolve_js_trips_no_page_evaluate_blocklist_pattern() {
        // The expression runs through `page_evaluate`, which rejects a fixed
        // pattern set (`utils.ts::PAGE_EVALUATE_STRUCTURAL_PATTERNS` /
        // `..._NETWORK_PATTERNS`). A resolution step that cannot be evaluated
        // is a dead endpoint.
        let js = type_into_resolve_js("document.querySelectorAll('input')", 0);
        for banned in [
            "import(",
            "require(",
            "__proto__",
            "constructor[",
            "eval(",
            "new Function",
            "document.cookie",
            "window.open",
            "crypto.subtle",
            "fetch(",
            "XMLHttpRequest",
            "sendBeacon",
            "WebSocket",
        ] {
            assert!(!js.contains(banned), "blocklisted `{banned}` in: {js}");
        }
    }
}

/// F2 — direct `/ui-bridge/control/components` envelope shape (Direction B).
/// Verifies the response carries `data.components` AS THE ONLY components
/// field. Pure unit test against the literal shape the handler emits; an
/// axum-router-level integration test would need a fully constructed
/// `ApiState`.
///
/// Loop B Item B: the deprecated top-level `components` mirror was
/// removed. `components_envelope_omits_top_level_mirror` locks the
/// regression in — if a future refactor re-adds the duplicate it must
/// also delete this assertion, making the contract change visible in
/// review.
#[cfg(test)]
mod components_envelope_tests {
    use serde_json::json;

    /// Mirrors the literal at the bottom of `ui_bridge_get_components_handler`.
    /// Update in lock-step if the handler's `Ok(Json(serde_json::json!(...)))`
    /// payload changes.
    fn components_response(components: Vec<serde_json::Value>) -> serde_json::Value {
        let components_array = serde_json::Value::Array(components);
        json!({
            "success": true,
            "data": { "components": components_array },
        })
    }

    #[test]
    fn direct_components_response_has_direction_b_shape() {
        let body = components_response(vec![json!({"id": "comp-a"}), json!({"id": "comp-b"})]);
        assert_eq!(body.get("success"), Some(&json!(true)));
        // Direction B: `data` is `{components: [...]}` (object-shaped).
        let inner = body
            .get("data")
            .and_then(|d| d.get("components"))
            .and_then(|v| v.as_array())
            .expect("data.components must be an array");
        assert_eq!(inner.len(), 2);
        assert_eq!(inner[0].get("id").and_then(|v| v.as_str()), Some("comp-a"));
    }

    #[test]
    fn components_envelope_omits_top_level_mirror() {
        // Loop B Item B regression — the top-level `components` field
        // was a deprecated back-compat duplicate of `data.components`
        // that confused callers about which key was authoritative. It
        // has been dropped; `data.components` is the single source of
        // truth. Re-adding the mirror must require deleting this test,
        // which keeps the envelope-shape contract visible.
        let body = components_response(vec![json!({"id": "comp-a"})]);
        assert!(
            body.get("components").is_none(),
            "top-level `components` mirror must NOT appear in the response — \
             callers must read `data.components`. Found: {:?}",
            body.get("components")
        );
        // Canonical path remains intact.
        let inner = body
            .get("data")
            .and_then(|d| d.get("components"))
            .and_then(|v| v.as_array())
            .expect("data.components must still be present");
        assert_eq!(inner.len(), 1);
    }

    #[test]
    fn empty_components_still_has_canonical_field() {
        let body = components_response(vec![]);
        assert!(body
            .get("data")
            .and_then(|d| d.get("components"))
            .and_then(|v| v.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(false));
        assert!(
            body.get("components").is_none(),
            "regression guard: top-level `components` must stay omitted even \
             on the empty-list path"
        );
    }
}

// ============================================================================
// Phase A1: WS-vs-IPC dispatch tests for the component handlers.
//
// These exercise `try_ws_dispatch_for_app` and `ws_collect_components`
// directly because constructing a full `ApiState` for axum router-level
// integration tests is heavy. The handlers themselves are 5-line wrappers
// around these helpers plus an unchanged IPC fallback, so what matters for
// regression coverage is proving:
//
//   1. WS-transport apps: dispatch goes over the websocket and the relayed
//      response surfaces back unchanged.
//   2. ID-collision: when an HTTP-transport app and a WS-transport app share
//      a name (or no app exists), the WS branch is only taken for true
//      WS-transport entries — `try_ws_dispatch_for_app` returns `None` for
//      Http and unregistered ids, so handlers correctly fall through.
//   3. Fallback: when no entry exists, the helper returns `None` so the
//      handler's IPC path runs.
//
// These three guarantees together prove the WS-vs-IPC selection rule that
// Phase A1 introduces.
// ============================================================================
#[cfg(test)]
mod ws_dispatch_selection_tests {
    use super::{try_ws_dispatch_for_app, ws_collect_components};
    use crate::mcp::app_discovery::DiscoveredApp;
    use crate::mcp::app_dispatch::AppDispatcher;
    use crate::mcp::app_registry::{AppRegistry, AppTransport};
    use crate::mcp::command_relay::{CommandRelay, CommandResponse};
    use crate::mcp::ws_relay::WsConnectionManager;
    use reqwest::Method;
    use serde_json::json;
    use std::time::Duration;

    fn sample_app(app_id: &str) -> DiscoveredApp {
        DiscoveredApp {
            app_id: app_id.to_string(),
            app_name: app_id.to_string(),
            app_type: "web".into(),
            framework: None,
            url: "http://unused".into(),
            port: 0,
            base_path: "".into(),
            version: None,
            capabilities: vec![],
            element_count: None,
            component_count: None,
            discovered_at: 0,
        }
    }

    /// Test 1 — WS-vs-IPC dispatch: register a WS-transport wrapper, call the
    /// helper that the action handler uses, and prove the WS path was taken
    /// (a frame appears on the relay's outbound channel, the wrapper resolves
    /// it, and the helper returns the resolved value as `Some(Ok(_))`).
    #[tokio::test]
    async fn ws_dispatch_for_app_routes_websocket_traffic() {
        let registry = AppRegistry::new();
        let ws = WsConnectionManager::new();
        let (_conn_id, mut outbound_rx) = ws.test_register("test-wrapper").await;
        registry
            .upsert(
                sample_app("test-wrapper"),
                None,
                AppTransport::Websocket,
                Some(7),
                None,
            )
            .await;
        let relay = CommandRelay::with_timeout(ws.clone(), Duration::from_secs(2));
        let dispatcher = AppDispatcher::new(registry.clone(), relay.clone());

        // Spawn the dispatcher call so we can play wrapper on the other end.
        let registry_handle = registry.clone();
        let dispatcher_handle = dispatcher.clone();
        let dispatch_fut = tokio::spawn(async move {
            try_ws_dispatch_for_app(
                &registry_handle,
                &dispatcher_handle,
                "test-wrapper",
                "executeComponentAction",
                Method::POST,
                "/ui-bridge/control/component/test-wrapper/action/echo",
                json!({
                    "componentId": "test-wrapper",
                    "actionId": "echo",
                    "params": { "input": "hi" }
                }),
            )
            .await
        });

        // Pull the outbound frame the dispatcher pushed to the wrapper.
        let frame = outbound_rx.recv().await.expect("frame must arrive");
        let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
        let command_id = v["commandId"].as_str().unwrap().to_string();
        assert_eq!(v["action"], "executeComponentAction");
        assert_eq!(v["payload"]["actionId"], "echo");
        assert_eq!(v["payload"]["params"]["input"], "hi");

        // Resolve as the wrapper would.
        relay
            .resolve(CommandResponse {
                command_id,
                success: true,
                result: Some(json!({ "output": "hi" })),
                error: None,
            })
            .await;

        let outcome = dispatch_fut.await.unwrap();
        match outcome {
            Some(Ok(value)) => assert_eq!(value, json!({ "output": "hi" })),
            other => panic!("expected Some(Ok(...)) from WS dispatch, got: {:?}", other),
        }
    }

    /// Test 2 — ID collision: register a WS-transport wrapper named
    /// `terminal` (a name that collides in spirit with a runner-internal
    /// React component id). The helper used by the action handler must take
    /// the WS path. Then verify that an HTTP-transport entry with the same
    /// id triggers `None` (so the IPC fallback in the handler would run) —
    /// proving the read paths the runner-internal `terminal` component owns
    /// are not silently rerouted to a non-existent WS connection.
    #[tokio::test]
    async fn id_collision_ws_wins_for_action_endpoint_only() {
        let registry = AppRegistry::new();
        let ws = WsConnectionManager::new();
        let (_conn_id, mut outbound_rx) = ws.test_register("terminal").await;
        registry
            .upsert(
                sample_app("terminal"),
                None,
                AppTransport::Websocket,
                Some(99),
                None,
            )
            .await;
        let relay = CommandRelay::with_timeout(ws.clone(), Duration::from_secs(2));
        let dispatcher = AppDispatcher::new(registry.clone(), relay.clone());

        // Action handler path → must take WS branch.
        let r1 = registry.clone();
        let d1 = dispatcher.clone();
        let action_fut = tokio::spawn(async move {
            try_ws_dispatch_for_app(
                &r1,
                &d1,
                "terminal",
                "executeComponentAction",
                Method::POST,
                "/ui-bridge/control/component/terminal/action/run",
                json!({ "componentId": "terminal", "actionId": "run", "params": {} }),
            )
            .await
        });
        let frame = outbound_rx.recv().await.expect("WS frame must arrive");
        let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
        let command_id = v["commandId"].as_str().unwrap().to_string();
        relay
            .resolve(CommandResponse {
                command_id,
                success: true,
                result: Some(json!({ "ran": true })),
                error: None,
            })
            .await;
        match action_fut.await.unwrap() {
            Some(Ok(value)) => assert_eq!(value, json!({ "ran": true })),
            other => panic!("expected WS to win on action endpoint, got: {:?}", other),
        }

        // Now overwrite registration with HTTP transport for the same id.
        // Helpers MUST return `None` so the action handler would fall
        // through to its IPC path — proving the WS-wins rule applies only
        // to true WS-transport registrations, not to any registered id.
        registry
            .upsert(sample_app("terminal"), None, AppTransport::Http, None, None)
            .await;
        let none_outcome = try_ws_dispatch_for_app(
            &registry,
            &dispatcher,
            "terminal",
            "executeComponentAction",
            Method::POST,
            "/ui-bridge/control/component/terminal/action/run",
            json!({}),
        )
        .await;
        assert!(
            none_outcome.is_none(),
            "HTTP-transport entry must NOT take the WS branch; got: {:?}",
            none_outcome
        );
    }

    /// Test 3 — Fallback: with no entry in `AppRegistry`, the helper returns
    /// `None` and the handler's IPC path takes over. We assert no frame
    /// reaches the relay's outbound channel (would be visible as a
    /// `try_recv()` Ok, since WS dispatch never fires).
    #[tokio::test]
    async fn no_registration_falls_back_to_ipc() {
        let registry = AppRegistry::new();
        let ws = WsConnectionManager::new();
        // We register a WS connection for a DIFFERENT app to prove the
        // helper consults the id, not just any-WS-app-is-registered.
        let (_conn_id, mut outbound_rx) = ws.test_register("other-app").await;
        registry
            .upsert(
                sample_app("other-app"),
                None,
                AppTransport::Websocket,
                Some(13),
                None,
            )
            .await;
        let relay = CommandRelay::with_timeout(ws.clone(), Duration::from_secs(2));
        let dispatcher = AppDispatcher::new(registry.clone(), relay);

        let outcome = try_ws_dispatch_for_app(
            &registry,
            &dispatcher,
            "unregistered-id",
            "executeComponentAction",
            Method::POST,
            "/ui-bridge/control/component/unregistered-id/action/x",
            json!({}),
        )
        .await;
        assert!(
            outcome.is_none(),
            "unregistered id must return None so IPC fallback runs"
        );
        assert!(
            outbound_rx.try_recv().is_err(),
            "no WS frame must have been dispatched"
        );
    }

    /// Test 4 — `ws_collect_components` merges WS app responses while
    /// degrading gracefully: a non-WS app contributes nothing, and a wrapper
    /// returning a bare array or a `{components: [...]}` object is
    /// unwrapped and concatenated.
    #[tokio::test]
    async fn ws_collect_components_merges_live_wrappers() {
        let registry = AppRegistry::new();
        let ws = WsConnectionManager::new();
        let (_a_conn, mut a_rx) = ws.test_register("alpha").await;
        let (_b_conn, mut b_rx) = ws.test_register("beta").await;
        registry
            .upsert(
                sample_app("alpha"),
                None,
                AppTransport::Websocket,
                Some(1),
                None,
            )
            .await;
        registry
            .upsert(
                sample_app("beta"),
                None,
                AppTransport::Websocket,
                Some(2),
                None,
            )
            .await;
        // An HTTP-transport entry should be ignored entirely by
        // `ws_collect_components`.
        registry
            .upsert(
                sample_app("legacy-http"),
                None,
                AppTransport::Http,
                None,
                None,
            )
            .await;

        let relay = CommandRelay::with_timeout(ws.clone(), Duration::from_secs(2));
        let dispatcher = AppDispatcher::new(registry.clone(), relay.clone());

        let registry_h = registry.clone();
        let dispatcher_h = dispatcher.clone();
        let collect_fut =
            tokio::spawn(async move { ws_collect_components(&registry_h, &dispatcher_h).await });

        // `ws_collect_components` dispatches to each WS app SEQUENTIALLY
        // (it awaits each dispatch before moving to the next app), and
        // HashMap iteration order is unspecified. So we cannot assume the
        // dispatcher will hit alpha before beta — instead, we race both
        // receivers and resolve whichever arrives, twice. The wrapper-name
        // determines the response payload shape so each is exercised.
        let relay_for_resolve = relay.clone();
        let resolve_one = |frame: String, app_id: &str| {
            let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
            let cmd = v["commandId"].as_str().unwrap().to_string();
            let result = if app_id == "alpha" {
                // Bare array shape from the frame's payload context.
                json!([{ "id": "alpha-1" }, { "id": "alpha-2" }])
            } else {
                // `{components: [...]}` shape.
                json!({ "components": [{ "id": "beta-1" }] })
            };
            let relay_h = relay_for_resolve.clone();
            async move {
                relay_h
                    .resolve(CommandResponse {
                        command_id: cmd,
                        success: true,
                        result: Some(result),
                        error: None,
                    })
                    .await;
            }
        };

        // Resolve frames in the order they're produced, regardless of
        // which receiver fires first.
        for _ in 0..2 {
            tokio::select! {
                Some(frame) = a_rx.recv() => resolve_one(frame, "alpha").await,
                Some(frame) = b_rx.recv() => resolve_one(frame, "beta").await,
            }
        }

        let merged = collect_fut.await.unwrap();
        // Order between alpha and beta is unspecified; assert membership.
        let ids: std::collections::BTreeSet<String> = merged
            .iter()
            .filter_map(|v| v.get("id").and_then(|s| s.as_str()).map(String::from))
            .collect();
        let expected: std::collections::BTreeSet<String> = ["alpha-1", "alpha-2", "beta-1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(ids, expected, "merged ids must contain every WS component");
    }
}

#[cfg(test)]
mod validate_action_name_tests {
    //! Validation seam for the invalid-action gate that lives at the top of
    //! `ui_bridge_execute_action_handler`. Exercises the pure helper so we
    //! don't need an axum router / live IPC harness — matches the precedent
    //! set by `validate_wait_for_element_state_request` /
    //! `validate_tab_id`. Locks down both the accept-list (every known
    //! action round-trips) and the reject-shape (`INVALID_REQUEST` code +
    //! `supportedActions` context payload) so a future rename can't
    //! silently let `press` through to the RecoveryExecutor again.
    use super::{validate_action_name, SUPPORTED_ACTION_NAMES};
    use crate::mcp::ui_bridge::types::UiBridgeErrorCode;

    #[test]
    fn every_supported_action_is_accepted() {
        for name in SUPPORTED_ACTION_NAMES {
            let got = validate_action_name(name)
                .unwrap_or_else(|_| panic!("expected {name} to validate"));
            assert_eq!(got, *name, "validator must return the canonical name");
        }
    }

    #[test]
    fn canonical_click_round_trips() {
        // Sanity check the click path explicitly — it's the dominant call
        // site and the one most likely to regress under refactor.
        assert_eq!(validate_action_name("click").unwrap(), "click");
    }

    #[test]
    fn unknown_action_is_rejected_with_invalid_request_code() {
        let err = validate_action_name("press").expect_err("press must not be accepted");
        assert!(
            matches!(err.code, UiBridgeErrorCode::InvalidRequest),
            "expected INVALID_REQUEST code, got {:?}",
            err.code
        );
        assert!(
            err.message.contains("press"),
            "message must surface the rejected name, got: {}",
            err.message
        );
    }

    #[test]
    fn unknown_action_carries_supported_list_in_context() {
        let err = validate_action_name("slap").expect_err("slap must not be accepted");
        let ctx = err.context.as_ref().expect("context must be present");
        let supported = ctx
            .get("supportedActions")
            .and_then(|v| v.as_array())
            .expect("supportedActions must be an array");
        let names: Vec<&str> = supported.iter().filter_map(|v| v.as_str()).collect();
        // Spot-check a couple of canonical names — full equality with
        // SUPPORTED_ACTION_NAMES is locked down by
        // `every_supported_action_is_accepted` above.
        assert!(names.contains(&"click"), "supportedActions must list click");
        assert!(names.contains(&"type"), "supportedActions must list type");
        assert!(
            names.contains(&"scrollIntoView"),
            "supportedActions must list scrollIntoView"
        );
        // And the offending name is echoed back so callers don't have to
        // remember what they sent.
        assert_eq!(ctx.get("action").and_then(|v| v.as_str()), Some("slap"));
    }

    #[test]
    fn empty_action_is_rejected() {
        let err = validate_action_name("").expect_err("empty action must be rejected");
        assert!(matches!(err.code, UiBridgeErrorCode::InvalidRequest));
    }

    #[test]
    fn whitespace_only_action_is_rejected() {
        // Trim-then-check semantics: a body whose `action` field is `"   "`
        // is the same misuse as an empty string.
        let err = validate_action_name("   ").expect_err("whitespace-only action must be rejected");
        assert!(matches!(err.code, UiBridgeErrorCode::InvalidRequest));
    }

    #[test]
    fn case_sensitive_match_is_enforced() {
        // `Click` (capital C) is NOT in the SDK vocabulary; rejecting it
        // here matches what the wrapper SDK's relay-handlers.ts would do
        // and prevents downstream "no handler" silent failures.
        let err = validate_action_name("Click").expect_err("Click must not be accepted");
        assert!(matches!(err.code, UiBridgeErrorCode::InvalidRequest));
    }
}

#[cfg(test)]
mod element_resolution_tests {
    //! A MISSING element and a FAILING action are different answers.
    //!
    //! `POST /control/element/{id}/action` resolves the element before it
    //! validates the action name. `is_element_miss_reply` is the predicate
    //! that decides which of the two a `get_element` reply is, and it has to
    //! be right in BOTH directions:
    //!
    //! * a bogus id must answer `ELEMENT_NOT_FOUND` / 404 even when the
    //!   action name is a custom one the runner cannot know about, and
    //! * a real element whose action genuinely fails must NOT be relabelled
    //!   as missing — a gate that answers `ELEMENT_NOT_FOUND` for everything
    //!   is a different bug, not a fix.
    //!
    //! The trap the predicate exists for: the frontend reports a registry
    //! miss through a SUCCESSFUL IPC (`{success:false, error:""}`), so
    //! matching on the transport `Err(_)` never sees it.
    use super::{api_error_detailed, is_element_miss_reply, UiBridgeError};
    use serde_json::json;

    #[test]
    fn the_empty_error_miss_shape_is_recognised() {
        // The exact reply the frontend's `get_element` handler sends for an
        // id that was never registered: transport OK, `error` empty.
        assert!(is_element_miss_reply(
            &json!({ "success": false, "error": "" })
        ));
        assert!(is_element_miss_reply(&json!({ "success": false })));
    }

    #[test]
    fn the_prose_miss_shapes_are_recognised() {
        for prose in [
            "Element not found",
            "No element found matching criteria bogus",
            "element bogus was never registered or discovered",
        ] {
            assert!(
                is_element_miss_reply(&json!({ "success": false, "error": prose })),
                "{prose:?} must read as a registry miss"
            );
        }
    }

    #[test]
    fn a_real_element_reply_is_not_a_miss() {
        // A live element carries no `success` field at all — it IS the data.
        let elem = json!({
            "id": "terminal-input-abc",
            "element_type": "TEXTAREA",
            "actions": ["focus", "blur"],
            "customActions": ["writeToTerminal", "sendKeys"],
        });
        assert!(!is_element_miss_reply(&elem));
    }

    /// The discriminator that keeps this from becoming "always 404".
    ///
    /// A present element whose action failed for its own reason reports
    /// `success:false` too — with a typed prose the caller needs. Reading
    /// that as a registry miss would destroy `TERMINAL_EXITED` on the way to
    /// the wire and send the caller hunting for an element that is right
    /// there.
    #[test]
    fn a_genuine_action_failure_is_not_a_miss() {
        for prose in [
            "TERMINAL_EXITED: the pane's process is gone",
            "clipboard read was denied",
            "writeToTerminal: 'text' is required",
        ] {
            assert!(
                !is_element_miss_reply(&json!({ "success": false, "error": prose })),
                "{prose:?} is an action failure, not a missing element"
            );
        }
    }

    #[test]
    fn the_miss_envelope_carries_the_canonical_code_and_id() {
        let detail = UiBridgeError::element_not_found_by_id("bogus");
        let message = detail.message.clone();
        let body = api_error_detailed(message, detail);
        let detail = body.error_detail.expect("error_detail must be populated");
        assert_eq!(
            serde_json::to_value(&detail.code).unwrap(),
            json!("ELEMENT_NOT_FOUND")
        );
        assert_eq!(
            detail
                .context
                .as_ref()
                .and_then(|c| c.get("elementId"))
                .and_then(|v| v.as_str()),
            Some("bogus"),
            "the rejected id must come back so the caller can self-correct"
        );
    }
}

#[cfg(test)]
mod action_not_supported_tests {
    //! Which failures are "not supported" and which are genuine FAILURES.
    //!
    //! `setValue` against an element that doesn't advertise it is rejected
    //! PRE-IPC with `ACTION_NOT_SUPPORTED` and the advertised list. A failure
    //! that arrives POST-dispatch is the opposite case: the action was
    //! supported, it ran, and it failed — so it keeps its own typed error and
    //! is coded `ACTION_FAILED`. This module exercises the pure helpers
    //! (`extract_supported_actions`, `is_action_advertised`,
    //! `recovery_is_genuine`, `as_action_failure`) plus the pre-IPC envelope
    //! shape. The handler itself is too IPC-coupled to spin up in a unit test
    //! (it needs `ApiState`, the IPC dispatcher, the recovery executor's PG
    //! telemetry sink), so the pre-IPC envelope is locked down by
    //! reconstructing it from the same `serde_json::json!{...}` recipe the
    //! handler uses — if the body template changes there, this test must
    //! change in lockstep, which is exactly the regression guard we want.
    use super::{
        advertise_custom_actions_in_payload, api_error_detailed, as_action_failure,
        classify_transport_error, extract_custom_actions, extract_supported_actions,
        fold_custom_actions_into_element, is_action_advertised, recovery_is_genuine, ActionResult,
        Json, StatusCode, UiBridgeError,
    };
    use crate::mcp::types::ApiResponse;
    use crate::mcp::ui_bridge::recovery_executor::{RecoveryOutcome, RecoveryVia};
    use serde_json::json;

    #[test]
    fn extract_supported_actions_from_string_array() {
        let elem = json!({
            "id": "btn-1",
            "actions": ["focus", "blur", "type", "clear", "click"],
        });
        let got = extract_supported_actions(&elem).expect("present array");
        assert_eq!(got, vec!["focus", "blur", "type", "clear", "click"]);
    }

    #[test]
    fn extract_supported_actions_from_object_array() {
        // Some snapshot payloads carry per-action objects, not bare strings.
        let elem = json!({
            "id": "btn-1",
            "actions": [
                { "name": "focus" },
                { "action": "click", "params": [] },
            ],
        });
        let got = extract_supported_actions(&elem).expect("present array");
        assert_eq!(got, vec!["focus", "click"]);
    }

    #[test]
    fn extract_supported_actions_absent_is_none() {
        let elem = json!({ "id": "btn-1" });
        assert!(extract_supported_actions(&elem).is_none());
    }

    #[test]
    fn extract_supported_actions_non_array_is_none() {
        let elem = json!({ "id": "btn-1", "actions": "click" });
        assert!(extract_supported_actions(&elem).is_none());
    }

    #[test]
    fn is_action_advertised_matches_listed_action() {
        let supported = Some(vec![
            "focus".to_string(),
            "blur".to_string(),
            "type".to_string(),
            "clear".to_string(),
            "click".to_string(),
        ]);
        assert!(is_action_advertised("click", &supported));
        assert!(is_action_advertised("type", &supported));
    }

    #[test]
    fn is_action_advertised_rejects_unlisted_action() {
        let supported = Some(vec![
            "focus".to_string(),
            "blur".to_string(),
            "type".to_string(),
            "clear".to_string(),
            "click".to_string(),
        ]);
        assert!(!is_action_advertised("setValue", &supported));
        assert!(!is_action_advertised("submit", &supported));
    }

    #[test]
    fn is_action_advertised_unknown_supported_list_is_permissive() {
        // None → we don't know what the element supports; never accuse.
        assert!(is_action_advertised("setValue", &None));
    }

    // ── Custom-action reachability via the /control/ route ──────────────
    // The control route used to 400 "Unknown action" for app-registered
    // custom actions (pasteText/getScrollback/sendKeys on terminal-input)
    // even though the SDK route + frontend dispatcher accept them. These
    // tests lock down the two pure helpers that make the control route now
    // reach + advertise custom actions.

    #[test]
    fn extract_custom_actions_from_string_array() {
        // The SDK serializes `customActions` as a bare string array
        // (`Object.keys(el.customActions)`), distinct from `actions`.
        let elem = json!({
            "id": "terminal-input-abc",
            "actions": ["focus", "blur"],
            "customActions": ["sendKeys", "pasteText", "getScrollback"],
        });
        let got = extract_custom_actions(&elem).expect("present array");
        assert_eq!(got, vec!["sendKeys", "pasteText", "getScrollback"]);
    }

    #[test]
    fn extract_custom_actions_from_object_array() {
        // Accept `{ name | action | id }` objects for parity with
        // extract_supported_actions.
        let elem = json!({
            "id": "terminal-input-abc",
            "customActions": [
                { "id": "pasteText" },
                { "name": "getScrollback" },
                { "action": "sendKeys" },
            ],
        });
        let got = extract_custom_actions(&elem).expect("present array");
        assert_eq!(got, vec!["pasteText", "getScrollback", "sendKeys"]);
    }

    #[test]
    fn extract_custom_actions_absent_is_none() {
        let elem = json!({ "id": "btn-1", "actions": ["click"] });
        assert!(extract_custom_actions(&elem).is_none());
    }

    #[test]
    fn extract_custom_actions_non_array_is_none() {
        let elem = json!({ "id": "btn-1", "customActions": "pasteText" });
        assert!(extract_custom_actions(&elem).is_none());
    }

    #[test]
    fn custom_action_advertised_once_folded_into_supported_list() {
        // Reproduce the handler's fold: built-in `actions` + `customActions`
        // → one supported list. `is_action_advertised` must then ACCEPT a
        // registered custom action (pasteText) the same way it accepts a
        // built-in, while still REJECTING a genuinely unknown action.
        let elem = json!({
            "id": "terminal-input-abc",
            "actions": ["focus", "blur"],
            "customActions": ["sendKeys", "pasteText", "getScrollback"],
        });
        let mut builtin = extract_supported_actions(&elem).expect("actions present");
        builtin.extend(extract_custom_actions(&elem).expect("customActions present"));
        let supported = Some(builtin);

        // Built-in still accepted.
        assert!(is_action_advertised("focus", &supported));
        // Custom actions now accepted (the bug: these were 400'd before).
        assert!(is_action_advertised("pasteText", &supported));
        assert!(is_action_advertised("getScrollback", &supported));
        assert!(is_action_advertised("sendKeys", &supported));
        // A truly unknown action is still rejected.
        assert!(!is_action_advertised("teleport", &supported));
    }

    // ── Discover/snapshot serialization fold (the advertisement gap) ────
    // These exercise the ACTUAL response-serialization path the discover and
    // snapshot handlers run on the IPC payload — not just the extract_*
    // helpers — proving a registered custom action now appears in the
    // advertised `actions` array a discover consumer reads.

    #[test]
    fn fold_appends_custom_actions_into_element_actions() {
        let mut elem = json!({
            "id": "terminal-input-abc",
            "actions": ["focus", "blur"],
            "customActions": ["pasteText", "getScrollback"],
        });
        fold_custom_actions_into_element(&mut elem);
        let actions = extract_supported_actions(&elem).expect("actions present");
        // Built-ins preserved, custom actions now ALSO advertised in `actions`.
        assert!(actions.contains(&"focus".to_string()));
        assert!(actions.contains(&"blur".to_string()));
        assert!(
            actions.contains(&"pasteText".to_string()),
            "pasteText must now appear in advertised actions: {actions:?}"
        );
        assert!(
            actions.contains(&"getScrollback".to_string()),
            "getScrollback must now appear in advertised actions: {actions:?}"
        );
        // The dedicated `customActions` field is left intact (additive).
        assert!(elem
            .get("customActions")
            .and_then(|v| v.as_array())
            .is_some());
    }

    #[test]
    fn fold_does_not_duplicate_already_listed_action() {
        // If the SDK already enumerated a name in `actions`, folding must not
        // produce a duplicate.
        let mut elem = json!({
            "id": "x",
            "actions": ["focus", "pasteText"],
            "customActions": ["pasteText", "getScrollback"],
        });
        fold_custom_actions_into_element(&mut elem);
        let actions = extract_supported_actions(&elem).expect("actions present");
        let paste_count = actions.iter().filter(|a| *a == "pasteText").count();
        assert_eq!(
            paste_count, 1,
            "pasteText must not be duplicated: {actions:?}"
        );
    }

    #[test]
    fn fold_synthesizes_actions_when_only_custom_present() {
        // Element with NO built-in `actions` array but custom actions present:
        // create an `actions` list from the custom names so it advertises them.
        let mut elem = json!({
            "id": "x",
            "customActions": ["pasteText"],
        });
        fold_custom_actions_into_element(&mut elem);
        let actions = extract_supported_actions(&elem).expect("actions synthesized");
        assert_eq!(actions, vec!["pasteText".to_string()]);
    }

    #[test]
    fn fold_leaves_element_without_custom_actions_unchanged() {
        let mut elem = json!({ "id": "btn", "actions": ["click", "focus"] });
        let before = elem.clone();
        fold_custom_actions_into_element(&mut elem);
        assert_eq!(elem, before);
    }

    #[test]
    fn advertise_custom_actions_walks_elements_array_shape() {
        // The discover/snapshot `{ elements: [...] }` payload shape — the one
        // the handlers actually receive from the SDK. Prove the terminal-input
        // element advertises pasteText/getScrollback after the fold.
        let mut payload = json!({
            "elements": [
                {
                    "id": "terminal-input-term-1",
                    "type": "input",
                    "actions": ["focus", "blur"],
                    "customActions": ["sendKeys", "pasteText", "getScrollback"],
                },
                { "id": "btn-save", "actions": ["click", "focus", "blur"] },
            ],
            "count": 2,
        });
        advertise_custom_actions_in_payload(&mut payload);

        let elems = payload["elements"].as_array().expect("elements array");
        let term = &elems[0];
        let term_actions = extract_supported_actions(term).expect("actions present");
        assert!(
            term_actions.contains(&"pasteText".to_string()),
            "{term_actions:?}"
        );
        assert!(
            term_actions.contains(&"getScrollback".to_string()),
            "{term_actions:?}"
        );
        assert!(
            term_actions.contains(&"sendKeys".to_string()),
            "{term_actions:?}"
        );
        assert!(
            term_actions.contains(&"focus".to_string()),
            "{term_actions:?}"
        );

        // A plain button without custom actions is untouched.
        let btn_actions = extract_supported_actions(&elems[1]).expect("actions present");
        assert_eq!(
            btn_actions,
            vec!["click".to_string(), "focus".to_string(), "blur".to_string()]
        );
    }

    #[test]
    fn advertise_custom_actions_walks_nested_data_elements_shape() {
        // The `{ data: { elements: [...] } }` layout (one of the three the
        // discover cache-count helper recognizes).
        let mut payload = json!({
            "data": { "elements": [
                {
                    "id": "terminal-input-term-1",
                    "actions": ["focus"],
                    "customActions": ["pasteText"],
                },
            ]},
        });
        advertise_custom_actions_in_payload(&mut payload);
        let term = &payload["data"]["elements"][0];
        let actions = extract_supported_actions(term).expect("actions present");
        assert!(actions.contains(&"pasteText".to_string()), "{actions:?}");
    }

    #[test]
    fn advertise_custom_actions_is_idempotent() {
        let mut payload = json!({
            "elements": [
                {
                    "id": "terminal-input-term-1",
                    "actions": ["focus", "blur"],
                    "customActions": ["pasteText", "getScrollback"],
                },
            ],
        });
        advertise_custom_actions_in_payload(&mut payload);
        let once = payload.clone();
        advertise_custom_actions_in_payload(&mut payload);
        assert_eq!(payload, once, "fold must be idempotent");
    }

    /// A recovery counts only when it chose a command AND the original action
    /// then succeeded. `recovered:true, commandChosen:null` — the shape the
    /// LLM fallback produced — is an assertion about nothing.
    #[test]
    fn only_a_command_backed_recovery_counts_as_recovered() {
        let mk = |recovered: bool, cmd: Option<&str>| RecoveryOutcome {
            recovered,
            via: RecoveryVia::LlmFallback,
            command_chosen: cmd.map(str::to_string),
            error_code: None,
            result: None,
        };
        assert!(recovery_is_genuine(&mk(true, Some("scroll_into_view"))));
        assert!(
            !recovery_is_genuine(&mk(true, None)),
            "recovered with no command chosen is not a recovery"
        );
        assert!(!recovery_is_genuine(&mk(false, Some("scroll_into_view"))));
        assert!(!recovery_is_genuine(&mk(false, None)));
    }

    /// THE iteration-9 defect: a genuine post-dispatch FAILURE was relabelled
    /// `ACTION_NOT_SUPPORTED`, which both contradicted the runner's own
    /// advertised action list (`writeToTerminal` IS supported by a terminal
    /// element — its pty had exited) and destroyed the typed error, so
    /// `TERMINAL_EXITED` existed at the handler and was unobservable on the
    /// wire. The failure must reach the caller verbatim, coded `ACTION_FAILED`.
    #[test]
    fn post_dispatch_failure_keeps_its_typed_error_and_codes_action_failed() {
        let msg = "TERMINAL_EXITED: terminal term-3 is not writable — its process \
                   exited with code 0. Restart the session before writing to it.";
        let failure: ActionResult = Err((
            StatusCode::BAD_REQUEST,
            Json(api_error_detailed(msg, classify_transport_error(msg))),
        ));
        let recovery = serde_json::to_value(RecoveryOutcome {
            recovered: false,
            via: RecoveryVia::None,
            command_chosen: None,
            error_code: None,
            result: None,
        })
        .unwrap();

        let (status, Json(body)) = as_action_failure(
            failure,
            "writeToTerminal",
            "terminal-input-term-3",
            recovery,
        )
        .expect_err("a failure stays a failure");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let wire = serde_json::to_value(&body).unwrap();
        let text = wire.to_string();
        assert!(
            text.contains("TERMINAL_EXITED"),
            "the handler's typed error must survive to the wire: {text}"
        );
        assert!(
            !text.contains("ACTION_NOT_SUPPORTED"),
            "a dispatched action that failed is NOT unsupported: {text}"
        );
        assert_eq!(
            wire.pointer("/error_detail/code").and_then(|v| v.as_str()),
            Some("ACTION_FAILED"),
            "the existing ACTION_FAILED variant is what an action failure means"
        );
        assert_eq!(
            wire.pointer("/error_detail/context/requested_action")
                .and_then(|v| v.as_str()),
            Some("writeToTerminal")
        );
        assert!(
            wire.pointer("/error_detail/context/recovery").is_some(),
            "the recovery audit trail is still surfaced: {text}"
        );
    }

    /// A message the classifier DID recognise keeps its more specific code —
    /// re-coding everything to `ACTION_FAILED` would be a different bug.
    #[test]
    fn a_classified_failure_keeps_its_specific_code() {
        let msg = "No element found matching '#ghost'";
        let failure: ActionResult = Err((
            StatusCode::BAD_REQUEST,
            Json(api_error_detailed(msg, classify_transport_error(msg))),
        ));
        let (_, Json(body)) =
            as_action_failure(failure, "click", "ghost", serde_json::Value::Null).unwrap_err();
        let wire = serde_json::to_value(&body).unwrap();
        assert_eq!(
            wire.pointer("/error_detail/code").and_then(|v| v.as_str()),
            Some("ELEMENT_NOT_FOUND")
        );
        assert!(wire.pointer("/error_detail/context/recovery").is_some());
    }

    /// A transport failure (the frontend never answered) is not an action
    /// outcome, so it is left exactly as-is.
    #[test]
    fn transport_failures_are_not_relabelled_as_action_failures() {
        let msg = "UI Bridge frontend did not become ready";
        let failure: ActionResult = Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error_detailed(msg, classify_transport_error(msg))),
        ));
        let (status, Json(body)) =
            as_action_failure(failure, "click", "btn-1", serde_json::Value::Null).unwrap_err();
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        let wire = serde_json::to_value(&body).unwrap();
        assert_eq!(
            wire.pointer("/error_detail/code").and_then(|v| v.as_str()),
            Some("FRONTEND_NOT_READY"),
            "a transport error keeps its transport code"
        );
    }

    /// A successful action is never touched by the failure re-coder.
    #[test]
    fn a_success_passes_through_untouched() {
        let ok: ActionResult = Ok(Json(ApiResponse::success(json!({ "clicked": true }))));
        let out = as_action_failure(ok, "click", "btn-1", json!({ "recovered": false })).unwrap();
        let wire = serde_json::to_value(&out.0).unwrap();
        assert_eq!(wire.get("success").and_then(|v| v.as_bool()), Some(true));
        assert!(!wire.to_string().contains("ACTION_FAILED"));
    }

    /// Iter-2 item 2 — pre-IPC gate envelope. When the element advertises
    /// `actions: [...]` and the requested action is not in that list, the
    /// handler short-circuits BEFORE IPC / RecoveryExecutor / LLM fallback
    /// and returns HTTP 400 with `error_detail.code = "ACTION_NOT_SUPPORTED"`
    /// and `error_detail.context.supported_actions` carrying the element's
    /// advertised list.
    #[test]
    fn pre_ipc_action_not_supported_envelope_shape() {
        let action_name = "scrollIntoView";
        let supported = vec!["click".to_string(), "focus".to_string()];
        let detail = UiBridgeError {
            code: crate::mcp::ui_bridge::types::UiBridgeErrorCode::ActionNotSupported,
            message: format!(
                "Action '{}' is not supported by element 'btn-1'. \
                 Element advertises: {:?}",
                action_name, supported
            ),
            recovery: Some(crate::mcp::ui_bridge::types::RecoveryHint::Unrecoverable),
            context: Some(json!({
                "code": "ACTION_NOT_SUPPORTED",
                "supported_actions": supported,
                "requested_action": action_name,
            })),
        };
        let envelope = ApiResponse::<()> {
            success: false,
            data: None,
            error: Some(detail.message.clone()),
            error_detail: Some(detail),
            hint: None,
            code: None,
            suggestions: None,
        };
        let env_json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(
            env_json.get("success").and_then(|v| v.as_bool()),
            Some(false)
        );
        let ed = env_json
            .get("error_detail")
            .expect("contract: error_detail must serialise (camelCase via #[serde(rename_all)])");
        assert_eq!(
            ed.get("code").and_then(|v| v.as_str()),
            Some("ACTION_NOT_SUPPORTED"),
            "contract: error_detail.code must be ACTION_NOT_SUPPORTED on the pre-IPC path"
        );
        let ctx = ed
            .get("context")
            .expect("contract: error_detail.context must carry the supported-action list");
        let supp = ctx
            .get("supported_actions")
            .and_then(|v| v.as_array())
            .expect("contract: context.supported_actions must be an array");
        let names: Vec<&str> = supp.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"click"));
        assert!(names.contains(&"focus"));
        assert!(
            !names.contains(&"scrollIntoView"),
            "contract: requested action must NOT appear in the advertised list"
        );
        assert_eq!(
            ctx.get("requested_action").and_then(|v| v.as_str()),
            Some("scrollIntoView"),
            "contract: context.requested_action echoes the rejected action"
        );
    }
}

#[cfg(test)]
mod type_action_param_validation_tests {
    //! Loop B iter 2 — `{action:"type", params:{value:"foo"}}` and
    //! `{action:"type", params:{}}` must return HTTP 400 with the
    //! documented `INVALID_PARAM` / `MISSING_PARAM` envelope BEFORE the
    //! request can fall into `RecoveryExecutor::llm_fallback` and surface
    //! a misleading 200. Runner-side analogue of ui-bridge PR #33's
    //! SDK-boundary gate.
    //!
    //! Mirrors the `action_not_supported_tests` shape: exercises the pure
    //! validator helper (`validate_type_action_params`) and the structured
    //! error constructors (`UiBridgeError::type_param_{invalid,missing}`)
    //! so the contract is locked down without spinning up an axum router.
    //! The handler-side wiring (`api_error_detailed(msg, detail)` + HTTP
    //! 400) is too IPC-coupled for a pure unit test, so the helper-level
    //! tests act as the regression guard for the envelope shape.
    use super::{api_error_detailed, validate_type_action_params, TypeParamError, UiBridgeError};
    use crate::mcp::ui_bridge::types::UiBridgeErrorCode;
    use serde_json::{json, Map};

    fn empty_extra() -> Map<String, serde_json::Value> {
        Map::new()
    }

    // ── validator helper ─────────────────────────────────────────────────

    #[test]
    fn type_with_params_text_ok() {
        let params = Some(json!({ "text": "hello" }));
        assert_eq!(
            validate_type_action_params("type", &params, &empty_extra()),
            Ok(())
        );
    }

    #[test]
    fn type_with_flat_text_ok() {
        // Back-compat: `extra`-flatten merges top-level `text` into params.
        // The validator must honor that — the dispatcher will see the
        // merged shape downstream.
        let mut extra = empty_extra();
        extra.insert("text".into(), json!("hello"));
        assert_eq!(validate_type_action_params("type", &None, &extra), Ok(()));
    }

    #[test]
    fn type_with_params_value_is_wrong_field() {
        let params = Some(json!({ "value": "foo" }));
        assert_eq!(
            validate_type_action_params("type", &params, &empty_extra()),
            Err(TypeParamError::WrongFieldValueInsteadOfText)
        );
    }

    #[test]
    fn type_with_flat_value_is_wrong_field() {
        // Flat-form `value` should hit the same gate — the dispatcher's
        // `merged_params` will see `{value: "foo"}` either way.
        let mut extra = empty_extra();
        extra.insert("value".into(), json!("foo"));
        assert_eq!(
            validate_type_action_params("type", &None, &extra),
            Err(TypeParamError::WrongFieldValueInsteadOfText)
        );
    }

    #[test]
    fn type_with_empty_params_is_missing() {
        let params = Some(json!({}));
        assert_eq!(
            validate_type_action_params("type", &params, &empty_extra()),
            Err(TypeParamError::MissingTextField)
        );
    }

    #[test]
    fn type_with_no_params_at_all_is_missing() {
        assert_eq!(
            validate_type_action_params("type", &None, &empty_extra()),
            Err(TypeParamError::MissingTextField)
        );
    }

    #[test]
    fn type_with_non_string_text_is_missing() {
        // `text: null` / `text: 123` would confuse the typing engine even
        // if the field is technically present. Treat like absence so the
        // 400 fires deterministically rather than letting a JSON-typed
        // mismatch flow through to IPC.
        let params = Some(json!({ "text": 42 }));
        assert_eq!(
            validate_type_action_params("type", &params, &empty_extra()),
            Err(TypeParamError::MissingTextField)
        );
        let params = Some(json!({ "text": null }));
        assert_eq!(
            validate_type_action_params("type", &params, &empty_extra()),
            Err(TypeParamError::MissingTextField)
        );
    }

    #[test]
    fn type_with_value_wins_over_missing_when_both_off() {
        // If BOTH `value` and `text` (non-string) are present, the
        // "wrong field" hint is more useful than "missing" — the caller
        // clearly intended typing, just named the field wrong. The
        // current rule is: if `text` is not a string AND `value` is
        // present → WrongField (covered above). Sanity-double-check:
        let params = Some(json!({ "text": null, "value": "foo" }));
        assert_eq!(
            validate_type_action_params("type", &params, &empty_extra()),
            Err(TypeParamError::WrongFieldValueInsteadOfText)
        );
    }

    #[test]
    fn non_type_actions_are_not_gated() {
        // The validator is per-action by design — `setValue` and friends
        // own their own `value` semantics; we must NOT manufacture a
        // 400 for them. Pass-through verified for every other action
        // name in the supported list.
        let params = Some(json!({ "value": "foo" }));
        for action in &[
            "setValue",
            "select",
            "click",
            "doubleClick",
            "double_click",
            "double",
            "right",
            "middle",
            "sendKeys",
            "clear",
            "focus",
            "blur",
            "hover",
            "scroll",
            "scrollIntoView",
            "scroll_into_view",
            "check",
            "uncheck",
            "toggle",
            "drag",
            "submit",
            "reset",
            "autocomplete",
        ] {
            assert_eq!(
                validate_type_action_params(action, &params, &empty_extra()),
                Ok(()),
                "non-type action '{}' must not be gated by the type validator",
                action
            );
        }
    }

    // ── error constructors ────────────────────────────────────────────────

    #[test]
    fn type_param_invalid_carries_documented_message_and_hint() {
        // The exact docs message from
        // `ui-bridge/docs-site/docs/api/runner-features.md:336-341`. The
        // literal-string check is the regression guard — if anyone ever
        // changes this prose, both docs AND this test must move in
        // lockstep.
        let err = UiBridgeError::type_param_invalid();
        assert!(
            matches!(err.code, UiBridgeErrorCode::InvalidParam),
            "code must serialize to INVALID_PARAM"
        );
        assert_eq!(
            err.message,
            "type: 'value' is unknown; did you mean 'text'?"
        );
        let ctx = err.context.expect("context must carry didYouMean hint");
        assert_eq!(ctx.get("didYouMean").and_then(|v| v.as_str()), Some("text"));
        assert_eq!(
            ctx.get("field").and_then(|v| v.as_str()),
            Some("params.text")
        );
        assert_eq!(ctx.get("action").and_then(|v| v.as_str()), Some("type"));
    }

    #[test]
    fn type_param_missing_carries_documented_message() {
        let err = UiBridgeError::type_param_missing();
        assert!(
            matches!(err.code, UiBridgeErrorCode::MissingParam),
            "code must serialize to MISSING_PARAM"
        );
        assert_eq!(err.message, "type requires 'text' parameter");
        let ctx = err.context.expect("context must carry required-field hint");
        let required = ctx
            .get("required")
            .and_then(|v| v.as_array())
            .expect("required must be an array");
        assert_eq!(required.len(), 1);
        assert_eq!(required[0].as_str(), Some("text"));
    }

    // ── envelope shape ────────────────────────────────────────────────────

    #[test]
    fn envelope_for_wrong_field_serializes_to_400_contract() {
        // Build the body the handler will emit on the
        // `WrongFieldValueInsteadOfText` branch and lock down the JSON
        // shape that callers see. HTTP 400 is enforced by the handler;
        // here we verify the body's contract surface.
        let detail = UiBridgeError::type_param_invalid();
        let msg = detail.message.clone();
        let body = api_error_detailed(msg, detail);
        let body_json = serde_json::to_value(&body).expect("serializes");

        // success: false envelope.
        assert_eq!(
            body_json.get("success").and_then(|v| v.as_bool()),
            Some(false)
        );
        // Top-level `error` carries the documented prose.
        assert_eq!(
            body_json.get("error").and_then(|v| v.as_str()),
            Some("type: 'value' is unknown; did you mean 'text'?")
        );
        // `error_detail.code` is the machine-readable discriminator.
        let detail_json = body_json
            .get("error_detail")
            .expect("error_detail must be carried");
        assert_eq!(
            detail_json.get("code").and_then(|v| v.as_str()),
            Some("INVALID_PARAM"),
            "code must serialize as SCREAMING_SNAKE_CASE INVALID_PARAM"
        );
        assert_eq!(
            detail_json.get("message").and_then(|v| v.as_str()),
            Some("type: 'value' is unknown; did you mean 'text'?")
        );
        // The recovery hint lives in `error_detail.context`
        // (`didYouMean` + `field` per the loop B iter 2 spec).
        let ctx = detail_json.get("context").expect("context must be carried");
        assert_eq!(ctx.get("didYouMean").and_then(|v| v.as_str()), Some("text"));
        assert_eq!(
            ctx.get("field").and_then(|v| v.as_str()),
            Some("params.text")
        );
    }

    #[test]
    fn envelope_for_missing_field_serializes_to_400_contract() {
        let detail = UiBridgeError::type_param_missing();
        let msg = detail.message.clone();
        let body = api_error_detailed(msg, detail);
        let body_json = serde_json::to_value(&body).expect("serializes");

        assert_eq!(
            body_json.get("success").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            body_json.get("error").and_then(|v| v.as_str()),
            Some("type requires 'text' parameter")
        );
        let detail_json = body_json
            .get("error_detail")
            .expect("error_detail must be carried");
        assert_eq!(
            detail_json.get("code").and_then(|v| v.as_str()),
            Some("MISSING_PARAM")
        );
    }
}

#[cfg(test)]
mod read_value_tests {
    //! Validation seam for `read-value`'s `all`, the half of
    //! `@qontinui/ui-bridge` PR #156 the runner still had to grow. It is pure,
    //! so it needs neither a router nor a webview.
    //!
    //! The `send-keys` key/target grammar is deliberately NOT here: that route
    //! and its vocabulary live in `mcp/ui_bridge/keyboard.rs`, which owns the
    //! single copy. A second implementation in this module registered the same
    //! axum path and would have panicked the router at startup.

    use super::validate_read_value_all;
    use serde_json::json;

    // ---- read-value `all` -------------------------------------------------

    #[test]
    fn all_defaults_to_false_when_absent_or_null() {
        assert!(!validate_read_value_all(&json!({ "selector": "input" })).unwrap());
        assert!(!validate_read_value_all(&json!({ "selector": "input", "all": null })).unwrap());
    }

    #[test]
    fn all_true_is_honoured() {
        assert!(validate_read_value_all(&json!({ "selector": "input", "all": true })).unwrap());
    }

    #[test]
    fn a_non_boolean_all_is_rejected_by_name() {
        let msg = validate_read_value_all(&json!({ "selector": "i", "all": "yes" })).unwrap_err();
        assert!(msg.contains("'all'"), "{msg}");
        assert!(msg.contains("string"), "{msg}");
    }

    #[test]
    fn all_and_index_together_are_rejected_naming_both() {
        let msg = validate_read_value_all(&json!({ "selector": "i", "all": true, "index": 2 }))
            .unwrap_err();
        assert!(msg.contains("'all'") && msg.contains("'index'"), "{msg}");
    }

    #[test]
    fn all_false_with_index_is_fine() {
        // Only `all: true` conflicts — `all:false` is the default spelled out.
        assert!(
            !validate_read_value_all(&json!({ "selector": "i", "all": false, "index": 2 }))
                .unwrap()
        );
    }
}
