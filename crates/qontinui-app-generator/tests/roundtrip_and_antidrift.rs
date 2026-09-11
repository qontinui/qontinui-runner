//! Phase 3 item 0 — the round-trip + anti-drift pair carried in from Phase 2 item 5.
//!
//! Both assertions run over the **real** [`generate_app`] output. `stub_app_generate`
//! (`qontinui-schemas/rust/tests/vertical_slice.rs`) stays exactly where it is: it proves the
//! schemas-local seam contract independently of any generator, and deleting it would remove
//! that independence.
//!
//! - **Round-trip** — every ref `enumerate_nodes(spec)` emits that node #1 owns is present in
//!   `GeneratedApp.produced_refs`, and the verdict from `verdict_with_spec_check` reaches
//!   coverage `1.0` on the `uiStates` / `navigation` / `operations` sections.
//! - **Anti-drift** — drop one `ui_state` from the generator's INPUT, evaluate the generated
//!   app against the FULL spec, and the verdict gains a `CoverageGap` for the dropped
//!   `uiStates.<id>` ref. The generator cannot hide a missing screen behind an assertion.
//!
//! ## Which refs node #1 owns (coordinator §2), stated so the round-trip cannot drift
//!
//! `enumerate_nodes` emits three kinds of node in the `operations` section:
//! `operations.<op>` (this crate), `operations.<op>.inputs.<f>.validation` and
//! `operations.<op>.effect`. The latter two are observed by node #2's live-endpoint leg
//! (`crates/qontinui-backend-generator/tests/runtime_integration.rs`) — `.effect` is an
//! `Assumed` node that reaches `filled_assumed`, never `covered`. So the round-trip asserts
//! produced-ness over the refs THIS generator owns, and reaches section coverage `1.0`
//! through the union with `backend_refs`, which is exactly how the verify phase composes the
//! two halves.
//!
//! Snapshot source (Phase 1 done-criterion): the deterministic `snapshot_element_from`
//! projection of the generator's own instrumentation, one snapshot per state. The on-device
//! render leg stays deferred in `runtime_render_deferred.rs`.

use std::collections::BTreeSet;

use qontinui_app_generator::generate_app;
use qontinui_app_generator::verify::{
    coverage_evidence_from_app, spec_check_per_state, verdict_with_spec_check,
};
use qontinui_types::completeness_eval::enumerate_nodes;
use qontinui_types::completeness_verdict::SpecSection;
use qontinui_types::functional_spec::FunctionalSpec;
use qontinui_types::priorities_profile::Profile;

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../qontinui-schemas/rust/tests/fixtures/functional_spec"
);

const EVALUATED_AT: &str = "2026-06-14T00:00:00Z";

