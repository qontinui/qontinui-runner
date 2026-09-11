//! Phase 3 item 3 — **the falsification test**: prove that OBSERVATION, not assertion,
//! drives coverage.
//!
//! Every other test in this crate confirms the generator does the right thing. This one
//! confirms the verify adapter notices when it does the WRONG thing — which is the only way
//! a coverage number means anything. A test that passes against both the intact and the
//! broken generator asserts nothing at all.
//!
//! The break is applied through the generator's **own seam** —
//! [`InstrumentedElement::accessibility_role`] / [`InstrumentedElement::label`] /
//! `visible_text` on the `Connect` button of `pairing-confirm` — not by hand-editing an
//! expected snapshot. Every snapshot in the loop is still the deterministic
//! `snapshot_element_from` projection the matcher actually consumes, so the mutation
//! propagates exactly the way a real instrumentation bug would.
//!
//! Both directions are asserted here, in this one file:
//!
//! - **intact** — `uiStates.pairing-confirm` and `navigation.show-pairing-error` ARE covered,
//!   and the state scores `match_rate == 1.0` / `Green`;
//! - **broken** — the state's `match_rate` DROPS below `Green`, `uiStates.pairing-confirm` is
//!   NOT covered, the verdict carries a `BehaviorMismatch` gap for
//!   `navigation.show-pairing-error` (the button that transition needs) with a non-empty
//!   `detail`, and `ui_states_spec_check` reflects the miss.
//!
//! Snapshot source (Phase 1 done-criterion): deterministic per-state
//! `snapshot_element_from` projection. The on-device render leg stays deferred in
//! `runtime_render_deferred.rs`.

use qontinui_app_generator::verify::{
    coverage_evidence_from_app, spec_check_per_state, verdict_with_spec_check,
};
use qontinui_app_generator::{generate_app, GeneratedApp};
use qontinui_types::completeness_eval::CoverageEvidence;
use qontinui_types::completeness_verdict::{CompletenessVerdict, GapReason};
use qontinui_types::functional_spec::FunctionalSpec;
use qontinui_types::priorities_profile::Profile;
use qontinui_types::spec_check::{AssertionOutcome, ClassificationStatus, SpecCheckResult};

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../qontinui-schemas/rust/tests/fixtures/functional_spec"
);

const EVALUATED_AT: &str = "2026-06-14T00:00:00Z";

/// The state whose instrumentation the mutation breaks.
const STATE: &str = "pairing-confirm";
/// The transition whose trigger lives on that state.
const TRANSITION: &str = "show-pairing-error";
/// The assertion the `Connect` button satisfies when instrumentation is intact.
const CONNECT_ASSERTION: &str = "pairing-confirm-connect-button";

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

/// Break the `Connect` button's instrumentation through the generator's own seam: the
/// element is still registered and still snapshot-able, it just carries the WRONG
/// `accessibilityRole` and the WRONG visible text / accessible name — exactly the shape of a
/// real presentational-fill regression that a shipped app would carry silently.
///
/// The nav graph is deliberately left untouched: `nav_edges` was resolved before the
/// mutation, so `show-pairing-error` still believes it has a trigger. That is what makes the
/// navigation gap a `BehaviorMismatch` (the button is claimed but not observable) rather than
/// a `NotGenerated` one.
fn break_connect_button(app: &mut GeneratedApp) {
    let screen = app
        .screens
        .iter_mut()
        .find(|s| s.state_id == STATE)
        .expect("the fixture generates a pairing-confirm screen");
    let button = screen
        .elements
        .iter_mut()
        .find(|e| e.accessibility_role.as_deref() == Some("button"))
        .expect("the pairing-confirm screen instruments a button element");
    button.accessibility_role = Some("text".to_string());
    button.label = Some("Continue".to_string());
    button.visible_text = Some("Continue".to_string());
    button.needs_press = false;
}

