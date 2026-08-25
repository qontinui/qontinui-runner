//! Request/response types for UI Bridge handlers.
//!
//! Extracted from the original monolithic ui_bridge.rs. Types that are
//! tightly coupled to a single handler family live in that submodule
//! (e.g. `ActionQueryParams` in `elements.rs`); only the broadly shared
//! top-level request types live here.

use serde::{Deserialize, Serialize};

use crate::mcp::envelope::RequestHints;

/// Request to execute an action on an element
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIBridgeActionRequest {
    pub(crate) action: String,
    #[serde(default)]
    pub(crate) params: Option<serde_json::Value>,
    #[serde(default)]
    pub(crate) wait_options: Option<serde_json::Value>,
    /// Optional opt-in detector: when set, the handler takes a discover
    /// snapshot before and after the action and reports whether the DOM
    /// element graph changed. Lets callers detect "click had no effect"
    /// silent no-ops (e.g. clicking a disabled-but-hit-testable button).
    ///
    /// Accepts either a bool (`expectChange: true`, uses defaults) or an
    /// object: `{ "settleMs": 250 }` to control the post-action settle
    /// delay before the second snapshot.
    #[serde(default)]
    pub(crate) expect_change: Option<serde_json::Value>,
    /// Opt-in staleness gate: the snapshot id the caller reasoned from
    /// (`ubs2_<count36>_<mountEvidence36>_<content>_<generation>`, minted by
    /// `POST /ui-bridge/control/discover`).
    ///
    /// When supplied, the handler takes a pre-action snapshot and REFUSES the
    /// action — before anything commits — with `ELEMENT_STALE` +
    /// `context.staleReason = "snapshot-superseded"` if the world has moved
    /// since. Omitting it preserves today's behaviour exactly; that opt-in
    /// boundary is the design decision in plan
    /// `2026-08-20-ui-bridge-snapshot-identity-and-selector-candidates`
    /// (option B: a caller that supplies an id gets a hard legible failure
    /// instead of a blind click, and re-snapshotting is cheap to recover from
    /// whereas a wrong click may not be).
    ///
    /// **Unknown is not stale.** An id this runner cannot parse, or a state
    /// where the pre-action snapshot could not be taken at all, does NOT
    /// reject: "cannot judge" is a different thing from "is stale", and
    /// failing closed on it would break callers for no safety gain. Both cases
    /// are logged and the action proceeds exactly as if no id had been given.
    ///
    /// Residuals, stated rather than implied:
    ///
    /// - **Per window.** `POST /control/discover` mints ids for the MAIN
    ///   webview. An action targeting a pop-out window (`?windowLabel=`)
    ///   compares against THAT window's element set, which no citable id
    ///   currently describes — so do not cite an id on a windowed action.
    /// - **Millisecond resolution.** The mount half of the signature is a hash
    ///   over `registeredAt`, which is millisecond-resolution, so a remount
    ///   completing inside the same millisecond stays invisible.
    /// - **Unobserved content change.** Element-set churn and remounts are
    ///   caught unconditionally (they live in `count` + `generation`, which a
    ///   mount-only fold reproduces exactly). A pure content change is caught
    ///   only because this route stamps a fresh snapshot of its own before
    ///   acting; a caller path that takes NO intervening snapshot — the SDK's
    ///   in-process executor, for one — cannot see it, because nothing
    ///   observed it. The gate is a strong precondition, not a total
    ///   guarantee.
    #[serde(default)]
    pub(crate) from_snapshot_id: Option<String>,
    /// D3 effect-calculus per-request opt-in: predict-then-verify for THIS
    /// action, without flipping the executor-wide flag. Forwarded verbatim to
    /// the SDK inside the action envelope — the runner never interprets it.
    ///
    /// It **must** be declared here rather than left to `extra`: `extra` is a
    /// flatten catch-all that gets merged into `params`, so an undeclared SDK
    /// request field is delivered as an action *parameter* and
    /// `request.verifyEffect` reads `undefined` on the other side.
    #[serde(default)]
    pub(crate) verify_effect: Option<bool>,
    /// Opt into the ranked list of OTHER strategies that would have resolved
    /// the target, returned on `elementResolution.alternates`.
    ///
    /// Purely an SDK-side concern — the runner computes nothing for it and
    /// only carries it into the action envelope. Opt-in because building the
    /// list forces the whole resolution chain to run instead of stopping at
    /// the first hit (`O(elements × candidates)`), and a *request* field
    /// rather than a config setting so the same call cannot return different
    /// shapes on different machines.
    ///
    /// Same declaration requirement as `verify_effect` above, and for the same
    /// reason: as an undeclared field it was merged into `params`, which made
    /// the opt-in unreachable on every runner transport.
    #[serde(default)]
    pub(crate) include_resolution_alternates: Option<bool>,
    /// Capture any extra top-level fields (e.g., targetPosition, text, clear)
    /// so they can be merged into params for actions that accept flat format.
    ///
    /// **Only genuine flat action parameters belong here.** Anything that is
    /// part of the SDK's `ControlActionRequest` grammar must be a declared
    /// field above, or it silently becomes a parameter instead of a request
    /// field.
    #[serde(flatten)]
    pub(crate) extra: serde_json::Map<String, serde_json::Value>,
}

