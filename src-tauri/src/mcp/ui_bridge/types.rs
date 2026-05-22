//! Request/response types for UI Bridge handlers.
//!
//! Extracted from the original monolithic ui_bridge.rs. Types that are
//! tightly coupled to a single handler family live in that submodule
//! (e.g. `ActionQueryParams` in `elements.rs`); only the broadly shared
//! top-level request types live here.

use serde::{Deserialize, Serialize};

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
    /// Capture any extra top-level fields (e.g., targetPosition, text, clear)
    /// so they can be merged into params for actions that accept flat format.
    #[serde(flatten)]
    pub(crate) extra: serde_json::Map<String, serde_json::Value>,
}

/// Request to execute an action on a component
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIBridgeComponentActionRequest {
    #[serde(default)]
    pub(crate) params: Option<serde_json::Value>,
}

/// Discovery options request
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
    WindowNotFound,
    // Element targeting errors
    ElementNotFound,
    ElementNotVisible,
    ElementNotEnabled,
    ElementStale,
    // Action errors
    ActionFailed,
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
        UiBridgeErrorCode::InternalError => RecoveryHint::Unrecoverable,
    }
}
