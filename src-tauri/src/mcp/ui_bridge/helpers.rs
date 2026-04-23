//! Shared helpers for UI Bridge HTTP handlers.
//!
//! Extracted from `mod.rs` so future per-family handler extractions
//! (`bookmarks.rs`, `design.rs`, etc.) can pull these in without dragging
//! the entire monolith along. None of the helpers below are part of the
//! public surface — each is `pub(super)` so siblings inside
//! `crate::mcp::ui_bridge` can use them, but external callers cannot.
//!
//! Three loose categories live here:
//!   - JS evaluation plumbing for the Tauri WebView
//!     (`direct_webview_evaluate*`, `safe_evaluate`, `evaluate_js_expression`)
//!   - Snapshot/diff helpers (`snapshot_signature`, `compute_snapshot_diff`,
//!     `count_elements_in_discover_payload`)
//!   - Response-shape extractors and field filters
//!     (`filter_element_fields`, `extract_*_match`, `extract_first_element_id`,
//!     `glob_match`)

use std::sync::Arc;

use crate::mcp::types::ApiState;

use super::request::ui_bridge_request_sync;

/// Allowed top-level field names for the `?fields=` filter on
/// `GET /control/element/{id}`. Unknown names are silently dropped to keep
/// callers forward-compatible if the element schema gains fields later.
///
/// `state.computedStyles` is special-cased: when requested, the entire `state`
/// object is filtered down to only `{computedStyles: ...}` rather than being
/// returned in full.
pub(super) const ELEMENT_ALLOWED_FIELDS: &[&str] = &[
    "id",
    "type",
    "label",
    "text",
    "value",
    "visible",
    "enabled",
    "focused",
    "rect",
    "normalizedRect",
    "ariaLabel",
    "actions",
    "state",
    "state.computedStyles",
    // Common related fields that callers also rely on; cheap to include.
    "category",
    "identifier",
    "registeredAt",
    "mounted",
    "customActions",
];

/// Filter an element JSON object down to a requested subset of top-level
/// fields. The `fields` parameter is a comma-separated list parsed from the
/// `?fields=` query string. Unknown names are dropped silently.
///
/// Special handling: when `state.computedStyles` (or `state` itself) appears
/// in the requested list, only the matching nested keys are kept inside
/// `state`.
pub(super) fn filter_element_fields(
    element: &serde_json::Value,
    fields_csv: &str,
) -> serde_json::Value {
    let requested: Vec<&str> = fields_csv
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter(|s| ELEMENT_ALLOWED_FIELDS.contains(s))
        .collect();

    if requested.is_empty() {
        // No allowed fields after filtering — return an empty object instead
        // of the full payload so callers get a consistent "you asked for
        // nothing useful" response.
        return serde_json::json!({});
    }

    let Some(obj) = element.as_object() else {
        return element.clone();
    };

    let want_state_full = requested.contains(&"state");
    let want_state_styles = requested.contains(&"state.computedStyles");

    let mut out = serde_json::Map::new();
    for &name in &requested {
        if name == "state.computedStyles" {
            // Handled below in the state composition step.
            continue;
        }
        if name == "state" {
            // Will be filled in below.
            continue;
        }
        if let Some(v) = obj.get(name) {
            out.insert(name.to_string(), v.clone());
        }
    }

    if want_state_full {
        if let Some(state) = obj.get("state") {
            out.insert("state".to_string(), state.clone());
        }
    } else if want_state_styles {
        if let Some(state_obj) = obj.get("state").and_then(|v| v.as_object()) {
            let mut sub = serde_json::Map::new();
            if let Some(cs) = state_obj.get("computedStyles") {
                sub.insert("computedStyles".to_string(), cs.clone());
            }
            out.insert("state".to_string(), serde_json::Value::Object(sub));
        }
    }

    serde_json::Value::Object(out)
}

/// Cheap fingerprint of a discover snapshot for click-had-no-effect
/// detection. Returns `(element_count, hash)` where `hash` is a stable
/// hash over each element's `id`, `category`, and `state.textContent`.
/// Two equal signatures = no observable DOM mutation between snapshots.
pub(super) fn snapshot_signature(snapshot: &serde_json::Value) -> (usize, u64) {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let elements = snapshot
        .get("elements")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    for el in &elements {
        if let Some(s) = el.get("id").and_then(|v| v.as_str()) {
            s.hash(&mut hasher);
        }
        if let Some(s) = el.get("category").and_then(|v| v.as_str()) {
            s.hash(&mut hasher);
        }
        if let Some(s) = el
            .get("state")
            .and_then(|v| v.get("textContent"))
            .and_then(|v| v.as_str())
        {
            s.hash(&mut hasher);
        }
        if let Some(s) = el
            .get("state")
            .and_then(|v| v.get("ariaPressed"))
            .and_then(|v| v.as_bool())
        {
            s.hash(&mut hasher);
        }
    }
    (elements.len(), hasher.finish())
}