impl RequestHints for UIBridgeActionRequest {
    fn shape_error_suggestions() -> Option<Vec<String>> {
        Some(vec![
            "Required field: `action` (string). \
             Optional: `params` (object), `waitOptions` (object), `expectChange` (bool or object), \
             `fromSnapshotId` (string, from POST /ui-bridge/control/discover)."
                .to_string(),
            "Use the element's advertised actions. Common values: click, type, hover, \
             scroll, focus, blur, clear, select, setValue."
                .to_string(),
        ])
    }
    fn shape_error_data() -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "allowedActions": [
                "click", "doubleClick", "double_click", "double", "right", "middle",
                "type", "sendKeys", "clear", "select", "focus", "blur", "hover",
                "scroll", "scrollIntoView", "scroll_into_view", "check", "uncheck",
                "toggle", "drag", "setValue", "submit", "reset", "autocomplete"
            ]
        }))
    }
}

/// Request to execute an action on a component.
///
/// Accepts BOTH the wrapped shape `{"params":{...}}` and the bare shape the
/// action's own `paramSchema` advertises (`{"layoutId":"single"}`). Flat
/// top-level keys are captured via `#[serde(flatten)] extra` and merged into
/// `params` by [`Self::merged_params`] — the same predictable "the schema IS
/// the contract" handling the element-action route already uses
/// (`UIBridgeActionRequest.extra` + the merge in `elements.rs`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIBridgeComponentActionRequest {
    #[serde(default)]
    pub(crate) params: Option<serde_json::Value>,
    /// Capture any extra top-level fields (the bare `{"layoutId":"single"}`
    /// shape) so they can be merged into `params`.
    #[serde(flatten)]
    pub(crate) extra: serde_json::Map<String, serde_json::Value>,
}

impl UIBridgeComponentActionRequest {
    /// Resolve the effective params object, merging any flat top-level keys
    /// (`extra`) into an explicit `params` object. Explicit `params` wins on
    /// key collision (`.entry(k).or_insert(v)`), mirroring the element-action
    /// merge precedence in `elements.rs`.
    pub(crate) fn merged_params(self) -> Option<serde_json::Value> {
        if self.extra.is_empty() {
            return self.params;
        }
        let mut base = match self.params {
            Some(serde_json::Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        for (k, v) in self.extra {
            base.entry(k).or_insert(v);
        }
        Some(serde_json::Value::Object(base))
    }
}

impl RequestHints for UIBridgeComponentActionRequest {}

/// The discovery filters, in the nested `{"options": {...}}` spelling.
///
/// This is the shape the SDK itself uses and the shape the runner's own
/// internal sweeps send over IPC (`json!({"options": {"interactiveOnly":
/// false}})` in `elements.rs` and `recovery_executor.rs`), where it IS
/// honoured. The HTTP request struct did not have an `options` field at all,
/// so an `options` body deserialized to all-`None` and the handler forwarded
/// nulls — the filters were silently dropped:
///
/// ```text
/// {}                                                          -> 122 elements
/// {"options":{"includeHidden":true,"interactiveOnly":false}}  -> 122  (ignored)
/// {"includeHidden":true,"interactiveOnly":false}              -> 208
/// ```
///
/// Two grammars for one concept, one of them silently inert. Two independent
/// agents hit this while measuring something else, so it actively produced
/// wrong measurements. Both spellings are now accepted.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIBridgeDiscoveryOptions {
    #[serde(default)]
    pub(crate) root: Option<String>,
    #[serde(default, alias = "interactive_only")]
    pub(crate) interactive_only: Option<bool>,
    #[serde(default, alias = "include_hidden")]
    pub(crate) include_hidden: Option<bool>,
    #[serde(default)]
    pub(crate) limit: Option<u32>,
    #[serde(default)]
    pub(crate) types: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) selector: Option<String>,
    /// Accepted here too so a caller that puts everything in `options` is not
    /// half-honoured. `force` is a meta-flag rather than a filter, so a
    /// top-level spelling still wins.
    #[serde(default)]
    pub(crate) force: Option<bool>,
}

/// Discovery options request.
///
/// Accepts the filters either at the top level or nested under `options`
/// (see [`UIBridgeDiscoveryOptions`]). Where a field appears in BOTH, the
/// top-level value wins — it is the more specific spelling and the one
/// existing callers already use, so adding `options` support cannot change
/// what an existing request resolves to.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIBridgeDiscoveryRequest {
    #[serde(default)]
    pub(crate) root: Option<String>,
    #[serde(default, alias = "interactive_only")]
    pub(crate) interactive_only: Option<bool>,
    #[serde(default, alias = "include_hidden")]
    pub(crate) include_hidden: Option<bool>,
    #[serde(default)]
    pub(crate) limit: Option<u32>,
    #[serde(default)]
    pub(crate) types: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) selector: Option<String>,
    /// When true, drop the registry's cached id/label entries and rebuild
    /// from the live DOM before returning. Workaround for React components
    /// that swap visible button text via state — the registry stamps a
    /// label at first registration and the existing per-element early-return
    /// in the auto-register scanner never re-derives it on subsequent scans.
    #[serde(default)]
    pub(crate) force: Option<bool>,
    /// The nested spelling. Merged by [`UIBridgeDiscoveryRequest::resolve`].
    #[serde(default)]
    pub(crate) options: Option<UIBridgeDiscoveryOptions>,
}

