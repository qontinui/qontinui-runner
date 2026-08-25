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

use axum::{http::StatusCode, response::Json};

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::str_utils::truncate_str_ellipsis;

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

/// FNV-1a 64-bit offset basis. Part of the normative snapshot-signature
/// spec v1 — see [`SnapshotSignature`].
const FNV1A64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a 64-bit prime. Part of the normative snapshot-signature spec v1.
const FNV1A64_PRIME: u64 = 0x0000_0100_0000_01b3;

/// The one fold the snapshot signature is allowed to use.
///
/// Deliberately NOT `DefaultHasher`. That was the previous implementation and
/// it made the "one definition, not two" requirement unachievable: `std`'s
/// `DefaultHasher` is SipHash-1-3 under a fixed-but-**unspecified** key whose
/// output Rust does not guarantee across releases, so no TypeScript (or any
/// other) implementation could ever reproduce it. FNV-1a-64 is four lines of
/// arithmetic in every language and is pinned here by golden vectors.
#[derive(Debug, Clone, Copy)]
struct Fnv1a64(u64);

impl Fnv1a64 {
    fn new() -> Self {
        Self(FNV1A64_OFFSET_BASIS)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(FNV1A64_PRIME);
        }
    }

    fn finish(self) -> u64 {
        self.0
    }
}

/// Render `n` in lowercase base-36, the `count` component of a snapshot id.
///
/// Hand-rolled because Rust has no `to_string_radix`; the spec names
/// JavaScript's `Number.prototype.toString(36)`, which is lowercase and
/// unpadded, and this must match it byte for byte.
fn to_base36(mut n: usize) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[n % 36]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("base36 digits are ASCII")
}

