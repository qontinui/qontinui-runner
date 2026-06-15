//! Phase 1 oracle + Phase 2 anti-overpromise golden tests.
//!
//! The falsifiable assertion (plan Phase 1.3/1.4): given the committed
//! connect-runner snapshot + discovery fixtures and an LLM-inferred (here:
//! deliberately OVER-confident) entities/operations/auth, the **deterministic**
//! `assemble_spec` + `clamp_provenance` produce a spec whose
//! `enumerate_nodes (ref, section, provenance)` set EQUALS the frozen oracle
//! `connect-runner.functional_spec.json`, and that spec round-trips through
//! `evaluate_completeness` to coverage 1.0 with no gaps.
//!
//! This locks down the deterministic core. The LLM inference that produces the
//! `inferred-overconfident` content in production is runtime-deferred (see
//! `llm_runtime_deferred.rs`); here we substitute a fixed over-confident input
//! and prove the clamp makes it honest regardless of what the model claimed.

use std::collections::{BTreeMap, BTreeSet};

use qontinui_comprehension::clamp::EvidenceClass;
use qontinui_comprehension::input::DiscoveryResult;
use qontinui_comprehension::mapping::degraded_evidence_classes;
use qontinui_comprehension::worker::assemble_spec;

use qontinui_types::completeness_eval::{enumerate_nodes, evaluate_completeness, CoverageEvidence};
use qontinui_types::endpoint_for::endpoint_for;
use qontinui_types::functional_spec::{FunctionalSpec, SpecProvenance};
use qontinui_types::priorities_profile::Profile;

const SNAPSHOT: &str = include_str!("fixtures/comprehension/connect-runner.snapshot.json");
const DISCOVERY: &str = include_str!("fixtures/comprehension/connect-runner.discovery.json");
const INFERRED: &str =
    include_str!("fixtures/comprehension/connect-runner.inferred-overconfident.json");
const ORACLE: &str =
    include_str!("fixtures/comprehension/connect-runner.functional_spec.oracle.json");
const PROFILE: &str = include_str!("fixtures/comprehension/connect-runner.profile.json");

const SOURCE_URL: &str = "https://app.qontinui.io/connect-runner";
const OBSERVED_AT: &str = "2026-06-14T00:00:00Z";

/// The known evidence classes for connect-runner's degraded explorer path,
/// derived from the snapshot: deviceName/deviceId are rendered/form fields;
/// state/callback are deduced (opaque token + redirect semantics). This is what
/// the worker computes from the snapshot in production; here it is built via the
/// same `degraded_evidence_classes` helper so the test exercises the real path.
fn evidence_classes() -> BTreeMap<String, EvidenceClass> {
    // Sanity: the input fixtures parse against their mirror types.
    let _snap: qontinui_comprehension::input::Snapshot =
        serde_json::from_str(SNAPSHOT).expect("snapshot fixture parses");
    let discovery: DiscoveryResult =
        serde_json::from_str(DISCOVERY).expect("discovery fixture parses");
    assert_eq!(discovery.states.len(), 2, "two clustered states");

    degraded_evidence_classes(
        &["deviceName".into(), "deviceId".into()],
        &["Device".into()],
        &["pairConfirm".into()],
        &[
            ("Device".into(), "state".into()),
            ("Device".into(), "callback".into()),
        ],
    )
}

fn comprehend() -> FunctionalSpec {
    let inferred: FunctionalSpec = serde_json::from_str(INFERRED).expect("inferred fixture parses");
    let discovery: DiscoveryResult = serde_json::from_str(DISCOVERY).unwrap();
    let classes = evidence_classes();
    assemble_spec(
        inferred,
        &discovery,
        &classes,
        SOURCE_URL,
        Some(OBSERVED_AT),
    )
}

/// (ref, section, provenance) tuples — the single naming contract #3 must
/// produce exactly (plan Phase 1.3). Provenance is rendered to a string so the
/// tuple is `Ord` (the wire enum is `Hash`/`Eq` but not `Ord`).
fn node_tuples(spec: &FunctionalSpec) -> BTreeSet<(String, String, String)> {
    enumerate_nodes(spec)
        .into_iter()
        .map(|n| {
            (
                n.r#ref,
                format!("{:?}", n.section),
                format!("{:?}", n.provenance),
            )
        })
        .collect()
}

#[test]
fn comprehended_node_set_equals_oracle() {
    let got = comprehend();
    let oracle: FunctionalSpec = serde_json::from_str(ORACLE).expect("oracle parses");

    assert_eq!(
        node_tuples(&got),
        node_tuples(&oracle),
        "comprehended (ref, section, provenance) node set must equal the frozen oracle"
    );
}

