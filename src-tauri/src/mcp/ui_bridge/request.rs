//! Core IPC request machinery for the UI Bridge.
//!
//! Hosts the main `ui_bridge_request_sync` entrypoint used by every handler,
//! the response dispatcher called from the Tauri event listener, and helpers
//! for readiness diagnostics and error classification wrapping.

use axum::{http::StatusCode, response::Json};
use std::sync::Arc;
use tauri::Emitter;
use tracing::{debug, error, info, warn};

use crate::mcp::types::{api_error_detailed, ApiResponse, ApiState};
use crate::timeout_config::Timeouts;

use super::circuit_breaker::CircuitBreakerState;
use super::types::{classify_transport_error, UiBridgeErrorCode};

/// UI Bridge timeout is fetched from centralized config
/// This needs a reasonable timeout since it's synchronous communication with the frontend.
pub(super) fn get_ui_bridge_timeout_ms() -> u64 {
    Timeouts::ui_bridge_ipc().as_millis() as u64
}

/// The one request type whose frontend handler runs a timer of its OWN against
/// this envelope's wait, and so has to be told what that wait is.
///
/// `usePageEvents.ts::page_evaluate` auto-awaits a top-level Promise and gives
/// up on its own schedule. Every other handler answers as soon as it can and
/// only ever loses the race by being slow, so only this one needs the budget.
const EVALUATE_REQUEST_TYPE: &str = "page_evaluate";

/// Wire field carrying [`get_ui_bridge_timeout_ms`] to the frontend. Spelled
/// exactly as the TAGGED `/control/page/evaluate` route spells it
/// (`page.rs::tagged_page_evaluate`), so `describeEvaluateBudget` reads one
/// vocabulary on both routes instead of two.
const EVALUATE_TIMEOUT_FIELD: &str = "timeout_ms";

/// Wire field marking [`EVALUATE_TIMEOUT_FIELD`] as a budget nobody chose.
/// Always `true` here — see [`stamp_legacy_evaluate_budget`].
const EVALUATE_TIMEOUT_FROM_DEFAULT_FIELD: &str = "timeout_from_default";

/// Tell a legacy `page_evaluate` request how long this envelope will actually
/// wait for it.
///
/// The frontend handler auto-awaits a top-level Promise and, having no idea
/// what the Rust side's budget was, awaited a hardcoded 30 s. This envelope
/// waits [`get_ui_bridge_timeout_ms`] — 10 s by default. So the two ends
/// disagreed by twenty seconds in the WRONG direction: the Rust side gave up
/// first every time, and:
///
///   * the caller got the generic `"UI Bridge request timed out after 10000ms.
///     Is the frontend running?"` — which is not merely terse but actively
///     false, since the frontend is running and still awaiting;
///   * the frontend's own message — the budget, its provenance, and the field
///     that raises it — was unreachable on this route, dead configuration
///     rather than the diagnostic it was written to be; and
///   * the frontend went on to answer at up to 30 s into a pending slot the
///     timeout had already removed, so the reply was silently dropped.
///
/// Sending the real budget puts the frontend inside it by
/// `PAGE_EVALUATE_TIMEOUT_MARGIN_MS` (250 ms, subtracted frontend-side), which
/// is the same margin the tagged route uses to make the precise message win the
/// race deterministically.
///
/// The budget is stamped UNCONDITIONALLY, overwriting anything the caller sent.
/// That is not a caller's choice to honour on this route: the wait is
/// `get_ui_bridge_timeout_ms()` whatever the payload says, so honouring a larger
/// caller-supplied number would re-open the same gap one field along. For the
/// same reason it is always flagged as a default — a legacy-route caller cannot
/// choose a budget, so telling them it "came from the `timeoutMs` you sent"
/// would be the misattribution the tagged route's message was rewritten to
/// remove. The remediation the frontend prints names the tagged route, which is
/// the honest advice: to pick a budget, switch to
/// `POST /ui-bridge/control/page/evaluate`.
///
/// Consumer: `describeEvaluateBudget` in
/// `src/hooks/ui-bridge-events/utils.ts`, via the legacy `ui-bridge-request`
/// envelope's flattened payload.
fn stamp_legacy_evaluate_budget(
    request_type: &str,
    mut payload: serde_json::Value,
) -> serde_json::Value {
    if request_type != EVALUATE_REQUEST_TYPE {
        return payload;
    }
    // A non-Object payload flattens to nothing on the wire anyway, so there is
    // no place to put the budget and nothing to correct.
    if let Some(obj) = payload.as_object_mut() {
        obj.insert(
            EVALUATE_TIMEOUT_FIELD.to_string(),
            serde_json::Value::from(get_ui_bridge_timeout_ms()),
        );
        obj.insert(
            EVALUATE_TIMEOUT_FROM_DEFAULT_FIELD.to_string(),
            serde_json::Value::Bool(true),
        );
    }
    payload
}

/// Default window label for single-window operation
/// (`plans/2026-06-03-runner-popout-terminal-windows.md`). A request with no
/// explicit target routes here, keeping single-window behavior byte-identical.
pub(crate) const MAIN_WINDOW_LABEL: &str = "main";

/// Reserved payload field that routes a request to a specific pop-out window.
///
/// A UI Bridge control endpoint addresses a pop-out window (discoverable via
/// `GET /ui-bridge/control/runner-windows`) by including
/// `{ "windowLabel": "term-1" }` in the request payload. `ui_bridge_request_sync`
/// consumes the field via [`split_target_window`] so it never reaches the
/// frontend handler as request *data*; [`ui_bridge_request_inner`] re-attaches it
/// to the emitted envelope so the addressed window's listener can match it
/// against its own `getCurrentWindow().label`. The name matches the routing field
/// the frontend already echoes on responses, so request → emit → response use one
/// field end-to-end.
pub(crate) const TARGET_WINDOW_FIELD: &str = "windowLabel";

/// Stamp a target window onto a request payload, for handlers that build a typed
/// payload (so a client's `windowLabel` would otherwise be dropped by struct
/// deserialization). No-op when `window_label` is `None`, empty, or
/// [`MAIN_WINDOW_LABEL`], so the single-window default payload is unchanged. A
/// `null` base is promoted to `{}` first; a non-object, non-null base is returned
/// untouched (it can't carry a field). Centralizes the field name so endpoints
/// don't hand-roll the routing convention.
pub(crate) fn target_window_payload(
    mut base: serde_json::Value,
    window_label: Option<&str>,
) -> serde_json::Value {
    let label = match window_label {
        Some(l) if !l.is_empty() && l != MAIN_WINDOW_LABEL => l,
        _ => return base,
    };
    if base.is_null() {
        base = serde_json::json!({});
    }
    if let Some(obj) = base.as_object_mut() {
        obj.insert(
            TARGET_WINDOW_FIELD.to_string(),
            serde_json::Value::String(label.to_string()),
        );
    }
    base
}

/// Step keys that address the *step* rather than the action it carries.
///
/// Used by [`step_action_payload`] to decide what to lift into the action
/// envelope. Everything NOT listed here rides along, so a field added to the
/// SDK action grammar reaches the frontend without a change to this file.
///
/// To be precise about what that buys: it stops the RUNNER hop dropping the
/// field. `useControlEvents.ts` still forwards only `action`/`params`/
/// `waitOptions` to `executeAction`, so delivering a new opt-in end-to-end
/// needs the frontend change too — this just means it is no longer gone
/// before it gets there (see [`element_action_payload`]).
const STEP_RESERVED_KEYS: &[&str] = &["type", "elementId", "element_id", "label"];

/// Build the `execute_action` IPC payload for an element action.
///
/// **This is the one place the element-action wire convention is spelled.**
/// The frontend handler (`src/hooks/ui-bridge-events/useControlEvents.ts`,
/// `case "execute_action"`) destructures `{ elementId, action }` and rejects
/// the request outright when either is missing. It then normalizes `action` as
/// *either* a bare string *or* the whole envelope
/// `{ action, params, waitOptions, ... }` — and reads `params` / `waitOptions`
/// **only off that envelope**. A sibling `params` at the top level of the
/// payload is never read by anything.
///
/// Hand-rolling this payload is therefore a trap with two distinct failure
/// modes, and both were live defects fixed alongside this helper
/// (`plans/2026-08-25-ui-bridge-request-path-loses-fields-structurally.md`):
///
/// 1. Emitting `id` instead of `elementId` fails **every** request with
///    `"elementId and action are required"` — a hard, total failure.
/// 2. Flattening the envelope to a bare action string silently drops `params`,
///    `waitOptions` and every opt-in field. The request still looks well-formed
///    to the receiver, so it succeeds *wrongly*: a `type` action types nothing.
///
/// So the envelope is forwarded **by identity**, never rebuilt field-by-field.
/// That makes this hop lossless; it does not by itself make a new field
/// *effective*, since the frontend handler picks the fields it forwards on.
///
/// The one deliberate exception is `windowLabel`, hoisted out of the envelope
/// to the payload root below — it is declared on the SDK envelope but consumed
/// at the root, so forwarding it verbatim is precisely what makes it inert.
/// A bare-string envelope passes through untouched (the frontend normalizes it).
/// An envelope with no action name defaults to `click`, preserving the batch
/// handlers' historical default rather than dispatching `undefined`.
pub(crate) fn element_action_payload(
    element_id: &str,
    action: serde_json::Value,
) -> serde_json::Value {
    let action = match action {
        // The SDK proxy-fallback shape. The frontend turns `"click"` into
        // `{ action: "click" }` itself, so pass it through rather than
        // second-guessing it here.
        serde_json::Value::String(_) => action,
        serde_json::Value::Object(mut obj) => {
            // Default on "no usable action NAME", not on "key absent". A
            // present-but-non-string `action` (a `null` from a serializer
            // emitting an absent optional, say) must land on the same default
            // the hand-rolled `as_str().unwrap_or("click")` gave it — an
            // occupied-entry check would forward `{"action": null}`, which the
            // frontend rejects as "Action 'null' is not allowed".
            if obj.get("action").and_then(|v| v.as_str()).is_none() {
                obj.insert("action".to_string(), serde_json::json!("click"));
            }
            serde_json::Value::Object(obj)
        }
        // null / bool / number / array cannot carry an action name.
        _ => serde_json::json!({ "action": "click" }),
    };

    // `windowLabel` is DECLARED on the SDK's `ControlActionRequest` envelope
    // ("carried for transports that forward the request bag verbatim"), but it
    // is consumed at the payload ROOT by `split_target_window` — so an envelope
    // that merely carries it routes nothing and the step silently runs against
    // the main window. Forwarding the envelope whole is what makes that
    // reachable, so hoist it here, at the single chokepoint, rather than
    // leaving the one declared envelope field that does nothing.
    let mut action = action;
    let window_label = action
        .as_object_mut()
        .and_then(|obj| obj.remove(TARGET_WINDOW_FIELD))
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|s| !s.is_empty());

    target_window_payload(
        serde_json::json!({
            "elementId": element_id,
            "action": action,
        }),
        window_label.as_deref(),
    )
}