impl UIBridgeDiscoveryRequest {
    /// Collapse the two spellings into the single set of filters the IPC
    /// payload is built from. Top-level wins over `options` field by field.
    pub(crate) fn resolve(&self) -> UIBridgeDiscoveryOptions {
        let nested = self.options.as_ref();
        UIBridgeDiscoveryOptions {
            root: self
                .root
                .clone()
                .or_else(|| nested.and_then(|o| o.root.clone())),
            interactive_only: self
                .interactive_only
                .or_else(|| nested.and_then(|o| o.interactive_only)),
            include_hidden: self
                .include_hidden
                .or_else(|| nested.and_then(|o| o.include_hidden)),
            limit: self.limit.or_else(|| nested.and_then(|o| o.limit)),
            types: self
                .types
                .clone()
                .or_else(|| nested.and_then(|o| o.types.clone())),
            selector: self
                .selector
                .clone()
                .or_else(|| nested.and_then(|o| o.selector.clone())),
            force: self.force.or_else(|| nested.and_then(|o| o.force)),
        }
    }
}

/// Request to start UI Bridge exploration
#[derive(Debug, Deserialize)]
pub struct StartUIBridgeExplorationRequest {
    /// Target type: "web", "desktop", or "mobile"
    #[serde(default = "default_target_type")]
    pub target_type: String,
    /// Connection URL for the target application
    pub connection_url: String,
    /// Maximum navigation depth (default: 2)
    #[serde(default)]
    pub max_depth: Option<i32>,
    /// Maximum elements per page (default: 20)
    #[serde(default)]
    pub max_elements_per_page: Option<i32>,
    /// Maximum total elements to explore (default: 100)
    #[serde(default)]
    pub max_total_elements: Option<i32>,
    /// Delay between actions in milliseconds (default: 500)
    #[serde(default)]
    pub action_delay_ms: Option<i32>,
    /// Keywords in element text/id to skip
    #[serde(default)]
    pub blocked_keywords: Option<Vec<String>>,
    /// Keywords that are always safe to interact with
    #[serde(default)]
    pub safe_keywords: Option<Vec<String>>,
    /// CSS selectors to skip
    #[serde(default)]
    pub blocked_selectors: Option<Vec<String>>,
    /// Whether to capture screenshots (default: false)
    #[serde(default)]
    pub capture_screenshots: Option<bool>,
    /// Whether to run state discovery on results (default: true)
    #[serde(default)]
    pub run_state_discovery: Option<bool>,
}

pub(crate) fn default_target_type() -> String {
    "web".to_string()
}

/// Request to write to the clipboard
#[derive(Debug, Deserialize)]
pub struct ClipboardWriteRequest {
    pub text: String,
    #[serde(default)]
    pub html: Option<String>,
}

/// Request for getting UI Bridge exploration status
#[derive(Debug, Deserialize)]
pub struct UIBridgeExplorationStatusRequest {
    pub job_id: Option<String>,
}

/// Request for discovering states from render logs
#[derive(Debug, Deserialize)]
pub struct DiscoverStatesRequest {
    /// Array of DOM snapshot render log entries
    pub render_logs: Vec<serde_json::Value>,
}

// ============================================================================
// Structured Error Taxonomy
// ============================================================================