#[test]
fn comprehended_credibility_bands_match_oracle() {
    let got = comprehend();
    let device = &got.entities[0];
    let by_name = |name: &str| {
        device
            .fields
            .iter()
            .find(|f| f.name == name)
            .unwrap()
            .clone()
    };

    // Rendered fields are Observed (no credibility).
    assert_eq!(by_name("deviceName").confidence, SpecProvenance::Observed);
    assert_eq!(by_name("deviceId").confidence, SpecProvenance::Observed);

    // Deduced fields clamped to Inferred with credibility ≤ 0.7.
    let state = by_name("state");
    assert_eq!(state.confidence, SpecProvenance::Inferred);
    assert!(
        state.credibility.unwrap() <= 0.7 + 1e-9,
        "state credibility ≤ 0.7"
    );
    let callback = by_name("callback");
    assert_eq!(callback.confidence, SpecProvenance::Inferred);
    assert!(
        callback.credibility.unwrap() <= 0.7 + 1e-9,
        "callback credibility ≤ 0.7"
    );

    // Auth clamped to Inferred with credibility ≤ 0.5 (cf. oracle auth: 0.5).
    let auth = got.auth.as_ref().unwrap();
    assert_eq!(auth.model, "session");
    assert_eq!(auth.confidence, SpecProvenance::Inferred);
    assert!(
        auth.credibility.unwrap() <= 0.5 + 1e-9,
        "auth credibility ≤ 0.5"
    );
}

#[test]
fn operation_effect_is_forced_assumed_and_ledgered() {
    let got = comprehend();
    let op = &got.operations[0];
    assert_eq!(op.name, "pairConfirm");
    let eff = op.effect.as_ref().unwrap();
    assert_eq!(
        eff.confidence,
        SpecProvenance::Assumed,
        "operation effect is Assumed by construction, even though the LLM claimed Observed"
    );
    assert!(
        eff.credibility.is_none(),
        "Assumed effect carries no credibility"
    );

    // The ledger is collated mechanically from the Assumed node — it must match.
    assert_eq!(got.assumptions.len(), 1);
    assert_eq!(got.assumptions[0].r#ref, "operations.pairConfirm.effect");
    assert!(got.assumptions[0].overridable);
}

#[test]
fn ui_states_carry_populated_assertions_and_discovery_provenance() {
    // The cross-plan seam: #3 OWNS populating IrState.assertions (so #1's
    // ui_states_spec_check is not vacuous), citing the real discovery module.
    let got = comprehend();
    assert_eq!(got.ui_states.len(), 2);
    for st in &got.ui_states {
        assert!(
            !st.assertions.is_empty(),
            "state {} must carry non-empty assertions",
            st.id
        );
        let prov = st.provenance.as_ref().expect("discovery provenance");
        assert_eq!(prov.source, "discovery");
        assert_eq!(
            prov.file.as_deref(),
            Some("discovery/state_discovery/strategies/fingerprint.py"),
            "provenance.file must cite the real fingerprint strategy module"
        );
    }
    // The initial state matches the oracle's.
    let confirm = got
        .ui_states
        .iter()
        .find(|s| s.id == "pairing-confirm")
        .unwrap();
    assert_eq!(confirm.is_initial, Some(true));
    assert_eq!(got.navigation.len(), 1);
    assert_eq!(got.navigation[0].id, "show-pairing-error");
}

#[test]
fn comprehended_spec_round_trips_to_full_coverage() {
    // Plan Phase 1.4: feed the comprehended spec through evaluate_completeness
    // with complete evidence and assert coverage == 1.0, gaps empty.
    let spec = comprehend();

    // Complete evidence: every Observed/Inferred node covered, every Assumed
    // node filled (the "complete generation" the slice's stub generators model).
    let mut covered = BTreeSet::new();
    let mut filled_assumed = BTreeSet::new();
    for node in enumerate_nodes(&spec) {
        match node.provenance {
            SpecProvenance::Assumed => {
                filled_assumed.insert(node.r#ref);
            }
            _ => {
                covered.insert(node.r#ref);
            }
        }
    }
    let evidence = CoverageEvidence {
        covered,
        gaps: BTreeMap::new(),
        filled_assumed,
    };
    let verdict = evaluate_completeness(&spec, &evidence, OBSERVED_AT);

    assert!(
        (verdict.coverage - 1.0).abs() < 1e-9,
        "comprehended spec must round-trip to coverage 1.0; got {}",
        verdict.coverage
    );
    assert!(verdict.gaps.is_empty(), "no gaps: {:?}", verdict.gaps);
    assert!((verdict.assumed_fill_rate - 1.0).abs() < 1e-9);
    assert!(verdict.coverage_is_consistent());
}

#[test]
fn observed_endpoint_feeds_endpoint_for_override_path() {
    // Cross-plan seam #1: comprehension records the observed endpoint in the
    // operation's provenance so endpoint_for's override path is fed. v0
    // endpoint_for derives deterministically from a correctly-named custom
    // operation; assert the comprehended pairConfirm derives the observed route.
    let spec = comprehend();
    let profile: Profile = serde_json::from_str(PROFILE).expect("profile parses");
    let op = &spec.operations[0];

    // The operation carries an honest provenance string naming the observed POST.
    let prov = op.provenance.as_deref().unwrap_or_default();
    assert!(
        prov.contains("/api/v1/devices/pair-confirm"),
        "operation provenance must record the observed endpoint; got {prov:?}"
    );

    // And the shared derivation reproduces that exact observed endpoint.
    let endpoint = endpoint_for(op, &profile);
    assert_eq!(endpoint.method, "POST");
    assert_eq!(endpoint.path, "/api/v1/devices/pair-confirm");
}
