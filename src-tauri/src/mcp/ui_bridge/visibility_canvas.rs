//! The canvas census behind `/control/visibility`'s `unknown_canvas` verdict.
//!
//! Plan `2026-10-22-occlusion-sweep-is-blind-to-canvas-painted-text`, D3 and
//! Phase 4. A `<canvas>` paints its text as pixels, so the registry's
//! `textContent` is empty and its occlusion sample cannot tell "covered blank
//! box" from "covered terminal". The SDK's `registry.getCanvasCoverage()`
//! counts the visible canvases and says which ones a `paintsText` registration
//! accounts for. The runner asks the webview for that census over the
//! `get_canvas_coverage` IPC and parses the answer here.
//!
//! # Every failure is `Unavailable`, never a 500 and never `Measured`
//!
//! The census only ever WITHHOLDS a `clear`. So an answer that cannot be read
//! must land on the side that withholds it:
//!
//! | IPC outcome                                   | Parsed as                                   |
//! |-----------------------------------------------|---------------------------------------------|
//! | `null` (no accessor: page SDK predates it)    | `Unavailable { PageSdkLacksCoverage }`      |
//! | `{state:"unavailable", reason:"accessor-threw"}` (the TS arm's throw marker) | `Unavailable { AccessorThrew }` |
//! | `{state:"unavailable", reason:"no-document"}` (a DOM-less registry)       | `Unavailable { NoDocument }`                |
//! | transport `Err`, a `{success:false}` envelope, or any malformed body | `Unavailable { IpcFailed }` |
//! | `{canvases, attributed, unattributed[]}`      | `Measured { .. }`                           |
//!
//! This deliberately differs from `modal_ids_from_context`, which degrades
//! quietly: a missing modal stack only loses a filter, while a missing canvas
//! census loses the ability to say "clear" truthfully (policy
//! `verification-and-evidence/silent-empty-is-unknown`). Do not "harmonise"
//! the two.

use serde_json::{json, Value};

/// Why the canvas census could not be read. The wire strings are the SDK's
/// `canvasCoverage.reason` union, letter for letter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoverageUnavailableReason {
    /// The webview's SDK has no `getCanvasCoverage` accessor: upgrade the page SDK.
    PageSdkLacksCoverage,
    /// The accessor exists but threw (or answered nothing).
    AccessorThrew,
    /// The registry that answered runs with no DOM (a server-side registry),
    /// so there were no canvases it could count.
    NoDocument,
    /// The IPC failed or its answer could not be read.
    IpcFailed,
}

impl CoverageUnavailableReason {
    pub(crate) fn as_wire(self) -> &'static str {
        match self {
            Self::PageSdkLacksCoverage => "page-sdk-lacks-coverage",
            Self::AccessorThrew => "accessor-threw",
            Self::NoDocument => "no-document",
            Self::IpcFailed => "ipc-failed",
        }
    }

    fn from_wire(s: &str) -> Option<Self> {
        match s {
            "page-sdk-lacks-coverage" => Some(Self::PageSdkLacksCoverage),
            "accessor-threw" => Some(Self::AccessorThrew),
            "no-document" => Some(Self::NoDocument),
            "ipc-failed" => Some(Self::IpcFailed),
            _ => None,
        }
    }
}

/// One visible canvas no `paintsText` registration accounts for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UnattributedCanvas {
    pub descriptor: String,
    /// `{x, y, width, height}`, passed through as the webview measured it.
    pub rect: Value,
}

/// The parsed `get_canvas_coverage` answer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CanvasCoverage {
    Measured {
        canvases: u64,
        attributed: u64,
        unattributed: Vec<UnattributedCanvas>,
    },
    Unavailable {
        reason: CoverageUnavailableReason,
    },
}

impl CanvasCoverage {
    /// Parse the `get_canvas_coverage` IPC result. Never fails: anything that
    /// is not a well-formed census is `Unavailable` (see the module table).
    pub(crate) fn from_ipc(result: Result<&Value, &str>) -> Self {
        match result {
            Ok(value) => Self::from_answer(value),
            Err(_) => Self::unavailable(CoverageUnavailableReason::IpcFailed),
        }
    }