/// Node #2's half of the union for the connect-runner fixture — mirrored from what
/// `qontinui-backend-generator/tests/runtime_integration.rs::evidence_from_live` covers when
/// the live backend is healthy. Passing it in (rather than deriving it here) is the
/// coordinator-§2 contract: `coverage_evidence_from_app` never invents #2's observations.
fn backend_refs() -> Vec<String> {
    [
        "entities.Device",
        "entities.Device.fields.deviceName",
        "entities.Device.fields.deviceId",
        "entities.Device.fields.state",
        "entities.Device.fields.callback",
        "operations.pairConfirm.inputs.callback.validation",
        "auth",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

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

/// The refs node #1 is responsible for producing: every `uiStates.*` / `navigation.*` node
/// plus the top-level `operations.<name>` node.
fn node1_owned_refs(spec: &FunctionalSpec) -> Vec<String> {
    enumerate_nodes(spec)
        .into_iter()
        .filter(|n| match n.section {
            SpecSection::UiStates | SpecSection::Navigation => true,
            // `operations.<op>` only — the `.inputs.*.validation` / `.effect` sub-nodes are
            // node #2's observations (see the module docs).
            SpecSection::Operations => n.r#ref.matches('.').count() == 1,
            _ => false,
        })
        .map(|n| n.r#ref)
        .collect()
}

fn section_coverage(
    verdict: &qontinui_types::completeness_verdict::CompletenessVerdict,
    section: SpecSection,
) -> f64 {
    verdict
        .sections
        .iter()
        .find(|s| s.section == section)
        .unwrap_or_else(|| panic!("verdict carries a {section:?} section"))
        .coverage
}

// ===========================================================================
// Round-trip
// ===========================================================================

#[test]
fn every_node1_owned_ref_is_produced_by_the_real_generator() {
    let spec = load_spec();
    let app = generate_app(&spec, &load_profile());

    let produced: BTreeSet<String> = app.produced_refs.iter().cloned().collect();
    for r in node1_owned_refs(&spec) {
        assert!(
            produced.contains(&r),
            "enumerate_nodes emits `{r}` but generate_app did not produce it; produced = {:?}",
            app.produced_refs
        );
    }

    // The convention itself: nothing produced under a name enumerate_nodes would never emit.
    let enumerated: BTreeSet<String> = enumerate_nodes(&spec)
        .into_iter()
        .map(|n| n.r#ref)
        .collect();
    for r in &app.produced_refs {
        assert!(
            enumerated.contains(r),
            "generate_app produced `{r}`, which is not an enumerate_nodes ref (dotted-convention drift)"
        );
    }
}

#[test]
fn round_trip_reaches_full_coverage_on_the_ui_nav_and_operations_sections() {
    let spec = load_spec();
    let app = generate_app(&spec, &load_profile());

    let result = spec_check_per_state(&app, &spec);
    let evidence = coverage_evidence_from_app(&app, &result, &backend_refs());
    let verdict = verdict_with_spec_check(&spec, &evidence, &result, EVALUATED_AT);

    assert_eq!(
        section_coverage(&verdict, SpecSection::UiStates),
        1.0,
        "every uiStates node is confirmed by a real per-state matcher run; gaps = {:?}",
        verdict.gaps
    );
    assert_eq!(
        section_coverage(&verdict, SpecSection::Navigation),
        1.0,
        "every navigation node is confirmed; gaps = {:?}",
        verdict.gaps
    );
    assert_eq!(
        section_coverage(&verdict, SpecSection::Operations),
        1.0,
        "the operations section closes under the #1 + #2 union; gaps = {:?}",
        verdict.gaps
    );

    // The rubric's own self-check, and the field this phase exists to populate.
    assert!(
        verdict.coverage_is_consistent(),
        "coverage must agree with the provenance mix and counted gaps"
    );
    assert!(
        verdict.ui_states_spec_check.is_some(),
        "the verdict carries the per-state spec-check run"
    );
}

// ===========================================================================
// Anti-drift
// ===========================================================================

#[test]
fn dropping_a_ui_state_from_the_generator_input_surfaces_a_coverage_gap() {
    let full = load_spec();
    let profile = load_profile();

    // The generator is fed a REDUCED spec (one state dropped); the verdict is scored against
    // the FULL spec. This is drift: the app no longer covers everything the spec observed.
    let dropped_id = "pairing-error";
    let mut reduced = full.clone();
    reduced.ui_states.retain(|s| s.id != dropped_id);
    assert_eq!(
        reduced.ui_states.len(),
        full.ui_states.len() - 1,
        "the fixture really does carry the state we drop"
    );

    let app = generate_app(&reduced, &profile);
    assert!(
        !app.produced_refs
            .iter()
            .any(|r| r == &format!("uiStates.{dropped_id}")),
        "the reduced generator run must not claim the dropped state"
    );

    let result = spec_check_per_state(&app, &full);
    let evidence = coverage_evidence_from_app(&app, &result, &backend_refs());
    let verdict = verdict_with_spec_check(&full, &evidence, &result, EVALUATED_AT);

    let gap = verdict
        .gaps
        .iter()
        .find(|g| g.r#ref == format!("uiStates.{dropped_id}"))
        .unwrap_or_else(|| {
            panic!(
                "dropping `{dropped_id}` must surface a CoverageGap; gaps = {:?}",
                verdict.gaps
            )
        });
    assert_eq!(gap.section, SpecSection::UiStates);
    assert!(
        section_coverage(&verdict, SpecSection::UiStates) < 1.0,
        "a dropped state must cost uiStates coverage"
    );

    // The state that WAS generated is still confirmed — the drift is localized, not a
    // blanket failure that would pass for any reason at all.
    assert!(
        evidence.covered.contains("uiStates.pairing-confirm"),
        "the surviving state is still confirmed by the matcher; covered = {:?}",
        evidence.covered
    );
}