/// Run the full Phase-3 verify loop over an app.
fn verify(
    app: &GeneratedApp,
    spec: &FunctionalSpec,
) -> (SpecCheckResult, CoverageEvidence, CompletenessVerdict) {
    let result = spec_check_per_state(app, spec);
    let evidence = coverage_evidence_from_app(app, &result, &[]);
    let verdict = verdict_with_spec_check(spec, &evidence, &result, EVALUATED_AT);
    (result, evidence, verdict)
}

fn match_rate(result: &SpecCheckResult, state_id: &str) -> f32 {
    result
        .state_results
        .iter()
        .find(|s| s.state_id == state_id)
        .unwrap_or_else(|| panic!("spec-check returns a result for `{state_id}`"))
        .match_rate
}

// ===========================================================================
// Direction 1 — INTACT: the refs are covered, and the matcher says why.
// ===========================================================================

#[test]
fn intact_instrumentation_is_observed_and_covers_the_state_and_its_transition() {
    let spec = load_spec();
    let app = generate_app(&spec, &load_profile());
    let (result, evidence, verdict) = verify(&app, &spec);

    assert_eq!(
        match_rate(&result, STATE),
        1.0,
        "with intact instrumentation every assertion on `{STATE}` finds its element"
    );
    let sr = result
        .state_results
        .iter()
        .find(|s| s.state_id == STATE)
        .unwrap();
    assert_eq!(
        sr.classification,
        ClassificationStatus::Green,
        "a fully-matched state classifies Green"
    );

    assert!(
        evidence.covered.contains(&format!("uiStates.{STATE}")),
        "uiStates.{STATE} is covered; covered = {:?}",
        evidence.covered
    );
    assert!(
        evidence
            .covered
            .contains(&format!("navigation.{TRANSITION}")),
        "navigation.{TRANSITION} is covered; covered = {:?}",
        evidence.covered
    );
    assert!(
        !verdict
            .gaps
            .iter()
            .any(|g| g.r#ref == format!("uiStates.{STATE}")
                || g.r#ref == format!("navigation.{TRANSITION}")),
        "an intact app gaps neither ref; gaps = {:?}",
        verdict.gaps
    );
}

// ===========================================================================
// Direction 2 — BROKEN: the same loop refuses to cover what it cannot observe.
// ===========================================================================

#[test]
fn broken_instrumentation_drops_the_match_rate_and_withdraws_coverage() {
    let spec = load_spec();
    let profile = load_profile();

    let intact = generate_app(&spec, &profile);
    let (intact_result, _, _) = verify(&intact, &spec);
    let intact_rate = match_rate(&intact_result, STATE);

    let mut broken = generate_app(&spec, &profile);
    break_connect_button(&mut broken);
    let (broken_result, evidence, verdict) = verify(&broken, &spec);
    let broken_rate = match_rate(&broken_result, STATE);

    // 1. The matcher's own number moves. This is the load-bearing comparison: if the
    //    mutation did not reach the matcher, everything below would pass vacuously.
    assert!(
        broken_rate < intact_rate,
        "breaking the Connect button's role/label must DROP `{STATE}`'s match rate \
         (intact {intact_rate}, broken {broken_rate})"
    );
    let broken_sr = broken_result
        .state_results
        .iter()
        .find(|s| s.state_id == STATE)
        .unwrap();
    assert_ne!(
        broken_sr.classification,
        ClassificationStatus::Green,
        "a state whose critical assertion misses cannot classify Green"
    );

    // 2. The specific assertion that names the Connect button is the one that fails.
    let connect = broken_sr
        .assertions
        .iter()
        .find(|a| a.assertion_id == CONNECT_ASSERTION)
        .unwrap_or_else(|| panic!("the matcher evaluates `{CONNECT_ASSERTION}`"));
    assert!(
        matches!(connect.outcome, AssertionOutcome::Fail { .. }),
        "`{CONNECT_ASSERTION}` must fail against the broken instrumentation, got {:?}",
        connect.outcome
    );

    // 3. Coverage is WITHDRAWN — the ref the intact run covered is no longer covered.
    assert!(
        !evidence.covered.contains(&format!("uiStates.{STATE}")),
        "uiStates.{STATE} must NOT be covered when the matcher cannot confirm it; covered = {:?}",
        evidence.covered
    );

    // 4. The transition that needs that button becomes a BehaviorMismatch gap with a real
    //    diagnostic — not a silent pass, and not a bare NotGenerated.
    let nav_gap = verdict
        .gaps
        .iter()
        .find(|g| g.r#ref == format!("navigation.{TRANSITION}"))
        .unwrap_or_else(|| {
            panic!(
                "navigation.{TRANSITION} must gap when its trigger screen is unconfirmed; gaps = {:?}",
                verdict.gaps
            )
        });
    assert_eq!(
        nav_gap.reason,
        GapReason::BehaviorMismatch,
        "the trigger was generated but is not observable — a behavior mismatch, not an absence"
    );
    let detail = nav_gap
        .detail
        .as_deref()
        .expect("a BehaviorMismatch gap carries the matcher's diagnostic");
    assert!(
        !detail.trim().is_empty(),
        "the gap detail must actually say something"
    );
    assert!(
        detail.contains(STATE),
        "the detail names the source screen that failed: {detail}"
    );

    // 5. The state's own gap carries the matcher's miss, naming the failing assertion.
    let state_gap = verdict
        .gaps
        .iter()
        .find(|g| g.r#ref == format!("uiStates.{STATE}"))
        .unwrap_or_else(|| panic!("uiStates.{STATE} gaps; gaps = {:?}", verdict.gaps));
    assert_eq!(state_gap.reason, GapReason::BehaviorMismatch);
    let state_detail = state_gap.detail.as_deref().unwrap_or_default();
    assert!(
        state_detail.contains(CONNECT_ASSERTION),
        "the miss diagnostic names the assertion that failed: {state_detail}"
    );

    // 6. `ui_states_spec_check` is populated AND reflects the miss — the field this whole
    //    phase exists to fill, carrying the evidence rather than a summary of it.
    let embedded = verdict
        .ui_states_spec_check
        .as_ref()
        .expect("the verdict carries the per-state spec-check run");
    let embedded_sr = embedded
        .state_results
        .iter()
        .find(|s| s.state_id == STATE)
        .expect("the embedded run carries the broken state");
    assert_eq!(
        embedded_sr.match_rate, broken_rate,
        "the embedded result is the run the evidence was derived from, not a fresh one"
    );
    assert!(
        embedded_sr
            .assertions
            .iter()
            .any(|a| a.assertion_id == CONNECT_ASSERTION
                && matches!(a.outcome, AssertionOutcome::Fail { .. })),
        "the embedded spec-check result shows the Connect assertion missing"
    );
}

// ===========================================================================
// The contrast, in one assertion — coverage is a function of observation only.
// ===========================================================================

#[test]
fn the_same_spec_yields_opposite_coverage_from_intact_and_broken_instrumentation() {
    // Identical spec, identical rubric, identical adapter. The ONLY difference is what the
    // snapshot exposes — so any divergence below is attributable to observation alone.
    let spec = load_spec();
    let profile = load_profile();
    let state_ref = format!("uiStates.{STATE}");
    let nav_ref = format!("navigation.{TRANSITION}");

    let intact = generate_app(&spec, &profile);
    let (_, intact_evidence, intact_verdict) = verify(&intact, &spec);

    let mut broken = generate_app(&spec, &profile);
    break_connect_button(&mut broken);
    let (_, broken_evidence, broken_verdict) = verify(&broken, &spec);

    assert!(
        intact_evidence.covered.contains(&state_ref) && intact_evidence.covered.contains(&nav_ref),
        "intact: both refs covered"
    );
    assert!(
        !broken_evidence.covered.contains(&state_ref)
            && !broken_evidence.covered.contains(&nav_ref),
        "broken: neither ref covered"
    );
    assert!(
        broken_verdict.coverage < intact_verdict.coverage,
        "overall coverage must fall when instrumentation breaks (intact {}, broken {})",
        intact_verdict.coverage,
        broken_verdict.coverage
    );
    assert!(
        intact_verdict.coverage_is_consistent() && broken_verdict.coverage_is_consistent(),
        "both verdicts stay internally consistent with the frozen rubric"
    );
}
