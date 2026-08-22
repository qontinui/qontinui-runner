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
//!     (`return_expression_js`, `direct_webview_evaluate_with_result`,
//!     `safe_evaluate`, `evaluate_js_expression`)
//!   - Snapshot/diff helpers (`snapshot_signature`, `compute_snapshot_diff`,
//!     `count_elements_in_discover_payload`)
//!   - Response-shape extractors and field filters
//!     (`filter_element_fields`, `extract_*_match`, `extract_first_element_id`,
//!     `glob_match`)

use std::sync::Arc;

use crate::mcp::types::ApiState;

use super::request::{ui_bridge_request_sync, ui_bridge_request_sync_in_window, MAIN_WINDOW_LABEL};

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
/// detection.
///
/// TWO independent hashes, deliberately not one:
///
/// - `content` folds each element's `id`, `category`, `state.textContent` and
///   `state.ariaPressed` — what the element *shows*.
/// - `generation` folds each element's `registeredAt`, the per-registration
///   `Date.now()` the SDK registry stamps
///   (`ui-bridge/packages/ui-bridge/src/core/registry.ts`) — WHICH MOUNT the
///   element belongs to.
///
/// The split is what makes a same-shape REMOUNT visible. The registry
/// deliberately preserves element IDs across a remount
/// (`preserveIdAcrossRemount` + the recently-removed fingerprint cache), and a
/// component that is destroyed and recreated in the same state renders
/// identical text — so a content-only signature reported
/// `effectChanged: false` for a click that had in fact torn the whole subtree
/// down and rebuilt it. A driver trusting that concluded "the click was a
/// no-op", the exact opposite of the truth (that is how the Setup Wizard's
/// Tier-step remount stayed invisible through a whole investigation).
/// `registeredAt` is re-stamped by a real unregister→register cycle, so the
/// generation hash moves while the content hash does not.
///
/// Residual: `registeredAt` has millisecond resolution, so a remount that
/// completes inside the same millisecond is still invisible. Every observed
/// one has been ≥1ms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SnapshotSignature {
    /// Number of elements in the snapshot.
    pub count: usize,
    /// Hash over what the elements show.
    pub content: u64,
    /// Hash over which mount the elements belong to.
    pub generation: u64,
}

impl SnapshotSignature {
    /// True when nothing observable differs — same elements, same content,
    /// same mounts.
    pub fn unchanged_from(&self, other: &Self) -> bool {
        self == other
    }

    /// True when the elements are the same and show the same thing, but they
    /// belong to a DIFFERENT mount — i.e. the subtree was unmounted and
    /// recreated rather than left alone.
    pub fn remounted_from(&self, other: &Self) -> bool {
        self.count == other.count
            && self.content == other.content
            && self.generation != other.generation
    }
}

