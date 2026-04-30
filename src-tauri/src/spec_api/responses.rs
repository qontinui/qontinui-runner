//! Response shapes for the Spec API.
//!
//! Section 2's hard constraint: every empty/error response must carry a
//! `reason: String` field. The constructors below make that the only way to
//! produce an empty/error response — no `Default::default()` or
//! "ok: false" without a reason.

use serde::Serialize;
use serde_json::Value;

/// Generic error envelope. Used wherever the success shape would otherwise
/// be a different type — handlers wrap this in `axum::response::Json`.
#[derive(Debug, Clone, Serialize)]
pub struct SpecError {
    pub ok: bool,
    pub reason: String,
    /// Optional structured detail attached to the reason (e.g. the missing
    /// page id). Always serialized when present so callers can do
    /// `if (err.id) ...` without a second probe.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

impl SpecError {
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
/// can tell apart "registry has zero pages" from "spec store unwired".
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

/// Common envelope for query/derive results — `matches: []` with a `reason`
/// when nothing matched, never silently empty.
#[derive(Debug, Clone, Serialize)]
pub struct QueryResult<T: Serialize> {
    pub ok: bool,
    pub matches: Vec<T>,
    /// Always present — explains why the result is empty when `matches` is
    /// `[]`, otherwise descriptive (e.g. `"matched-by-group"`).
    pub reason: String,
}

impl<T: Serialize> QueryResult<T> {
    pub fn ok(matches: Vec<T>, reason: impl Into<String>) -> Self {
        Self {
            ok: true,
            matches,
            reason: reason.into(),
        }
    }
}
