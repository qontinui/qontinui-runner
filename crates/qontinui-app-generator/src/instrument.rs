//! IR assertion -> deterministic UI-Bridge instrumentation mapping.
//!
//! This is the **observability-critical, byte-exact** core of the IR inversion
//! (plan D2): each [`IrAssertion`]'s `target.criteria` names the element a snapshot
//! must expose for the assertion to pass. We invert that criteria into the exact
//! `useUIElement` / `useUIElementWithProps` call the generated Expo screen makes,
//! plus the host-component props (`accessibilityRole`, visible `<Text>`), such that
//! when the UI Bridge native SDK registers the element the resulting
//! `UIBridgeElement` satisfies the criteria the matcher checks.
//!
//! ## What the native SDK exposes (the contract we invert against)
//!
//! `useUIElement({ id, type, label, accessibilityRole })` registers an element whose
//! snapshot `UIBridgeElement` carries (see `useUIElement.ts` +
//! `qontinui-mobile/.../AddConnectionForm.tsx`):
//!
//! - `id` ← the hook `id` (also `testID` / `identifier.test_id`) → matches `criteria.id`.
//! - `role` ← `accessibilityRole` (forwarded onto `props.accessibilityRole`, which the
//!   snapshot serializer maps to the wire `role` — Stream-A A.5) → matches `criteria.role`.
//! - `accessible_name` / `aria_label` ← `label` (`accessibilityLabel`) → matches
//!   `criteria.accessible_name` / `criteria.aria_label`.
//! - `text` ← the element's visible `<Text>` child (innerText-equivalent on native)
//!   → matches `criteria.text` / `criteria.text_contains`.
//!
//! The matcher (`qontinui-spec-check`) routes every text comparison through
//! `normalize_text` (case-fold + whitespace-collapse), so exact-casing is not required
//! for a match — but we preserve the authored casing in the emitted screen for fidelity.

use crate::criteria::ParsedCriteria;

/// A single instrumented element the generated screen registers — the deterministic
/// inversion of one [`crate::criteria::ParsedCriteria`]. The screen renderer turns this
/// into a `useUIElement`/`useUIElementWithProps` call site + its host component; the
/// golden test turns it into a `UIBridgeElement` to prove the mapping is matcher-correct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentedElement {
    /// Stable UI-Bridge element id (the hook `id`, also `testID`). Always present —
    /// derived from the assertion id so the screen is reproducible + the element is
    /// uniquely addressable even when the criteria itself declares no `id`.
    pub element_id: String,
    /// UI-Bridge native element `type` (`"button"`, `"input"`, `"text"`, `"view"`, …).
    /// Selected from the criteria role so press-needing types route through
    /// `useUIElementWithProps` (the SDK rejects `button`/`pressable` on the slim hook).
    pub element_type: String,
    /// `accessibilityRole` prop forwarded onto the host component → snapshot `role`.
    /// `None` when the criteria declared no role.
    pub accessibility_role: Option<String>,
    /// `label` passed to the hook → snapshot `accessible_name` / `aria_label`.
    /// `None` when the criteria declared no accessible-name signal.
    pub label: Option<String>,
    /// Visible text rendered in a `<Text>` child → snapshot `text`. Carries either the
    /// criteria's exact `text` or a phrase that *contains* the criteria's
    /// `text_contains` needle. `None` for elements with no text assertion.
    pub visible_text: Option<String>,
    /// True when this element needs a press handler (`button`/`pressable`) and must be
    /// emitted with `useUIElementWithProps` + `captureProps({ onPress })`.
    pub needs_press: bool,
}

