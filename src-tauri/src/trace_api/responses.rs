//! Response shapes for the Trace API (Section 5b).
//!
//! Mirrors the `spec_api::responses` idiom: every empty/error response carries
//! a `reason: String` field — the constructors below are the only way to build
//! these values, so we never produce a silent "ok: false" without a reason.

use serde::Serialize;
use serde_json::Value;

/// Generic error envelope. Used wherever the success shape would otherwise
/// be a different type — handlers wrap this in `axum::response::Json`.
#[derive(Debug, Clone, Serialize)]
pub struct TraceError {
    pub ok: bool,
    pub reason: String,
    /// Optional structured detail attached to the reason (e.g. the missing
    /// session id). Always serialized when present so callers can do
    /// `if (err.id) ...` without a second probe.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

impl TraceError {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            ok: false,
            reason: reason.into(),
            detail: None,
        }
    }

    pub fn with_detail(reason: impl Into<String>, detail: Value) -> Self {
        Self {
            ok: false,
            reason: reason.into(),
            detail: Some(detail),
        }
    }
}

/// Wrapper for "successful but empty" results. Carries a reason so callers
/// can tell apart "no recording sessions yet" from "trace api unwired".
#[derive(Debug, Clone, Serialize)]
pub struct EmptyOk {
    pub ok: bool,
    pub reason: String,
}

impl EmptyOk {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            ok: true,
            reason: reason.into(),
        }
    }
}