/// Cheap fingerprint of a discover snapshot: the identity of a snapshot, and
/// the click-had-no-effect / remount detector's comparison token.
///
/// > **The fold is a cross-language SPECIFICATION, not an implementation
/// > detail.** It is "snapshot signature spec v1", normative in plan
/// > `2026-08-20-ui-bridge-snapshot-identity-and-selector-candidates`, and the
/// > `ui-bridge` TypeScript SDK implements the same fold over the same fields
/// > in the same order. The two sides are pinned to each other by the golden
/// > vectors in `snapshot-signature-golden.json` (committed beside this file
/// > as `fixtures/snapshot-signature-golden.json` and asserted by
/// > `golden_vectors_*` below). Changing any byte of the fold — a field, its
/// > order, its terminator, the endianness of `registeredAt` — breaks the SDK
/// > silently and must be a v2 with a new id prefix, never an edit in place.
///
/// **Spec v1.** Iterate `elements` in array order. Per element, feed these
/// byte sequences, in this order, into the two independent FNV-1a-64 states:
///
/// | Field | Condition | Bytes | Into |
/// |---|---|---|---|
/// | `id` | is a string | `utf8(id)` then `0xFF` | content *and* generation |
/// | `category` | is a string | `utf8(category)` then `0xFF` | content |
/// | `state.textContent` | is a string | `utf8(value)` then `0xFF` | content |
/// | `state.ariaPressed` | is a boolean | one byte, `0x01` / `0x00` | content |
/// | `registeredAt` | is an integer ≥ 0 | the u64 as 8 bytes **little-endian** | generation |
///
/// A field that is absent or of the wrong type contributes **no bytes at
/// all**. That is load-bearing, not laziness: a serializer that omits
/// `registeredAt` then folds nothing into `generation` beyond the ids, so it
/// simply never reports a remount rather than reporting a spurious one.
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
/// Two residuals, inherited and NOT fixed by spec v1. Anything built on this
/// signature — the pre-action staleness gate included — is a strong signal,
/// not a total guarantee, and must say so rather than implying otherwise.
///
/// 1. **Millisecond resolution.** `registeredAt` is millisecond-resolution, so
///    a remount completing inside the same millisecond is still invisible.
///    Every observed one has been ≥1ms.
/// 2. **Unobserved content change.** `count` and `generation` depend only on
///    `id` and `registeredAt`, so a *mount-only* fold — no DOM access at all —
///    reproduces both exactly, which is why element-set churn and remounts are
///    catchable on any path, however cheap. `content` is not reproducible that
///    way: it needs the elements' rendered state, so a pure content change is
///    only visible to a caller that actually took a newer snapshot. This route
///    always takes one (the pre-action discover), so all three change kinds
///    are live here; a path that stamps no intervening snapshot — the SDK's
///    in-process executor — can only see the first two. Nothing observed the
///    change, so nothing can prove it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SnapshotSignature {
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

    /// Why `self` (the world as it stands) supersedes `cited` (the snapshot a
    /// caller reasoned from), or `None` when it does not.
    ///
    /// The classification is derived entirely from the two predicates above —
    /// it does not re-derive the remount test, it asks
    /// [`Self::remounted_from`] — so the pre-action gate and the post-action
    /// `expectChange` report can never disagree about what a remount is.
    ///
    /// - `remounted` — the case nothing else in the stack can catch: the
    ///   elements still resolve and still show the same thing, so
    ///   `ELEMENT_STALE` stays silent, but they were re-registered under a new
    ///   mount and any state inside that subtree is gone.
    /// - `elementCountChanged` — elements appeared or disappeared.
    /// - `contentChanged` — same number of elements, showing something else.
    pub fn superseded_reason(&self, cited: &Self) -> Option<&'static str> {
        if self.unchanged_from(cited) {
            None
        } else if self.remounted_from(cited) {
            Some("remounted")
        } else if self.count != cited.count {
            Some("elementCountChanged")
        } else {
            Some("contentChanged")
        }
    }

    /// The content-addressed snapshot id: `ubs1_<count36>_<content>_<generation>`.
    ///
    /// Content-addressed, not a counter, and that is the point: two ids alone
    /// answer both questions the structured value answers, with no shared
    /// object and no server-side snapshot table. *Equal* means nothing
    /// observable changed ([`Self::unchanged_from`]); *same count and content,
    /// different generation* means a REMOUNT ([`Self::remounted_from`]). It
    /// also fixes the defect in the AI layer's `snapshot-<counter>-<Date.now()>`
    /// id (`ui-bridge/.../ai/semantic-snapshot.ts`), whose counter is
    /// per-instance and therefore collides across processes.
    ///
    /// `ubs1` is the spec version. A future fold change takes `ubs2`; it never
    /// edits v1 in place, because an id minted by one side and compared by the
    /// other is exactly the cross-language contract the golden vectors pin.
    pub fn snapshot_id(&self) -> String {
        format!(
            "ubs1_{}_{:016x}_{:016x}",
            to_base36(self.count),
            self.content,
            self.generation
        )
    }

    /// Parse an id minted by [`Self::snapshot_id`] back into the signature it
    /// addresses, so a cited id can be compared with [`Self::unchanged_from`]
    /// / [`Self::remounted_from`] instead of by string equality — the caller
    /// gets *why* its snapshot is stale, not just *that* it is.
    ///
    /// Returns `None` for anything that is not a well-formed v1 id (wrong
    /// prefix, wrong arity, non-hex halves, count that does not parse). A
    /// caller that supplied a malformed id must be told so explicitly rather
    /// than have it silently treated as "no id supplied".
    pub fn from_snapshot_id(id: &str) -> Option<Self> {
        let mut parts = id.split('_');
        if parts.next()? != "ubs1" {
            return None;
        }
        let count_raw = parts.next()?;
        let content_raw = parts.next()?;
        let generation_raw = parts.next()?;
        if parts.next().is_some() {
            return None;
        }
        if count_raw.is_empty()
            || !count_raw
                .bytes()
                .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
        {
            return None;
        }
        let count = usize::from_str_radix(count_raw, 36).ok()?;
        if content_raw.len() != 16 || generation_raw.len() != 16 {
            return None;
        }
        Some(Self {
            count,
            content: u64::from_str_radix(content_raw, 16).ok()?,
            generation: u64::from_str_radix(generation_raw, 16).ok()?,
        })
    }
}