/// Build the `execute_action` payload for one batch step, in either grammar.
///
/// The batch endpoints disagree about how a step carries its action — which is
/// the drift this whole helper family exists to absorb — so both are accepted:
///
/// * **nested** (`{"elementId": "x", "action": {"action": "type", "params": {...}}}`),
///   used by `/control/batch-actions`' SDK caller. The envelope is forwarded by
///   identity.
/// * **flat** (`{"type": "action", "element_id": "x", "action": "type", "params": {...}}`),
///   used by `/control/batch-execute` and documented for `/control/batch`. Every
///   non-reserved key is lifted into the envelope, so `params`, `waitOptions`,
///   `expectChange` and any later opt-in ride along instead of being dropped by
///   a hand-maintained field list.
///
/// A nested envelope wins over a flat sibling of the same name; flat keys only
/// fill gaps. Accepting both matters because the runner's own capabilities
/// manifest documents `/control/batch-actions` steps as FLAT while the SDK type
/// is nested — so a caller following either is served, instead of one of them
/// silently losing its params.
///
/// The element id is read from `elementId` **or** `element_id`: the two
/// capability batch endpoints disagreed on the casing (one read each), so both
/// spellings are accepted rather than breaking whichever callers exist. Each
/// arm is checked for a *string*, so a `null` under the preferred spelling
/// falls through to the other rather than resolving to `""`.
pub(crate) fn step_action_payload(step: &serde_json::Value) -> serde_json::Value {
    let element_id = step
        .get("elementId")
        .and_then(|v| v.as_str())
        .or_else(|| step.get("element_id").and_then(|v| v.as_str()))
        .unwrap_or_default();

    // Base: a nested `action` envelope if the step carries one.
    let mut envelope = match step.get("action") {
        Some(serde_json::Value::Object(nested)) => nested.clone(),
        _ => serde_json::Map::new(),
    };

    if let Some(obj) = step.as_object() {
        for (key, value) in obj {
            if STEP_RESERVED_KEYS.contains(&key.as_str()) {
                continue;
            }
            // An explicitly nested envelope wins over a flat sibling of the
            // same name; flat keys only fill gaps.
            if key == "action" && value.is_object() {
                continue;
            }
            envelope.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }

    element_action_payload(element_id, serde_json::Value::Object(envelope))
}

/// Split an optional `windowLabel` routing field out of a request payload.
///
/// Returns `(target_window, payload_without_label)`. Absent / empty / non-string
/// `windowLabel` (and any non-object payload) yields [`MAIN_WINDOW_LABEL`], so the
/// single-window default is unchanged. Consuming the field here is what lets every
/// handler that forwards a JSON-object payload target a window with zero per-handler
/// plumbing — the request funnels through one chokepoint.
fn split_target_window(payload: serde_json::Value) -> (String, serde_json::Value) {
    if let serde_json::Value::Object(mut map) = payload {
        let label = map
            .remove(TARGET_WINDOW_FIELD)
            .and_then(|v| v.as_str().map(str::to_string))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| MAIN_WINDOW_LABEL.to_string());
        (label, serde_json::Value::Object(map))
    } else {
        (MAIN_WINDOW_LABEL.to_string(), payload)
    }
}

/// Build the `ui_bridge_pending` map key.
///
/// Keying by `(window_label, request_id)` rather than `request_id` alone makes a
/// response from a window the request was NOT addressed to a no-op (its computed
/// key can't match the stored one), which structurally eliminates the broadcast
/// response race once multiple windows mount the SDK. `request_id` is a UUID, so
/// the delimiter can never collide with a (simple `main`/`term-N`) window label.
/// Both dispatch paths (this module and `helpers::direct_webview_evaluate_with_result`)
/// and the response dispatcher route through this single helper so the key shape
/// can never drift between producer and consumer.
pub(crate) fn pending_key(window_label: &str, request_id: &str) -> String {
    format!("{window_label}\u{1f}{request_id}")
}

/// Whether targeted per-window `ui-bridge-request` emit is enabled.
///
/// Default ON now that pop-out windows answer the bridge (capabilities + per-realm
/// port seed shipped). Each request is emitted only to its target window
/// (`app.get_webview_window(label).emit(...)`), falling back to broadcast if the
/// webview can't be resolved (mid-teardown) so a request is never dropped. For the
/// single-window case this targets "main" only — functionally identical to the old
/// broadcast, minus the cross-window noise. Set `QONTINUI_UI_BRIDGE_MULTI_WINDOW=0`
/// (or `false`/`off`) to revert to unconditional broadcast. Read per-call so it can
/// be toggled without a restart.
///
/// `pub(crate)` so the tagged `page/evaluate` dispatcher (which owns its own
/// event pair and store rather than funneling through `ui_bridge_request_inner`)
/// can honor the same flag.
pub(crate) fn multi_window_dispatch_enabled() -> bool {
    std::env::var("QONTINUI_UI_BRIDGE_MULTI_WINDOW")
        .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off")))
        .unwrap_or(true)
}

// ============================================================================
// Frontend readiness classification (shared by every diagnostics surface)
// ============================================================================

/// Hint emitted when the React tree threw and the top-level `ErrorBoundary`
/// is showing its fallback.
///
/// Shared by `gather_readiness_diagnostics` and
/// `super::errors::ui_bridge_diagnostics_handler` so the two ladders can never
/// drift on the one branch that distinguishes "crashed" from "responsive".
pub(super) const TREE_CRASHED_HINT: &str = "React tree threw and the error boundary is showing its fallback — the SDK may still pong; snapshot will be empty. See uiError.";

/// Distinct frontend-liveness states, ordered by diagnostic priority.
///
/// Both diagnostics surfaces (`GET /ui-bridge/diagnostics` and
/// `GET /ui-bridge/diagnostics/readiness`) classify with
/// [`classify_frontend_state`] and then map the state to their own wording, so
/// the *ladder* is shared even though the two response bodies phrase their
/// hints differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrontendState {
    /// The Tauri main WebView window doesn't exist at all.
    WindowMissing,
    /// No pong yet, but the process is <3s old — probably still booting.
    Booting,
    /// No pong ever and console errors were recorded — crashed during mount.
    CrashedDuringMount,
    /// No pong ever and the window isn't visible — WebView may not have rendered.
    WindowNotVisible,
    /// No pong ever, no other corroborating signal.
    NeverPonged,
    /// The React tree threw and the error boundary reported it to Rust.
    ///
    /// This branch sits **above** [`FrontendState::Stale`] and
    /// [`FrontendState::Responsive`] deliberately: the SDK's pong loop can
    /// survive under the error boundary's fallback, so a crashed tree
    /// otherwise reports as responsive forever.
    TreeCrashed,
    /// Ponged at some point, but not in the last 30s.
    Stale,
    /// Ponged recently and no crash reported.
    Responsive,
}

impl FrontendState {
    /// Whether the frontend should be reported as ready.
    ///
    /// `last_pong > 0` is a latch — it flips true on the first pong and is
    /// never reset — so on its own it cannot represent a tree that mounted and
    /// *then* died. [`FrontendState::TreeCrashed`] forces it back to false; a
    /// mounted-but-crashed tree is not ready.
    pub(crate) fn is_ready(self, last_pong: u64) -> bool {
        last_pong > 0 && self != FrontendState::TreeCrashed
    }

    /// Stable wire name for this state.
    ///
    /// `/health` reports it as `frontendState` so a consumer can tell the
    /// distinct reasons a frontend is not ready apart from each other — a
    /// single boolean collapses "still booting", "never mounted", "mounted then
    /// crashed" and "went silent" into one indistinguishable `false`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            FrontendState::WindowMissing => "window_missing",
            FrontendState::Booting => "booting",
            FrontendState::CrashedDuringMount => "crashed_during_mount",
            FrontendState::WindowNotVisible => "window_not_visible",
            FrontendState::NeverPonged => "never_ponged",
            FrontendState::TreeCrashed => "tree_crashed",
            FrontendState::Stale => "stale",
            FrontendState::Responsive => "responsive",
        }
    }
}

/// Inputs to [`classify_frontend_state`]. Grouped into a struct so the
/// call sites can't transpose same-typed positional arguments.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FrontendStateInputs {
    pub window_exists: bool,
    pub window_visible: bool,
    pub last_pong: u64,
    pub last_pong_age_ms: u64,
    pub console_error_count: u64,
    pub process_uptime_ms: u64,
    pub has_ui_error: bool,
}

/// Classify the frontend's liveness from the Rust-side signals only.
///
/// Pure and IPC-free by construction: every input is read from `ApiState` or
/// the Tauri window handle, so this answers correctly even while the React
/// tree is down (which is exactly when it matters).
pub(crate) fn classify_frontend_state(i: FrontendStateInputs) -> FrontendState {
    if !i.window_exists {
        FrontendState::WindowMissing
    } else if i.last_pong == 0 && i.process_uptime_ms < 3000 {
        FrontendState::Booting
    } else if i.last_pong == 0 && i.console_error_count > 0 {
        FrontendState::CrashedDuringMount
    } else if i.last_pong == 0 && !i.window_visible {
        FrontendState::WindowNotVisible
    } else if i.last_pong == 0 {
        FrontendState::NeverPonged
    } else if i.has_ui_error {
        FrontendState::TreeCrashed
    } else if crate::ui_error::ui_stale(
        i.last_pong,
        i.last_pong_age_ms,
        crate::ui_error::UI_STALE_AFTER_MS,
    ) {
        // Same predicate `compute_derived_status` uses, at the diagnostics
        // calibration. The `last_pong == 0` half is belt-and-braces here — the
        // branches above already short-circuit every never-ponged case — but
        // it documents the intent at both call sites and stops the two ladders
        // from drifting if either set of branches is ever reordered.
        FrontendState::Stale
    } else {
        FrontendState::Responsive
    }
}

/// The Rust-side crash signals both diagnostics surfaces join in, so a
/// UI-Bridge driver never has to leave the UI-Bridge routes to find out *why*
/// the page went dark. Produced by [`gather_ui_error_signals`].
pub(super) struct UiErrorSignals {
    /// Whether a React `ErrorBoundary` report is outstanding. Tracked
    /// separately from `json` so a (theoretical) serialization failure can
    /// never silently downgrade a crashed tree to "healthy".
    pub present: bool,
    /// The serialized `UiError`, or `Value::Null` when the tree is healthy.
    pub json: serde_json::Value,
    /// `"healthy" | "degraded" | "errored"` — the same value `/health` and
    /// every heartbeat publish, via [`crate::ui_error::compute_derived_status`].
    pub derived_status: &'static str,
}

