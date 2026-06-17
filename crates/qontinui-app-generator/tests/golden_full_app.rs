//! Phase-2 golden test: invert the **whole** connect-runner `FunctionalSpec`
//! (both `IrState`s + the `IrTransition` + the `pairConfirm` operation) into a
//! multi-screen, navigable, data-layer-wired app, and prove the inversion correct against
//! the **real** matcher + the **real** shared endpoint derivation — deterministically,
//! without an Expo render.
//!
//! What this verifies that Phase 1 did not:
//!
//! 1. **Every** state (`pairing-confirm` AND `pairing-error`) inverts to instrumentation
//!    that scores `FullMatch` against the real `qontinui_spec_check::evaluate` — proving
//!    the per-screen observability seam holds across the whole page, not just one screen.
//! 2. The `show-pairing-error` transition's trigger resolves to a **real** instrumented
//!    button on its source screen (`pairing-confirm`) — the same element the matcher would
//!    find for the transition's `action.target` (the Phase-3 nav-coverage prereq).
//! 3. The `pairConfirm` operation inverts to a data-layer call at the endpoint the
//!    **shared** `endpoint_for` derives (`POST /api/v1/devices/pair-confirm`) — the one
//!    #1<->#2 contract, byte-checked in the emitted client.
//!
//! The on-device render+snapshot leg remains runtime-deferred (no generated-screen render
//! harness exists; the device runs a fixed production build). See
//! `runtime_render_deferred.rs`.

use qontinui_app_generator::{generate_app, snapshot_element_from};
use qontinui_types::functional_spec::FunctionalSpec;
use qontinui_types::ir::IrPageSpec;
use qontinui_types::priorities_profile::Profile;
use qontinui_types::ui_bridge::UIBridgeSnapshot;

use qontinui_spec_check::{evaluate, MatchOutcome};

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../qontinui-schemas/rust/tests/fixtures/functional_spec"
);

fn load_spec() -> FunctionalSpec {
    let path = format!("{FIXTURE_DIR}/connect-runner.functional_spec.json");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    serde_json::from_str(&raw).expect("connect-runner fixture parses as FunctionalSpec")
}

fn load_profile() -> Profile {
    let path = format!("{FIXTURE_DIR}/connect-runner.profile.json");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    serde_json::from_str(&raw).expect("connect-runner profile parses as Profile")
}

#[test]
fn whole_page_inverts_to_one_screen_per_state_and_one_edge_per_transition() {
    let spec = load_spec();
    let app = generate_app(&spec, &load_profile());

    assert_eq!(
        app.screens.len(),
        spec.ui_states.len(),
        "one screen per state"
    );
    assert_eq!(
        app.nav_edges.len(),
        spec.navigation.len(),
        "one nav edge per transition"
    );
    assert_eq!(
        app.services.len(),
        spec.operations.len(),
        "one service method per operation"
    );

    // Both fixture states are present as screens.
    let ids: Vec<&str> = app.screens.iter().map(|s| s.state_id.as_str()).collect();
    assert!(ids.contains(&"pairing-confirm"));
    assert!(ids.contains(&"pairing-error"));
}

#[test]
fn every_state_scores_full_match_against_the_real_matcher() {
    // The whole-page observability proof: project EVERY generated screen's instrumentation
    // manifest into the snapshot the native SDK would register, run the REAL matcher over
    // the whole IrPageSpec, and assert every state matches. This is the Phase-1 proof,
    // lifted to all states at once.
    let spec = load_spec();
    let app = generate_app(&spec, &load_profile());

    // One snapshot carrying every instrumented element across every screen (the matcher
    // searches the whole element set per state, mirroring an app whose elements are all
    // mounted/observable — Phase-3 will instead snapshot each state in isolation).
    let elements = app
        .screens
        .iter()
        .flat_map(|s| s.elements.iter())
        .map(snapshot_element_from)
        .collect();

    let snapshot = UIBridgeSnapshot {
        timestamp: 0,
        elements,
        components: Vec::new(),
        workflows: Vec::new(),
        modal_stack: None,
        toasts: None,
        undo_redo: None,
        current_route: Some("/pairing-confirm".into()),
        segments: vec!["pairing-confirm".into()],
    };

    let page = IrPageSpec {
        version: "1.0".into(),
        id: "connect-runner".into(),
        name: "Connect Runner".into(),
        description: None,
        metadata: None,
        provenance: None,
        states: spec.ui_states.clone(),
        transitions: spec.navigation.clone(),
        synthesized_groups: None,
        initial_state: Some("pairing-confirm".into()),
        api_assertions: None,
    };

    let result = evaluate(&snapshot, &page);

    // Every state in the fixture scores a full per-state match.
    for state in &spec.ui_states {
        let sr = result
            .state_results
            .iter()
            .find(|s| s.state_id == state.id)
            .unwrap_or_else(|| panic!("matcher returns a result for {}", state.id));
        assert_eq!(
            sr.match_rate, 1.0,
            "state {} must score a full per-state match (every assertion's element exists)",
            state.id
        );
    }

    assert_eq!(
        result.summary.match_outcome,
        MatchOutcome::FullMatch,
        "overall outcome is FullMatch across the whole page"
    );
    assert_eq!(
        result.summary.severity_counts.critical, 0,
        "no critical misses anywhere on the page"
    );
}

