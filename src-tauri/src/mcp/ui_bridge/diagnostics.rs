//! Canonical UI Bridge diagnostic-code parsing for runner-side consumers.
//!
//! Phase 5 of the 2026-05-18 *UI Bridge Diagnostic Discipline* plan. Replaces
//! the brittle `data.get("error") == Some("ELEMENT_NOT_FOUND")` string-match
//! pattern with discrimination on the **shipped Wave-1 Rust mirror**
//! [`qontinui_types::ui_bridge_diagnostics::UiBridgeErrorCode`] (generated from
//! `ui-bridge/diagnostics/codes.json`, canonical `UB-` strings only).
//!
//! ## Where codes come from
//!
//! A UI Bridge failure response can carry the canonical code in three places,
//! checked in priority order:
//!
//! 1. `failureDetails.errorCode` — the Wave-1 P1 canonical surface (preferred;
//!    sync control-action failures populate this per Phase 3).
//! 2. top-level `code` — the `APIResponse.code` surface (Wave-1 P1).
//! 3. top-level `errorCode` — a flatter variant some IPC envelopes use.
//!
//! All three are parsed via [`UiBridgeErrorCode::from_str`] (canonical `UB-`
//! only — there is intentionally no legacy arm in the shipped mirror, per
//! plan D1's single-user coordinated cutover).
//!
//! ## Runner-internal legacy bridge (NOT a D1 violation)
//!
//! Plan D1 forbids a legacy-recognition surface for **the SDK vocabulary**.
//! The SDK has cut over. But the runner *also* hosts its own React frontend
//! whose IPC handlers (`src/hooks/ui-bridge-events/useControlEvents.ts`) are a
//! *separate, runner-internal producer* that historically emitted bare strings
//! like `ELEMENT_NOT_FOUND` / `element_not_found` on the IPC envelope's prose
//! `error` field. Those frontend handlers are migrated in the same Phase 5
//! wave to also emit a canonical `errorCode`/`code`; [`legacy_bare_to_canonical`]
//! exists ONLY to bridge an in-flight frontend bundle (the `dist/` build can
//! lag the Rust binary by one rebuild) and is explicitly scoped to the small,
//! enumerated set of strings the runner's own frontend emits — not an open
//! `From<&str>` over the whole vocabulary. It is observable and removable once
//! the frontend cutover is verified end-to-end.

use std::str::FromStr;

pub use qontinui_types::ui_bridge_diagnostics::UiBridgeErrorCode as CanonicalCode;

/// Bridge the small, enumerated set of bare strings the runner's *own* React
/// frontend IPC handlers historically emit (`useControlEvents.ts` and
/// siblings) to the canonical mirror code.
///
/// This is deliberately a closed `match` over runner-internal producers, NOT
/// a general legacy-recognition table for the SDK vocabulary (which would
/// violate plan D1). Returns `None` for anything not in the runner-frontend
/// vocabulary so callers fall through to "unknown" rather than guessing.
pub fn legacy_bare_to_canonical(s: &str) -> Option<CanonicalCode> {
    // Case-insensitive on the well-known SCREAMING_SNAKE / lower_snake forms
    // the runner frontend uses. Keep this list tight and enumerated.
    let norm = s.trim();
    match norm {
        "ELEMENT_NOT_FOUND" | "element_not_found" => Some(CanonicalCode::UbElemNotFound),
        "ELEMENT_NOT_VISIBLE" | "element_not_visible" => Some(CanonicalCode::UbElemNotVisible),
        "ELEMENT_NOT_ENABLED" | "element_not_enabled" | "ELEMENT_DISABLED" => {
            Some(CanonicalCode::UbElemNotEnabled)
        }
        "STALE_ELEMENT" | "stale_element" | "ELEMENT_STALE" => Some(CanonicalCode::UbStaleElement),
        "ACTION_FAILED" | "action_failed" => Some(CanonicalCode::UbActionFailed),
        "ACTION_TIMEOUT" | "action_timeout" => Some(CanonicalCode::UbActionTimeout),
        "NOT_FOUND" => Some(CanonicalCode::UbElemNotFound),
        _ => None,
    }
}

/// Parse a single field value into a canonical code: canonical `UB-` first
/// (the shipped mirror), then the runner-internal legacy bridge.
fn parse_code_field(value: &str) -> Option<CanonicalCode> {
    CanonicalCode::from_str(value)
        .ok()
        .or_else(|| legacy_bare_to_canonical(value))
}