/// Machine-readable error codes for UI Bridge operations.
/// Enables AI agents to match on error type rather than parsing strings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UiBridgeErrorCode {
    // Transport errors
    Timeout,
    CircuitBreakerOpen,
    ConcurrencyLimitReached,
    FrontendUnresponsive,
    FrontendNotReady,
    /// The **native** event loop has stopped pumping messages, so anything
    /// that must be *dispatched* to it — `window.close()`, `AppHandle::exit`,
    /// every window getter, `eval` from off-thread — silently queues instead
    /// of running.
    ///
    /// Deliberately distinct from [`Self::FrontendNotReady`] and
    /// [`Self::FrontendUnresponsive`], and the distinction is the whole point
    /// of the variant: in this failure the frontend is typically **fine**
    /// (WebView2 services `fetch` out-of-process, so the UI Bridge pong keeps
    /// arriving) and it is the Win32 host loop that is wedged. A caller that
    /// cannot tell the two apart retries a readiness wait that will never
    /// succeed. Produced by the close door in `page.rs` off
    /// `health_monitor::ui_thread_pumping`; see plan
    /// `2026-08-19-runner-blocked-ui-thread-cannot-be-closed`, Phase 3.
    EventLoopUnresponsive,
    WindowNotFound,
    // Element targeting errors
    ElementNotFound,
    ElementNotVisible,
    ElementNotEnabled,
    /// The caller's reference no longer points at what it pointed at.
    ///
    /// Two populations share this code, told apart by `context.staleReason`
    /// (the SDK's `UB-STALE-ELEMENT` family, mirrored here so both sides emit
    /// one taxonomy):
    ///
    /// - `unmounted` / `rerendered` / `detached` — the ELEMENT resolves to
    ///   nothing live. Recovery: re-find the element.
    /// - `snapshot-superseded` — the whole SNAPSHOT the caller reasoned from
    ///   is out of date, refused BEFORE the action commits. Recovery:
    ///   **re-snapshot**, not re-find. Re-finding here would *succeed* and
    ///   click the wrong thing, which is exactly the failure this reason
    ///   exists to prevent, so its message is written at the rejection site
    ///   rather than pulled from a shared catalog.
    ElementStale,
    // Action errors
    ActionFailed,
    /// The requested action name is not in the element's advertised
    /// `actions` list. Issued pre-IPC by `ui_bridge_execute_action_handler`
    /// so the caller gets a flat HTTP 400 with the supported-action list in
    /// `context.supported_actions` BEFORE the Phase 5 RecoveryExecutor can
    /// synthesize a misleading `recovered:true` success against a side-action.
    /// Loop iter-2 item 2 — mirrors the post-recovery contract-violation
    /// branch's `data.code = "ACTION_NOT_SUPPORTED"` shape so callers see a
    /// consistent envelope on both pre-IPC and post-recovery rejection.
    ActionNotSupported,
    // Assertion errors
    AssertionFailed,
    UnknownAssertionType,
    // Request-shape errors (rejected upstream of any IPC/recovery flow)
    InvalidRequest,
    /// A request field is present but has the wrong shape / wrong name.
    /// Issued specifically for per-action param-name validation (loop B
    /// iter 2 — runner-side analogue of ui-bridge PR #33's SDK-boundary
    /// `WRONG_TYPE_PARAM` gate). Carries a `didYouMean` recovery hint in
    /// `context` so callers can self-correct without a round-trip.
    InvalidParam,
    /// A required request field is missing. Sibling of [`InvalidParam`]
    /// for the "field absent" case. The runner emits this for
    /// `{action:"type", params:{}}` (no `text`, no `value`) so callers
    /// get a deterministic 400 instead of a recovered no-op via the
    /// LLM fallback.
    MissingParam,
    /// The `state` parameter on a `wait-for-element-state` request is not
    /// one of the recognised enum values. The list is not repeated here —
    /// `intents::ALLOWED_STATES` is the single source of truth, and a prose
    /// copy is exactly how this drifted past the ui-bridge #144
    /// `disabled`/`ariaDisabled` split.
    /// Issued as a flat HTTP 400 with `context.allowed_states: [...]` so
    /// callers can self-correct without parsing prose. Loop iter-2 item 3 —
    /// mirrors the `tab/activate` -> `knownTabs` and the action-name gate's
    /// envelope shape.
    InvalidState,
    /// The `tabId` on a `tab/activate` request is not in the static
    /// `VALID_TAB_IDS` registry. Issued as a flat HTTP 400 with
    /// `context.knownTabs: [...]` so callers can self-correct without
    /// parsing prose. Loop iter-3 item 1 — previously the rejection
    /// envelope had `error_detail = None`, so `error_detail.code` came
    /// back as `null` even though the `error` prose and `data.knownTabs`
    /// payload were present. Now the machine-readable code lives in
    /// `error_detail.code = "INVALID_TAB_ID"` to match the rest of the
    /// envelope taxonomy.
    InvalidTabId,
    // System errors
    InternalError,
}

/// Machine-readable recovery hints telling AI agents what to do next.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RecoveryHint {
    /// Re-take a DOM snapshot; element refs may have changed
    Resnapshot,
    /// Retry the same operation after a delay (milliseconds)
    RetryAfterMs(u64),
    /// Wait for the circuit breaker cooldown to expire
    WaitForRecovery,
    /// Scroll or navigate to make the element visible
    ScrollIntoView,
    /// The element exists but is disabled; wait for it to become enabled
    WaitForEnabled,
    /// Use a different selector or broaden the search criteria
    BroadenSelector,
    /// The operation cannot be recovered; skip or report failure
    Unrecoverable,
}

/// Structured error with machine-readable code and recovery hint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiBridgeError {
    pub code: UiBridgeErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<RecoveryHint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
}

impl UiBridgeError {
    pub fn timeout(duration_ms: u64) -> Self {
        Self {
            code: UiBridgeErrorCode::Timeout,
            message: format!("UI Bridge request timed out after {}ms", duration_ms),
            recovery: Some(RecoveryHint::RetryAfterMs(1000)),
            context: Some(serde_json::json!({"timeout_ms": duration_ms})),
        }
    }

    pub fn circuit_breaker_open() -> Self {
        Self {
            code: UiBridgeErrorCode::CircuitBreakerOpen,
            message: "UI Bridge temporarily unavailable (circuit breaker open)".to_string(),
            recovery: Some(RecoveryHint::WaitForRecovery),
            context: None,
        }
    }

    pub fn concurrency_limit() -> Self {
        Self {
            code: UiBridgeErrorCode::ConcurrencyLimitReached,
            message: "UI Bridge concurrency limit reached (timeout acquiring permit)".to_string(),
            recovery: Some(RecoveryHint::RetryAfterMs(500)),
            context: None,
        }
    }

    pub fn window_not_found() -> Self {
        Self {
            code: UiBridgeErrorCode::WindowNotFound,
            message: "Main webview window not found".to_string(),
            recovery: Some(RecoveryHint::Unrecoverable),
            context: None,
        }
    }

    pub fn element_not_found(selector: &str) -> Self {
        Self {
            code: UiBridgeErrorCode::ElementNotFound,
            message: format!("No element found matching criteria {}", selector),
            recovery: Some(RecoveryHint::Resnapshot),
            context: Some(serde_json::json!({"selector": selector})),
        }
    }