/// Count elements in a discover payload, handling the common shapes
/// `{"elements": [...]}` and `[...]`. Returns 0 if the shape is unfamiliar.
pub(super) fn count_elements_in_discover_payload(data: &serde_json::Value) -> usize {
    if let Some(arr) = data.as_array() {
        return arr.len();
    }
    if let Some(arr) = data.get("elements").and_then(|v| v.as_array()) {
        return arr.len();
    }
    if let Some(arr) = data
        .get("data")
        .and_then(|d| d.get("elements"))
        .and_then(|v| v.as_array())
    {
        return arr.len();
    }
    0
}

/// Evaluate JS in the WebView using the IPC response channel as a callback.
///
/// This wraps the expression in a function that sends the result back via
/// POST to the IPC response endpoint, bypassing the Tauri event system.
/// Used as a fallback when the SDK's event handlers aren't responding.
pub(super) async fn direct_webview_evaluate_with_result(
    state: &Arc<ApiState>,
    expression: &str,
    timeout_override_ms: Option<u64>,
    await_promise: bool,
) -> Result<String, String> {
    use tauri::Manager;

    let window = state
        .app_handle
        .get_webview_window(qontinui_runner_lib::get_main_window_label())
        .ok_or_else(|| "WebView window 'main' not found".to_string())?;

    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();

    // Register the pending request
    {
        let mut pending = state.ui_bridge_pending.lock().await;
        pending.insert(request_id.clone(), tx);
        state
            .ui_bridge_pending_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    // Use the known API port — location.port is empty on tauri.localhost
    let api_port = state
        .app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);
    if api_port == 0 {
        // Remove the pending request we just registered
        let mut pending = state.ui_bridge_pending.lock().await;
        pending.remove(&request_id);
        state
            .ui_bridge_pending_count
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        return Err("API port not yet bound — direct eval unavailable".to_string());
    }

    // Encode the user's expression as a JS *string literal* so that any
    // characters that would break a raw splice (newlines inside a string
    // literal, `*/`, unpaired quotes, etc.) survive intact. JSON strings are
    // a strict subset of JS strings, so serde_json output is a valid JS
    // literal. We then `eval()` that literal inside the webview — this
    // matches Chrome DevTools' `Runtime.evaluate` contract. (Previously the
    // expression was spliced into the JS template directly, which corrupted
    // any body containing a literal newline inside a string literal, e.g.
    // `invoke("report_ui_error", {stack: "at A\n  at B"})`.)
    let expr_literal = serde_json::to_string(expression)
        .map_err(|e| format!("Failed to encode expression as JS literal: {}", e))?;

    // When `awaitPromise` is true, mirror DevTools behavior: if the
    // expression evaluates to a Promise (e.g. an async `invoke(...)` or
    // an IIFE returning a Promise), resolve it before reporting the value.
    // When false, keep the pre-existing direct-path semantics: send the
    // raw return value straight through (non-resolved Promises become
    // `"[object Promise]"` after JSON.stringify), so existing callers see
    // no behavior change beyond the newline/escape bugfix.
    let eval_inner = if await_promise {
        format!("await Promise.resolve(eval({}))", expr_literal)
    } else {
        format!("eval({})", expr_literal)
    };

    // Build JS that evaluates the expression and POSTs the result back
    // via the IPC response HTTP endpoint
    let callback_js = format!(
        r#"(async function() {{
            var reqId = "{}";
            try {{
                var result = {};
                var value = (result === undefined) ? null : result;
                await fetch("http://127.0.0.1:{}/ui-bridge/ipc-response", {{
                    method: "POST",
                    headers: {{ "Content-Type": "application/json" }},
                    body: JSON.stringify({{
                        requestId: reqId,
                        type: "page_evaluate",
                        success: true,
                        data: {{ result: {{ value: (typeof value === "string") ? value : JSON.stringify(value) }} }}
                    }})
                }});
            }} catch(e) {{
                await fetch("http://127.0.0.1:{}/ui-bridge/ipc-response", {{
                    method: "POST",
                    headers: {{ "Content-Type": "application/json" }},
                    body: JSON.stringify({{
                        requestId: reqId,
                        type: "page_evaluate",
                        success: false,
                        error: e.message
                    }})
                }}).catch(function() {{}});
            }}
        }})()"#,
        request_id, eval_inner, api_port, api_port
    );

    window
        .eval(&callback_js)
        .map_err(|e| format!("WebView eval dispatch failed: {}", e))?;

    // Wait for the response with a timeout (caller can override for long async ops)
    let timeout_secs = timeout_override_ms.map(|ms| ms / 1000).unwrap_or(10);
    match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx).await {
        Ok(Ok(data)) => {
            if let Some(result) = data
                .get("result")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
            {
                Ok(result.to_string())
            } else if let Some(err) = data.get("error").and_then(|e| e.as_str()) {
                Err(format!("JS error: {}", err))
            } else {
                Ok(serde_json::to_string(&data).unwrap_or_default())
            }
        }
        Ok(Err(_)) => Err("Response channel dropped".to_string()),
        Err(_) => {
            // Clean up pending request
            let mut pending = state.ui_bridge_pending.lock().await;
            if pending.remove(&request_id).is_some() {
                state
                    .ui_bridge_pending_count
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }
            Err(format!("Direct eval timed out after {}s", timeout_secs))
        }
    }
}

