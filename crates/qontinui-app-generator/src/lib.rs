//! `qontinui-app-generator` — IR inversion (Phase 1).
//!
//! Inverts a [`qontinui_types::ir::IrState`] into one UI-Bridge-instrumented Expo
//! screen (`.tsx`) whose registered elements correspond to the state's
//! [`qontinui_types::ir::IrAssertion`]s — the load-bearing observability seam of the
//! website->mobile regeneration program
//! (`plans/2026-06-14-app-generator-ir-inversion.md`, node #1).
//!
//! ## Phase 1 scope (per-screen observability seam, deterministic core)
//!
//! - [`screen::generate_screen`] — `IrState` -> [`screen::ScreenArtifact`] (the emitted
//!   `.tsx` + the structured instrumentation manifest). Pure-deterministic; no LLM.
//! - [`criteria::ParsedCriteria`] — tolerant projection of the loose
//!   `IrAssertion.target.criteria` (honors both `text_contains` and `textContains`).
//! - [`instrument::InstrumentedElement`] — the byte-exact IR-criteria ->
//!   `useUIElement` instrumentation mapping.
//! - [`snapshot_element_from`] — projects an instrumented element into the
//!   [`qontinui_types::ui_bridge::UIBridgeElement`] the native SDK would register, so a
//!   golden test can run the real `qontinui_spec_check::evaluate` matcher and prove the
//!   mapping is matcher-correct **deterministically**, without an Expo render.
//!
//! ## Phase 2 scope (whole-page inversion, deterministic core)
//!
//! - [`app::generate_app`] — `FunctionalSpec` + `Profile` -> [`app::GeneratedApp`]: one
//!   instrumented screen per `IrState` (reusing the Phase-1 renderer verbatim), one
//!   navigator edge per `IrTransition` whose press handler is resolved to a **real**
//!   source-screen element, and one data-layer service method per `Operation` whose URL is
//!   derived by the **shared** [`qontinui_types::endpoint_for::endpoint_for`] (the single
//!   #1<->#2 contract — never re-derived). The whole-page golden test
//!   (`tests/golden_full_app.rs`) runs the real matcher over **every** state and asserts a
//!   `FullMatch`, lifting the Phase-1 per-screen proof to the entire connect-runner page.
//! - Deferred (honestly): the **LLM presentational fill** (layout/styling via
//!   `run_claude_cli`) and the **on-device render+snapshot** verify leg — there is no
//!   generated-screen render harness (the device runs a fixed production Expo build, not a
//!   dev-client that can load an arbitrary emitted `.tsx`). See the deferred test for the
//!   precise gap + the harness proposal.
//!
//! ## Runtime-deferred (NOT in this crate)
//!
//! The full verify loop the plan describes (run the emitted screen under the UI Bridge
//! native server -> capture a real `NativeBridgeSnapshot` -> `spec_check::evaluate`)
//! requires a Metro/Expo + device/simulator harness that is not part of the Rust
//! toolchain. The crate leaves the wiring in place via [`snapshot_element_from`] and
//! documents the deferral in `tests/runtime_render_deferred.rs`.

pub mod app;
pub mod criteria;
pub mod instrument;
pub mod screen;

pub use app::{generate_app, GeneratedApp, NavEdge, ServiceMethod};
pub use criteria::ParsedCriteria;
pub use instrument::InstrumentedElement;
pub use screen::{generate_screen, ScreenArtifact};

use qontinui_types::ui_bridge::{ElementIdentifier, ElementRect, ElementState, UIBridgeElement};

/// Project an [`InstrumentedElement`] into the [`UIBridgeElement`] the UI Bridge native
/// SDK registers for it, mirroring `useUIElement.ts`:
///
/// - `id` / `identifier.test_id` ← the hook `id` (`testID`).
/// - `role` ← `accessibilityRole` (the snapshot serializer's Stream-A A.5 mapping).
/// - `accessible_name` / `aria_label` ← the hook `label` (`accessibilityLabel`).
/// - `text` ← the visible `<Text>` child.
///
/// This is the deterministic stand-in for "render the screen + snapshot it": it encodes
/// the exact registry contract so a golden test can assert the matcher scores the
/// inverted state a `FullMatch`. The real on-device snapshot is the runtime-deferred leg.
pub fn snapshot_element_from(el: &InstrumentedElement) -> UIBridgeElement {
    UIBridgeElement {
        id: el.element_id.clone(),
        element_type: el.element_type.clone(),
        label: el.label.clone(),
        actions: Vec::new(),
        custom_actions: None,
        identifier: ElementIdentifier {
            ui_id: Some(el.element_id.clone()),
            test_id: Some(el.element_id.clone()),
            awas_id: None,
            html_id: None,
            xpath: String::new(),
            selector: String::new(),
        },
        state: ElementState {
            visible: true,
            enabled: true,
            focused: false,
            rect: ElementRect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 40.0,
                top: 0.0,
                right: 100.0,
                bottom: 40.0,
                left: 0.0,
            },
            value: None,
            checked: None,
            selected_options: None,
            text_content: el.visible_text.clone(),
        },
        registered_at: 0,
        mounted: true,
        bbox: None,
        visible: Some(true),
        role: el.accessibility_role.clone(),
        tag_name: None,
        aria_label: el.label.clone(),
        accessible_name: el.label.clone(),
        text: el.visible_text.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_element_carries_id_role_and_text() {
        let el = InstrumentedElement {
            element_id: "connect-btn".into(),
            element_type: "button".into(),
            accessibility_role: Some("button".into()),
            label: Some("Connect".into()),
            visible_text: Some("Connect".into()),
            needs_press: true,
        };
        let wire = snapshot_element_from(&el);
        assert_eq!(wire.id, "connect-btn");
        assert_eq!(wire.role.as_deref(), Some("button"));
        assert_eq!(wire.text.as_deref(), Some("Connect"));
        assert_eq!(wire.identifier.test_id.as_deref(), Some("connect-btn"));
    }
}
