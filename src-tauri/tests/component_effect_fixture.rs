//! The component-action `effect` annotation must survive SDK -> runner -> Rust.
//!
//! Plan `2026-09-04-effect-calculus-joins-the-component-action-registry`, Phase 1.
//!
//! # Why this is not already covered
//!
//! `qontinui-schemas`' own tests (`rust/src/ui_bridge.rs`,
//! `component_action_info_round_trips_destructive_effect` and siblings) pin the
//! serde contract against literal JSON. They are correct and they stay as they
//! are — but a hand-written literal proves only that *if* a body carrying
//! `effect` arrives, serde reads it. It cannot see whether the runner ever
//! emits one.
//!
//! It did not. `serializeComponent`
//! (`src/hooks/ui-bridge-events/utils.ts`) builds its per-action object from a
//! CLOSED list of explicit picks with no spread, and `effect` was not on it —
//! so the annotation was stripped on the way out while
//! [`ComponentActionInfo::effect`] had been declared on the Rust side for some
//! time. A compiling Rust type is not evidence of a value crossing a boundary.
//!
//! # What this test reads
//!
//! `tests/fixtures/control-components-effect.json` is a CAPTURED
//! `/control/components` response body, not a hand-typed one. It is produced by
//! `scripts/capture-component-effect-fixture.cjs`, which parses the real
//! `useUIComponent` registration out of `src/components/settings/Settings.tsx`
//! and pushes it through the real `serializeComponent`. Regenerate it with:
//!
//! ```text
//! node scripts/capture-component-effect-fixture.cjs --update
//! ```
//!
//! and verify it (CI) by running the same script with no arguments. Editing the
//! fixture by hand defeats the point and trips that verify step.
//!
//! # Mutation-checked
//!
//! Both halves were deleted in turn and this test observed RED:
//!   * `effect: "read"` removed from `settings-panel.list-tabs` in Settings.tsx;
//!   * `effect: a.effect` removed from `serializeComponent`'s allow-list.
//! A test never observed failing has not been shown to test anything.

use qontinui_types::ir::IrEffect;
use qontinui_types::ui_bridge::UIBridgeComponent;

/// The captured body, embedded at compile time so the test needs no cwd.
const CAPTURED_BODY: &str = include_str!("fixtures/control-components-effect.json");

/// Pull `data.components` out of the envelope
/// `ui_bridge_get_components_handler` emits and deserialize it into the shared
/// type. Deserialization is the assertion: `UIBridgeComponent` is the type
/// every Rust consumer of the component listing reads.
fn captured_components() -> Vec<UIBridgeComponent> {
    let body: serde_json::Value =
        serde_json::from_str(CAPTURED_BODY).expect("captured fixture is valid JSON");

    assert_eq!(
        body.get("success"),
        Some(&serde_json::Value::Bool(true)),
        "the capture must be a SUCCESS envelope — an error body proves nothing about the \
         serializer's field list"
    );

    let components = body
        .get("data")
        .and_then(|d| d.get("components"))
        .cloned()
        .expect("captured body must carry `data.components` (Direction B envelope)");

    serde_json::from_value(components)
        .expect("captured components must deserialize into Vec<UIBridgeComponent>")
}

fn fixture_component() -> UIBridgeComponent {
    captured_components()
        .into_iter()
        .find(|c| c.id == "settings-panel")
        .expect(
            "captured body must contain the `settings-panel` fixture component — regenerate with \
             `node scripts/capture-component-effect-fixture.cjs --update`",
        )
}

fn effect_of(component: &UIBridgeComponent, action_id: &str) -> Option<IrEffect> {
    component
        .actions
        .iter()
        .find(|a| a.id == action_id)
        .unwrap_or_else(|| panic!("`settings-panel` has no action `{action_id}` in the capture"))
        .effect
}

/// The load-bearing assertion: two author-declared effects, captured off the
/// runner's own serializer, arriving as typed Rust values.
#[test]
fn captured_component_actions_carry_their_declared_effect() {
    let component = fixture_component();

    assert_eq!(
        effect_of(&component, "list-tabs"),
        Some(IrEffect::Read),
        "`settings-panel.list-tabs` declares `effect: 'read'` in Settings.tsx. Reading None here \
         means the value was eaten between the SDK and Rust — check the per-action field list in \
         `src/hooks/ui-bridge-events/utils.ts` (serializeComponent), which is a CLOSED allow-list."
    );

    assert_eq!(
        effect_of(&component, "switch-tab"),
        Some(IrEffect::Write),
        "`settings-panel.switch-tab` declares `effect: 'write'` in Settings.tsx — same boundary, \
         same failure mode as `list-tabs` above."
    );
}

/// An unannotated action must arrive as `None`, not as a fabricated `read`.
///
/// Absent means *unclassified, not safe*. If the serializer (or any layer under
/// it) ever started defaulting the field, an action nobody judged would read as
/// safe and an autonomous walk would fire it. `save` and `reset` are
/// deliberately left unannotated in this phase so that failure mode has a
/// witness.
#[test]
fn an_unannotated_action_stays_unclassified_rather_than_defaulting() {
    let component = fixture_component();

    for action_id in ["save", "reset"] {
        assert_eq!(
            effect_of(&component, action_id),
            None,
            "`settings-panel.{action_id}` declares no effect, so it must deserialize as None. A \
             value here means some layer invented a default — which would let an unjudged \
             destructive action masquerade as classified."
        );
    }
}

/// The capture must be a real projection, not an empty shell that would let the
/// assertions above pass vacuously if the fixture were ever regenerated from a
/// broken parse.
#[test]
fn the_capture_is_a_populated_component_listing() {
    let components = captured_components();
    assert_eq!(
        components.len(),
        1,
        "the fixture captures exactly the `settings-panel` registration"
    );

    let component = &components[0];
    assert_eq!(component.name, "Settings Panel");
    assert_eq!(
        component.actions.len(),
        4,
        "settings-panel registers save / reset / switch-tab / list-tabs; a shorter capture means \
         the AST parse in capture-component-effect-fixture.cjs silently dropped an action"
    );
    assert_eq!(
        component.actions.iter().filter(|a| a.effect.is_some()).count(),
        2,
        "exactly the two Phase-1 annotations are expected; Phase 2 annotates the rest and should \
         update this number"
    );
    assert_eq!(
        component.action_invocation_path.as_deref(),
        Some("/ui-bridge/control/component/settings-panel/action/{actionId}"),
        "the capture must carry the server-annotated invocation template, proving it came \
         through serializeComponent rather than being hand-written"
    );
}