/// Evaluate a JS expression directly in the Tauri WebView using window.eval().
///
/// This bypasses the IPC event system entirely, so it works even when the
/// UI Bridge SDK hasn't initialized. The expression is wrapped in a try/catch
/// to prevent evaluation errors from crashing the WebView connection.
///
/// Returns the stringified result. For structured data, the expression should
/// return JSON.stringify(...).
#[allow(dead_code)]
pub(super) async fn direct_webview_evaluate(
    app_handle: &tauri::AppHandle,
    expression: &str,
) -> Result<String, String> {
    use tauri::Manager;

    let window = app_handle
        .get_webview_window(qontinui_runner_lib::get_main_window_label())
        .ok_or_else(|| "WebView window 'main' not found".to_string())?;

    // Wrap in try/catch with timeout guard to prevent crashes
    let safe_js = format!(
        r#"(function() {{
            try {{
                var __result = (function() {{ return {}; }})();
                if (__result === undefined) return "undefined";
                if (__result === null) return "null";
                return String(__result);
            }} catch(e) {{
                return "ERROR:" + e.message;
            }}
        }})()"#,
        expression
    );

    // Tauri's eval() is fire-and-forget (returns Ok(()) on success).
    // To get a return value, we use a callback pattern via a Tauri event.
    // However, for simplicity and reliability, we use the IPC response channel
    // if available, or fall back to a polling approach.
    //
    // The most robust approach: use the existing page_evaluate IPC path first,
    // and only fall back to direct eval for side-effect-only operations.
    //
    // For the new endpoints, we'll use a hybrid: construct the full JS inline
    // and use IPC to get the result back, but with error wrapping.

    // Use the existing IPC path but with our safe-wrapped expression
    window
        .eval(&safe_js)
        .map_err(|e| format!("WebView eval failed: {}", e))?;

    // Since eval() is fire-and-forget in Tauri v2, we can't get a return value
    // directly. Instead, we'll use the IPC request_sync path with our wrapped expression.
    Ok("eval_dispatched".to_string())
}

/// Evaluate a JS expression via IPC with automatic error wrapping.
/// This is the safe version of page_evaluate that wraps expressions in try/catch
/// so errors return as JSON instead of crashing the connection.
pub(super) async fn safe_evaluate(
    state: &Arc<ApiState>,
    expression: &str,
) -> Result<serde_json::Value, String> {
    // Wrap the expression in try/catch for safety
    let safe_expr = format!(
        r#"(() => {{ try {{ return JSON.stringify({{ success: true, value: (function() {{ {} }})() }}); }} catch(e) {{ return JSON.stringify({{ success: false, error: e.message, stack: e.stack }}); }} }})()"#,
        expression
    );

    let payload = serde_json::json!({ "expression": safe_expr });

    match ui_bridge_request_sync(state, "page_evaluate", payload).await {
        Ok(data) => {
            // Try to parse the inner result
            if let Some(result) = data
                .get("result")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
            {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(result) {
                    if parsed.get("success") == Some(&serde_json::Value::Bool(false)) {
                        let error_msg = parsed
                            .get("error")
                            .and_then(|e| e.as_str())
                            .unwrap_or("Unknown error");
                        return Err(format!("JavaScript evaluation error: {}", error_msg));
                    }
                    return Ok(parsed);
                }
            }
            Ok(data)
        }
        Err(e) => Err(e),
    }
}

