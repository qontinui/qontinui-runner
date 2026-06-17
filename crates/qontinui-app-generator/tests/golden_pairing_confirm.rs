//! Phase-1 golden test: invert the `pairing-confirm` `IrState` from the connect-runner
//! oracle fixture into one UI-Bridge-instrumented Expo screen, and prove the inversion
//! is correct two ways:
//!
//! 1. **String-level correspondence** — for every `IrAssertion` in the state, the
//!    emitted `.tsx` contains a correspondingly-instrumented element: a `useUIElement`/
//!    `useUIElementWithProps` call carrying the id, the criteria role
//!    (`accessibilityRole`), and the criteria text the assertion searches for.
//!
//! 2. **Matcher-level correspondence (deterministic)** — projecting each instrumented
//!    element into the `UIBridgeElement` the native SDK registers (`snapshot_element_from`)
//!    and running the REAL `qontinui_spec_check::evaluate` matcher against that snapshot
//!    scores `pairing-confirm` at `match_rate == 1.0` / `FullMatch`. This proves the
//!    IR -> instrumentation mapping satisfies the exact contract the verify phase checks,
//!    without an Expo render in the loop.
//!
//! The on-device leg (real Expo render -> NativeBridgeSnapshot -> evaluate) is
//! runtime-deferred; see `runtime_render_deferred.rs`.

use qontinui_app_generator::{generate_screen, snapshot_element_from};
use qontinui_types::functional_spec::FunctionalSpec;
use qontinui_types::ir::{IrPageSpec, IrState};
use qontinui_types::ui_bridge::UIBridgeSnapshot;

use qontinui_spec_check::{evaluate, MatchOutcome};

/// Path to the connect-runner oracle fixture in the shared `qontinui-schemas` checkout.
/// The crate lives at `qontinui-runner/crates/qontinui-app-generator`; `qontinui-schemas`
/// is a sibling of `qontinui-runner` under the workspace root.
const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../qontinui-schemas/rust/tests/fixtures/functional_spec/connect-runner.functional_spec.json"
);

fn load_spec() -> FunctionalSpec {
    let raw =
        std::fs::read_to_string(FIXTURE).unwrap_or_else(|e| panic!("read fixture {FIXTURE}: {e}"));
    serde_json::from_str(&raw).expect("connect-runner fixture parses as FunctionalSpec")
}

fn pairing_confirm(spec: &FunctionalSpec) -> IrState {
    spec.ui_states
        .iter()
        .find(|s| s.id == "pairing-confirm")
        .expect("connect-runner fixture has a pairing-confirm state")
        .clone()
}

#[test]
fn pairing_confirm_state_has_nonempty_assertions() {
    // Guard: the whole Phase-1 proof is vacuous if the fixture's assertions are empty
    // (cross-plan requirement #4: #3 must populate IrState.assertions).
    let state = pairing_confirm(&load_spec());
    assert!(
        !state.assertions.is_empty(),
        "pairing-confirm must carry assertions for the observability proof to mean anything"
    );
}