/// Compute the spec-v1 signature of a snapshot payload. See
/// [`SnapshotSignature`] for the normative fold — this function IS that spec,
/// and the golden-vector tests below are what keep it and the TypeScript SDK
/// from drifting.
pub(crate) fn snapshot_signature(snapshot: &serde_json::Value) -> SnapshotSignature {
    /// A field's byte sequence is terminated so that `{id:"ab",category:"c"}`
    /// and `{id:"abc"}` cannot fold to the same bytes. `0xFF` is never a valid
    /// UTF-8 byte, so no string value can contain it.
    const FIELD_TERMINATOR: u8 = 0xFF;

    let elements = snapshot
        .get("elements")
        .and_then(|v| v.as_array())
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    let mut content = Fnv1a64::new();
    let mut generation = Fnv1a64::new();
    for el in elements {
        if let Some(s) = el.get("id").and_then(|v| v.as_str()) {
            content.write(s.as_bytes());
            content.write(&[FIELD_TERMINATOR]);
            // The id is folded into BOTH hashes so a generation change is
            // always attributable to a specific element rather than to a
            // reordering of the array.
            generation.write(s.as_bytes());
            generation.write(&[FIELD_TERMINATOR]);
        }
        if let Some(s) = el.get("category").and_then(|v| v.as_str()) {
            content.write(s.as_bytes());
            content.write(&[FIELD_TERMINATOR]);
        }
        if let Some(s) = el
            .get("state")
            .and_then(|v| v.get("textContent"))
            .and_then(|v| v.as_str())
        {
            content.write(s.as_bytes());
            content.write(&[FIELD_TERMINATOR]);
        }
        if let Some(b) = el
            .get("state")
            .and_then(|v| v.get("ariaPressed"))
            .and_then(|v| v.as_bool())
        {
            content.write(&[u8::from(b)]);
        }
        // Absent on a serializer that does not emit it — contributes NO bytes,
        // so such a snapshot simply never reports a remount rather than
        // reporting a spurious one. `as_u64` is also the spec's "integer ≥ 0"
        // predicate: it rejects negatives, fractions, strings and booleans.
        if let Some(n) = el.get("registeredAt").and_then(|v| v.as_u64()) {
            generation.write(&n.to_le_bytes());
        }
    }
    SnapshotSignature {
        count: elements.len(),
        content: content.finish(),
        generation: generation.finish(),
    }
}