pub(super) fn snapshot_signature(snapshot: &serde_json::Value) -> SnapshotSignature {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let elements = snapshot
        .get("elements")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut content = DefaultHasher::new();
    let mut generation = DefaultHasher::new();
    for el in &elements {
        if let Some(s) = el.get("id").and_then(|v| v.as_str()) {
            s.hash(&mut content);
            // The id is folded into BOTH hashes so a generation change is
            // always attributable to a specific element rather than to a
            // reordering of the array.
            s.hash(&mut generation);
        }
        if let Some(s) = el.get("category").and_then(|v| v.as_str()) {
            s.hash(&mut content);
        }
        if let Some(s) = el
            .get("state")
            .and_then(|v| v.get("textContent"))
            .and_then(|v| v.as_str())
        {
            s.hash(&mut content);
        }
        if let Some(s) = el
            .get("state")
            .and_then(|v| v.get("ariaPressed"))
            .and_then(|v| v.as_bool())
        {
            s.hash(&mut content);
        }
        // Absent on a serializer that does not emit it — folded as a constant
        // so such a snapshot simply never reports a remount, rather than
        // reporting a spurious one.
        if let Some(n) = el.get("registeredAt").and_then(|v| v.as_u64()) {
            n.hash(&mut generation);
        }
    }
    SnapshotSignature {
        count: elements.len(),
        content: content.finish(),
        generation: generation.finish(),
    }
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

/// Splice a caller-supplied expression into a JS `return` position without
/// letting automatic semicolon insertion (ASI) silently swallow it.
///
/// Returns the statement `return <expr>\n;`, ready to be embedded as (part of)
/// a function body.
///
/// # Why this exists
///
/// Two things go wrong when a caller's expression is spliced raw into
/// `return {}` :
///
/// 1. **A LEADING NEWLINE.** `format!("return {}", "\n({a:1})")` produces
///
///    ```js
///    return
///    ({a:1})
///    ```
///
///    and ASI terminates the `return` on the first line, so the function
///    returns `undefined`. Critically this is NOT a `SyntaxError` — the
///    expression parses and runs, it just yields nothing — so the caller sees
///    `success: true` with an empty result. That is a SILENT FALSE GREEN: it
///    corrupts test evidence rather than merely losing it. Any driver writing
///    a multi-line expression the natural way (heredoc, triple-quoted string,
///    `--data-binary @file`) hits it. `trim_start()` is the fix, and is the
///    exact mirror of the `trimStart()` guard on the frontend's bare-`return`
///    compile arm (`compileEvaluateExpression` in
///    `src/hooks/ui-bridge-events/utils.ts`), which fixed the same defect for
///    `POST /control/page/evaluate`.
///
/// 2. **A TRAILING LINE COMMENT.** `return 1 // done;` comments out the
///    terminating `;` and everything the template puts after it on that line,
///    corrupting the whole generated program (for `evaluate-batch`, every
///    other expression in the batch with it). Emitting the `;` on its own
///    line ends the comment first.
///
/// # What this deliberately does NOT handle
///
/// A LEADING line comment (`"// note\nfoo()"`) still ASI-truncates here. The
/// frontend fixes that case by wrapping as `return (<expr>\n)` and falling
/// back to a bare `return` wrap when the parenthesised form is a
/// `SyntaxError` — a try-compile chain these Rust-built templates cannot use,
/// because they have no `SyntaxError` to catch (a parse error kills the whole
/// emitted program) and cannot compile dynamically: `new Function` and `eval`
/// are both structural blocks in the frontend's `page_evaluate` blocklist, so
/// any expression containing them is rejected before it runs. Wrapping
/// unconditionally in parens instead would regress every `;`-terminated
/// expression (`document.title;`), which works today. Fixing the evidenced
/// silent failure with zero behavior change for expressions that already work
/// is the better trade at these two legacy endpoints.
pub(super) fn return_expression_js(expression: &str) -> String {
    format!("return {}\n;", expression.trim_start())
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

    // This direct-eval path always targets the main window, and the injected JS
    // POSTs its response back without a `windowLabel` — so it must register and
    // clean up under the SAME composite (main, id) key the response dispatcher
    // computes (it defaults a missing label to "main"). See
    // `super::request::pending_key`.
    let pkey = super::request::pending_key(super::request::MAIN_WINDOW_LABEL, &request_id);

    // Register the pending request
    {
        let mut pending = state.ui_bridge_pending.lock().await;
        pending.insert(pkey.clone(), tx);
        state
            .ui_bridge_pending_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    // Use the known API base URL — location.port is empty on tauri.localhost,
    // so the injected JS must address the runner's HTTP API explicitly. Reads
    // the actually-bound port from AppState.api_port (set at server start).
    let api_port = state
        .app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);
    if api_port == 0 {
        // Remove the pending request we just registered
        let mut pending = state.ui_bridge_pending.lock().await;
        pending.remove(&pkey);
        state
            .ui_bridge_pending_count
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        return Err("API port not yet bound — direct eval unavailable".to_string());
    }
    let ipc_url = format!(
        "{}/ui-bridge/ipc-response",
        crate::mcp::types::get_self_base_url(&state.app_state)
    );

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
                await fetch("{}", {{
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
                await fetch("{}", {{
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
        request_id, eval_inner, ipc_url, ipc_url
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
            if pending.remove(&pkey).is_some() {
                state
                    .ui_bridge_pending_count
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }
            Err(format!("Direct eval timed out after {}s", timeout_secs))
        }
    }
}

/// Read the evaluated value out of a legacy-IPC `page_evaluate` response.
///
/// The frontend handler (`usePageEvents.ts::page_evaluate`) now emits exactly
/// ONE envelope — `{value, type}` — for every result. It used to vary the shape
/// by result type (objects bare at the top level, everything else boxed as
/// `{result: {value}}`), so each consumer here hand-rolled the same
/// `data["result"]["value"]` walk and silently fell through to its own default
/// whenever the walk missed. One reader now, so the walk cannot drift per site.
///
/// `None` means the expression evaluated to `undefined` (JSON drops the key;
/// the `type` discriminant is what still says so) or the payload is not an
/// evaluate envelope at all. Callers must treat `None` as "no value", never as
/// a value.
pub(super) fn evaluate_ipc_value(data: &serde_json::Value) -> Option<&serde_json::Value> {
    data.get("value")
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
            if let Some(result) = evaluate_ipc_value(&data).and_then(|v| v.as_str()) {
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
            if let Some(result) = evaluate_ipc_value(&data) {
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

/// Like [`evaluate_js_expression`] but targets a specific runner window.
///
/// For the main window (empty label or [`MAIN_WINDOW_LABEL`]) this is identical
/// to [`evaluate_js_expression`] — the IPC fast-path with a direct-WebView
/// fallback. For a pop-out window it routes the IPC request to that window via
/// [`ui_bridge_request_sync_in_window`]. There is intentionally NO direct-eval
/// fallback for non-main windows: [`direct_webview_evaluate_with_result`] is
/// main-coupled (it resolves `get_main_window_label()` and its injected JS posts
/// the result back under the `(main, id)` key), so falling back would silently
/// run the expression in the WRONG window. An unknown label yields the same
/// structured error as `page/evaluate` (`page.rs`), pointing at the discovery
/// route.
pub(super) async fn evaluate_js_expression_in_window(
    state: &Arc<ApiState>,
    expression: &str,
    window_label: &str,
) -> Result<String, String> {
    if window_label.is_empty() || window_label == MAIN_WINDOW_LABEL {
        return evaluate_js_expression(state, expression).await;
    }

    // Fail fast on an unknown label rather than hanging on an IPC request
    // nothing can answer — same contract as `page/evaluate`.
    {
        use tauri::Manager;
        if state.app_handle.get_webview_window(window_label).is_none() {
            return Err(format!(
                "No runner window labeled '{window_label}'. Discover live windows via \
                 GET /ui-bridge/control/runner-windows."
            ));
        }
    }

    let payload = serde_json::json!({ "expression": expression });
    let data =
        ui_bridge_request_sync_in_window(state, "page_evaluate", payload, window_label).await?;

    // Surface an inner evaluation error rather than masking it with a main-window
    // fallback (which would be the wrong window).
    if data.get("success") == Some(&serde_json::Value::Bool(false)) || data.get("error").is_some() {
        let msg = data
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("evaluation failed in target window");
        return Err(msg.to_string());
    }
    if let Some(result) = evaluate_ipc_value(&data) {
        match result {
            serde_json::Value::String(s) => Ok(s.clone()),
            other => Ok(other.to_string()),
        }
    } else {
        Ok(data.to_string())
    }
}

/// Read the optional `windowLabel` body field used by the read/convenience
/// family (`read-value`, `type-into`, `click-by-text`, …) to scope a read to a
/// pop-out window. Absent, non-string, or empty → `""` (the main window).
pub(super) fn read_window_label(body: &serde_json::Value) -> &str {
    body.get("windowLabel")
        .and_then(|v| v.as_str())
        .unwrap_or("")
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

#[cfg(test)]
mod helpers_tests {
    use super::{read_window_label, return_expression_js, snapshot_signature};
    use serde_json::json;

    /// THE REGRESSION. A leading newline used to produce `return\n({a:1})`,
    /// which ASI truncates to a bare `return` — the function yields
    /// `undefined` and the caller is told `success: true` with no data.
    /// Nothing may sit between `return ` and the expression except the space.
    #[test]
    fn leading_newline_cannot_be_swallowed_by_asi() {
        let js = return_expression_js("\n({a:1})");
        assert!(
            js.starts_with("return ({a:1})"),
            "leading newline must be stripped, got {js:?}"
        );
    }

    /// `\r\n` line endings (a Windows heredoc / CRLF request body) are the
    /// same defect — `trim_start` covers all Unicode whitespace, not just LF.
    #[test]
    fn leading_crlf_and_repeated_newlines_are_stripped() {
        for expr in ["\r\n({a:1})", "\n\n\n({a:1})", "   \n\t({a:1})"] {
            let js = return_expression_js(expr);
            assert!(
                js.starts_with("return ({a:1})"),
                "expected {expr:?} to compile to a same-line return, got {js:?}"
            );
        }
    }

    /// The mirror case: a TRAILING line comment must not comment out the
    /// terminating `;` (and, at the batch call site, everything the template
    /// emits after it on that line).
    #[test]
    fn trailing_line_comment_cannot_swallow_the_terminator() {
        let js = return_expression_js("1 + 1 // done");
        assert_eq!(js, "return 1 + 1 // done\n;");
        assert!(
            js.ends_with("\n;"),
            "terminator must start its own line, got {js:?}"
        );
    }

    /// Expressions that already work must be untouched apart from the
    /// terminator moving to its own line — in particular `;`-terminated ones,
    /// which an unconditional parenthesised wrap would have broken.
    #[test]
    fn ordinary_expressions_are_unchanged() {
        assert_eq!(
            return_expression_js("document.title"),
            "return document.title\n;"
        );
        assert_eq!(return_expression_js("2;"), "return 2;\n;");
        assert_eq!(return_expression_js("({a:1})"), "return ({a:1})\n;");
    }

    /// A trailing newline was never the bug and stays harmless.
    #[test]
    fn trailing_whitespace_is_preserved_harmlessly() {
        assert_eq!(return_expression_js("1 + 1\n"), "return 1 + 1\n\n;");
    }

    #[test]
    fn absent_window_label_is_main() {
        assert_eq!(read_window_label(&json!({ "selector": "input" })), "");
    }

    #[test]
    fn empty_window_label_is_main() {
        assert_eq!(read_window_label(&json!({ "windowLabel": "" })), "");
    }

    #[test]
    fn non_string_window_label_is_main() {
        assert_eq!(read_window_label(&json!({ "windowLabel": 42 })), "");
    }

    #[test]
    fn present_window_label_is_returned() {
        assert_eq!(
            read_window_label(&json!({ "windowLabel": "term-2" })),
            "term-2"
        );
    }

    // ---- snapshot_signature: remount detection (R7) -------------------

    /// A four-button Tier step. `gen` stands in for the mount generation the
    /// SDK stamps as `registeredAt`.
    fn tier_step_snapshot(gen: u64, heading: &str) -> serde_json::Value {
        json!({
            "elements": [
                { "id": "button-local", "category": "interactive", "registeredAt": gen,
                  "state": { "textContent": "Local AI (Tier 0)" } },
                { "id": "button-byo-key", "category": "interactive", "registeredAt": gen,
                  "state": { "textContent": "Use my own API key (Tier 1)" } },
                { "id": "button-tier2", "category": "interactive", "registeredAt": gen,
                  "state": { "textContent": "Sign in to Qontinui (Tier 2)" } },
                { "id": "heading", "category": "text", "registeredAt": gen,
                  "state": { "textContent": heading } },
            ]
        })
    }

    #[test]
    fn identical_snapshot_is_unchanged_and_not_remounted() {
        let a = snapshot_signature(&tier_step_snapshot(1000, "How will you use Qontinui?"));
        let b = snapshot_signature(&tier_step_snapshot(1000, "How will you use Qontinui?"));
        assert!(b.unchanged_from(&a));
        assert!(!b.remounted_from(&a));
    }

    /// THE case this split exists for: the wizard is torn down and rebuilt in
    /// the same state. Element IDs are preserved by the registry and every
    /// label is identical, so a content-only signature reported "nothing
    /// happened" for a click that destroyed the operator's wizard progress.
    #[test]
    fn same_shape_remount_is_detected() {
        let pre = snapshot_signature(&tier_step_snapshot(1000, "How will you use Qontinui?"));
        let post = snapshot_signature(&tier_step_snapshot(1017, "How will you use Qontinui?"));
        assert_eq!(
            pre.content, post.content,
            "content is identical by construction"
        );
        assert_ne!(pre.generation, post.generation);
        assert!(!post.unchanged_from(&pre), "a remount is a change");
        assert!(post.remounted_from(&pre));
    }

    /// A real step transition changes what is shown, so it is a change but NOT
    /// a remount — a driver must be able to tell the two apart.
    #[test]
    fn content_change_is_not_reported_as_a_remount() {
        let pre = snapshot_signature(&tier_step_snapshot(1000, "How will you use Qontinui?"));
        let post = snapshot_signature(&tier_step_snapshot(1000, "Welcome to Qontinui"));
        assert!(!post.unchanged_from(&pre));
        assert!(!post.remounted_from(&pre));
    }

    #[test]
    fn element_count_change_is_not_reported_as_a_remount() {
        let pre = snapshot_signature(&tier_step_snapshot(1000, "How will you use Qontinui?"));
        let post = snapshot_signature(&json!({ "elements": [] }));
        assert!(!post.unchanged_from(&pre));
        assert!(!post.remounted_from(&pre));
    }

    /// A serializer that does not emit `registeredAt` must never produce a
    /// spurious remount verdict — it just falls back to content-only.
    #[test]
    fn missing_registered_at_never_reports_a_remount() {
        let no_gen = json!({
            "elements": [
                { "id": "a", "category": "interactive", "state": { "textContent": "x" } }
            ]
        });
        let pre = snapshot_signature(&no_gen);
        let post = snapshot_signature(&no_gen);
        assert!(post.unchanged_from(&pre));
        assert!(!post.remounted_from(&pre));
    }
}