/// Extract the matched element from an `ai_find` response, if any.
///
/// The frontend's ai_find shape is `{ element: {...}, confidence: 0.x,
/// alternatives: [...] }` wrapped in the usual `data` envelope. Returns
/// the element only if confidence >= min_confidence.
pub(super) fn extract_ai_find_match(
    data: &serde_json::Value,
    min_confidence: f64,
) -> Option<serde_json::Value> {
    // Walk into the response trying both {elem, conf} at top level and
    // nested under `data`, matching other ai_find callers.
    let find_match = |v: &serde_json::Value| -> Option<serde_json::Value> {
        let element = v.get("element")?;
        if element.is_null() {
            return None;
        }
        let conf = v.get("confidence").and_then(|c| c.as_f64()).unwrap_or(0.0);
        if conf >= min_confidence {
            Some(element.clone())
        } else {
            None
        }
    };
    if let Some(e) = find_match(data) {
        return Some(e);
    }
    if let Some(inner) = data.get("data") {
        if let Some(e) = find_match(inner) {
            return Some(e);
        }
    }
    None
}

/// Extract the element from a `get_element` response, if it represents a
/// real element (not a null / not-found placeholder).
pub(super) fn extract_get_element_match(data: &serde_json::Value) -> Option<serde_json::Value> {
    let check = |v: &serde_json::Value| -> Option<serde_json::Value> {
        if v.is_null() {
            return None;
        }
        // Some frontends return {found: false} — respect it.
        if v.get("found").and_then(|f| f.as_bool()) == Some(false) {
            return None;
        }
        // Require at least an id field to count as a real match.
        if v.get("id").is_some() || v.get("elementId").is_some() {
            return Some(v.clone());
        }
        // Or a nested `element` field.
        if let Some(el) = v.get("element") {
            if !el.is_null() && (el.get("id").is_some() || el.get("elementId").is_some()) {
                return Some(el.clone());
            }
        }
        None
    };
    if let Some(e) = check(data) {
        return Some(e);
    }
    if let Some(inner) = data.get("data") {
        if let Some(e) = check(inner) {
            return Some(e);
        }
    }
    None
}