#[test]
fn each_assertion_yields_a_correspondingly_instrumented_element() {
    let state = pairing_confirm(&load_spec());
    let artifact = generate_screen(&state);

    // Exactly one instrumented element per (non-empty-criteria) assertion.
    assert_eq!(
        artifact.elements.len(),
        state.assertions.len(),
        "every assertion must invert to one instrumented element"
    );

    let tsx = &artifact.tsx;
    assert_eq!(artifact.file_path, "app/pairing-confirm.tsx");
    assert!(tsx.contains("@qontinui/ui-bridge-native"));
    assert!(tsx.contains("export default function PairingConfirmScreen"));

    // The Connect button (assertion `pairing-confirm-connect-button`,
    // criteria {role: button, text: Connect}) → useUIElementWithProps + accessibilityRole.
    let connect = artifact
        .elements
        .iter()
        .find(|e| e.accessibility_role.as_deref() == Some("button"))
        .expect("the button assertion inverts to a button-role element");
    assert!(connect.needs_press, "button must be press-capable");
    assert_eq!(connect.visible_text.as_deref(), Some("Connect"));
    assert!(
        tsx.contains("useUIElementWithProps"),
        "press-needing button must use useUIElementWithProps"
    );
    assert!(
        tsx.contains(&format!("id: '{}'", connect.element_id)),
        "emitted screen registers the connect element id"
    );
    assert!(
        tsx.contains("accessibilityRole: 'button'"),
        "emitted screen forwards accessibilityRole=button (snapshot role source)"
    );
    assert!(
        tsx.contains("<Text>Connect</Text>"),
        "emitted screen renders the Connect visible text"
    );

    // The authorization-copy assertion (criteria {textContains: authorize}, now camelCase
    // in the fixture) → a text element whose visible copy CONTAINS "authorize". (The
    // generator's tolerant ParsedCriteria honors either casing; the fixture having moved to
    // camelCase means the MATCHER now reads this assertion too — see the note below.)
    let copy = artifact
        .elements
        .iter()
        .find(|e| {
            e.accessibility_role.is_none()
                && e.visible_text
                    .as_deref()
                    .map(|t| t.contains("authorize"))
                    .unwrap_or(false)
        })
        .expect(
            "the text_contains:authorize assertion inverts to an authorize-bearing text element",
        );
    assert!(
        tsx.contains(copy.visible_text.as_deref().unwrap()),
        "emitted screen renders the authorize-bearing copy"
    );
}

#[test]
fn inverted_state_scores_full_match_against_the_real_matcher() {
    let spec = load_spec();
    let state = pairing_confirm(&spec);
    let artifact = generate_screen(&state);

    // Project the generator's instrumentation manifest into the snapshot the native SDK
    // would register, then run the REAL matcher (deterministic stand-in for the render).
    let snapshot = UIBridgeSnapshot {
        timestamp: 0,
        elements: artifact
            .elements
            .iter()
            .map(snapshot_element_from)
            .collect(),
        components: Vec::new(),
        workflows: Vec::new(),
        modal_stack: None,
        toasts: None,
        undo_redo: None,
        current_route: Some("/pairing-confirm".into()),
        segments: vec!["pairing-confirm".into()],
    };

    // Build the single-state IrPageSpec the matcher evaluates (projecting the fixture's
    // ui_states/navigation into the IrPageSpec shape, per plan §4.3 / D5).
    let page = IrPageSpec {
        version: "1.0".into(),
        id: "connect-runner".into(),
        name: "Connect Runner".into(),
        description: None,
        metadata: None,
        provenance: None,
        states: vec![state.clone()],
        transitions: spec.navigation.clone(),
        synthesized_groups: None,
        initial_state: Some(state.id.clone()),
        api_assertions: None,
    };

    let result = evaluate(&snapshot, &page);

    let sr = result
        .state_results
        .iter()
        .find(|s| s.state_id == "pairing-confirm")
        .expect("matcher returns a result for pairing-confirm");

    // NOTE: the fixture now writes the authorization-copy criteria as camelCase
    // `textContains` (the cross-crate reconciliation §8.4 landed), so the MATCHER reads it
    // too — the copy assertion matches a REAL authorize-bearing element (no longer vacuous).
    // Both the connect-button assertion ({role, text}) and the copy assertion ({textContains})
    // resolve to real instrumented elements, so the state scores 1.0 on substance, not on a
    // vacuous pass. The generator's tolerant ParsedCriteria still honors snake_case for
    // resilience against any pre-reconciliation fixtures; this assert pins the behavior.
    assert_eq!(
        sr.match_rate, 1.0,
        "pairing-confirm scores a full per-state match"
    );
    assert_eq!(
        result.summary.match_outcome,
        MatchOutcome::FullMatch,
        "overall outcome is FullMatch"
    );
    assert_eq!(
        result.summary.severity_counts.critical, 0,
        "no critical misses"
    );
}
