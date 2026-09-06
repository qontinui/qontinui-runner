//! The component-action `effect` annotation must survive SDK -> runner -> Rust.
//!
//! Plan `2026-09-04-effect-calculus-joins-the-component-action-registry`,
//! Phases 1 and 2.
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
//! `useUIComponent` registrations out of `src/components/settings/Settings.tsx`
//! and `src/components/terminal/ZoneProfilePicker.tsx` and pushes them through
//! the real `serializeComponent`. Regenerate it with:
//!
//! ```text
//! node scripts/capture-component-effect-fixture.cjs --update
//! ```
//!
//! and verify it (CI) by running the same script with no arguments. Editing the
//! fixture by hand defeats the point and trips that verify step.
//!
//! # What Phase 2 changed here
//!
//! Phase 1 captured `settings-panel` alone, carrying `read` and `write`, and
//! relied on its then-unannotated `save` / `reset` as the witness that an
//! absent annotation stays `None`. Phase 2 annotated all 60 registered actions,
//! which removed that witness from the fixture — so this file no longer asserts
//! it against an action that no longer exists (an assertion whose subject is
//! gone is vacuous, `testing` `a-test-must-be-able-to-fail`). It is re-grounded
//! below on a captured action with its `effect` key deleted.
//!
//! Phase 2 also added `zone-profile-picker` to the capture, for one reason:
//! `destructive` is the value the whole annotation exists for, and until now no
//! committed artifact carried one across this seam. All three `IrEffect`
//! variants are now represented.
//!
//! # Mutation-checked
//!
//! Both halves were deleted in turn and this test observed RED:
//!   * `effect: "read"` removed from `settings-panel.list-tabs` in Settings.tsx;
//!   * `effect: a.effect` removed from `serializeComponent`'s allow-list.
//! A test never observed failing has not been shown to test anything.

use qontinui_types::ir::IrEffect;
use qontinui_types::ui_bridge::{ComponentActionInfo, UIBridgeComponent};

/// The captured body, embedded at compile time so the test needs no cwd.
const CAPTURED_BODY: &str = include_str!("fixtures/control-components-effect.json");

/// The `data.components` array as raw JSON, before typing.
fn captured_components_json() -> serde_json::Value {
    let body: serde_json::Value =
        serde_json::from_str(CAPTURED_BODY).expect("captured fixture is valid JSON");

    assert_eq!(
        body.get("success"),
        Some(&serde_json::Value::Bool(true)),
        "the capture must be a SUCCESS envelope — an error body proves nothing about the \
         serializer's field list"
    );

    body.get("data")
        .and_then(|d| d.get("components"))
        .cloned()
        .expect("captured body must carry `data.components` (Direction B envelope)")
}

/// Pull `data.components` out of the envelope
/// `ui_bridge_get_components_handler` emits and deserialize it into the shared
/// type. Deserialization is the assertion: `UIBridgeComponent` is the type
/// every Rust consumer of the component listing reads.
fn captured_components() -> Vec<UIBridgeComponent> {
    serde_json::from_value(captured_components_json())
        .expect("captured components must deserialize into Vec<UIBridgeComponent>")
}

fn fixture_component(component_id: &str) -> UIBridgeComponent {
    captured_components()
        .into_iter()
        .find(|c| c.id == component_id)
        .unwrap_or_else(|| {
            panic!(
                "captured body must contain the `{component_id}` fixture component — regenerate \
                 with `node scripts/capture-component-effect-fixture.cjs --update`"
            )
        })
}

fn effect_of(component: &UIBridgeComponent, action_id: &str) -> Option<IrEffect> {
    let component_id = &component.id;
    component
        .actions
        .iter()
        .find(|a| a.id == action_id)
        .unwrap_or_else(|| panic!("`{component_id}` has no action `{action_id}` in the capture"))
        .effect
}

/// The load-bearing assertion: author-declared effects, captured off the
/// runner's own serializer, arriving as typed Rust values — one of each
/// `IrEffect` variant, so no variant's transport is left unproven.
#[test]
fn captured_component_actions_carry_their_declared_effect() {
    let settings = fixture_component("settings-panel");

    assert_eq!(
        effect_of(&settings, "list-tabs"),
        Some(IrEffect::Read),
        "`settings-panel.list-tabs` declares `effect: 'read'` in Settings.tsx. Reading None here \
         means the value was eaten between the SDK and Rust — check the per-action field list in \
         `src/hooks/ui-bridge-events/utils.ts` (serializeComponent), which is a CLOSED allow-list."
    );

    assert_eq!(
        effect_of(&settings, "switch-tab"),
        Some(IrEffect::Write),
        "`settings-panel.switch-tab` declares `effect: 'write'` in Settings.tsx — same boundary, \
         same failure mode as `list-tabs` above."
    );

    let profiles = fixture_component("zone-profile-picker");

    assert_eq!(
        effect_of(&profiles, "delete-profile"),
        Some(IrEffect::Destructive),
        "`zone-profile-picker.delete-profile` declares `effect: 'destructive'` in \
         ZoneProfilePicker.tsx. This is the variant the annotation exists for — an autonomous \
         walk MUST NOT fire it — so it is the one whose transport most needs proving, and until \
         Phase 2 no captured body carried one at all."
    );

    assert_eq!(
        effect_of(&profiles, "list-profiles"),
        Some(IrEffect::Read),
        "`zone-profile-picker.list-profiles` declares `effect: 'read'`; captured alongside three \
         destructive siblings, so a serializer that stamped one value onto every action would be \
         caught here rather than passing."
    );
}

