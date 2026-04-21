//! Click-target resolvers ("providers") for UI Bridge element actions.
//!
//! A provider takes an element identifier (id or label) and the current
//! UI Bridge snapshot and returns either a concrete [`ClickTarget`]
//! (hit) or a [`ProviderMiss`] reason so the caller can fall through
//! to the next provider in the chain.
//!
//! ## Chain order
//!
//! 1. [`BboxProvider`] — reads `bbox` + `visible` directly from snapshot
//!    elements. Cheap, deterministic, requires no model inference.
//! 2. VLM provider (e.g. `uitars`) — screenshot → model → `<point>x y</point>`.
//!    Registered externally; this module only defines the fast path.
//!
//! The chain dispatches through [`resolve_click_target`], which runs
//! [`BboxProvider`] first and leaves the fall-through to the caller
//! when all in-process providers miss.
//!
//! ## Coordinate semantics
//!
//! The UI Bridge `bbox` field is viewport-relative CSS pixels (the
//! return value of `Element.getBoundingClientRect()`). The runner
//! dispatches clicks through the SDK's `/control/elements/{id}/action`
//! endpoint, which performs a synthetic DOM click inside the webview —
//! no OS-level cursor movement, no DPR scaling required. This module
//! therefore returns CSS pixels as-is. If a future caller dispatches
//! OS-level clicks, it should scale the returned point by the webview's
//! device pixel ratio before feeding it to the native input API.

use serde::Serialize;
use tracing::debug;

/// A resolved click location produced by a [`ClickProvider`].
///
/// Coordinates are viewport-relative CSS pixels; the provider name is
/// for telemetry and diagnostic logs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClickTarget {
    /// UI Bridge element id of the resolved element.
    pub element_id: String,
    /// Click X coordinate in viewport CSS pixels.
    pub x: f64,
    /// Click Y coordinate in viewport CSS pixels.
    pub y: f64,
    /// Name of the provider that produced this target (for telemetry).
    pub provider: &'static str,
}

/// Reason a provider declined to resolve a target.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderMiss {
    /// No element matched the identifier.
    NoMatch,
    /// The element was found but has no bbox field.
    NoBbox,
    /// The element has a bbox but is flagged not visible (zero-area or
    /// `state.visible == false`).
    NotVisible,
    /// The element is flagged disabled (`state.enabled == false`).
    Disabled,
    /// The snapshot JSON was structurally invalid for this provider.
    InvalidSnapshot,
}

impl ProviderMiss {
    /// Human-readable short form for log messages.
    pub fn as_reason(&self) -> &'static str {
        match self {
            ProviderMiss::NoMatch => "no matching element",
            ProviderMiss::NoBbox => "element has no bbox",
            ProviderMiss::NotVisible => "element not visible",
            ProviderMiss::Disabled => "element disabled",
            ProviderMiss::InvalidSnapshot => "invalid snapshot",
        }
    }
}

/// Click-target resolvers implement this trait.
///
/// A provider inspects a snapshot and either returns a
/// [`ClickTarget`] or explains why it could not (a [`ProviderMiss`]).
pub trait ClickProvider: Send + Sync {
    /// Stable name for logs/telemetry. Must be a static string.
    fn name(&self) -> &'static str;

    /// Resolve the click target for the given element identifier.
    ///
    /// `snapshot` is the JSON payload returned by the UI Bridge
    /// `/control/snapshot` endpoint (or equivalent proxy URL). It may
    /// be wrapped in the control-endpoint envelope (`{ "data": {
    /// "elements": [...] } }`) or served directly as `{ "elements":
    /// [...] }` — both shapes are accepted.
    fn resolve(
        &self,
        identifier: &ElementIdentifier,
        snapshot: &serde_json::Value,
    ) -> Result<ClickTarget, ProviderMiss>;
}