    fn unavailable(reason: CoverageUnavailableReason) -> Self {
        Self::Unavailable { reason }
    }

    fn from_answer(value: &Value) -> Self {
        use CoverageUnavailableReason as R;
        if value.is_null() {
            return Self::unavailable(R::PageSdkLacksCoverage);
        }
        let Some(obj) = value.as_object() else {
            return Self::unavailable(R::IpcFailed);
        };
        // A frontend `{success:false}` answer (e.g. "Unknown request type"
        // from a frontend predating this IPC) arrives as an `Ok` object
        // carrying `success: false` (`request.rs::extract_response_data`).
        if obj.get("success").and_then(Value::as_bool) == Some(false) {
            return Self::unavailable(R::IpcFailed);
        }
        if obj.get("state").and_then(Value::as_str) == Some("unavailable") {
            let reason = obj
                .get("reason")
                .and_then(Value::as_str)
                .and_then(R::from_wire)
                .unwrap_or(R::IpcFailed);
            return Self::unavailable(reason);
        }
        // The census itself. The SDK accessor answers the bare
        // `{canvases, attributed, unattributed}`; an already-tagged
        // `state: "measured"` is accepted too. Any other `state` is malformed.
        if let Some(state) = obj.get("state") {
            if state.as_str() != Some("measured") {
                return Self::unavailable(R::IpcFailed);
            }
        }
        let canvases = obj.get("canvases").and_then(Value::as_u64);
        let attributed = obj.get("attributed").and_then(Value::as_u64);
        let unattributed = obj.get("unattributed").and_then(Value::as_array);
        let (Some(canvases), Some(attributed), Some(unattributed)) =
            (canvases, attributed, unattributed)
        else {
            return Self::unavailable(R::IpcFailed);
        };
        let mut parsed = Vec::with_capacity(unattributed.len());
        for entry in unattributed {
            let descriptor = entry.get("descriptor").and_then(Value::as_str);
            let rect = entry.get("rect").filter(|r| r.is_object());
            match (descriptor, rect) {
                (Some(d), Some(r)) => parsed.push(UnattributedCanvas {
                    descriptor: d.to_string(),
                    rect: r.clone(),
                }),
                // A census that cannot name an unattributed canvas is not a
                // census a `clear` may stand on.
                _ => return Self::unavailable(R::IpcFailed),
            }
        }
        // Internally inconsistent counts are malformed, not a finding.
        if attributed > canvases {
            return Self::unavailable(R::IpcFailed);
        }
        Self::Measured {
            canvases,
            attributed,
            unattributed: parsed,
        }
    }

    /// Does this census withhold a `clear` verdict? True when coverage is
    /// unavailable, or any visible canvas is unattributed (D3, step 3).
    pub(crate) fn withholds_clear(&self) -> bool {
        match self {
            Self::Measured { unattributed, .. } => !unattributed.is_empty(),
            Self::Unavailable { .. } => true,
        }
    }

    /// The report's `canvasCoverage` field, in the SDK's wire shape.
    pub(crate) fn to_json(&self) -> Value {
        match self {
            Self::Measured {
                canvases,
                attributed,
                unattributed,
            } => json!({
                "state": "measured",
                "canvases": canvases,
                "attributed": attributed,
                "unattributed": unattributed
                    .iter()
                    .map(|u| json!({ "descriptor": u.descriptor, "rect": u.rect }))
                    .collect::<Vec<_>>(),
            }),
            Self::Unavailable { reason } => json!({
                "state": "unavailable",
                "reason": reason.as_wire(),
            }),
        }
    }

    /// A census that measured and found no canvas at all. Test fixture.
    #[cfg(test)]
    pub(crate) fn measured_empty() -> Self {
        Self::Measured {
            canvases: 0,
            attributed: 0,
            unattributed: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: Value) -> CanvasCoverage {
        CanvasCoverage::from_ipc(Ok(&v))
    }

    fn reason_of(c: &CanvasCoverage) -> &'static str {
        match c {
            CanvasCoverage::Unavailable { reason } => reason.as_wire(),
            CanvasCoverage::Measured { .. } => panic!("expected Unavailable, got {c:?}"),
        }
    }