/// An unannotated action must arrive as `None`, not as a fabricated `read`.
///
/// Absent means *unclassified, not safe*. If the serializer (or any layer under
/// it) ever started defaulting the field, an action nobody judged would read as
/// safe and an autonomous walk would fire it.
///
/// Phase 1 witnessed this with `settings-panel.save` / `reset`, which were then
/// unannotated. Phase 2 annotated every registered action, so no such witness
/// remains in the corpus — and an assertion over an empty set passes for the
/// wrong reason. The subject here is therefore a REAL captured action with its
/// `effect` key deleted: still not a hand-typed shape, and it exercises exactly
/// the encoding `serializeComponent` produces for an unclassified action
/// (`JSON.stringify` drops an `undefined` value, so the key is ABSENT rather
/// than `null`).
#[test]
fn an_absent_effect_key_deserializes_as_none_rather_than_defaulting() {
    let mut action = captured_components_json()[0]["actions"][0].clone();

    assert!(
        action.get("effect").is_some(),
        "the captured action this test strips must HAVE an effect to strip — otherwise the \
         mutation below is a no-op and this test proves nothing"
    );

    action
        .as_object_mut()
        .expect("a captured action is a JSON object")
        .remove("effect");

    let parsed: ComponentActionInfo =
        serde_json::from_value(action).expect("an action without `effect` must still deserialize");

    assert_eq!(
        parsed.effect, None,
        "an action carrying no `effect` key must deserialize as None. A value here means some \
         layer invented a default — which would let an unjudged destructive action masquerade as \
         classified."
    );
}

/// The capture must be a real projection, not an empty shell that would let the
/// assertions above pass vacuously if the fixture were ever regenerated from a
/// broken parse.
#[test]
fn the_capture_is_a_populated_component_listing() {
    let components = captured_components();
    assert_eq!(
        components.len(),
        2,
        "the fixture captures the `settings-panel` and `zone-profile-picker` registrations"
    );

    let settings = fixture_component("settings-panel");
    assert_eq!(settings.name, "Settings Panel");
    assert_eq!(
        settings.actions.len(),
        4,
        "settings-panel registers save / reset / switch-tab / list-tabs; a shorter capture means \
         the AST parse in capture-component-effect-fixture.cjs silently dropped an action"
    );

    let profiles = fixture_component("zone-profile-picker");
    assert_eq!(profiles.name, "Zone Profile Picker");
    assert_eq!(
        profiles.actions.len(),
        4,
        "zone-profile-picker registers load-profile / save-profile / delete-profile / \
         list-profiles; a shorter capture means the AST parse dropped an action"
    );

    let annotated = components
        .iter()
        .flat_map(|c| c.actions.iter())
        .filter(|a| a.effect.is_some())
        .count();
    assert_eq!(
        annotated, 8,
        "Phase 2 annotated every registered component action, so all 8 captured actions carry an \
         effect (Phase 1's number here was 2, when only switch-tab and list-tabs were declared). \
         Enumerated coverage over ALL 60 actions is the vitest walk in \
         `src/lib/ui-bridge/action-effect-coverage.test.ts`; this count only guards the fixture."
    );

    // `IrEffect` derives neither `Ord` nor `Hash`, so this is a linear scan
    // rather than a set. Spelled as an explicit per-variant check anyway: it
    // names WHICH variant is missing, where a bare `len() == 3` would only say
    // that one is.
    let captured_effects: Vec<IrEffect> = components
        .iter()
        .flat_map(|c| c.actions.iter())
        .filter_map(|a| a.effect)
        .collect();
    for variant in [IrEffect::Read, IrEffect::Write, IrEffect::Destructive] {
        assert!(
            captured_effects.contains(&variant),
            "no captured action carries `{variant:?}`, so that variant has an UNPROVEN path \
             across the SDK -> serializer -> Rust boundary. Every IrEffect value must appear in \
             the fixture; `destructive` above all, since it is the one an autonomous walk must \
             refuse to fire. Captured: {captured_effects:?}"
        );
    }

    for (component, expected) in [
        (
            &settings,
            "/ui-bridge/control/component/settings-panel/action/{actionId}",
        ),
        (
            &profiles,
            "/ui-bridge/control/component/zone-profile-picker/action/{actionId}",
        ),
    ] {
        assert_eq!(
            component.action_invocation_path.as_deref(),
            Some(expected),
            "the capture must carry the server-annotated invocation template, proving it came \
             through serializeComponent rather than being hand-written"
        );
    }
}