/// Narrow identifier used to match an element in a snapshot.
///
/// At this phase the chain only supports `id` and `label` lookups — the
/// two keys that [`useUIElement`] call sites actually pass. Richer
/// criteria (testId, htmlId, role) live in the existing
/// `element_matches_criteria` helper and are routed through the VLM
/// fallback path when the bbox provider misses.
#[derive(Debug, Clone, PartialEq)]
pub enum ElementIdentifier<'a> {
    /// Exact match against the element's `id` field.
    Id(&'a str),
    /// Case-insensitive exact match against the element's `label` field.
    Label(&'a str),
}

/// Reads `bbox` + `visible` from snapshot elements and returns the
/// center of the bbox as the click coordinate.
///
/// Fast path for SDK-registered elements that expose a live DOM ref.
#[derive(Debug, Default)]
pub struct BboxProvider;

impl ClickProvider for BboxProvider {
    fn name(&self) -> &'static str {
        "bbox"
    }

    fn resolve(
        &self,
        identifier: &ElementIdentifier,
        snapshot: &serde_json::Value,
    ) -> Result<ClickTarget, ProviderMiss> {
        let elements = extract_elements(snapshot).ok_or(ProviderMiss::InvalidSnapshot)?;

        let el = find_by_identifier(elements, identifier).ok_or(ProviderMiss::NoMatch)?;

        // `state.enabled` is authoritative when present. If the
        // snapshot omits the full state block, we only block on an
        // explicit `false`.
        if let Some(false) = el
            .get("state")
            .and_then(|s| s.get("enabled"))
            .and_then(|v| v.as_bool())
        {
            return Err(ProviderMiss::Disabled);
        }

        // Prefer the new top-level `visible` flag (cheap SDK signal).
        // If absent, require the richer `state.visible` to not be
        // explicitly false — this avoids clicking occluded elements.
        let top_visible = el.get("visible").and_then(|v| v.as_bool());
        let state_visible = el
            .get("state")
            .and_then(|s| s.get("visible"))
            .and_then(|v| v.as_bool());

        match (top_visible, state_visible) {
            (Some(false), _) => return Err(ProviderMiss::NotVisible),
            (None, Some(false)) => return Err(ProviderMiss::NotVisible),
            _ => {}
        }

        let bbox = el.get("bbox").ok_or(ProviderMiss::NoBbox)?;
        let (x, y, w, h) = extract_bbox_xywh(bbox).ok_or(ProviderMiss::NoBbox)?;

        if w <= 0.0 || h <= 0.0 {
            return Err(ProviderMiss::NotVisible);
        }

        let element_id = el
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let cx = x + w / 2.0;
        let cy = y + h / 2.0;

        Ok(ClickTarget {
            element_id,
            x: cx,
            y: cy,
            provider: self.name(),
        })
    }
}

/// Dispatch an identifier through the provider chain and return the
/// first hit. Logs each miss at `debug` level so callers can verify
/// the fast path is actually taken once deployed.
///
/// At this phase the chain is `[BboxProvider]`. The VLM/uitars provider
/// lives outside the Rust runner (Python/SDK dispatch) and is invoked
/// by the caller when this function returns `None`.
pub fn resolve_click_target(
    identifier: &ElementIdentifier,
    snapshot: &serde_json::Value,
) -> Option<ClickTarget> {
    let providers: [&dyn ClickProvider; 1] = [&BboxProvider];

    for provider in providers {
        match provider.resolve(identifier, snapshot) {
            Ok(target) => {
                debug!(
                    provider = provider.name(),
                    element_id = %target.element_id,
                    x = target.x,
                    y = target.y,
                    ?identifier,
                    "click provider hit"
                );
                return Some(target);
            }
            Err(miss) => {
                debug!(
                    provider = provider.name(),
                    reason = miss.as_reason(),
                    ?identifier,
                    "click provider miss — trying next"
                );
            }
        }
    }

    debug!(?identifier, "all in-process click providers missed; caller should fall through to VLM");
    None
}

/// Extract the `elements` array from either
/// `{ "data": { "elements": [...] } }` (control-endpoint envelope) or
/// the bare `{ "elements": [...] }` shape.
fn extract_elements(snapshot: &serde_json::Value) -> Option<&[serde_json::Value]> {
    snapshot
        .get("data")
        .and_then(|d| d.get("elements"))
        .or_else(|| snapshot.get("elements"))
        .and_then(|e| e.as_array())
        .map(Vec::as_slice)
}

/// Find the element matching `identifier`. Id match is exact; label
/// match is case-insensitive exact. Id match is tried first even when
/// the caller passes `Label`, so a label that happens to collide with
/// an id still resolves to the id'd element.
fn find_by_identifier<'a>(
    elements: &'a [serde_json::Value],
    identifier: &ElementIdentifier,
) -> Option<&'a serde_json::Value> {
    match identifier {
        ElementIdentifier::Id(id) => elements
            .iter()
            .find(|el| el.get("id").and_then(|v| v.as_str()) == Some(id)),
        ElementIdentifier::Label(label) => {
            let lowered = label.to_lowercase();
            elements.iter().find(|el| {
                el.get("label")
                    .and_then(|v| v.as_str())
                    .map(|l| l.eq_ignore_ascii_case(&lowered) || l.to_lowercase() == lowered)
                    .unwrap_or(false)
            })
        }
    }
}