#[test]
fn pairing_error_alert_and_copy_invert_to_matchable_elements() {
    // pairing-error carries {role: alert} (critical) + {textContains: invalid} (warning).
    // Confirm the alert assertion (the load-bearing one) inverts to a real element the
    // matcher finds — this is the second state Phase 1 never exercised.
    let spec = load_spec();
    let app = generate_app(&spec, &load_profile());
    let err = app
        .screens
        .iter()
        .find(|s| s.state_id == "pairing-error")
        .expect("pairing-error screen generated");

    let alert = err
        .elements
        .iter()
        .find(|e| e.accessibility_role.as_deref() == Some("alert"))
        .expect("the alert assertion inverts to an alert-role element");
    assert!(!alert.needs_press, "an alert is not a press target");
    assert!(
        err.tsx.contains("accessibilityRole: 'alert'"),
        "emitted pairing-error screen forwards accessibilityRole=alert"
    );

    let copy = err
        .elements
        .iter()
        .find(|e| {
            e.visible_text
                .as_deref()
                .map(|t| t.contains("invalid"))
                .unwrap_or(false)
        })
        .expect("the textContains:invalid assertion inverts to an invalid-bearing element");
    assert!(err.tsx.contains(copy.visible_text.as_deref().unwrap()));
}

#[test]
fn transition_handler_is_wired_into_the_source_screen_and_invokes_the_operation() {
    let spec = load_spec();
    let app = generate_app(&spec, &load_profile());

    let edge = &app.nav_edges[0];
    assert_eq!(edge.transition_id, "show-pairing-error");
    assert_eq!(edge.from_state, "pairing-confirm");
    assert_eq!(edge.to_state, "pairing-error");
    // The transition fires from a REAL instrumented button on the source screen.
    assert!(
        edge.trigger_element_id.is_some(),
        "the show-pairing-error transition must wire to a real Connect button"
    );
    // The single mutating operation (pairConfirm) is linked to the click.
    assert_eq!(edge.invokes_operation.as_deref(), Some("pairConfirm"));

    // The source screen's emitted source documents the navigation + data-layer call.
    let confirm = app
        .screens
        .iter()
        .find(|s| s.state_id == "pairing-confirm")
        .unwrap();
    assert!(
        confirm.tsx.contains("router.push('/pairing-error')"),
        "source screen documents the navigation target"
    );
    assert!(
        confirm.tsx.contains("api.pairConfirm()"),
        "source screen documents the linked data-layer call"
    );
}

#[test]
fn data_layer_targets_the_observed_endpoint_via_shared_derivation() {
    let spec = load_spec();
    let app = generate_app(&spec, &load_profile());

    let m = app
        .services
        .iter()
        .find(|m| m.operation == "pairConfirm")
        .expect("pairConfirm service method generated");
    // The shared endpoint_for rule: custom op pairConfirm on Device ->
    // POST /api/v1/devices/pair-confirm — the page's actually-observed endpoint.
    assert_eq!(m.endpoint.method, "POST");
    assert_eq!(m.endpoint.path, "/api/v1/devices/pair-confirm");
    assert!(app
        .data_layer_tsx
        .contains("this.http.post('/api/v1/devices/pair-confirm'"));
}

#[test]
fn generation_is_deterministic() {
    // Byte-for-byte reproducibility across two runs (no LLM, no clock, no map iteration
    // order) — the property the whole observability seam rests on.
    let spec = load_spec();
    let profile = load_profile();
    let a = generate_app(&spec, &profile);
    let b = generate_app(&spec, &profile);
    let a_tsx: Vec<&String> = a.screens.iter().map(|s| &s.tsx).collect();
    let b_tsx: Vec<&String> = b.screens.iter().map(|s| &s.tsx).collect();
    assert_eq!(
        a_tsx, b_tsx,
        "screen sources are byte-identical across runs"
    );
    assert_eq!(a.data_layer_tsx, b.data_layer_tsx);
    assert_eq!(a.produced_refs, b.produced_refs);
}
