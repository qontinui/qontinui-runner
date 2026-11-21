//! The shared refusal on every runner error envelope (plan
//! `2026-09-20-the-published-product-works-without-knowing-a-development-environment-exists`,
//! Phase D1).
//!
//! Every runner HTTP error is an [`ApiResponse`](crate::mcp::types::ApiResponse)
//! carrying a machine-readable `code` (top-level and in `error_detail`). This
//! module projects that code onto the cross-repo `qontinui_types::refusal`
//! envelope, which `ApiResponse` serves ADDITIVELY under the top-level
//! `refusal` key — the same nesting qontinui-web uses (`REFUSAL_KEY =
//! "refusal"`). `error_detail.code` / `error_detail.context` stay: the
//! ui-bridge SDKs read them.
//!
//! The projection:
//!
//! * `Refusal.code` — the closed [`RefusalCode`] the runner code belongs to.
//!   A `UiBridgeErrorCode` maps through an exhaustive `match`, so a new variant
//!   cannot compile without a refusal class. Envelope codes (`INVALID_JSON`,
//!   `NOT_FOUND`, …) and any other handler string fall back to the HTTP
//!   status.
//! * `Refusal.discriminator` — the runner code itself (`ELEMENT_NOT_FOUND`,
//!   `NO_TAB_CONNECTED`), which is what narrows the class to the case. Absent
//!   when the code already IS a `RefusalCode` wire value.
//! * `Refusal.next_action` — the handler's own `error_detail.recovery` when it
//!   chose one; else [`next_action_for`] for a `UiBridgeErrorCode`; else the
//!   class's default ([`default_next_action`]), with a `Retry-After` header's
//!   seconds on a `retry_later`.
//! * `Refusal.detail` — the envelope's `error` sentence, verbatim.
//!
//! The envelope middleware (`envelope::envelope_rewrite_middleware`) is the
//! one place this is applied, so every enveloped error — whichever constructor
//! built it — leaves the runner with a `refusal`.

use axum::http::StatusCode;
use qontinui_types::refusal::{NextAction, NextActionKind, Refusal, RefusalCode, RefusalSource};

use crate::mcp::ui_bridge::{next_action_for, UiBridgeErrorCode};

/// The refusal class of a `UiBridgeErrorCode`. Exhaustive on purpose.
pub fn refusal_code_for_ui_bridge(code: UiBridgeErrorCode) -> RefusalCode {
    use UiBridgeErrorCode as C;
    match code {
        C::Timeout => RefusalCode::UpstreamTimeout,
        C::CircuitBreakerOpen
        | C::FrontendUnresponsive
        | C::FrontendNotReady
        | C::EventLoopUnresponsive
        | C::WindowNotFound => RefusalCode::ServiceUnavailable,
        C::ConcurrencyLimitReached => RefusalCode::RateLimited,
        C::ElementNotFound => RefusalCode::NotFound,
        // The element exists but the page's current state refuses the action.
        C::ElementNotVisible
        | C::ElementNotEnabled
        | C::ElementStale
        | C::ActionFailed
        | C::AssertionFailed
        | C::TerminalNoMountedView => RefusalCode::Conflict,
        C::ActionNotSupported
        | C::UnknownAssertionType
        | C::InvalidRequest
        | C::InvalidParam
        | C::MissingParam
        | C::InvalidState
        | C::InvalidTabId
        | C::SendKeysInvalid
        | C::WriteTextInvalid
        | C::PasteTextInvalid
        | C::ScrollbackMaxLinesInvalid => RefusalCode::InvalidRequest,
        C::UpstreamFetchFailed => RefusalCode::UpstreamUnavailable,
        C::InternalError => RefusalCode::InternalError,
    }
}

/// The refusal class an HTTP error status implies, for a code this module
/// does not otherwise know.
pub fn refusal_code_for_status(status: StatusCode) -> RefusalCode {
    match status {
        StatusCode::BAD_REQUEST
        | StatusCode::METHOD_NOT_ALLOWED
        | StatusCode::PAYLOAD_TOO_LARGE
        | StatusCode::UNSUPPORTED_MEDIA_TYPE
        | StatusCode::UNPROCESSABLE_ENTITY => RefusalCode::InvalidRequest,
        StatusCode::UNAUTHORIZED => RefusalCode::AuthenticationRequired,
        StatusCode::FORBIDDEN => RefusalCode::PermissionDenied,
        StatusCode::NOT_FOUND | StatusCode::GONE => RefusalCode::NotFound,
        StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED | StatusCode::LOCKED => {
            RefusalCode::Conflict
        }
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => RefusalCode::UpstreamTimeout,
        StatusCode::TOO_MANY_REQUESTS => RefusalCode::RateLimited,
        StatusCode::BAD_GATEWAY => RefusalCode::UpstreamUnavailable,
        StatusCode::SERVICE_UNAVAILABLE => RefusalCode::ServiceUnavailable,
        s if s.is_server_error() => RefusalCode::InternalError,
        _ => RefusalCode::Unknown,
    }
}