    /// Reject an unknown action name BEFORE any IPC / RecoveryExecutor flow
    /// can synthesize a misleading success. Mirrors the `tab/activate` ->
    /// `knownTabs` prior art: hint carries the full supported-action list so
    /// callers can self-correct without an extra round-trip.
    pub fn invalid_action(unknown: &str, supported: &[&str]) -> Self {
        Self {
            code: UiBridgeErrorCode::InvalidRequest,
            message: format!(
                "Unknown action '{}'. Supported actions: {}",
                unknown,
                supported.join(", ")
            ),
            recovery: Some(RecoveryHint::Unrecoverable),
            context: Some(serde_json::json!({
                "action": unknown,
                "supportedActions": supported,
            })),
        }
    }

    /// Reject `{action:"type", params:{value:"..."}}` BEFORE the IPC + Phase 5
    /// `RecoveryExecutor` chain can synthesize a misleading success. Issued
    /// when the caller named `value` (the param for `setValue` / `select`)
    /// instead of `text`. Mirrors the SDK-side `WRONG_TYPE_PARAM` envelope
    /// added in ui-bridge PR #33 (`relay-handlers.ts::executeElementAction`)
    /// for the non-SDK dispatch path that hits the runner directly. Docs
    /// reference: `ui-bridge/docs-site/docs/api/runner-features.md` §
    /// "Per-action param names" promises `HTTP 400` with the literal
    /// `type: 'value' is unknown; did you mean 'text'?` message.
    ///
    /// `context.didYouMean` + `context.field` give callers a deterministic
    /// self-correction hint without a round-trip — the
    /// `recovery: { didYouMean: "text", field: "params.text" }` contract
    /// documented in the loop B iter 2 spec.
    pub fn type_param_invalid() -> Self {
        Self {
            code: UiBridgeErrorCode::InvalidParam,
            message: "type: 'value' is unknown; did you mean 'text'?".to_string(),
            recovery: Some(RecoveryHint::Unrecoverable),
            context: Some(serde_json::json!({
                "action": "type",
                "provided": "value",
                "didYouMean": "text",
                "field": "params.text",
            })),
        }
    }

    /// Reject `{action:"type", params:{}}` (no `text`, no `value`) BEFORE
    /// the recovery chain. Sibling of [`type_param_invalid`] for the
    /// missing-field case. Returned as HTTP 400 with `code: MISSING_PARAM`
    /// so the caller can distinguish "field absent" from "field present
    /// with wrong name" without parsing the prose. Docs reference: same
    /// section as `type_param_invalid` — the runner-side analogue of the
    /// SDK's `MISSING_PARAM` envelope (ui-bridge PR #33).
    pub fn type_param_missing() -> Self {
        Self {
            code: UiBridgeErrorCode::MissingParam,
            message: "type requires 'text' parameter".to_string(),
            recovery: Some(RecoveryHint::Unrecoverable),
            context: Some(serde_json::json!({
                "action": "type",
                "required": ["text"],
                "field": "params.text",
            })),
        }
    }

    /// Reject `wait-for-element-state` when the `state` parameter is not in
    /// the recognised enum — see `intents::ALLOWED_STATES`, which the caller
    /// passes in as `allowed`. Loop iter-2 item 3 —
    /// previously returned an `api_error(prose)` envelope with no machine-
    /// readable code, making the error indistinguishable from any other
    /// transport failure. Now: HTTP 400 with `error_detail.code:
    /// "INVALID_STATE"` and `context.allowed_states` for self-correction.
    pub fn invalid_state(provided: &str, allowed: &[&str]) -> Self {
        Self {
            code: UiBridgeErrorCode::InvalidState,
            message: format!(
                "wait-for-element-state: unknown state '{}', expected {}",
                provided,
                allowed.join("|")
            ),
            recovery: Some(RecoveryHint::Unrecoverable),
            context: Some(serde_json::json!({
                "code": "INVALID_STATE",
                "provided_state": provided,
                "allowed_states": allowed,
            })),
        }
    }

    /// Reject `tab/activate` when the `tabId` parameter is not in the
    /// static `VALID_TAB_IDS` registry. Loop iter-3 item 1 — previously
    /// the handler returned `ApiResponse { error_detail: None, ... }`,
    /// so `error_detail.code` came back as `null` even though the prose
    /// `error` and `data.knownTabs` payload were present. Now: HTTP 400
    /// with `error_detail.code: "INVALID_TAB_ID"` and `context.knownTabs`
    /// for self-correction, matching the rest of the envelope taxonomy.
    pub fn invalid_tab_id(provided: &str, known: &[&str]) -> Self {
        Self {
            code: UiBridgeErrorCode::InvalidTabId,
            message: format!("Unknown tabId: \"{}\"", provided),
            recovery: Some(RecoveryHint::Unrecoverable),
            context: Some(serde_json::json!({
                "code": "INVALID_TAB_ID",
                "tabId": provided,
                "knownTabs": known,
            })),
        }
    }

    /// Reject `GET /control/element/:id` when the registry has no entry
    /// for the requested element id. Loop iter-3 item 3 — previously
    /// `wrap_ipc_result` produced `ApiResponse { success:false, error_detail
    /// .code: null, error: "" }` on the empty-error case because the
    /// frontend's `get_element` IPC handler returns `{ success:false,
    /// error: "" }` for unknown ids (no element to report on). Now: HTTP
    /// 400 with `error_detail.code: "ELEMENT_NOT_FOUND"` and
    /// `context.elementId` carrying the rejected id, matching the rest of
    /// the envelope taxonomy and the existing `element_not_found(selector)`
    /// factory.
    pub fn element_not_found_by_id(element_id: &str) -> Self {
        Self {
            code: UiBridgeErrorCode::ElementNotFound,
            message: format!("No element with id {}", element_id),
            recovery: Some(RecoveryHint::Resnapshot),
            context: Some(serde_json::json!({
                "code": "ELEMENT_NOT_FOUND",
                "elementId": element_id,
            })),
        }
    }