/// Extract the first element ID from a find/search result.
///
/// Handles multiple response formats:
///   - `{ results: [{ elementId, ... }] }` (ai_search)
///   - `{ elements: [{ id, ... }] }` (find)
///   - `{ id, ... }` (direct element)
///   - `[{ id, ... }]` (array of elements)
pub(super) fn extract_first_element_id(data: &serde_json::Value) -> Option<String> {
    // find returns { elements: [{ id, ... }] }
    if let Some(elements) = data.get("elements").and_then(|v| v.as_array()) {
        if let Some(first) = elements.first() {
            if let Some(id) = first.get("id").and_then(|v| v.as_str()) {
                return Some(id.to_string());
            }
        }
    }
    // ai_search returns { results: [{ elementId, ... }] }
    if let Some(results) = data.get("results").and_then(|v| v.as_array()) {
        if let Some(first) = results.first() {
            if let Some(id) = first
                .get("elementId")
                .or_else(|| first.get("id"))
                .and_then(|v| v.as_str())
            {
                return Some(id.to_string());
            }
        }
    }
    // Direct element response
    if let Some(id) = data.get("id").and_then(|v| v.as_str()) {
        return Some(id.to_string());
    }
    // Array of elements
    if let Some(arr) = data.as_array() {
        if let Some(first) = arr.first() {
            if let Some(id) = first.get("id").and_then(|v| v.as_str()) {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// Evaluate a JS expression, trying IPC first then direct WebView eval.
/// Returns the raw string result from the evaluation.
pub(super) async fn evaluate_js_expression(
    state: &Arc<ApiState>,
    expression: &str,
) -> Result<String, String> {
    let payload = serde_json::json!({ "expression": expression });

    // Try IPC path first (uses SDK event handlers, fastest)
    match ui_bridge_request_sync(state, "page_evaluate", payload).await {
        Ok(data) => {
            // Check for inner error (e.g., "Expression rejected: contains prohibited pattern")
            if data.get("success") == Some(&serde_json::Value::Bool(false))
                || data.get("error").is_some()
            {
                // IPC returned an error — fall back to direct eval
                return direct_webview_evaluate_with_result(state, expression, None, false).await;
            }
            // Extract the result value from the IPC response
            if let Some(result) = data.get("result").and_then(|r| r.get("value")) {
                match result {
                    serde_json::Value::String(s) => Ok(s.clone()),
                    other => Ok(other.to_string()),
                }
            } else {
                Ok(data.to_string())
            }
        }
        Err(_ipc_err) => {
            // Fallback to direct WebView evaluation
            direct_webview_evaluate_with_result(state, expression, None, false).await
        }
    }
}

/// Match a path string against a glob pattern. `*` matches a single path
/// segment (no `/`); `**` matches any sequence of characters including `/`.
/// All other characters match literally.
pub(super) fn glob_match(pattern: &str, path: &str) -> bool {
    fn helper(p: &[u8], s: &[u8]) -> bool {
        let mut pi = 0usize;
        let mut si = 0usize;
        let mut star: Option<(usize, usize)> = None; // (pattern_idx_after_*, source_idx_when_started)
        let mut star_is_double = false;

        while si < s.len() {
            if pi < p.len() && p[pi] == b'*' {
                let double = pi + 1 < p.len() && p[pi + 1] == b'*';
                if double {
                    pi += 2;
                } else {
                    pi += 1;
                }
                star = Some((pi, si));
                star_is_double = double;
                continue;
            }
            if pi < p.len() && p[pi] == s[si] {
                pi += 1;
                si += 1;
                continue;
            }
            // Mismatch — backtrack to last star, advance source by one char.
            if let Some((p_after, s_start)) = star {
                // Single-star can't cross a path separator.
                if !star_is_double && s[s_start] == b'/' {
                    return false;
                }
                if !star_is_double {
                    // Walk forward but stop if we'd consume a '/'.
                    let next = s_start + 1;
                    if next > s.len() {
                        return false;
                    }
                    if s_start < s.len() && s[s_start] == b'/' {
                        return false;
                    }
                    star = Some((p_after, next));
                    si = next;
                    pi = p_after;
                    continue;
                } else {
                    let next = s_start + 1;
                    star = Some((p_after, next));
                    si = next;
                    pi = p_after;
                    continue;
                }
            }
            return false;
        }
        // Trailing stars in the pattern still match.
        while pi < p.len() && p[pi] == b'*' {
            pi += 1;
            if pi < p.len() && p[pi] == b'*' {
                pi += 1;
            }
        }
        pi == p.len()
    }
    helper(pattern.as_bytes(), path.as_bytes())
}

/// Compute a minimal id-set diff between two snapshot JSON values.
/// Returns counts plus the actual id arrays so callers can react to specific
/// elements that appeared/disappeared. Falls back to an empty diff if either
/// snapshot is missing.
pub(super) fn compute_snapshot_diff(
    pre: Option<&serde_json::Value>,
    post: Option<&serde_json::Value>,
) -> serde_json::Value {
    fn extract_ids(snap: Option<&serde_json::Value>) -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        if let Some(s) = snap {
            if let Some(arr) = s.get("elements").and_then(|e| e.as_array()) {
                for el in arr {
                    if let Some(id) = el.get("id").and_then(|v| v.as_str()) {
                        out.insert(id.to_string());
                    }
                }
            }
        }
        out
    }

    let pre_ids = extract_ids(pre);
    let post_ids = extract_ids(post);
    let added: Vec<String> = post_ids.difference(&pre_ids).cloned().collect();
    let removed: Vec<String> = pre_ids.difference(&post_ids).cloned().collect();

    serde_json::json!({
        "addedIds": added,
        "removedIds": removed,
        "addedCount": added.len(),
        "removedCount": removed.len(),
        "preCount": pre_ids.len(),
        "postCount": post_ids.len(),
    })
}

/// Deserialize a timestamp that can be either a number (epoch ms) or an ISO 8601 string.
///
/// Used by Query types across `errors.rs` and `network.rs`
/// (`since` field on console errors, browser events, timeline, network requests).
pub(super) fn deserialize_timestamp<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct TimestampVisitor;
    impl<'de> de::Visitor<'de> for TimestampVisitor {
        type Value = Option<f64>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a number (epoch ms) or ISO 8601 string")
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D: serde::Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Self::Value, D::Error> {
            deserializer.deserialize_any(TimestampInnerVisitor)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
    }

    struct TimestampInnerVisitor;
    impl<'de> de::Visitor<'de> for TimestampInnerVisitor {
        type Value = Option<f64>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a number or ISO 8601 string")
        }

        fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
            Ok(Some(v))
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
            Ok(Some(v as f64))
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
            Ok(Some(v as f64))
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            // Try parsing as float first
            if let Ok(f) = v.parse::<f64>() {
                return Ok(Some(f));
            }
            // Try ISO 8601
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(v) {
                return Ok(Some(dt.timestamp_millis() as f64));
            }
            // Try common ISO variants
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%S") {
                return Ok(Some(dt.and_utc().timestamp_millis() as f64));
            }
            Err(de::Error::custom(format!(
                "invalid timestamp: expected number (epoch ms) or ISO 8601 string, got '{}'",
                v
            )))
        }
    }

    deserializer.deserialize_option(TimestampVisitor)
}