/// What to do next for a refusal class when the producer named nothing more
/// specific. `retry_after_s` is the response's `Retry-After`, when it had one.
pub fn default_next_action(
    code: RefusalCode,
    status: StatusCode,
    retry_after_s: Option<u32>,
) -> NextAction {
    match code {
        // 501: the route exists and will not do this, whatever is sent.
        _ if status == StatusCode::NOT_IMPLEMENTED => NextAction::new(NextActionKind::NoneTerminal),
        RefusalCode::InvalidRequest | RefusalCode::NotFound | RefusalCode::GlossaryTermUnknown => {
            NextAction::new(NextActionKind::FixRequest)
        }
        RefusalCode::AuthenticationRequired | RefusalCode::CredentialRejected => {
            NextAction::new(NextActionKind::SignIn)
        }
        RefusalCode::PermissionDenied => NextAction::new(NextActionKind::RequestAccess),
        RefusalCode::Conflict
        | RefusalCode::RateLimited
        | RefusalCode::QuotaExceeded
        | RefusalCode::UpstreamUnavailable
        | RefusalCode::UpstreamTimeout
        | RefusalCode::DeviceNotConnected
        | RefusalCode::ServiceUnavailable => NextAction::retry_later(retry_after_s),
        RefusalCode::WorkspaceRootUnresolved => {
            NextAction::new(NextActionKind::SetSetting).with_target("Workspace root")
        }
        RefusalCode::EndpointUnresolved => NextAction::new(NextActionKind::SetSetting),
        RefusalCode::SiblingCheckoutAbsent => NextAction::new(NextActionKind::NoneTerminal),
        // A 500 with nothing more specific is the runner's own failure.
        RefusalCode::InternalError | RefusalCode::Unknown => {
            NextAction::new(NextActionKind::ReportDefect)
        }
    }
}

/// Parse a runner code string as a `UiBridgeErrorCode` (its serde wire form).
fn as_ui_bridge_code(code: &str) -> Option<UiBridgeErrorCode> {
    serde_json::from_value(serde_json::Value::String(code.to_string())).ok()
}

/// Build the refusal for an enveloped runner error.
///
/// * `code` — the envelope's (reconciled) top-level code.
/// * `message` — the envelope's `error` sentence; carried as `detail`.
/// * `recovery` — the handler's own `error_detail.recovery`, which wins.
/// * `retry_after_s` — the response's `Retry-After` header, in seconds.
pub fn refusal_for_envelope(
    code: &str,
    status: StatusCode,
    message: Option<&str>,
    recovery: Option<NextAction>,
    retry_after_s: Option<u32>,
) -> Refusal {
    let ui_code = as_ui_bridge_code(code);
    let shared_code = RefusalCode::from_wire(code);
    let refusal_code = match (ui_code, shared_code) {
        (Some(c), _) => refusal_code_for_ui_bridge(c),
        (None, Some(c)) => c,
        (None, None) => refusal_code_for_status(status),
    };
    let next_action = recovery
        .or_else(|| ui_code.map(|c| next_action_for(&c)))
        .unwrap_or_else(|| default_next_action(refusal_code, status, retry_after_s));
    let mut refusal = Refusal::new(
        refusal_code,
        next_action,
        RefusalSource::Runner,
        chrono::Utc::now().to_rfc3339(),
    );
    if shared_code.is_none() && !code.is_empty() {
        refusal = refusal.with_discriminator(code);
    }
    if let Some(m) = message.filter(|m| !m.trim().is_empty()) {
        refusal = refusal.with_detail(m);
    }
    refusal
}