    #[test]
    fn null_means_the_page_sdk_lacks_the_accessor() {
        let c = parse(Value::Null);
        assert_eq!(reason_of(&c), "page-sdk-lacks-coverage");
        assert!(c.withholds_clear());
    }

    #[test]
    fn an_ipc_error_is_ipc_failed() {
        let c = CanvasCoverage::from_ipc(Err("webview timed out"));
        assert_eq!(reason_of(&c), "ipc-failed");
        assert!(c.withholds_clear());
    }

    #[test]
    fn the_ts_throw_marker_is_accessor_threw() {
        let c = parse(json!({ "state": "unavailable", "reason": "accessor-threw" }));
        assert_eq!(reason_of(&c), "accessor-threw");
    }

    #[test]
    fn a_dom_less_registry_is_no_document_and_withholds_clear() {
        let c = parse(json!({ "state": "unavailable", "reason": "no-document" }));
        assert_eq!(reason_of(&c), "no-document");
        assert!(c.withholds_clear());
        assert_eq!(
            c.to_json(),
            json!({ "state": "unavailable", "reason": "no-document" })
        );
    }

    #[test]
    fn an_unknown_unavailable_reason_is_ipc_failed_not_measured() {
        let c = parse(json!({ "state": "unavailable", "reason": "cosmic-rays" }));
        assert_eq!(reason_of(&c), "ipc-failed");
    }

    #[test]
    fn a_failure_envelope_from_an_old_frontend_is_ipc_failed() {
        let c = parse(json!({ "success": false, "error": "Unknown request type" }));
        assert_eq!(reason_of(&c), "ipc-failed");
    }

    #[test]
    fn malformed_bodies_are_unavailable_never_measured() {
        for bad in [
            json!(42),
            json!("coverage"),
            json!([]),
            json!({}),
            json!({ "canvases": 1, "attributed": 1 }),
            json!({ "canvases": -1, "attributed": 0, "unattributed": [] }),
            json!({ "canvases": 1, "attributed": 2, "unattributed": [] }),
            json!({ "canvases": 1, "attributed": 0, "unattributed": [{ "descriptor": "canvas" }] }),
            json!({ "canvases": 1, "attributed": 0, "unattributed": [{ "rect": {} }] }),
            json!({ "state": "bogus", "canvases": 0, "attributed": 0, "unattributed": [] }),
        ] {
            let c = parse(bad.clone());
            assert_eq!(reason_of(&c), "ipc-failed", "body {bad} must be ipc-failed");
            assert!(c.withholds_clear());
        }
    }

    #[test]
    fn a_full_census_is_measured_and_round_trips_to_the_wire_shape() {
        let rect = json!({ "x": 10, "y": 20, "width": 300, "height": 200 });
        let c = parse(json!({
            "canvases": 3,
            "attributed": 2,
            "unattributed": [{ "descriptor": "canvas.xterm-link-layer", "rect": rect }],
        }));
        assert!(
            c.withholds_clear(),
            "one unattributed canvas withholds clear"
        );
        assert_eq!(
            c.to_json(),
            json!({
                "state": "measured",
                "canvases": 3,
                "attributed": 2,
                "unattributed": [{ "descriptor": "canvas.xterm-link-layer", "rect": rect }],
            })
        );
    }

    #[test]
    fn an_explicitly_tagged_measured_census_is_accepted() {
        let c = parse(json!({
            "state": "measured", "canvases": 2, "attributed": 2, "unattributed": [],
        }));
        assert!(
            !c.withholds_clear(),
            "every canvas attributed: clear is allowed"
        );
    }

    #[test]
    fn zero_canvases_does_not_withhold_clear() {
        let c = parse(json!({ "canvases": 0, "attributed": 0, "unattributed": [] }));
        assert_eq!(c, CanvasCoverage::measured_empty());
        assert!(!c.withholds_clear());
    }

    #[test]
    fn unavailable_serialises_with_its_reason() {
        let c = CanvasCoverage::from_ipc(Err("boom"));
        assert_eq!(
            c.to_json(),
            json!({ "state": "unavailable", "reason": "ipc-failed" })
        );
    }
}