/// Read the Rust-side crash signals: the React `ErrorBoundary` report held on
/// `AppState` plus the `derived_status` the `/health` endpoint and every
/// heartbeat already compute from it.
///
/// `last_pong` / `last_pong_age_ms` are passed in rather than re-read here:
/// both callers already computed them for `classify_frontend_state`, and
/// sharing the same pair keeps the diagnostics verdict (`FrontendState`) and
/// the status verdict (`derived_status`) computed off one observation of the
/// atomic.
pub(super) async fn gather_ui_error_signals(
    state: &Arc<ApiState>,
    last_pong: u64,
    last_pong_age_ms: u64,
) -> UiErrorSignals {
    let ui_error = state.app_state.ui_error.get().await;
    let recent_crash = state.app_state.crash_dumps.get().await;
    // `pg_reachable = None`: this signal gatherer does not run a DB round-trip;
    // the bounded PG liveness probe lives only in the `/health` handler (B-5).
    let ui_dead = crate::ui_error::ui_stale(
        last_pong,
        last_pong_age_ms,
        crate::ui_error::UI_DEAD_AFTER_MS,
    );
    let derived_status = crate::ui_error::compute_derived_status(&crate::ui_error::HealthInputs {
        has_ui_error: ui_error.is_some(),
        has_recent_crash: recent_crash.is_some(),
        ui_dead: Some(ui_dead),
        embedding_reachable: crate::mcp_api::embedding_reachable_cached(),
        ..Default::default()
    });
    let json = match &ui_error {
        Some(err) => serde_json::to_value(err).unwrap_or(serde_json::Value::Null),
        None => serde_json::Value::Null,
    };
    UiErrorSignals {
        present: ui_error.is_some(),
        json,
        derived_status,
    }
}

/// Result of [`gather_readiness_diagnostics`].
///
/// Carries the readiness verdict alongside the JSON body so callers that need
/// to branch on it (`ui_bridge_readiness_handler`'s 200-vs-503 decision) read
/// a typed field rather than re-deriving the ladder or string-indexing into
/// the body — either of which would let the two answers drift apart.
pub(super) struct ReadinessDiagnostics {
    /// The `{error, diagnostics: {…}}` body returned to clients.
    pub body: serde_json::Value,
    /// Whether the frontend counts as connected — false for a crashed tree
    /// even while its pong loop is alive.
    pub sdk_connected: bool,
}

/// Gather structured readiness diagnostics when the frontend readiness gate
/// times out. Returns a JSON object with all available diagnostic fields so
/// agents can diagnose why the WebView never became ready.
pub(super) async fn gather_readiness_diagnostics(state: &Arc<ApiState>) -> ReadinessDiagnostics {
    use tauri::Manager;

    let last_pong = state
        .app_state
        .ui_bridge_last_pong
        .load(std::sync::atomic::Ordering::Relaxed);
    let console_error_count = state
        .ui_bridge_console_error_count
        .load(std::sync::atomic::Ordering::Relaxed);
    let cb_state = state.ui_bridge_circuit_breaker.get_state().await;
    let cb_failures = state.ui_bridge_circuit_breaker.get_failure_count().await;
    let pending_count = state
        .ui_bridge_pending_count
        .load(std::sync::atomic::Ordering::Relaxed);
    let available_permits = state.ui_bridge_semaphore.available_permits();
    let process_uptime_ms = state.started_at.elapsed().as_millis() as u64;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let last_pong_age_ms = if last_pong > 0 {
        now_ms.saturating_sub(last_pong)
    } else {
        0
    };

    // Check Tauri main window state
    let main_window = state
        .app_handle
        .get_webview_window(qontinui_runner_lib::get_main_window_label());
    let window_exists = main_window.is_some();
    let window_visible = main_window
        .as_ref()
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false);
    let webview_url = main_window
        .as_ref()
        .and_then(|w| w.url().ok())
        .map(|u| u.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Join the Rust-side crash state in. This survives a dead React tree,
    // which is precisely when the IPC-backed surfaces stop answering.
    let ui_error = gather_ui_error_signals(state, last_pong, last_pong_age_ms).await;

    let frontend_state = classify_frontend_state(FrontendStateInputs {
        window_exists,
        window_visible,
        last_pong,
        last_pong_age_ms,
        console_error_count,
        process_uptime_ms,
        has_ui_error: ui_error.present,
    });
    // `sdk_connected` was `last_pong > 0` — a latch that can never flip back.
    // A crashed tree is not connected even while its pong loop survives.
    let sdk_connected = frontend_state.is_ready(last_pong);

    // Build a human-readable hint based on the diagnostic state
    let hint = match frontend_state {
        FrontendState::WindowMissing => {
            "Main WebView window does not exist — window creation may have failed."
        }
        FrontendState::Booting => {
            "Process just started (<3s uptime). Frontend may still be loading — consider retrying."
        }
        FrontendState::CrashedDuringMount => {
            "Frontend never sent initial pong and console errors were recorded. Check the runner devtools console — frontend likely crashed during mount."
        }
        FrontendState::WindowNotVisible => {
            "Frontend never sent initial pong and main window is not visible — WebView may not have rendered."
        }
        FrontendState::NeverPonged => {
            "Frontend never sent initial pong. Check if the WebView loaded successfully."
        }
        FrontendState::TreeCrashed => TREE_CRASHED_HINT,
        FrontendState::Stale => {
            "Frontend was responsive but stopped responding over 30s ago. It may have crashed or frozen."
        }
        FrontendState::Responsive => {
            "Frontend was responsive recently but the readiness gate was not notified. Possible race condition."
        }
    };

    // Try to read window.__BOOT_ERRORS from the webview (set by index.html's
    // error catcher script in <head>). This captures JS errors that occur during
    // IIFE bundle execution, before the UI Bridge SDK can initialize.
    let boot_errors: serde_json::Value = if let Some(ref win) = main_window {
        match win.eval("window.__QONTINUI_DIAG_CALLBACK && window.__QONTINUI_DIAG_CALLBACK(JSON.stringify(window.__BOOT_ERRORS || []))") {
            _ => {
                // eval() is fire-and-forget in Tauri v2; we can't get return values.
                // Instead, the readiness endpoint reads __BOOT_ERRORS via page/evaluate.
                serde_json::json!(null)
            }
        }
    } else {
        serde_json::json!(null)
    };
    let _ = boot_errors; // reserved for future use when Tauri eval returns values

    ReadinessDiagnostics {
        sdk_connected,
        body: serde_json::json!({
            "error": "frontend_not_ready",
            "diagnostics": {
                "last_pong_age_ms": last_pong_age_ms,
                "window_visible": window_visible,
                "webview_url": webview_url,
                "sdk_connected": sdk_connected,
                "uptime_ms": process_uptime_ms,
                "hint": hint,
                "lastPongMs": last_pong,
                "consoleErrorCount": console_error_count,
                // Rust-side crash state — null when the React tree is healthy.
                "uiError": ui_error.json,
                "derivedStatus": ui_error.derived_status,
                "circuitBreakerState": format!("{:?}", cb_state),
                "circuitBreakerFailures": cb_failures,
                "pendingRequestCount": pending_count,
                "semaphoreAvailablePermits": available_permits,
                "tauriMainWindowExists": window_exists,
                "bootErrorsNote": "Call GET /ui-bridge/control/page/evaluate with expression 'JSON.stringify(window.__BOOT_ERRORS)' to retrieve boot-time JS errors"
            }
        }),
    }
}

/// Send a UI Bridge request and wait for the response synchronously.
///
/// Derives the target window from an optional `windowLabel` field in
/// `additional_payload` (see [`TARGET_WINDOW_FIELD`]) and delegates to
/// [`ui_bridge_request_sync_in_window`]. With no `windowLabel` the request targets
/// the main window, byte-identical to single-window behavior. This is the
/// entrypoint every HTTP handler funnels through, so addressing a pop-out window
/// needs no per-handler plumbing.
pub async fn ui_bridge_request_sync(
    state: &Arc<ApiState>,
    request_type: &str,
    additional_payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let (window_label, additional_payload) = split_target_window(additional_payload);
    ui_bridge_request_sync_in_window(state, request_type, additional_payload, &window_label).await
}

/// Send a UI Bridge request to an explicit target window and wait for the
/// response synchronously.
///
/// This creates a oneshot channel, stores the sender in the pending map keyed by
/// `(window_label, request_id)`, emits the request to the frontend, and waits for
/// the response with a timeout.
///
/// Includes circuit breaker, concurrency limiting, frontend liveness check, and
/// request deduplication for read-only operations. Rust callers that already hold
/// a target label (e.g. terminal-window commands) call this directly; the public
/// [`ui_bridge_request_sync`] derives the label from the payload.
pub async fn ui_bridge_request_sync_in_window(
    state: &Arc<ApiState>,
    request_type: &str,
    additional_payload: serde_json::Value,
    window_label: &str,
) -> Result<serde_json::Value, String> {
    // 1. Check circuit breaker
    state.ui_bridge_circuit_breaker.check().await?;

    // 1.5. Wait for frontend readiness if no pong has ever been received.
    // This prevents the race condition where requests arrive before React's
    // event listeners are set up after a supervisor-triggered restart.
    {
        let pong_check = state
            .app_state
            .ui_bridge_last_pong
            .load(std::sync::atomic::Ordering::Relaxed);
        if pong_check == 0 {
            tracing::info!("UI Bridge: Waiting for frontend readiness (no pong received yet)");
            let ready_timeout = std::time::Duration::from_secs(10);
            if tokio::time::timeout(ready_timeout, state.ui_bridge_ready.notified())
                .await
                .is_err()
            {
                // Gather structured diagnostics instead of returning a bare string
                let diag = gather_readiness_diagnostics(state).await;
                return Err(serde_json::to_string(&diag.body).unwrap_or_else(|_| {
                    "UI Bridge: Frontend did not become ready within 10s (diagnostics serialization failed)".to_string()
                }));
            }
            tracing::info!("UI Bridge: Frontend is now ready");
        }
    }

    // 2. Check frontend liveness (warn if stale, but don't fail — let IPC timeout handle it)
    let last_pong = state
        .app_state
        .ui_bridge_last_pong
        .load(std::sync::atomic::Ordering::Relaxed);
    if last_pong > 0 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let pong_age = now - last_pong;
        if pong_age > 15000 {
            warn!(
                "UI Bridge: Frontend may be unresponsive (last pong {}ms ago)",
                pong_age
            );
        }
    }

    // 3. Check for dedup opportunity on read-only requests. The key is scoped to
    // the target window so a read for one pop-out window never collapses into an
    // in-flight read for a different window (which would return the wrong window's
    // elements). For the main window this is just the legacy per-type dedup.
    let dedup_key = match request_type {
        "get_elements" | "get_snapshot" | "get_components" => {
            Some(format!("{window_label}\u{1f}{request_type}"))
        }
        _ => None,
    };

    if let Some(ref key) = dedup_key {
        let dedup = state.ui_bridge_dedup.lock().await;
        if let Some(tx) = dedup.get(key) {
            // Subscribe to existing in-flight request
            let mut rx = tx.subscribe();
            drop(dedup);
            debug!("UI Bridge: Deduplicating {} request", key);
            // Apply the same timeout to dedup waits so stale entries don't block forever
            let timeout_ms = get_ui_bridge_timeout_ms();
            match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx.recv())
                .await
            {
                Ok(Ok(result)) => return result,
                Ok(Err(_)) => return Err("Dedup channel closed".to_string()),
                Err(_) => {
                    // Dedup wait timed out — remove stale entry and fall through
                    // to make a fresh request
                    warn!(
                        "UI Bridge: Dedup wait timed out for {}, removing stale entry and retrying",
                        key
                    );
                    let mut dedup_map = state.ui_bridge_dedup.lock().await;
                    dedup_map.remove(key);
                    // Fall through to make a fresh request below
                }
            }
        }
    }

    // 4. Acquire semaphore permit (max 6 concurrent, 2s timeout)
    let _permit = match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        state.ui_bridge_semaphore.acquire(),
    )
    .await
    {
        Ok(Ok(permit)) => permit,
        Ok(Err(_)) => return Err("UI Bridge semaphore closed".to_string()),
        Err(_) => {
            return Err(
                "UI Bridge concurrency limit reached (timeout acquiring permit)".to_string(),
            );
        }
    };

    // 5. Set up dedup broadcast for read-only requests
    let dedup_tx = if let Some(ref key) = dedup_key {
        let (tx, _) = tokio::sync::broadcast::channel(1);
        let mut dedup = state.ui_bridge_dedup.lock().await;
        dedup.insert(key.clone(), tx.clone());
        Some(tx)
    } else {
        None
    };

    // 6. Execute the actual request
    let result =
        ui_bridge_request_inner(state, request_type, additional_payload, window_label).await;

    // 7. Update circuit breaker and attempt recovery if it opens
    match &result {
        Ok(_) => state.ui_bridge_circuit_breaker.record_success().await,
        Err(e) if e.contains("timed out") => {
            state.ui_bridge_circuit_breaker.record_failure().await;
            // If circuit breaker just opened, attempt auto-recovery
            if state.ui_bridge_circuit_breaker.get_state().await == CircuitBreakerState::Open {
                state
                    .ui_bridge_circuit_breaker
                    .attempt_recovery(&state.app_handle);
            }
        }
        Err(_) => {} // Non-timeout errors don't trigger circuit breaker
    }

    // 8. Broadcast dedup result
    if let (Some(ref key), Some(tx)) = (&dedup_key, &dedup_tx) {
        let _ = tx.send(result.clone());
        let mut dedup = state.ui_bridge_dedup.lock().await;
        dedup.remove(key);
    }

    result
}