/// Extract `(x, y, width, height)` from a bbox JSON object.
fn extract_bbox_xywh(bbox: &serde_json::Value) -> Option<(f64, f64, f64, f64)> {
    let x = bbox.get("x")?.as_f64()?;
    let y = bbox.get("y")?.as_f64()?;
    let w = bbox.get("width")?.as_f64()?;
    let h = bbox.get("height")?.as_f64()?;
    Some((x, y, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_snapshot() -> serde_json::Value {
        json!({
            "elements": [
                {
                    "id": "submit-btn",
                    "type": "button",
                    "label": "Submit",
                    "state": { "visible": true, "enabled": true },
                    "bbox": { "x": 120.0, "y": 44.0, "width": 88.0, "height": 32.0 },
                    "visible": true,
                },
                {
                    "id": "hidden-btn",
                    "type": "button",
                    "label": "Hidden",
                    "state": { "visible": false, "enabled": true },
                    "bbox": { "x": 0.0, "y": 0.0, "width": 0.0, "height": 0.0 },
                    "visible": false,
                },
                {
                    "id": "disabled-btn",
                    "type": "button",
                    "label": "Disabled",
                    "state": { "visible": true, "enabled": false },
                    "bbox": { "x": 0.0, "y": 100.0, "width": 50.0, "height": 20.0 },
                    "visible": true,
                },
                {
                    "id": "no-bbox-btn",
                    "type": "button",
                    "label": "Ghost",
                    "state": { "visible": true, "enabled": true }
                }
            ]
        })
    }

    #[test]
    fn bbox_provider_hits_center_of_visible_element() {
        let snap = sample_snapshot();
        let t = BboxProvider
            .resolve(&ElementIdentifier::Id("submit-btn"), &snap)
            .expect("expected bbox hit");
        assert_eq!(t.element_id, "submit-btn");
        assert_eq!(t.x, 120.0 + 88.0 / 2.0);
        assert_eq!(t.y, 44.0 + 32.0 / 2.0);
        assert_eq!(t.provider, "bbox");
    }

    #[test]
    fn bbox_provider_matches_by_label_case_insensitive() {
        let snap = sample_snapshot();
        let t = BboxProvider
            .resolve(&ElementIdentifier::Label("submit"), &snap)
            .expect("expected bbox hit via label");
        assert_eq!(t.element_id, "submit-btn");
    }

    #[test]
    fn bbox_provider_misses_when_not_visible() {
        let snap = sample_snapshot();
        let miss = BboxProvider
            .resolve(&ElementIdentifier::Id("hidden-btn"), &snap)
            .unwrap_err();
        assert_eq!(miss, ProviderMiss::NotVisible);
    }

    #[test]
    fn bbox_provider_misses_when_disabled() {
        let snap = sample_snapshot();
        let miss = BboxProvider
            .resolve(&ElementIdentifier::Id("disabled-btn"), &snap)
            .unwrap_err();
        assert_eq!(miss, ProviderMiss::Disabled);
    }

    #[test]
    fn bbox_provider_misses_when_bbox_absent() {
        let snap = sample_snapshot();
        let miss = BboxProvider
            .resolve(&ElementIdentifier::Id("no-bbox-btn"), &snap)
            .unwrap_err();
        assert_eq!(miss, ProviderMiss::NoBbox);
    }

    #[test]
    fn bbox_provider_misses_on_no_match() {
        let snap = sample_snapshot();
        let miss = BboxProvider
            .resolve(&ElementIdentifier::Id("does-not-exist"), &snap)
            .unwrap_err();
        assert_eq!(miss, ProviderMiss::NoMatch);
    }

    #[test]
    fn accepts_control_endpoint_envelope() {
        let snap = json!({
            "data": {
                "elements": [{
                    "id": "save",
                    "label": "Save",
                    "state": { "visible": true, "enabled": true },
                    "bbox": { "x": 10.0, "y": 20.0, "width": 40.0, "height": 12.0 },
                    "visible": true,
                }]
            }
        });
        let t = BboxProvider
            .resolve(&ElementIdentifier::Id("save"), &snap)
            .expect("envelope-wrapped snapshots must resolve");
        assert_eq!(t.x, 30.0);
        assert_eq!(t.y, 26.0);
    }

    #[test]
    fn chain_returns_none_when_all_miss() {
        let snap = json!({ "elements": [] });
        let result = resolve_click_target(&ElementIdentifier::Id("x"), &snap);
        assert!(result.is_none());
    }

    #[test]
    fn chain_returns_hit_on_first_provider() {
        let snap = sample_snapshot();
        let result = resolve_click_target(&ElementIdentifier::Id("submit-btn"), &snap);
        let target = result.expect("chain should hit bbox provider");
        assert_eq!(target.provider, "bbox");
        assert_eq!(target.element_id, "submit-btn");
    }

    #[test]
    fn invalid_snapshot_yields_invalid_snapshot_miss() {
        let snap = json!({ "nope": 123 });
        let miss = BboxProvider
            .resolve(&ElementIdentifier::Id("anything"), &snap)
            .unwrap_err();
        assert_eq!(miss, ProviderMiss::InvalidSnapshot);
    }
}