    /// Refuse an action whose `fromSnapshotId` no longer describes the page —
    /// the pre-action arm of the remount/effect signature.
    ///
    /// Deliberately NOT a new top-level code. It joins the SDK's
    /// `UB-STALE-ELEMENT` family as a fourth `staleReason`
    /// (`snapshot-superseded`, alongside `unmounted` / `rerendered` /
    /// `detached`) so a caller matches one code and switches on one field
    /// across both halves of the stack.
    ///
    /// `changeKind` is the finer discriminator, straight from the signature
    /// predicates in `helpers.rs`:
    ///
    /// - `remounted` — `SnapshotSignature::remounted_from`: the same elements
    ///   showing the same things, but re-registered under a NEW mount. This is
    ///   the case nothing could previously catch: the element still resolves,
    ///   so the element-level stale reasons never fire, yet any state inside
    ///   that subtree (a wizard's step, a form draft, scroll position) is gone.
    /// - `elementCountChanged` — elements appeared or disappeared.
    /// - `contentChanged` — the same number of elements, showing something
    ///   else.
    ///
    /// **The recovery text is written here, not looked up.** The other three
    /// stale reasons recover by re-FINDING the element; this one must not. A
    /// re-find would succeed and click the wrong thing — that is the whole
    /// failure this reason exists to prevent — so the message names
    /// re-SNAPSHOT, and `context.currentSnapshotId` is the id to re-reason
    /// from.
    pub fn snapshot_superseded(
        from_id: &str,
        current_id: &str,
        change_kind: &str,
        from_count: usize,
        current_count: usize,
    ) -> Self {
        Self {
            code: UiBridgeErrorCode::ElementStale,
            message: format!(
                "Refusing the action: the snapshot it cites has been superseded ({}). \
                 Cited {}, current is {}. Do NOT re-find the element — it will resolve, and to \
                 the wrong thing. Re-snapshot (POST /ui-bridge/control/discover with \
                 {{\"interactiveOnly\": false}}) and re-issue the action against the new \
                 snapshot id.",
                change_kind, from_id, current_id
            ),
            recovery: Some(RecoveryHint::Resnapshot),
            context: Some(serde_json::json!({
                "code": "UB-STALE-ELEMENT",
                "staleReason": "snapshot-superseded",
                "changeKind": change_kind,
                "fromSnapshotId": from_id,
                "currentSnapshotId": current_id,
                "fromElementCount": from_count,
                "currentElementCount": current_count,
            })),
        }
    }

    pub fn element_not_visible(selector: &str, total_found: usize) -> Self {
        Self {
            code: UiBridgeErrorCode::ElementNotVisible,
            message: format!(
                "Found {} element(s) matching '{}' but none are visible",
                total_found, selector
            ),
            recovery: Some(RecoveryHint::ScrollIntoView),
            context: Some(serde_json::json!({"selector": selector, "total_found": total_found})),
        }
    }

    pub fn action_failed(message: impl Into<String>) -> Self {
        Self {
            code: UiBridgeErrorCode::ActionFailed,
            message: message.into(),
            recovery: Some(RecoveryHint::Resnapshot),
            context: None,
        }
    }

    pub fn frontend_not_ready(diagnostics: serde_json::Value) -> Self {
        Self {
            code: UiBridgeErrorCode::FrontendNotReady,
            message: "UI Bridge frontend did not become ready".to_string(),
            recovery: Some(RecoveryHint::RetryAfterMs(2000)),
            context: Some(diagnostics),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: UiBridgeErrorCode::InternalError,
            message: message.into(),
            recovery: Some(RecoveryHint::Unrecoverable),
            context: None,
        }
    }
}

/// Classify a transport-level error string into a structured error.
/// Used by wrap_ipc_result and batch handler to convert legacy string errors.
/// Checks both transport-level and assertion-level patterns so that frontend
/// error messages like "No element found" get the correct error code.
pub fn classify_transport_error(error_msg: &str) -> UiBridgeError {
    // Transport-level errors
    if error_msg.contains("did not become ready") {
        // Try to parse the diagnostics JSON that gather_readiness_diagnostics produced
        let diagnostics = serde_json::from_str::<serde_json::Value>(error_msg)
            .unwrap_or_else(|_| serde_json::json!({"raw": error_msg}));
        UiBridgeError::frontend_not_ready(diagnostics)
    } else if error_msg.contains("timed out") {
        UiBridgeError::timeout(0)
    } else if error_msg.contains("circuit breaker") {
        UiBridgeError::circuit_breaker_open()
    } else if error_msg.contains("concurrency limit") {
        UiBridgeError::concurrency_limit()
    } else if error_msg.contains("window not found") || error_msg.contains("Window not found") {
        UiBridgeError::window_not_found()
    }
    // Assertion/element-level errors (from frontend IPC responses)
    else if error_msg.contains("No element found") || error_msg.contains("no element found") {
        UiBridgeError::element_not_found(error_msg)
    } else if error_msg.contains("none are visible") {
        UiBridgeError::element_not_visible(error_msg, 0)
    } else if error_msg.contains("Operation failed") || error_msg.contains("action failed") {
        UiBridgeError::action_failed(error_msg)
    } else {
        UiBridgeError::internal(error_msg)
    }
}