/// Inner implementation of ui_bridge_request_sync (the actual IPC logic).
///
/// `window_label` is the resolved target window ([`MAIN_WINDOW_LABEL`] for the
/// single-window default); the caller has already stripped any `windowLabel`
/// routing field from `additional_payload`.
async fn ui_bridge_request_inner(
    state: &Arc<ApiState>,
    request_type: &str,
    additional_payload: serde_json::Value,
    window_label: &str,
) -> Result<serde_json::Value, String> {
    let request_id = uuid::Uuid::new_v4().to_string();

    // Fail fast for a non-existent target window. Without this, an unknown label
    // would emit (or broadcast) an event every window's listener filters out by
    // label, so the caller would wait the full IPC timeout for a request nothing
    // can answer. A clear, immediate error naming the discovery route is the
    // honest failure. The main window is always present (created at boot), so the
    // default path skips the lookup entirely.
    if window_label != MAIN_WINDOW_LABEL {
        use tauri::Manager;
        if state.app_handle.get_webview_window(window_label).is_none() {
            return Err(format!(
                "No runner window labeled '{window_label}'. Discover live windows via \
                 GET /ui-bridge/control/runner-windows."
            ));
        }
    }

    // Carry the target window in the (flattened) envelope payload ONLY when it
    // is non-default, so the default single-window request is byte-identical on
    // the wire. The frontend listener reads `windowLabel` and ignores events not
    // addressed to its own `getCurrentWindow().label`.
    let additional_payload = if window_label != MAIN_WINDOW_LABEL {
        let mut p = additional_payload;
        if let Some(obj) = p.as_object_mut() {
            obj.insert(
                TARGET_WINDOW_FIELD.to_string(),
                serde_json::Value::String(window_label.to_string()),
            );
        }
        p
    } else {
        additional_payload
    };

    // Tell a `page_evaluate` request what this envelope's wait actually is, so
    // its frontend-side auto-await lands inside our timeout instead of twenty
    // seconds past it. No-op for every other request type.
    let additional_payload = stamp_legacy_evaluate_budget(request_type, additional_payload);

    // Build the typed envelope (Stage 1 of the ui-bridge-request envelope
    // concretization — see commit ea5d9a61f deferral note). Wire shape:
    // `{ requestId, type, ...additional_payload }`. Empty `Value::Object`
    // (the `json!({})` no-extra-payload case) flattens to nothing, so the
    // emitted payload is `{ requestId, type }` exactly as before.
    //
    // Non-Object additional_payload values would be silently dropped by the
    // legacy merge loop too (which short-circuited on `as_object()` =
    // `None`). All current call sites pass `Value::Object`, verified at
    // task time; if a caller starts passing `Value::String`/`Value::Null`,
    // the envelope will emit `requestId` + `type` only — matching the
    // legacy behavior — but the signal that something's off should surface
    // via the empty payload reaching the frontend handler.
    let envelope = qontinui_types::app_events::UiBridgeRequestEnvelope {
        request_id: request_id.clone(),
        request_type: request_type.to_string(),
        data: additional_payload,
    };
    let event_payload = serde_json::to_value(&envelope)
        .map_err(|e| format!("Failed to serialize UiBridgeRequestEnvelope: {}", e))?;

    // Create oneshot channel for the response
    let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();

    // Store the sender in the pending map under the composite (window, id) key.
    let pkey = pending_key(window_label, &request_id);
    {
        let mut pending = state.ui_bridge_pending.lock().await;
        pending.insert(pkey.clone(), tx);
        state
            .ui_bridge_pending_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    // Emit request to the React frontend. Default (flag off): broadcast to all
    // windows exactly as before. Flag on (Phase 1): target the specific window,
    // falling back to broadcast if its webview can't be resolved (e.g. mid
    // teardown) so a request is never silently dropped.
    let emit_result = if multi_window_dispatch_enabled() {
        use tauri::Manager;
        match state.app_handle.get_webview_window(window_label) {
            Some(win) => win.emit("ui-bridge-request", &event_payload),
            None => state.app_handle.emit("ui-bridge-request", &event_payload),
        }
    } else {
        state.app_handle.emit("ui-bridge-request", &event_payload)
    };
    if let Err(e) = emit_result {
        // Clean up the pending entry
        let mut pending = state.ui_bridge_pending.lock().await;
        if pending.remove(&pkey).is_some() {
            state
                .ui_bridge_pending_count
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
        return Err(format!("Failed to emit UI Bridge request: {}", e));
    }

    // Wait for response with timeout
    let timeout_duration = std::time::Duration::from_millis(get_ui_bridge_timeout_ms());
    match tokio::time::timeout(timeout_duration, rx).await {
        Ok(Ok(response)) => {
            // A successful IPC response means a full UI Bridge round-trip has
            // completed end to end. Flip the one-way latch, which `/health`
            // publishes as `uiBridgeIpcObserved`.
            //
            // NOT readiness — `/health`'s `frontendReady` is derived from
            // `classify_frontend_state` instead. This latch only ever flips
            // when something EXTERNAL calls a `/ui-bridge/*` route, so it
            // stayed false on healthy idle runners for as long as nobody
            // asked. See the `AppState::frontend_ready` doc.
            state
                .app_state
                .frontend_ready
                .store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(response)
        }
        Ok(Err(_)) => Err("UI Bridge request channel closed unexpectedly".to_string()),
        Err(_) => {
            // Timeout - clean up the pending entry
            let mut pending = state.ui_bridge_pending.lock().await;
            if pending.remove(&pkey).is_some() {
                state
                    .ui_bridge_pending_count
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }
            Err(format!(
                "UI Bridge request timed out after {}ms. Is the frontend running?",
                get_ui_bridge_timeout_ms()
            ))
        }
    }
}

/// Handle incoming UI Bridge response from the frontend.
///
/// This is called by the Tauri event listener set up in create_router.
pub async fn handle_ui_bridge_response(
    pending: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>,
        >,
    >,
    pending_count: Arc<std::sync::atomic::AtomicUsize>,
    response: serde_json::Value,
) {
    let request_id = response
        .get("requestId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // The responding window echoes its own label; absent for the single-window
    // default (and for any pre-window-aware frontend), so fall back to "main" —
    // which is exactly the key both dispatch paths store under in Phase 0.
    let window_label = response
        .get("windowLabel")
        .and_then(|v| v.as_str())
        .unwrap_or(MAIN_WINDOW_LABEL);

    if let Some(request_id) = request_id {
        let pkey = pending_key(window_label, &request_id);
        let mut pending_map = pending.lock().await;
        if let Some(sender) = pending_map.remove(&pkey) {
            pending_count.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            // Extract the data portion of the response, carrying the
            // envelope's failure verdict across the seam.
            let data = extract_response_data(&response);
            if sender.send(data).is_err() {
                warn!(
                    "UI Bridge: Failed to send response, receiver dropped for request {}",
                    request_id
                );
            } else {
                debug!("UI Bridge: Delivered response for request {}", request_id);
            }
        } else {
            warn!(
                "UI Bridge: No pending request found for response {}",
                request_id
            );
        }
    } else {
        warn!("UI Bridge: Response missing requestId: {:?}", response);
    }
}

/// Envelope fields a failure verdict travels with, in the order the HTTP layer
/// consumes them: [`wrap_ipc_result`] reads `error` and `hint`, and
/// `ai_analyze::as_recovery_failure` reads `code`.
const FAILURE_VERDICT_FIELDS: [&str; 3] = ["error", "code", "hint"];

/// Extract the payload the waiting HTTP handler should see from a frontend
/// response envelope, **preserving the envelope's failure verdict**.
///
/// The frontend replies with `{requestId, type, success, error?, hint?, data?}`
/// and only `data` is forwarded, because every IPC-backed handler reads its
/// result fields straight off that object. That extraction used to drop the
/// sibling `success`/`error` on the floor whenever a handler supplied a `data`
/// field — and [`wrap_ipc_result`]'s rule is "failure only if
/// `data.success == Some(false)`", so an **absent** `success` key read as
/// success. A handler that answered `{success: false, error: "…",
/// data: {error: "unknown_tab", …}}` reached the caller as
/// **HTTP 200 `{"success": true, …}`**.
///
/// Handlers used to dodge this one call site at a time by mirroring the failure
/// into `data` themselves (`recoveryFailureData` in `recoveryScope.ts`,
/// `useAISearchEvents.ts`). Doing it at the seam closes it for every handler,
/// including the five `usePageEvents` / `useDebugInspectEvents` refusals that
/// never knew they had to.
///
/// The rules:
///
/// - No `data` field → the whole envelope is forwarded (it already carries the
///   verdict), exactly as before.
/// - Envelope `success` is anything other than `false` → `data` is forwarded
///   untouched, so the healthy path is byte-identical.
/// - Envelope `success: false` + object `data` → `success: false` and every
///   [`FAILURE_VERDICT_FIELDS`] entry the envelope carries are stamped onto it,
///   **overwriting** any same-named field in `data`. The envelope is the
///   authority on the verdict, and the collision is not hypothetical: several
///   handlers put a machine code in `data.error` (`"unknown_tab"`,
///   `"invalid_stub"`, `"not_found"`) while the envelope's `error` holds the
///   prose a caller should read. Data-only fields (`recovered`, `knownTabs`,
///   `elementId`, …) are untouched. An overwritten value is not recoverable, so
///   a handler with a machine code to carry should put it in `code` (which the
///   envelope carries natively) or in a field of its own — not in `data.error`.
/// - Envelope `success: false` + non-object `data` (scalar, array, null) → a
///   failure envelope is synthesized with the original payload under `data`,
///   since a scalar has nowhere to carry a verdict.
pub(crate) fn extract_response_data(response: &serde_json::Value) -> serde_json::Value {
    let Some(data) = response.get("data") else {
        // No `data` sibling: the envelope IS the payload and already carries
        // whatever `success`/`error` the handler set.
        return response.clone();
    };

    if response.get("success").and_then(|v| v.as_bool()) != Some(false) {
        return data.clone();
    }

    let mut out = match data.as_object() {
        Some(obj) => obj.clone(),
        None => {
            let mut synthesized = serde_json::Map::new();
            synthesized.insert("data".to_string(), data.clone());
            synthesized
        }
    };

    out.insert("success".to_string(), serde_json::Value::Bool(false));
    for field in FAILURE_VERDICT_FIELDS {
        if let Some(value) = response.get(field) {
            out.insert(field.to_string(), value.clone());
        }
    }
    serde_json::Value::Object(out)
}

/// Wrap a UI Bridge IPC result into an API response, flattening any inner
/// `{success:false, error}` envelope from the frontend into a flat HTTP 400.
///
/// **F2 two-tier envelope contract** (sweep applied 2026-04-22):
/// - Inner `success: true` (or no `success` field at all — some handlers omit
///   it on the happy path) → HTTP 200 with `ApiResponse::success(data)`.
/// - Inner `success: false` → HTTP 400 with a flat `{success: false, error}`
///   body (no nested `data`, no inner success field). Falls back to a generic
///   "UI bridge call failed" message if `data.error` is missing or non-string.
/// - Transport-level `Err(_)` → HTTP 503 (frontend not ready) or HTTP 500
///   (everything else), with structured `error_detail` for machine-readable
///   recovery hints.
///
/// This mirrors the F2 fix originally landed in `design.rs` for the audit
/// handler (`unwrap_inner_audit_error`) and is now the canonical unwrapper
/// every IPC-backed handler funnels through.
///
/// The "no `success` field at all → HTTP 200" arm is only sound because
/// [`extract_response_data`] stamps an envelope-level `success: false` onto the
/// payload before it gets here. Without that, a handler's explicit refusal
/// arrived as a bare data object and this function read the absent key as a
/// healthy response — see that function for the defect.
///
/// Note: this is a **back-compat shift** for callers that previously saw
/// `HTTP 200 + {success:false, ...}` on soft failures — they now get HTTP 400.
pub(crate) fn wrap_ipc_result(
    result: Result<serde_json::Value, String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    match result {
        Ok(data) => {
            if data.get("success").and_then(|v| v.as_bool()) == Some(false) {
                // Inner-failure envelope: flatten to HTTP 400 + flat error body.
                let error_msg = data
                    .get("error")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "UI bridge call failed".to_string());
                let detail = classify_transport_error(&error_msg);
                // Forward an optional `hint` sibling field from the inner
                // IPC envelope (set by frontend handlers like
                // `useControlEvents` for typo-recovery on element-not-found
                // / action-not-allowed). The hint stays a sibling of
                // `error` — the success/error envelope shape is unchanged.
                let hint = data.get("hint").cloned();
                let mut body = api_error_detailed(error_msg, detail);
                body.hint = hint;
                Err((StatusCode::BAD_REQUEST, Json(body)))
            } else {
                // Healthy IPC response (success: true OR success absent).
                Ok(Json(ApiResponse::success(data)))
            }
        }
        Err(e) => {
            error!("UI Bridge API: {}", e);
            let detail = classify_transport_error(&e);
            let status = match detail.code {
                UiBridgeErrorCode::FrontendNotReady => StatusCode::SERVICE_UNAVAILABLE,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            Err((status, Json(api_error_detailed(e, detail))))
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod frontend_state_tests {
    //! Readiness-ladder regression tests.
    //!
    //! The defect these lock down: `ready`/`sdk_connected` used to be
    //! `last_pong > 0`, a latch that flips true on the frontend's first pong
    //! and is never reset. A React tree that crashed *after* mounting kept its
    //! SDK pong loop alive under the error boundary's fallback, so every
    //! diagnostics surface reported "Frontend is responsive" while
    //! `GET /control/snapshot` returned nothing.
    use super::{classify_frontend_state, FrontendState, FrontendStateInputs, TREE_CRASHED_HINT};

    /// A healthy, recently-ponging frontend. Individual tests mutate one field.
    fn healthy() -> FrontendStateInputs {
        FrontendStateInputs {
            window_exists: true,
            window_visible: true,
            last_pong: 1_700_000_000_000,
            last_pong_age_ms: 500,
            console_error_count: 0,
            process_uptime_ms: 120_000,
            has_ui_error: false,
        }
    }

    #[test]
    fn healthy_frontend_is_responsive_and_ready() {
        let i = healthy();
        let state = classify_frontend_state(i);
        assert_eq!(state, FrontendState::Responsive);
        assert!(state.is_ready(i.last_pong));
    }

    #[test]
    fn ui_error_with_recent_pong_reports_crashed_and_not_ready() {
        // THE regression: a fresh pong (500ms old) must NOT mask the crash.
        let i = FrontendStateInputs {
            has_ui_error: true,
            ..healthy()
        };
        let state = classify_frontend_state(i);
        assert_eq!(
            state,
            FrontendState::TreeCrashed,
            "a present ui_error must win over a recent pong"
        );
        assert!(
            !state.is_ready(i.last_pong),
            "a mounted-but-crashed tree is not ready even while it still pongs"
        );
    }

    #[test]
    fn crashed_hint_names_the_error_boundary_and_points_at_ui_error() {
        // The hint is the payload of this whole change — assert its content,
        // not just that a branch fired.
        assert!(TREE_CRASHED_HINT.contains("error boundary"));
        assert!(TREE_CRASHED_HINT.contains("uiError"));
    }

    #[test]
    fn ui_error_outranks_the_30s_stale_branch() {
        let i = FrontendStateInputs {
            has_ui_error: true,
            last_pong_age_ms: 90_000,
            ..healthy()
        };
        assert_eq!(classify_frontend_state(i), FrontendState::TreeCrashed);
        assert!(!classify_frontend_state(i).is_ready(i.last_pong));
    }

    #[test]
    fn stale_pong_without_ui_error_still_reports_stale_and_ready() {
        // Pre-existing behavior preserved: `ready` tracks the pong latch on
        // every non-crashed path, so only the crash branch changes verdict.
        let i = FrontendStateInputs {
            last_pong_age_ms: 90_000,
            ..healthy()
        };
        let state = classify_frontend_state(i);
        assert_eq!(state, FrontendState::Stale);
        assert!(state.is_ready(i.last_pong));
    }

    #[test]
    fn never_ponged_branches_outrank_ui_error() {
        // A crash during mount is better described by the boot-time branches,
        // which carry the console-error / window-visibility corroboration.
        let base = FrontendStateInputs {
            last_pong: 0,
            last_pong_age_ms: 0,
            has_ui_error: true,
            ..healthy()
        };

        assert_eq!(
            classify_frontend_state(FrontendStateInputs {
                process_uptime_ms: 1_000,
                ..base
            }),
            FrontendState::Booting
        );
        assert_eq!(
            classify_frontend_state(FrontendStateInputs {
                console_error_count: 3,
                ..base
            }),
            FrontendState::CrashedDuringMount
        );
        assert_eq!(
            classify_frontend_state(FrontendStateInputs {
                window_visible: false,
                ..base
            }),
            FrontendState::WindowNotVisible
        );
        assert_eq!(classify_frontend_state(base), FrontendState::NeverPonged);

        // None of them are ready — last_pong is 0.
        assert!(!classify_frontend_state(base).is_ready(base.last_pong));
    }

    #[test]
    fn stale_and_dead_rungs_cannot_drift_apart() {
        // The invariant the single-predicate refactor exists to protect.
        // `classify_frontend_state` (diagnostics) and `compute_derived_status`
        // (status) read the SAME `last_pong` atomic through the SAME
        // `ui_stale` predicate at two calibrations, so they can only ever
        // escalate in one direction: diagnostics say `Stale` well before
        // status says dead. Two hand-rolled ladders would drift silently.
        use crate::ui_error::{
            compute_derived_status, ui_stale, UI_DEAD_AFTER_MS, UI_STALE_AFTER_MS,
        };

        assert!(UI_STALE_AFTER_MS < UI_DEAD_AFTER_MS);
        let between = (UI_STALE_AFTER_MS + UI_DEAD_AFTER_MS) / 2;

        let i = FrontendStateInputs {
            last_pong_age_ms: between,
            ..healthy()
        };
        assert_eq!(
            classify_frontend_state(i),
            FrontendState::Stale,
            "an age past UI_STALE_AFTER_MS must classify as Stale"
        );
        let ui_dead = ui_stale(i.last_pong, i.last_pong_age_ms, UI_DEAD_AFTER_MS);
        assert!(!ui_dead);
        assert_eq!(
            compute_derived_status(&crate::ui_error::HealthInputs {
                ui_dead: Some(ui_dead),
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                relay_connected: Some(true),
                ..Default::default()
            }),
            "healthy",
            "the same age must still be healthy for the status rung"
        );

        // Past the dead rung, both surfaces agree the UI is gone.
        let i = FrontendStateInputs {
            last_pong_age_ms: UI_DEAD_AFTER_MS + 1,
            ..healthy()
        };
        assert_eq!(classify_frontend_state(i), FrontendState::Stale);
        let ui_dead = ui_stale(i.last_pong, i.last_pong_age_ms, UI_DEAD_AFTER_MS);
        assert!(ui_dead);
        assert_eq!(
            compute_derived_status(&crate::ui_error::HealthInputs {
                ui_dead: Some(ui_dead),
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                relay_connected: Some(true),
                ..Default::default()
            }),
            "errored"
        );
    }

    #[test]
    fn classify_uses_the_shared_stale_predicate_at_the_diagnostics_rung() {
        // De-duplication guard: the `> 30_000` literal that used to live in
        // `classify_frontend_state` is now `ui_stale(.., UI_STALE_AFTER_MS)`.
        // Behavior must be byte-identical either side of the boundary.
        use crate::ui_error::UI_STALE_AFTER_MS;

        let at = FrontendStateInputs {
            last_pong_age_ms: UI_STALE_AFTER_MS,
            ..healthy()
        };
        assert_eq!(
            classify_frontend_state(at),
            FrontendState::Responsive,
            "exactly at the threshold is still responsive (strict >)"
        );
        let past = FrontendStateInputs {
            last_pong_age_ms: UI_STALE_AFTER_MS + 1,
            ..healthy()
        };
        assert_eq!(classify_frontend_state(past), FrontendState::Stale);
    }

    #[test]
    fn missing_window_outranks_everything() {
        let i = FrontendStateInputs {
            window_exists: false,
            has_ui_error: true,
            ..healthy()
        };
        assert_eq!(classify_frontend_state(i), FrontendState::WindowMissing);
        // Unchanged from before this fix: with a live pong latch, a missing
        // window handle alone does not flip `ready`.
        assert!(classify_frontend_state(i).is_ready(i.last_pong));
    }

    #[test]
    fn is_ready_is_false_whenever_no_pong_was_ever_received() {
        for state in [
            FrontendState::WindowMissing,
            FrontendState::Booting,
            FrontendState::CrashedDuringMount,
            FrontendState::WindowNotVisible,
            FrontendState::NeverPonged,
            FrontendState::TreeCrashed,
            FrontendState::Stale,
            FrontendState::Responsive,
        ] {
            assert!(
                !state.is_ready(0),
                "{state:?} must not be ready with no pong"
            );
        }
    }

    /// `/health`'s `frontendReady` is `state == Responsive`, deliberately NOT
    /// `is_ready()`.
    ///
    /// `is_ready()` is lenient by design for the two UI-Bridge diagnostics
    /// routes — `missing_window_outranks_everything` above pins that a
    /// `WindowMissing` frontend with a live pong latch still reports ready
    /// there. A dead WebView window is emphatically NOT "the frontend can serve
    /// a UI Bridge call right now", which is the question `/health` answers, so
    /// this locks in that only `Responsive` qualifies.
    #[test]
    fn only_responsive_qualifies_as_health_frontend_ready() {
        for state in [
            FrontendState::WindowMissing,
            FrontendState::Booting,
            FrontendState::CrashedDuringMount,
            FrontendState::WindowNotVisible,
            FrontendState::NeverPonged,
            FrontendState::TreeCrashed,
            FrontendState::Stale,
        ] {
            assert_ne!(
                state,
                FrontendState::Responsive,
                "{state:?} must not satisfy /health's frontendReady"
            );
        }

        // The healthy fixture is the one state that does.
        assert_eq!(
            classify_frontend_state(healthy()),
            FrontendState::Responsive
        );

        // And the trap this guards: WindowMissing is `is_ready` but NOT
        // `Responsive`, so /health must never adopt `is_ready`.
        let gone = FrontendStateInputs {
            window_exists: false,
            ..healthy()
        };
        let gone_state = classify_frontend_state(gone);
        assert!(gone_state.is_ready(gone.last_pong));
        assert_ne!(gone_state, FrontendState::Responsive);
    }

    /// `as_str` is a wire contract — `/health` publishes it as `frontendState`
    /// and consumers branch on the literals. Renaming one silently breaks them.
    #[test]
    fn frontend_state_wire_names_are_stable() {
        assert_eq!(FrontendState::WindowMissing.as_str(), "window_missing");
        assert_eq!(FrontendState::Booting.as_str(), "booting");
        assert_eq!(
            FrontendState::CrashedDuringMount.as_str(),
            "crashed_during_mount"
        );
        assert_eq!(
            FrontendState::WindowNotVisible.as_str(),
            "window_not_visible"
        );
        assert_eq!(FrontendState::NeverPonged.as_str(), "never_ponged");
        assert_eq!(FrontendState::TreeCrashed.as_str(), "tree_crashed");
        assert_eq!(FrontendState::Stale.as_str(), "stale");
        assert_eq!(FrontendState::Responsive.as_str(), "responsive");
    }
}

#[cfg(test)]
mod wrap_ipc_result_tests {
    //! F2 two-tier envelope flattening tests for the canonical
    //! `wrap_ipc_result` helper. Lock down each decision point: inner
    //! success, inner failure (with/without error field), absent success
    //! field, and non-bool success values.
    use super::{
        extract_response_data, handle_ui_bridge_response, pending_key, split_target_window,
        wrap_ipc_result, MAIN_WINDOW_LABEL,
    };
    use axum::http::StatusCode;
    use serde_json::json;
    use std::collections::HashMap;
    use std::ops::Deref;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::{oneshot, Mutex};

    // ── Window-aware dispatch (Phase 0) ─────────────────────────────────────

    #[test]
    fn split_target_window_defaults_to_main_when_absent() {
        // No windowLabel → main, payload untouched (single-window default).
        let (label, payload) = split_target_window(json!({ "selector": "#btn" }));
        assert_eq!(label, MAIN_WINDOW_LABEL);
        assert_eq!(payload, json!({ "selector": "#btn" }));
    }

    #[test]
    fn split_target_window_extracts_and_strips_label() {
        // windowLabel routes the request AND is consumed (not forwarded as data).
        let (label, payload) =
            split_target_window(json!({ "windowLabel": "term-1", "selector": "#btn" }));
        assert_eq!(label, "term-1");
        assert_eq!(payload, json!({ "selector": "#btn" }));
    }

    #[test]
    fn split_target_window_empty_or_non_string_label_falls_back_to_main() {
        let (label, payload) = split_target_window(json!({ "windowLabel": "", "x": 1 }));
        assert_eq!(label, MAIN_WINDOW_LABEL);
        assert_eq!(payload, json!({ "x": 1 }), "empty label still consumed");

        let (label, payload) = split_target_window(json!({ "windowLabel": 7, "x": 1 }));
        assert_eq!(label, MAIN_WINDOW_LABEL);
        assert_eq!(
            payload,
            json!({ "x": 1 }),
            "non-string label still consumed"
        );
    }

    #[test]
    fn split_target_window_non_object_payload_is_unchanged() {
        // A non-object payload can't carry a routing field — pass it through as-is.
        let (label, payload) = split_target_window(json!("raw"));
        assert_eq!(label, MAIN_WINDOW_LABEL);
        assert_eq!(payload, json!("raw"));
    }

    #[test]
    fn pending_key_is_distinct_per_window_for_same_request_id() {
        let id = "11111111-2222-3333-4444-555555555555";
        // Same request id in two windows must NOT collide on the pending map.
        assert_ne!(pending_key("main", id), pending_key("term-1", id));
        // Same (window, id) is stable so insert and remove agree.
        assert_eq!(pending_key("main", id), pending_key("main", id));
    }

    #[tokio::test]
    async fn response_routes_only_to_the_addressed_window() {
        // Two windows registered the SAME request id; a response from one must
        // resolve only that window's sender and leave the other pending.
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let count = Arc::new(AtomicUsize::new(0));
        let id = "req-abc";
        let (tx_main, rx_main) = oneshot::channel::<serde_json::Value>();
        let (tx_term, rx_term) = oneshot::channel::<serde_json::Value>();
        {
            let mut p = pending.lock().await;
            p.insert(pending_key("main", id), tx_main);
            p.insert(pending_key("term-1", id), tx_term);
        }
        count.store(2, Ordering::Relaxed);

        let response = json!({ "requestId": id, "windowLabel": "term-1", "data": { "ok": true } });
        handle_ui_bridge_response(pending.clone(), count.clone(), response).await;

        // term-1's sender fired with the unwrapped data...
        assert_eq!(
            rx_term.await.expect("term-1 sender fired"),
            json!({ "ok": true })
        );
        // ...main's entry is untouched (still pending, count decremented by one).
        let p = pending.lock().await;
        assert!(p.contains_key(&pending_key("main", id)));
        assert!(!p.contains_key(&pending_key("term-1", id)));
        assert_eq!(count.load(Ordering::Relaxed), 1);
        drop(rx_main);
    }

    #[tokio::test]
    async fn response_without_label_defaults_to_main() {
        // A response that omits windowLabel (pre-window-aware frontend, or the
        // direct-eval path) must resolve the "main" key it was stored under.
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let count = Arc::new(AtomicUsize::new(0));
        let id = "req-xyz";
        let (tx, rx) = oneshot::channel::<serde_json::Value>();
        {
            let mut p = pending.lock().await;
            p.insert(pending_key(MAIN_WINDOW_LABEL, id), tx);
        }
        count.store(1, Ordering::Relaxed);

        let response = json!({ "requestId": id, "data": { "v": 1 } });
        handle_ui_bridge_response(pending.clone(), count.clone(), response).await;

        assert_eq!(rx.await.expect("main sender fired"), json!({ "v": 1 }));
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn inner_success_returns_http_200() {
        let data = json!({"success": true, "report": {"violations": []}});
        let resp = wrap_ipc_result(Ok(data.clone())).expect("inner success must produce Ok");
        let body = resp.deref();
        assert!(body.success);
        assert!(body.error.is_none());
        assert_eq!(body.data.as_ref().unwrap(), &data);
    }

    #[test]
    fn inner_failure_with_explicit_error_flattens_to_400() {
        let data = json!({
            "success": false,
            "error": "No style guide provided or loaded.",
            "type": "design_run_audit",
        });
        let (status, body) =
            wrap_ipc_result(Ok(data)).expect_err("inner failure must produce Err with HTTP 400");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let inner = body.deref();
        assert!(!inner.success);
        let msg = inner.error.as_deref().unwrap_or_default();
        assert!(
            msg.contains("No style guide provided"),
            "expected inner error to surface, got: {msg}"
        );
        // Outer body must be flat (no nested `data` from the inner envelope).
        assert!(inner.data.is_none(), "outer body must not nest inner data");
    }

    #[test]
    fn inner_failure_without_error_field_uses_fallback_message() {
        let data = json!({"success": false});
        let (status, body) = wrap_ipc_result(Ok(data)).expect_err("inner failure must produce Err");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let msg = body.deref().error.as_deref().unwrap_or_default();
        assert_eq!(msg, "UI bridge call failed");
    }

    #[test]
    fn absent_success_field_passes_through_as_200() {
        // Some IPC responses don't include `success` at all — those should
        // be treated as healthy success (no misclassification as failure).
        let data = json!({"report": {"violations": []}});
        let resp = wrap_ipc_result(Ok(data.clone())).expect("absent success field must produce Ok");
        let body = resp.deref();
        assert!(body.success);
        assert_eq!(body.data.as_ref().unwrap(), &data);
    }

    #[test]
    fn non_bool_success_value_passes_through_as_200() {
        // Robustness: if `success` is a string or number rather than a bool,
        // treat it as "shape unknown, don't flag failure" rather than
        // panicking or misclassifying it.
        let data = json!({"success": "true", "payload": 1});
        let resp = wrap_ipc_result(Ok(data)).expect("string success must produce Ok");
        assert!(resp.deref().success);

        let data = json!({"success": 1, "payload": 2});
        let resp = wrap_ipc_result(Ok(data)).expect("numeric success must produce Ok");
        assert!(resp.deref().success);
    }

    // ── Envelope failure propagation across the data-extraction seam ────────
    //
    // The defect: `handle_ui_bridge_response` forwarded ONLY `response.data`
    // when a handler supplied one, dropping the sibling `success: false` /
    // `error`. `wrap_ipc_result` then saw a payload with no `success` key,
    // took the "absent success is healthy" arm, and answered HTTP 200
    // `{"success": true, ...}` for a call the frontend had explicitly failed.
    // Each test below drives a REAL refusal shape from the frontend handlers.

    /// `usePageEvents.ts` `tab_activate` — unknown tabId.
    #[test]
    fn envelope_failure_with_dataless_success_becomes_a_failure() {
        let response = json!({
            "requestId": "req-tab",
            "type": "tab_activate",
            "success": false,
            "error": "unknown tabId: \"promts\"",
            "data": { "error": "unknown_tab", "knownTabs": ["prompts", "terminal"] },
        });

        let data = extract_response_data(&response);
        assert_eq!(
            data.get("success"),
            Some(&json!(false)),
            "the envelope's verdict must survive extraction, got: {data}"
        );

        let (status, body) = wrap_ipc_result(Ok(data))
            .expect_err("an explicit frontend refusal must not answer HTTP 200");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let inner = body.deref();
        assert!(!inner.success);
        assert_eq!(inner.error.as_deref(), Some("unknown tabId: \"promts\""));
    }

    /// `useDebugInspectEvents.ts` `element_not_found` — the refusal carries a
    /// `hint`, which must reach the flattened body rather than being dropped
    /// with the rest of the envelope.
    #[test]
    fn envelope_failure_forwards_error_code_and_hint_into_data() {
        let response = json!({
            "requestId": "req-inspect",
            "type": "debug_inspect_element",
            "success": false,
            "error": "Element not found: sumbit-btn",
            "code": "UB-ELEM-NOT-FOUND",
            "hint": { "closestMatches": ["submit-btn"] },
            "data": { "found": false, "elementId": "sumbit-btn" },
        });

        let data = extract_response_data(&response);
        assert_eq!(data.get("success"), Some(&json!(false)));
        assert_eq!(data.get("code"), Some(&json!("UB-ELEM-NOT-FOUND")));
        assert_eq!(data.get("found"), Some(&json!(false)), "data fields kept");

        let (status, body) = wrap_ipc_result(Ok(data)).expect_err("must flatten to a failure");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body.deref().hint,
            Some(json!({ "closestMatches": ["submit-btn"] })),
            "the envelope's hint must survive to the HTTP body"
        );
    }

    /// The envelope is the authority on the verdict fields; everything else in
    /// `data` survives. Several handlers put a machine CODE in `data.error`
    /// while the envelope's `error` holds the prose — picking `data` there
    /// would answer `"unknown_tab"` where the caller wants the sentence.
    #[test]
    fn envelope_verdict_fields_overwrite_same_named_data_fields() {
        let response = json!({
            "requestId": "req-recovery",
            "type": "ai_recovery_attempt",
            "success": false,
            "error": "RECOVERY_UNSCOPED: recovery requires params.elementId",
            "code": "RECOVERY_UNSCOPED",
            "data": {
                "error": "unscoped",
                "code": "STALE",
                "recovered": false,
                "elementId": "btn-1",
            },
        });

        let data = extract_response_data(&response);
        assert_eq!(
            data.get("error"),
            Some(&json!(
                "RECOVERY_UNSCOPED: recovery requires params.elementId"
            ))
        );
        assert_eq!(data.get("code"), Some(&json!("RECOVERY_UNSCOPED")));
        // Data-only fields are untouched.
        assert_eq!(data.get("recovered"), Some(&json!(false)));
        assert_eq!(data.get("elementId"), Some(&json!("btn-1")));
    }

    #[test]
    fn envelope_success_leaves_data_untouched() {
        // The happy path must be byte-identical: no stamped fields, no
        // envelope leakage into the payload.
        let response = json!({
            "requestId": "req-ok",
            "type": "tab_activate",
            "success": true,
            "data": { "activated": true, "tabId": "prompts" },
        });
        assert_eq!(
            extract_response_data(&response),
            json!({ "activated": true, "tabId": "prompts" })
        );

        // `success` absent entirely (handlers that omit it) — also untouched.
        let response = json!({ "requestId": "req-ok2", "data": { "v": 1 } });
        assert_eq!(extract_response_data(&response), json!({ "v": 1 }));
    }

    #[test]
    fn envelope_failure_with_non_object_data_is_wrapped() {
        // A scalar `data` has nowhere to carry the verdict, so synthesize an
        // envelope around it rather than laundering the failure.
        let response = json!({
            "requestId": "req-scalar",
            "success": false,
            "error": "stub not found: s-1",
            "data": "s-1",
        });
        let data = extract_response_data(&response);
        assert_eq!(
            data,
            json!({ "success": false, "error": "stub not found: s-1", "data": "s-1" })
        );
        let (status, _) = wrap_ipc_result(Ok(data)).expect_err("must flatten to a failure");
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn dispatcher_delivers_the_failure_verdict_to_the_waiting_handler() {
        // End-to-end through the real dispatcher: the `invalid_stub` refusal
        // from `usePageEvents.ts` must arrive at the HTTP layer as a failure.
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let count = Arc::new(AtomicUsize::new(0));
        let id = "req-stub";
        let (tx, rx) = oneshot::channel::<serde_json::Value>();
        {
            let mut p = pending.lock().await;
            p.insert(pending_key(MAIN_WINDOW_LABEL, id), tx);
        }
        count.store(1, Ordering::Relaxed);

        let response = json!({
            "requestId": id,
            "type": "network_stub_add",
            "success": false,
            "error": "urlPattern is required",
            "data": { "error": "invalid_stub", "field": "urlPattern" },
        });
        handle_ui_bridge_response(pending.clone(), count.clone(), response).await;

        let delivered = rx.await.expect("sender fired");
        let (status, body) = wrap_ipc_result(Ok(delivered))
            .expect_err("an invalid stub must not answer HTTP 200 success");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body.deref().error.as_deref(),
            Some("urlPattern is required")
        );
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn transport_error_returns_5xx() {
        // Sanity: a transport-level Err (e.g. timeout) still surfaces as
        // HTTP 5xx, not 400.
        let (status, _body) =
            wrap_ipc_result(Err("UI Bridge request timed out after 5000ms".to_string()))
                .expect_err("transport error must produce Err");
        // Either 500 (default) or 503 (frontend not ready) — not 400.
        assert_ne!(status, StatusCode::BAD_REQUEST);
        assert!(status.is_server_error());
    }
}

/// Regression tests for the element-action wire convention.
///
/// These exist because the convention was violated at four independent sites
/// and the violations were invisible to the compiler: every one of them built
/// an untyped `serde_json::json!` payload, so no amount of widening a DTO
/// would have caught them. The assertions below are deliberately written
/// against the *frontend handler's* reading of the payload
/// (`useControlEvents.ts` `case "execute_action"`), which is the actual
/// contract:
///   * it destructures `{ elementId, action }` — `id` is not a synonym;
///   * it reads `params` / `waitOptions` off the `action` envelope only.
#[cfg(test)]
mod element_action_payload_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn payload_uses_element_id_key_never_id() {
        let payload = element_action_payload("btn-1", json!({"action": "click"}));

        assert_eq!(
            payload.get("elementId").and_then(|v| v.as_str()),
            Some("btn-1"),
            "frontend destructures `elementId`; anything else fails the request \
             with 'elementId and action are required'"
        );
        assert!(
            payload.get("id").is_none(),
            "`id` was the exact key that broke every batch step — it must not reappear"
        );
    }

    #[test]
    fn params_live_inside_the_action_envelope_not_as_a_sibling() {
        let payload = element_action_payload(
            "input-1",
            json!({"action": "type", "params": {"text": "hi"}}),
        );

        assert!(
            payload.get("params").is_none(),
            "a top-level `params` sibling is never read by the frontend"
        );
        assert_eq!(
            payload
                .pointer("/action/params/text")
                .and_then(|v| v.as_str()),
            Some("hi"),
            "params must arrive nested in the action envelope"
        );
    }

    #[test]
    fn bare_string_action_passes_through_untouched() {
        // The SDK proxy-fallback shape. The frontend normalizes it itself.
        let payload = element_action_payload("btn-1", json!("click"));
        assert_eq!(
            payload.get("action").and_then(|v| v.as_str()),
            Some("click")
        );
    }

    #[test]
    fn missing_action_name_keeps_the_historical_click_default() {
        for envelope in [json!({}), json!(null), json!(7)] {
            let payload = element_action_payload("btn-1", envelope);
            assert_eq!(
                payload.pointer("/action/action").and_then(|v| v.as_str()),
                Some("click"),
                "an envelope with no action name must not dispatch `undefined`"
            );
        }
    }

    /// The headline regression: a batch step must round-trip EVERY field.
    ///
    /// This is the structural assertion the class needs — it carries an
    /// `unknownFutureOptIn` key that no code in this repo knows about. If a
    /// later change reintroduces field-by-field rebuilding, that key vanishes
    /// and this test fails, which is exactly the signal that was missing when
    /// four sites drifted.
    #[test]
    fn batch_step_round_trips_every_field() {
        let step = json!({
            "label": "fill the search box",
            "elementId": "search-input",
            "action": {
                "action": "type",
                "params": {"text": "qontinui"},
                "waitOptions": {"visible": true, "timeout": 2000},
                "expectChange": true,
                "fromSnapshotId": "ubs2_a_b_c_d",
                "verifyEffect": {"mode": "strict"},
                "unknownFutureOptIn": {"nested": ["value"]}
            }
        });

        let payload = step_action_payload(&step);

        assert_eq!(
            payload.get("elementId").and_then(|v| v.as_str()),
            Some("search-input")
        );
        // The envelope must arrive by identity — not a rebuilt subset of it.
        assert_eq!(
            payload.get("action"),
            step.get("action"),
            "the action envelope must be forwarded whole, field-for-field"
        );
        // Spelled out, so a failure names the field that got dropped.
        for field in [
            "params",
            "waitOptions",
            "expectChange",
            "fromSnapshotId",
            "verifyEffect",
            "unknownFutureOptIn",
        ] {
            assert!(
                payload.pointer(&format!("/action/{field}")).is_some(),
                "batch step dropped `{field}` on the way to the frontend"
            );
        }
        // `label` addresses the step, not the action — it should not leak in.
        assert!(payload.pointer("/action/label").is_none());
    }

    #[test]
    fn flat_step_lifts_every_non_reserved_field_into_the_envelope() {
        let step = json!({
            "type": "action",
            "elementId": "btn-1",
            "label": "submit it",
            "action": "click",
            "params": {"button": "left"},
            "waitOptions": {"enabled": true},
            "expectChange": true,
            "unknownFutureOptIn": 42
        });

        let payload = step_action_payload(&step);

        assert_eq!(
            payload.get("elementId").and_then(|v| v.as_str()),
            Some("btn-1")
        );
        assert!(payload.get("id").is_none());
        assert_eq!(
            payload.pointer("/action/action").and_then(|v| v.as_str()),
            Some("click")
        );
        for field in [
            "params",
            "waitOptions",
            "expectChange",
            "unknownFutureOptIn",
        ] {
            assert!(
                payload.pointer(&format!("/action/{field}")).is_some(),
                "flat step dropped `{field}` — the exact loss that made \
                 `waitOptions` unreachable on batch-execute"
            );
        }
        // Step-addressing keys must not be mistaken for action fields.
        for reserved in ["type", "label", "elementId", "element_id"] {
            assert!(
                payload.pointer(&format!("/action/{reserved}")).is_none(),
                "`{reserved}` addresses the step, not the action"
            );
        }
    }

    #[test]
    fn flat_step_accepts_both_element_id_spellings() {
        // The two capability batch endpoints had drifted: one read `element_id`,
        // the other `elementId`. Both must resolve.
        let snake = step_action_payload(&json!({"element_id": "btn-1", "action": "click"}));
        let camel = step_action_payload(&json!({"elementId": "btn-1", "action": "click"}));

        assert_eq!(
            snake.get("elementId").and_then(|v| v.as_str()),
            Some("btn-1")
        );
        assert_eq!(
            camel.get("elementId").and_then(|v| v.as_str()),
            Some("btn-1")
        );
        assert_eq!(snake, camel);
    }

    #[test]
    fn flat_step_accepts_an_already_nested_action_envelope() {
        // The two batch grammars have drifted before, so the flat lifter also
        // takes the nested shape rather than forwarding an object as the
        // action *name* (which is never valid).
        let payload = step_action_payload(&json!({
            "elementId": "btn-1",
            "action": {"action": "type", "params": {"text": "hi"}, "waitOptions": {"visible": true}}
        }));

        assert_eq!(
            payload.pointer("/action/action").and_then(|v| v.as_str()),
            Some("type")
        );
        assert_eq!(
            payload
                .pointer("/action/params/text")
                .and_then(|v| v.as_str()),
            Some("hi")
        );
        assert!(payload.pointer("/action/waitOptions").is_some());
    }

    #[test]
    fn nested_envelope_wins_over_a_flat_sibling_of_the_same_name() {
        let payload = step_action_payload(&json!({
            "elementId": "btn-1",
            "action": {"action": "type", "params": {"text": "nested"}},
            "params": {"text": "flat"}
        }));

        assert_eq!(
            payload
                .pointer("/action/params/text")
                .and_then(|v| v.as_str()),
            Some("nested"),
            "an explicit envelope must not be overwritten by a flat sibling"
        );
    }

    #[test]
    fn a_present_but_non_string_action_key_still_falls_back_to_click() {
        // Regression: keying the default off "entry absent" instead of "no
        // usable name" forwarded `{"action": null}`, which the frontend
        // rejects with "Action 'null' is not allowed" — a hard failure on a
        // path (`/control/batch`) that previously clicked. The hand-rolled
        // `as_str().unwrap_or("click")` this replaced defaulted on ALL of these.
        for bad in [json!(null), json!(false), json!(7), json!([])] {
            let payload = step_action_payload(&json!({"elementId": "btn-1", "action": bad}));
            assert_eq!(
                payload.pointer("/action/action").and_then(|v| v.as_str()),
                Some("click"),
                "a non-string action name must not reach the frontend verbatim"
            );
        }
    }

    #[test]
    fn flat_params_survive_a_bare_string_action_on_a_batch_step() {
        // The runner's own capabilities manifest documents batch-actions steps
        // as FLAT (`{elementId, action, params}`) while the SDK type is nested.
        // A caller following the manifest must not silently lose its params.
        let payload = step_action_payload(&json!({
            "elementId": "input-1",
            "action": "type",
            "params": {"text": "hi"}
        }));

        assert_eq!(
            payload.pointer("/action/action").and_then(|v| v.as_str()),
            Some("type")
        );
        assert_eq!(
            payload
                .pointer("/action/params/text")
                .and_then(|v| v.as_str()),
            Some("hi"),
            "flat params on a bare-string action step must reach the envelope"
        );
    }

    #[test]
    fn a_null_preferred_element_id_falls_through_to_the_other_spelling() {
        let payload = step_action_payload(
            &json!({"elementId": null, "element_id": "btn-1", "action": "click"}),
        );
        assert_eq!(
            payload.get("elementId").and_then(|v| v.as_str()),
            Some("btn-1")
        );
    }

    #[test]
    fn window_label_is_hoisted_to_the_payload_root_from_either_grammar() {
        // `windowLabel` is declared on the SDK action envelope but consumed at
        // the payload ROOT. Left in the envelope it routes nothing — the step
        // silently runs against the main window.
        let flat = step_action_payload(&json!({
            "elementId": "btn-1",
            "action": "click",
            "windowLabel": "term-1"
        }));
        let nested = step_action_payload(&json!({
            "elementId": "btn-1",
            "action": {"action": "click", "windowLabel": "term-1"}
        }));

        for (name, payload) in [("flat", &flat), ("nested", &nested)] {
            assert_eq!(
                payload.get(TARGET_WINDOW_FIELD).and_then(|v| v.as_str()),
                Some("term-1"),
                "{name} step must route to the addressed window"
            );
            assert!(
                payload.pointer("/action/windowLabel").is_none(),
                "{name} step left `windowLabel` in the envelope, where nothing reads it"
            );
        }
    }

    #[test]
    fn a_main_or_absent_window_label_leaves_the_payload_unrouted() {
        // `target_window_payload` no-ops for main/empty, so the single-window
        // default payload stays byte-identical.
        for step in [
            json!({"elementId": "btn-1", "action": "click"}),
            json!({"elementId": "btn-1", "action": "click", "windowLabel": "main"}),
            json!({"elementId": "btn-1", "action": "click", "windowLabel": ""}),
        ] {
            let payload = step_action_payload(&step);
            assert!(
                payload.get(TARGET_WINDOW_FIELD).is_none(),
                "no routing field should be stamped for {step}"
            );
            assert_eq!(
                payload.pointer("/action/action").and_then(|v| v.as_str()),
                Some("click")
            );
        }
    }

    #[test]
    fn window_label_still_composes_onto_an_action_payload() {
        // `target_window_payload` stamps the routing field onto the payload
        // root; the action envelope must be left alone by it.
        let payload = target_window_payload(
            element_action_payload("btn-1", json!({"action": "click"})),
            Some("term-1"),
        );
        assert_eq!(
            payload.get(TARGET_WINDOW_FIELD).and_then(|v| v.as_str()),
            Some("term-1")
        );
        assert_eq!(
            payload.pointer("/action/action").and_then(|v| v.as_str()),
            Some("click")
        );
    }
}

#[cfg(test)]
mod legacy_evaluate_budget_tests {
    //! The legacy `page_evaluate` envelope has to tell the frontend how long it
    //! will actually wait.
    //!
    //! The defect these lock down: the frontend handler auto-awaited a
    //! hardcoded 30 s while this envelope waited `ui_bridge_ipc()` (10 s by
    //! default), so the Rust side gave up first on every slow expression. The
    //! caller got the generic "Is the frontend running?" — false, it was
    //! running and still awaiting — the frontend's own budget/provenance
    //! message was unreachable, and the eventual answer was delivered into a
    //! pending slot the timeout had already removed.
    use super::{
        get_ui_bridge_timeout_ms, stamp_legacy_evaluate_budget, EVALUATE_REQUEST_TYPE,
        EVALUATE_TIMEOUT_FIELD, EVALUATE_TIMEOUT_FROM_DEFAULT_FIELD,
    };

    #[test]
    fn stamps_this_envelopes_own_wait_on_page_evaluate() {
        let out = stamp_legacy_evaluate_budget(
            EVALUATE_REQUEST_TYPE,
            serde_json::json!({ "expression": "document.title" }),
        );
        // Asserted against the accessor, not a literal: the budget is
        // operator-settable via QONTINUI_TIMEOUT_UI_BRIDGE_IPC, and a test that
        // hardcoded 10000 would only be checking the default.
        assert_eq!(
            out[EVALUATE_TIMEOUT_FIELD].as_u64(),
            Some(get_ui_bridge_timeout_ms()),
            "the frontend must be told the wait this envelope actually applies"
        );
        assert_eq!(
            out[EVALUATE_TIMEOUT_FROM_DEFAULT_FIELD].as_bool(),
            Some(true),
            "a legacy-route caller cannot choose a budget, so it is never theirs"
        );
        // The caller's own fields survive untouched.
        assert_eq!(out["expression"].as_str(), Some("document.title"));
    }

    #[test]
    fn overwrites_a_caller_supplied_budget_rather_than_honouring_it() {
        // `POST /page/evaluate` (legacy, via sdk_client) forwards an arbitrary
        // JSON body, so a caller CAN put `timeout_ms` on the wire. This
        // envelope still waits `get_ui_bridge_timeout_ms()` regardless, so
        // letting a larger number through to the frontend would re-open the
        // exact gap this stamp closes — one field further along.
        let out = stamp_legacy_evaluate_budget(
            EVALUATE_REQUEST_TYPE,
            serde_json::json!({ "expression": "x", "timeout_ms": 600_000 }),
        );
        assert_eq!(
            out[EVALUATE_TIMEOUT_FIELD].as_u64(),
            Some(get_ui_bridge_timeout_ms())
        );
        assert_eq!(
            out[EVALUATE_TIMEOUT_FROM_DEFAULT_FIELD].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn leaves_every_other_request_type_byte_identical() {
        // Only `page_evaluate` runs a timer of its own. Stamping a budget onto
        // anything else would be noise on the wire.
        for request_type in ["get_elements", "get_snapshot", "click_by_text", "type_into"] {
            let payload = serde_json::json!({ "selector": "#a" });
            assert_eq!(
                stamp_legacy_evaluate_budget(request_type, payload.clone()),
                payload,
                "{request_type} must not be stamped"
            );
        }
    }

    #[test]
    fn tolerates_a_non_object_payload() {
        // A non-Object payload flattens to nothing on the wire, so there is no
        // place to put the budget — and no reason to panic over it.
        for payload in [
            serde_json::Value::Null,
            serde_json::json!("raw"),
            serde_json::json!([1, 2]),
        ] {
            assert_eq!(
                stamp_legacy_evaluate_budget(EVALUATE_REQUEST_TYPE, payload.clone()),
                payload
            );
        }
    }

    #[test]
    fn budget_leaves_room_for_the_frontends_reporting_margin() {
        // The frontend subtracts PAGE_EVALUATE_TIMEOUT_MARGIN_MS (250 ms) from
        // whatever we send so its precise message wins the race. That only
        // works while the budget is comfortably above the margin.
        assert!(
            get_ui_bridge_timeout_ms() > 250,
            "an IPC budget at or under the frontend's 250ms margin would make \
             the frontend await the full budget and lose the race again"
        );
    }
}