impl InstrumentedElement {
    /// Invert one parsed assertion criteria into the instrumented element the screen
    /// must register. `element_id_seed` is the assertion id, which becomes the stable
    /// UI-Bridge id (unless the criteria itself pins an `id`, which then wins so the
    /// matcher's id lookup hits).
    pub fn from_criteria(element_id_seed: &str, crit: &ParsedCriteria) -> Self {
        let element_id = crit
            .id
            .clone()
            .unwrap_or_else(|| sanitize_id(element_id_seed));

        let role = crit.role.clone();
        let needs_press = matches!(role.as_deref(), Some("button") | Some("pressable"));

        // Pick the SDK element `type` from the role. The `type` drives the host
        // component the renderer picks; the snapshot `role` itself comes from
        // `accessibilityRole`, so an unmapped role still round-trips via that prop.
        let element_type = match role.as_deref() {
            Some("button") => "button",
            Some("link") => "pressable",
            Some("textbox") | Some("searchbox") => "input",
            Some(_) => "view",
            None => "text",
        }
        .to_string();

        // Visible text: an exact `text` criteria is reproduced verbatim; a
        // `text_contains` needle is embedded in a deterministic carrier phrase so the
        // snapshot's `text` *contains* the needle (the matcher's `text_contains` is an
        // asymmetric substring check).
        let visible_text = match (&crit.text, &crit.text_contains) {
            (Some(t), _) => Some(t.clone()),
            (None, Some(needle)) => Some(carrier_phrase(needle)),
            (None, None) => None,
        };

        // Accessible-name signal: prefer an explicit accessible_name/aria_label
        // criteria; otherwise fall back to the visible text so a text-only assertion
        // still yields a labelled, addressable element.
        let label = crit
            .accessible_name
            .clone()
            .or_else(|| crit.aria_label.clone())
            .or_else(|| visible_text.clone());

        InstrumentedElement {
            element_id,
            element_type,
            accessibility_role: role,
            label,
            visible_text,
            needs_press,
        }
    }
}

/// Build a deterministic carrier phrase that *contains* a `text_contains` needle, so the
/// emitted screen's visible text satisfies the asymmetric substring match. We surround
/// the needle with fixed scaffolding rather than echoing it bare so the copy reads like
/// real UI prose while remaining byte-stable for the golden test.
fn carrier_phrase(needle: &str) -> String {
    format!("This screen will {needle} the requested action.")
}

/// Normalize an assertion id into a UI-Bridge-safe element id. Lowercases, replaces
/// runs of non-alphanumeric characters with a single `-`, and trims leading/trailing
/// dashes. Deterministic + idempotent.
fn sanitize_id(seed: &str) -> String {
    let mut out = String::with_capacity(seed.len());
    let mut prev_dash = false;
    for ch in seed.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "element".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::criteria::ParsedCriteria;

    #[test]
    fn button_with_text_routes_to_press_needing_type() {
        let crit = ParsedCriteria {
            role: Some("button".into()),
            text: Some("Connect".into()),
            ..Default::default()
        };
        let el = InstrumentedElement::from_criteria("pairing-confirm-connect-button", &crit);
        assert_eq!(el.element_type, "button");
        assert!(el.needs_press);
        assert_eq!(el.accessibility_role.as_deref(), Some("button"));
        assert_eq!(el.visible_text.as_deref(), Some("Connect"));
        assert_eq!(el.element_id, "pairing-confirm-connect-button");
    }

    #[test]
    fn text_contains_embeds_needle_in_carrier() {
        let crit = ParsedCriteria {
            text_contains: Some("authorize".into()),
            ..Default::default()
        };
        let el = InstrumentedElement::from_criteria("pairing-confirm-authorization-copy", &crit);
        assert!(!el.needs_press);
        assert_eq!(el.element_type, "text");
        assert!(el.visible_text.as_deref().unwrap().contains("authorize"));
    }

    #[test]
    fn explicit_id_criteria_wins_over_seed() {
        let crit = ParsedCriteria {
            id: Some("connect-cta".into()),
            role: Some("button".into()),
            ..Default::default()
        };
        let el = InstrumentedElement::from_criteria("some-assertion-id", &crit);
        assert_eq!(el.element_id, "connect-cta");
    }

    #[test]
    fn alert_role_maps_to_view_type_without_press() {
        let crit = ParsedCriteria {
            role: Some("alert".into()),
            ..Default::default()
        };
        let el = InstrumentedElement::from_criteria("pairing-error-message", &crit);
        assert_eq!(el.element_type, "view");
        assert_eq!(el.accessibility_role.as_deref(), Some("alert"));
        assert!(!el.needs_press);
    }

    #[test]
    fn sanitize_id_is_deterministic_and_safe() {
        assert_eq!(
            sanitize_id("Pairing Confirm: Connect!"),
            "pairing-confirm-connect"
        );
        assert_eq!(sanitize_id("___"), "element");
    }
}