/// The ONE `discover` IPC payload a **citable** snapshot id is minted over.
///
/// Two call sites must agree byte for byte or the whole staleness gate
/// misfires: `ui_bridge_discover_handler`, which hands the caller a
/// `snapshotId`, and `execute_action`'s pre-action snapshot, which compares
/// the caller's cited id against the world as it stands. A signature is only
/// comparable with another signature over the same element SET, so "the same
/// discover options" is a correctness requirement, not tidiness — hence one
/// constructor rather than two literals that look alike today.
///
/// `force` is the one legitimate difference between the two: it triggers a
/// registry clear + rescan before the read, which changes *when* the elements
/// were registered but not *which* elements are returned, so a forced discover
/// still mints a citable id.
///
/// Any narrowing option (`root`, `selector`, `types`, `limit`,
/// `includeHidden`, or `interactiveOnly: true`) produces a DIFFERENT element
/// set, and an id minted over it would be refused by the gate every single
/// time. Such a discover reports `snapshotIdCitable: false` instead.
pub(super) fn citable_snapshot_discover_payload(force: bool) -> serde_json::Value {
    serde_json::json!({
        "options": {
            "root": serde_json::Value::Null,
            "interactiveOnly": false,
            "includeHidden": serde_json::Value::Null,
            "limit": serde_json::Value::Null,
            "types": serde_json::Value::Null,
            "selector": serde_json::Value::Null
        },
        "force": force
    })
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

/// Parse the JSON a `/control/page/*` convenience route expects back from
/// [`evaluate_js_expression`] / [`evaluate_js_expression_in_window`], reporting
/// an unparseable result as the **runner-side failure it is**.
///
/// THE DEFECT this closes: every one of these routes used to write
///
/// ```ignore
/// let parsed = serde_json::from_str(&raw)
///     .unwrap_or(serde_json::json!({"clicked": false, "error": "Parse error"}));
/// Ok(Json(ApiResponse::success(parsed)))
/// ```
///
/// — an **HTTP 200 `success: true`** for a call whose result the runner could
/// not even read. The failure was not hypothetical: `evaluate_js_expression`
/// falls back to [`direct_webview_evaluate_with_result`], which returns the raw
/// string the WebView produced, so anything that is not the handler's own
/// `JSON.stringify(...)` — `undefined` from a swallowed exception, an error
/// text, a truncated payload — landed in that arm. The caller was told the
/// click succeeded and handed `{"clicked": false, "error": "Parse error"}` as
/// the *success* payload; `"Parse error"` also names no route, no cause and no
/// offending text, so it could not be diagnosed from the response either.
///
/// `type-into` stopped laundering this in the same change that gave the
/// envelope seam its verdict (`extract_response_data`, plan
/// `2026-08-26-prompts-panel-manual-test-remediation`) but its four sibling
/// routes kept the old arm. This is that fix, made shared rather than inlined a
/// fifth time.
///
/// `route` names the endpoint and `what` the thing being parsed, so the message
/// reads e.g. `type-into: could not parse the element-resolution result (…)`.
/// The raw text is echoed (truncated) because the whole point is to make the
/// failure diagnosable.
pub(super) fn parse_eval_result(
    route: &str,
    what: &str,
    raw: &str,
) -> Result<serde_json::Value, (StatusCode, Json<ApiResponse<()>>)> {
    serde_json::from_str(raw).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "{route}: could not parse the {what} ({e}); raw: {}",
                truncate_str_ellipsis(raw, 200)
            ))),
        )
    })
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
    use super::{
        parse_eval_result, read_window_label, return_expression_js, snapshot_signature,
        SnapshotSignature,
    };
    use axum::http::StatusCode;
    use serde_json::json;
    use std::ops::Deref;

    // ── Unparseable eval results are runner failures, not successes ─────────
    //
    // THE DEFECT: `click-by-text`, `click-by-selector`, `read-value` and
    // `page/summary` each answered HTTP 200 `success: true` carrying
    // `{"clicked"|"found": false, "error": "Parse error"}` when the runner
    // could not parse what the WebView returned. `type-into` was fixed in the
    // change that introduced `extract_response_data`; these four were not.

    #[test]
    fn a_well_formed_result_parses_through_untouched() {
        let parsed = parse_eval_result("click-by-text", "click result", r#"{"clicked":true}"#)
            .expect("valid JSON must parse");
        assert_eq!(parsed, json!({ "clicked": true }));
    }

    /// The exact string the direct-WebView fallback yields when the injected
    /// expression throws and its result is swallowed. The old arm turned this
    /// into an HTTP 200 that claimed success.
    #[test]
    fn undefined_from_the_webview_fallback_is_a_failure_not_a_success() {
        let (status, body) = parse_eval_result("click-by-text", "click result", "undefined")
            .expect_err("an unparseable result must not answer HTTP 200");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let inner = body.deref();
        assert!(!inner.success);
        let msg = inner.error.as_deref().unwrap_or_default();
        assert!(
            msg.contains("click-by-text") && msg.contains("click result"),
            "the message must name the route and what failed to parse, got: {msg}"
        );
        assert!(
            msg.contains("undefined"),
            "the raw text must be echoed so the failure is diagnosable, got: {msg}"
        );
    }

    /// Every route that shares the helper names ITSELF, so a caller reading a
    /// 500 can tell which endpoint produced it — `"Parse error"` named none.
    #[test]
    fn each_route_names_itself_in_the_failure() {
        for (route, what) in [
            ("click-by-text", "click result"),
            ("click-by-selector", "click result"),
            ("read-value", "read result"),
            ("page/summary", "page summary"),
            ("type-into", "element-resolution result"),
        ] {
            let (status, body) = parse_eval_result(route, what, "<!doctype html>")
                .expect_err("unparseable input must fail");
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
            let msg = body.deref().error.clone().unwrap_or_default();
            assert!(
                msg.starts_with(route),
                "expected {route:?} first in {msg:?}"
            );
            assert!(msg.contains(what), "expected {what:?} in {msg:?}");
        }
    }

    /// A megabyte of unparseable HTML must not be copied into the error body.
    #[test]
    fn the_echoed_raw_text_is_truncated() {
        let raw = "x".repeat(10_000);
        let (_, body) = parse_eval_result("read-value", "read result", &raw)
            .expect_err("unparseable input must fail");
        let msg = body.deref().error.clone().unwrap_or_default();
        assert!(
            msg.len() < 500,
            "the raw echo must be truncated, got {} bytes",
            msg.len()
        );
    }

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

    // ---- snapshot signature spec v1: the cross-repo golden vectors -----
    //
    // THIS is the contract with the `ui-bridge` TypeScript SDK. Both sides
    // fold the same fixture and must produce identical hex. A prose "keep
    // these in sync" comment is precisely the drift these vectors exist to
    // prevent — if one of these fails, do NOT re-baseline it, because the
    // other repo is asserting the same numbers.

    /// The fixture is embedded rather than read from disk so the assertion
    /// cannot silently vanish when the test runs from a different cwd — a
    /// conformance test that can skip itself is worse than none.
    const GOLDEN_FIXTURE: &str = include_str!("fixtures/snapshot-signature-golden.json");

    fn golden_cases() -> serde_json::Map<String, serde_json::Value> {
        serde_json::from_str::<serde_json::Value>(GOLDEN_FIXTURE)
            .expect("golden fixture is valid JSON")
            .as_object()
            .expect("golden fixture is a JSON object of named cases")
            .clone()
    }

    /// The full roster. Named explicitly so removing a case from the fixture
    /// fails here instead of quietly shrinking the loop below to nothing.
    const GOLDEN_CASE_NAMES: &[&str] = &[
        "empty",
        "single_minimal",
        "single_full",
        "two_elements",
        "remount_of_two_elements",
        "missing_registeredAt",
        "wrong_types_ignored",
    ];

    #[test]
    fn golden_fixture_carries_every_expected_case() {
        let cases = golden_cases();
        for name in GOLDEN_CASE_NAMES {
            assert!(
                cases.contains_key(*name),
                "golden fixture is missing case {name:?} — the SDK asserts it too"
            );
        }
        assert_eq!(
            cases.len(),
            GOLDEN_CASE_NAMES.len(),
            "golden fixture gained a case this test does not name: {:?}",
            cases.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn golden_vectors_match_spec_v1() {
        for (name, case) in golden_cases() {
            let sig = snapshot_signature(&json!({ "elements": case["elements"].clone() }));
            assert_eq!(
                sig.count,
                case["count"].as_u64().expect("count") as usize,
                "{name}: count"
            );
            assert_eq!(
                format!("{:016x}", sig.content),
                case["content"].as_str().expect("content"),
                "{name}: content hash"
            );
            assert_eq!(
                format!("{:016x}", sig.generation),
                case["generation"].as_str().expect("generation"),
                "{name}: generation hash"
            );
            assert_eq!(
                sig.snapshot_id(),
                case["snapshotId"].as_str().expect("snapshotId"),
                "{name}: snapshot id"
            );
        }
    }

    /// THE case the whole plan is about, asserted against the shared fixture
    /// rather than a hand-built pair: the same two elements showing the same
    /// thing, re-registered under a new mount. `content` must be IDENTICAL,
    /// `generation` must DIFFER, and `remounted_from` must say so.
    #[test]
    fn golden_remount_case_is_reported_as_a_remount() {
        let cases = golden_cases();
        let pre = snapshot_signature(&json!({ "elements": cases["two_elements"]["elements"] }));
        let post = snapshot_signature(
            &json!({ "elements": cases["remount_of_two_elements"]["elements"] }),
        );

        assert_eq!(pre.content, post.content, "content is identical across the remount");
        assert_ne!(pre.generation, post.generation, "generation moves on a remount");
        assert!(!post.unchanged_from(&pre), "a remount is a change");
        assert!(
            post.remounted_from(&pre),
            "the golden remount case must satisfy remounted_from — this is the predicate the \
             pre-action staleness gate is built on"
        );
    }

    /// An empty snapshot folds to the bare offset basis in both states. Pinned
    /// separately because it is the one vector a broken `Fnv1a64::new` would
    /// still pass by accident if the loop never ran.
    #[test]
    fn empty_snapshot_is_the_bare_offset_basis() {
        let sig = snapshot_signature(&json!({ "elements": [] }));
        assert_eq!(sig.count, 0);
        assert_eq!(format!("{:016x}", sig.content), "cbf29ce484222325");
        assert_eq!(format!("{:016x}", sig.generation), "cbf29ce484222325");
        // A payload with no `elements` key at all is the same as an empty one.
        assert!(snapshot_signature(&json!({})).unchanged_from(&sig));
    }

    // ---- snapshot id round-trip ---------------------------------------

    #[test]
    fn snapshot_id_round_trips_through_from_snapshot_id() {
        for (name, case) in golden_cases() {
            let sig = snapshot_signature(&json!({ "elements": case["elements"].clone() }));
            let parsed = SnapshotSignature::from_snapshot_id(&sig.snapshot_id())
                .unwrap_or_else(|| panic!("{name}: id must parse back"));
            assert!(parsed.unchanged_from(&sig), "{name}: round-trip must be lossless");
        }
    }

    /// `count` is base-36, so a three-digit count must not be read as decimal.
    #[test]
    fn snapshot_id_count_is_base36() {
        let sig = SnapshotSignature {
            count: 1295, // 36^2 - 1 => "zz"
            content: 1,
            generation: 2,
        };
        assert_eq!(
            sig.snapshot_id(),
            "ubs1_zz_0000000000000001_0000000000000002"
        );
        assert_eq!(
            SnapshotSignature::from_snapshot_id(&sig.snapshot_id()).map(|s| s.count),
            Some(1295)
        );
    }

    /// A malformed id must be rejected, never silently read as "no id given" —
    /// the gate has to be able to tell a typo from an omission.
    #[test]
    fn malformed_snapshot_ids_are_rejected() {
        for bad in [
            "",
            "ubs1",
            "ubs1_1_deadbeef",                             // too few parts
            "ubs1_1_0000000000000001_0000000000000002_x",  // too many parts
            "ubs2_1_0000000000000001_0000000000000002",    // wrong version
            "ubs1__0000000000000001_0000000000000002",     // empty count
            "ubs1_1_000000000000001_0000000000000002",     // 15 hex chars
            "ubs1_1_0000000000000001_000000000000000g",    // non-hex
            "ubs1_-1_0000000000000001_0000000000000002",   // negative count
            "ubs1_1_0000000000000001_0000000000000002 ",   // trailing space
        ] {
            assert!(
                SnapshotSignature::from_snapshot_id(bad).is_none(),
                "{bad:?} must not parse as a snapshot id"
            );
        }
    }
}