/// Classify an assertion failure detail string into an error code.
/// Used by the verification phase to annotate failure context for AI agents.
pub fn classify_assertion_failure(detail: &str) -> UiBridgeErrorCode {
    if detail.contains("No element found") {
        UiBridgeErrorCode::ElementNotFound
    } else if detail.contains("none are visible") {
        UiBridgeErrorCode::ElementNotVisible
    } else if detail.contains("Unknown assertion type") {
        UiBridgeErrorCode::UnknownAssertionType
    } else {
        UiBridgeErrorCode::AssertionFailed
    }
}

/// Get the recovery hint for an error code.
pub fn recovery_hint_for(code: &UiBridgeErrorCode) -> RecoveryHint {
    match code {
        UiBridgeErrorCode::Timeout => RecoveryHint::RetryAfterMs(1000),
        UiBridgeErrorCode::CircuitBreakerOpen => RecoveryHint::WaitForRecovery,
        UiBridgeErrorCode::ConcurrencyLimitReached => RecoveryHint::RetryAfterMs(500),
        UiBridgeErrorCode::FrontendUnresponsive => RecoveryHint::WaitForRecovery,
        UiBridgeErrorCode::FrontendNotReady => RecoveryHint::RetryAfterMs(2000),
        // NOT `RetryAfterMs`: a wedged native loop does not clear on a fixed
        // backoff, and every retry re-enqueues onto the same blocked queue.
        // The loop *can* recover (a long synchronous handler eventually
        // returns), so it is not `Unrecoverable` either — the caller should
        // wait for the wedge to clear, or take the explicit force-close door.
        UiBridgeErrorCode::EventLoopUnresponsive => RecoveryHint::WaitForRecovery,
        UiBridgeErrorCode::WindowNotFound => RecoveryHint::Unrecoverable,
        UiBridgeErrorCode::ElementNotFound => RecoveryHint::Resnapshot,
        UiBridgeErrorCode::ElementNotVisible => RecoveryHint::ScrollIntoView,
        UiBridgeErrorCode::ElementNotEnabled => RecoveryHint::WaitForEnabled,
        UiBridgeErrorCode::ElementStale => RecoveryHint::Resnapshot,
        UiBridgeErrorCode::ActionFailed => RecoveryHint::Resnapshot,
        UiBridgeErrorCode::AssertionFailed => RecoveryHint::BroadenSelector,
        UiBridgeErrorCode::UnknownAssertionType => RecoveryHint::Unrecoverable,
        UiBridgeErrorCode::InvalidRequest => RecoveryHint::Unrecoverable,
        UiBridgeErrorCode::InvalidParam => RecoveryHint::Unrecoverable,
        UiBridgeErrorCode::MissingParam => RecoveryHint::Unrecoverable,
        UiBridgeErrorCode::InvalidState => RecoveryHint::Unrecoverable,
        UiBridgeErrorCode::InvalidTabId => RecoveryHint::Unrecoverable,
        UiBridgeErrorCode::ActionNotSupported => RecoveryHint::Unrecoverable,
        UiBridgeErrorCode::InternalError => RecoveryHint::Unrecoverable,
    }
}

#[cfg(test)]
mod component_action_params_tests {
    //! Lock down that the component-action request accepts BOTH the wrapped
    //! `{"params":{...}}` shape and the bare shape the action's `paramSchema`
    //! advertises (`{"layoutId":"single"}`) — the friction fix for the
    //! undocumented wrapper trap.

    use super::UIBridgeComponentActionRequest;

    fn parse(body: serde_json::Value) -> Option<serde_json::Value> {
        serde_json::from_value::<UIBridgeComponentActionRequest>(body)
            .expect("deserialize")
            .merged_params()
    }

    #[test]
    fn bare_shape_is_accepted_as_params() {
        let out = parse(serde_json::json!({ "layoutId": "single" }));
        assert_eq!(out, Some(serde_json::json!({ "layoutId": "single" })));
    }

    #[test]
    fn wrapped_shape_still_works() {
        let out = parse(serde_json::json!({ "params": { "layoutId": "quad" } }));
        assert_eq!(out, Some(serde_json::json!({ "layoutId": "quad" })));
    }

    #[test]
    fn explicit_params_wins_over_flat_collision() {
        let out = parse(serde_json::json!({
            "params": { "layoutId": "quad" },
            "layoutId": "single"
        }));
        assert_eq!(out, Some(serde_json::json!({ "layoutId": "quad" })));
    }

    #[test]
    fn empty_body_yields_no_params() {
        let out = parse(serde_json::json!({}));
        assert_eq!(out, None);
    }
}

#[cfg(test)]
mod iter3_factory_tests {
    //! Iter-3 — lock down the envelope shape of the new error-code
    //! factories so the wire contracts callers depend on can't drift.
    //! The `UiBridgeError` struct serialises to JSON with camelCase
    //! field names; the `code` is the SCREAMING_SNAKE_CASE wire string;
    //! the `context` carries structured self-correction payload.

    use super::{UiBridgeError, UiBridgeErrorCode};