/// Extract the canonical [`CanonicalCode`] from a UI Bridge failure response
/// body, checking the three canonical locations in priority order:
/// `failureDetails.errorCode` → top-level `code` → top-level `errorCode`,
/// each parsed canonical-first with the scoped runner-internal legacy bridge.
///
/// Returns `None` when no recognizable code is present (caller should treat
/// the failure as opaque and degrade to the existing prose/LLM path — never
/// panic on a missing/unknown code).
pub fn extract_error_code(data: &serde_json::Value) -> Option<CanonicalCode> {
    // 1. failureDetails.errorCode (Wave-1 P1 canonical surface, preferred).
    if let Some(code) = data
        .get("failureDetails")
        .and_then(|fd| fd.get("errorCode"))
        .and_then(|v| v.as_str())
        .and_then(parse_code_field)
    {
        return Some(code);
    }
    // 2. top-level `code` (APIResponse.code surface).
    if let Some(code) = data
        .get("code")
        .and_then(|v| v.as_str())
        .and_then(parse_code_field)
    {
        return Some(code);
    }
    // 3. top-level `errorCode` (flatter IPC-envelope variant).
    if let Some(code) = data
        .get("errorCode")
        .and_then(|v| v.as_str())
        .and_then(parse_code_field)
    {
        return Some(code);
    }
    // 4. Last-resort runner-internal bridge: some legacy frontend IPC
    //    envelopes put the bare code in the prose `error` field itself
    //    (e.g. `{ "error": "ELEMENT_NOT_FOUND" }`). Only the enumerated
    //    runner-frontend vocabulary is recognized here — arbitrary prose
    //    is NOT pattern-matched (that is `classify_transport_error`'s job,
    //    which stays as the transport-level fallback).
    data.get("error")
        .and_then(|v| v.as_str())
        .and_then(legacy_bare_to_canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_canonical_from_failure_details() {
        let data = json!({
            "success": false,
            "failureDetails": { "errorCode": "UB-ELEM-NOT-FOUND" },
            "error": "Element 'save-btn' not found",
        });
        assert_eq!(
            extract_error_code(&data),
            Some(CanonicalCode::UbElemNotFound)
        );
    }

    #[test]
    fn parses_canonical_from_top_level_code() {
        let data = json!({ "success": false, "code": "UB-ACTION-TIMEOUT" });
        assert_eq!(
            extract_error_code(&data),
            Some(CanonicalCode::UbActionTimeout)
        );
    }

    #[test]
    fn parses_canonical_from_top_level_error_code() {
        let data = json!({ "success": false, "errorCode": "UB-ELEM-NOT-VISIBLE" });
        assert_eq!(
            extract_error_code(&data),
            Some(CanonicalCode::UbElemNotVisible)
        );
    }

    #[test]
    fn failure_details_wins_over_top_level() {
        // Priority: failureDetails.errorCode must win even if a (stale)
        // top-level code disagrees.
        let data = json!({
            "failureDetails": { "errorCode": "UB-ELEM-NOT-FOUND" },
            "code": "UB-ACTION-FAILED",
        });
        assert_eq!(
            extract_error_code(&data),
            Some(CanonicalCode::UbElemNotFound)
        );
    }

    #[test]
    fn runner_frontend_legacy_bare_string_is_bridged() {
        // The runner's own React frontend (useControlEvents.ts) emits this
        // bare string on the prose `error` field for in-flight bundles.
        let data = json!({
            "success": true,
            "passed": false,
            "error": "ELEMENT_NOT_FOUND",
            "errorMessage": "Element 'x' not found",
        });
        assert_eq!(
            extract_error_code(&data),
            Some(CanonicalCode::UbElemNotFound)
        );
    }

    #[test]
    fn lower_snake_legacy_bare_string_is_bridged() {
        let data = json!({ "success": false, "error": "element_not_found" });
        assert_eq!(
            extract_error_code(&data),
            Some(CanonicalCode::UbElemNotFound)
        );
    }

    #[test]
    fn unknown_code_returns_none_never_panics() {
        let data = json!({ "success": false, "error": "totally unrecognized prose" });
        assert_eq!(extract_error_code(&data), None);
        let empty = json!({});
        assert_eq!(extract_error_code(&empty), None);
        // A non-canonical SDK-looking string is NOT silently bridged.
        let bogus = json!({ "code": "UB-NOT-A-REAL-CODE" });
        assert_eq!(extract_error_code(&bogus), None);
    }

    #[test]
    fn display_roundtrips_through_from_str() {
        for c in CanonicalCode::all() {
            let s = c.as_str();
            assert_eq!(CanonicalCode::from_str(s).unwrap(), *c);
        }
    }
}
