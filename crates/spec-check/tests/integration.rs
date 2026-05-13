//! Integration smoke test. Step 10c of Plan 02.
//!
//! Drives the crate's public `evaluate()` API against hand-built fixtures
//! and asserts the result shape. Fixtures are kept minimal — three elements
//! and three matching assertions — so the test stays a fast canary rather
//! than a coverage instrument.

use qontinui_spec_check::{evaluate, AssertionOutcome, IrPageSpec, MatchOutcome, UIBridgeSnapshot};

#[test]
fn smoke_evaluate_returns_full_match_on_matching_fixture() {
    let snap_json = include_str!("fixtures/smoke-snapshot.json");
    let spec_json = include_str!("fixtures/smoke-spec.json");

    let snapshot: UIBridgeSnapshot =
        serde_json::from_str(snap_json).expect("snapshot fixture parses");
    let spec: IrPageSpec = serde_json::from_str(spec_json).expect("spec fixture parses");

    let result = evaluate(&snapshot, &spec);

    // Top-level shape.
    assert_eq!(result.result_schema_version, 1);
    assert_eq!(result.spec_version, "1.0");
    assert_eq!(result.page_id, "smoke-page");
    assert!(result.spec_content_hash.starts_with("sha256-"));

    // One state, three assertions, all passing.
    assert_eq!(result.state_results.len(), 1);
    let state = &result.state_results[0];
    assert_eq!(state.state_id, "loaded");
    assert_eq!(state.match_rate, 1.0);
    assert_eq!(state.assertions.len(), 3);
    for ar in &state.assertions {
        assert!(
            matches!(ar.outcome, AssertionOutcome::Pass { .. }),
            "assertion {} expected to pass, got {:?}",
            ar.assertion_id,
            ar.outcome
        );
    }

    // Summary.
    assert_eq!(result.summary.match_outcome, MatchOutcome::FullMatch);
    assert!((result.summary.overall_match_rate - 1.0).abs() < 1e-6);
    let counts = &result.summary.severity_counts;
    assert_eq!(counts.critical, 0);
    assert_eq!(counts.error, 0);
    assert_eq!(counts.warning, 0);
    assert_eq!(counts.info, 0);

    // Recommendation should pick the only state.
    let rec = result
        .summary
        .recommended_state
        .as_ref()
        .expect("must recommend a state on full match");
    assert_eq!(rec.state_id, "loaded");

    // Bridge fingerprint shape — internal-evaluator sentinel, 3 elements.
    assert_eq!(result.bridge_fingerprint.app_id, "internal-evaluator");
    assert_eq!(result.bridge_fingerprint.element_count, 3);
}