    /// Iter-3 item 3 — `element_not_found_by_id` produces a
    /// `ELEMENT_NOT_FOUND`-coded detail whose `context.elementId`
    /// echoes the rejected id. Locks down what callers see in
    /// `error_detail.code` and `error_detail.context.elementId`
    /// when `GET /control/element/:id` rejects an unknown id.
    #[test]
    fn element_not_found_by_id_envelope_shape() {
        let detail = UiBridgeError::element_not_found_by_id("does-not-exist");
        assert!(
            matches!(detail.code, UiBridgeErrorCode::ElementNotFound),
            "code must be ElementNotFound"
        );
        assert!(
            detail.message.contains("does-not-exist"),
            "message echoes the rejected element id, got: {}",
            detail.message
        );
        let ctx = detail
            .context
            .as_ref()
            .expect("context must carry the structured payload");
        assert_eq!(
            ctx.get("code").and_then(|v| v.as_str()),
            Some("ELEMENT_NOT_FOUND")
        );
        assert_eq!(
            ctx.get("elementId").and_then(|v| v.as_str()),
            Some("does-not-exist")
        );
    }

    /// Iter-3 item 1 — `invalid_tab_id` factory contract: code is
    /// `INVALID_TAB_ID`, `context.tabId` echoes the rejected id, and
    /// `context.knownTabs` enumerates the full registry. This is the
    /// canonical structured surface; the prose `error` field is for
    /// human-readable debugging.
    #[test]
    fn invalid_tab_id_factory_envelope_shape() {
        let known = &["specs", "fleet", "terminal"];
        let detail = UiBridgeError::invalid_tab_id("not-a-tab", known);
        assert!(
            matches!(detail.code, UiBridgeErrorCode::InvalidTabId),
            "code must be InvalidTabId"
        );
        assert!(
            detail.message.contains("not-a-tab"),
            "message echoes the rejected id"
        );
        let ctx = detail
            .context
            .as_ref()
            .expect("context must carry the structured payload");
        assert_eq!(
            ctx.get("code").and_then(|v| v.as_str()),
            Some("INVALID_TAB_ID")
        );
        let known_arr = ctx
            .get("knownTabs")
            .and_then(|v| v.as_array())
            .expect("context.knownTabs must be an array");
        let names: Vec<&str> = known_arr.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(names, vec!["specs", "fleet", "terminal"]);
    }
}

#[cfg(test)]
mod discovery_options_grammar_tests {
    use super::UIBridgeDiscoveryRequest;

    fn resolved(body: &str) -> (Option<bool>, Option<bool>) {
        let req: UIBridgeDiscoveryRequest =
            serde_json::from_str(body).expect("body should deserialize");
        let o = req.resolve();
        (o.include_hidden, o.interactive_only)
    }

    /// The reported defect: the SDK's own `{"options": {...}}` spelling
    /// deserialized to all-`None` and the handler forwarded nulls, so
    /// `{"options":{"includeHidden":true,"interactiveOnly":false}}` returned
    /// 122 elements where the top-level spelling returned 208. Two grammars
    /// for one concept, one of them silently inert.
    ///
    /// All four spellings must now agree.
    #[test]
    fn all_four_spellings_agree() {
        let expected = (Some(true), Some(false));
        assert_eq!(
            resolved(r#"{"include_hidden":true,"interactive_only":false}"#),
            expected,
            "top-level snake_case"
        );
        assert_eq!(
            resolved(r#"{"includeHidden":true,"interactiveOnly":false}"#),
            expected,
            "top-level camelCase"
        );
        assert_eq!(
            resolved(r#"{"options":{"includeHidden":true,"interactiveOnly":false}}"#),
            expected,
            "nested camelCase — the spelling that was silently ignored"
        );
        assert_eq!(
            resolved(r#"{"options":{"include_hidden":true,"interactive_only":false}}"#),
            expected,
            "nested snake_case"
        );
    }

    /// Existing top-level callers must resolve to exactly what they did
    /// before, so adding `options` support cannot change any live request.
    #[test]
    fn top_level_wins_over_nested_on_conflict() {
        let req: UIBridgeDiscoveryRequest = serde_json::from_str(
            r#"{"includeHidden":true,"options":{"includeHidden":false,"interactiveOnly":false}}"#,
        )
        .unwrap();
        let o = req.resolve();
        assert_eq!(o.include_hidden, Some(true), "top-level must win");
        // ...while a field only the nested form carries is still honoured.
        assert_eq!(o.interactive_only, Some(false));
    }

    #[test]
    fn an_empty_body_resolves_to_no_filters() {
        let req: UIBridgeDiscoveryRequest = serde_json::from_str("{}").unwrap();
        let o = req.resolve();
        assert_eq!(o.include_hidden, None);
        assert_eq!(o.interactive_only, None);
        assert_eq!(o.limit, None);
    }

    /// The non-boolean filters travel through the nested form too — a caller
    /// putting everything in `options` must not be half-honoured.
    #[test]
    fn nested_options_carry_every_filter() {
        let req: UIBridgeDiscoveryRequest = serde_json::from_str(
            r##"{"options":{"root":"#app","limit":5,"selector":".btn","types":["button"],"force":true}}"##,
        )
        .unwrap();
        let o = req.resolve();
        assert_eq!(o.root.as_deref(), Some("#app"));
        assert_eq!(o.limit, Some(5));
        assert_eq!(o.selector.as_deref(), Some(".btn"));
        assert_eq!(o.types, Some(vec!["button".to_string()]));
        assert_eq!(o.force, Some(true));
    }
}