/// A `Retry-After` header value in whole seconds (the delta-seconds form; an
/// HTTP-date is not produced by this runner and reads as absent).
pub fn retry_after_seconds(value: Option<&axum::http::HeaderValue>) -> Option<u32> {
    value?.to_str().ok()?.trim().parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `UiBridgeErrorCode`, by its wire name — kept total by the
    /// exhaustive match in `refusal_code_for_ui_bridge` (a new variant must be
    /// classified there) and by `every_ui_bridge_code_round_trips` below.
    const ALL_UI_CODES: &[&str] = &[
        "TIMEOUT",
        "CIRCUIT_BREAKER_OPEN",
        "CONCURRENCY_LIMIT_REACHED",
        "FRONTEND_UNRESPONSIVE",
        "FRONTEND_NOT_READY",
        "EVENT_LOOP_UNRESPONSIVE",
        "WINDOW_NOT_FOUND",
        "ELEMENT_NOT_FOUND",
        "ELEMENT_NOT_VISIBLE",
        "ELEMENT_NOT_ENABLED",
        "ELEMENT_STALE",
        "ACTION_FAILED",
        "ACTION_NOT_SUPPORTED",
        "ASSERTION_FAILED",
        "UNKNOWN_ASSERTION_TYPE",
        "INVALID_REQUEST",
        "INVALID_PARAM",
        "MISSING_PARAM",
        "INVALID_STATE",
        "INVALID_TAB_ID",
        "SEND_KEYS_INVALID",
        "WRITE_TEXT_INVALID",
        "PASTE_TEXT_INVALID",
        "SCROLLBACK_MAX_LINES_INVALID",
        "TERMINAL_NO_MOUNTED_VIEW",
        "UPSTREAM_FETCH_FAILED",
        "INTERNAL_ERROR",
    ];

    #[test]
    fn every_ui_bridge_code_round_trips() {
        for name in ALL_UI_CODES {
            assert!(
                as_ui_bridge_code(name).is_some(),
                "{name} is not a UiBridgeErrorCode wire name"
            );
        }
    }

    /// Every runner code yields a refusal whose sentence is non-empty, whose
    /// discriminator is the runner code, and whose next action is the one the
    /// UI Bridge table names.
    #[test]
    fn every_ui_bridge_code_projects_to_a_rendered_refusal() {
        for name in ALL_UI_CODES {
            let code = as_ui_bridge_code(name).unwrap();
            let r = refusal_for_envelope(name, StatusCode::BAD_REQUEST, Some("msg"), None, None);
            assert!(!r.render().trim().is_empty(), "{name}");
            assert_eq!(r.discriminator.as_deref(), Some(*name));
            assert_eq!(r.next_action, next_action_for(&code), "{name}");
            assert_eq!(r.code, refusal_code_for_ui_bridge(code), "{name}");
            assert_eq!(r.source, RefusalSource::Runner);
            assert_eq!(r.detail.as_deref(), Some("msg"));
        }
    }

    /// The handler's own recovery wins over the code's table entry.
    #[test]
    fn a_handler_recovery_wins() {
        let r = refusal_for_envelope(
            "ELEMENT_NOT_FOUND",
            StatusCode::NOT_FOUND,
            None,
            Some(NextAction::new(NextActionKind::BroadenSelector)),
            None,
        );
        assert_eq!(r.next_action.kind, NextActionKind::BroadenSelector);
        assert_eq!(r.detail, None);
    }

    /// A code that is not a UI Bridge code (a relay code, an envelope code)
    /// takes the status's class and keeps the code as the discriminator.
    #[test]
    fn a_foreign_code_falls_back_to_the_status() {
        let cases = [
            (
                "NO_TAB_CONNECTED",
                StatusCode::SERVICE_UNAVAILABLE,
                RefusalCode::ServiceUnavailable,
                NextActionKind::RetryLater,
            ),
            (
                "INVALID_JSON",
                StatusCode::BAD_REQUEST,
                RefusalCode::InvalidRequest,
                NextActionKind::FixRequest,
            ),
            (
                "NOT_FOUND",
                StatusCode::NOT_FOUND,
                RefusalCode::NotFound,
                NextActionKind::FixRequest,
            ),
            (
                "UNAUTHORIZED",
                StatusCode::UNAUTHORIZED,
                RefusalCode::AuthenticationRequired,
                NextActionKind::SignIn,
            ),
            (
                "FORBIDDEN",
                StatusCode::FORBIDDEN,
                RefusalCode::PermissionDenied,
                NextActionKind::RequestAccess,
            ),
            (
                "CONFLICT",
                StatusCode::CONFLICT,
                RefusalCode::Conflict,
                NextActionKind::RetryLater,
            ),
            (
                "INTERNAL_ERROR",
                StatusCode::INTERNAL_SERVER_ERROR,
                RefusalCode::InternalError,
                NextActionKind::ReportDefect,
            ),
            (
                "NOT_IMPLEMENTED",
                StatusCode::NOT_IMPLEMENTED,
                RefusalCode::InternalError,
                NextActionKind::NoneTerminal,
            ),
            (
                "BAD_GATEWAY",
                StatusCode::BAD_GATEWAY,
                RefusalCode::UpstreamUnavailable,
                NextActionKind::RetryLater,
            ),
        ];
        for (code, status, want_code, want_kind) in cases {
            let r = refusal_for_envelope(code, status, None, None, None);
            assert_eq!(r.code, want_code, "{code}");
            assert_eq!(r.next_action.kind, want_kind, "{code}");
            assert_eq!(r.discriminator.as_deref(), Some(code));
            assert!(!r.render().is_empty());
        }
    }

    /// A code that is already a shared `RefusalCode` wire value is that class,
    /// with no redundant discriminator.
    #[test]
    fn a_shared_refusal_code_is_used_directly() {
        let r = refusal_for_envelope(
            "workspace_root_unresolved",
            StatusCode::UNPROCESSABLE_ENTITY,
            None,
            None,
            None,
        );
        assert_eq!(r.code, RefusalCode::WorkspaceRootUnresolved);
        assert_eq!(r.discriminator, None);
        assert_eq!(r.next_action.kind, NextActionKind::SetSetting);
    }

    /// `Retry-After` reaches a `retry_later` the producer did not time.
    #[test]
    fn retry_after_header_reaches_the_next_action() {
        let v = axum::http::HeaderValue::from_static("30");
        let secs = retry_after_seconds(Some(&v));
        assert_eq!(secs, Some(30));
        let r = refusal_for_envelope(
            "TOO_MANY_REQUESTS",
            StatusCode::TOO_MANY_REQUESTS,
            None,
            None,
            secs,
        );
        assert_eq!(r.next_action, NextAction::retry_later(Some(30)));
        assert_eq!(retry_after_seconds(None), None);
        let date = axum::http::HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT");
        assert_eq!(retry_after_seconds(Some(&date)), None);
    }

    /// The `RetryAfterMs` translation: milliseconds round UP to whole seconds,
    /// so a sub-second delay is never "retry in 0 seconds".
    #[test]
    fn a_sub_second_retry_rounds_up_never_to_zero() {
        assert_eq!(NextAction::retry_after_ms(500).retry_after_s, Some(1));
        assert_eq!(NextAction::retry_after_ms(1).retry_after_s, Some(1));
        assert_eq!(NextAction::retry_after_ms(1000).retry_after_s, Some(1));
        assert_eq!(NextAction::retry_after_ms(1001).retry_after_s, Some(2));
        assert_eq!(NextAction::retry_after_ms(2000).retry_after_s, Some(2));
        assert_eq!(
            next_action_for(&UiBridgeErrorCode::ConcurrencyLimitReached),
            NextAction::retry_later(Some(1)),
            "the 500 ms concurrency backoff must not become 0 s"
        );
    }

    /// The D2 conversions: request-shape codes say `fix_request` (not the old
    /// `UNRECOVERABLE`), and an internal error says `report_defect`.
    #[test]
    fn request_shape_codes_say_fix_request_and_internal_says_report_defect() {
        for code in [
            UiBridgeErrorCode::InvalidRequest,
            UiBridgeErrorCode::InvalidParam,
            UiBridgeErrorCode::MissingParam,
            UiBridgeErrorCode::InvalidState,
            UiBridgeErrorCode::InvalidTabId,
            UiBridgeErrorCode::ActionNotSupported,
            UiBridgeErrorCode::UnknownAssertionType,
        ] {
            assert_eq!(
                next_action_for(&code).kind,
                NextActionKind::FixRequest,
                "{code:?}"
            );
        }
        assert_eq!(
            next_action_for(&UiBridgeErrorCode::InternalError).kind,
            NextActionKind::ReportDefect
        );
        assert_eq!(
            next_action_for(&UiBridgeErrorCode::WindowNotFound).kind,
            NextActionKind::NoneTerminal
        );
    }
}
