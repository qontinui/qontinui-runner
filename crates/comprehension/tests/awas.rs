//! Phase 3 golden tests — the AWAS ingestion path.
//!
//! The falsifiable claim (plan Phase 3 done-criterion): a cooperating site
//! comprehends at strictly higher confidence than the same site would via
//! explorer-only, demonstrated on one fixture pair — the hand-authored
//! `connect-runner.awas-manifest.json` seed merged with the SAME
//! `inferred-overconfident` + discovery inputs the Phase 1 oracle uses. The
//! honesty invariant (§5) holds throughout: the effect stays `Assumed` and
//! ledgered, and an over-confident (or under-confident) LLM input cannot move a
//! seeded node.

use std::collections::{BTreeMap, BTreeSet};

use qontinui_comprehension::awas_seed::{awas_to_spec_seed, AwasManifest};
use qontinui_comprehension::clamp::{collate_assumptions, EvidenceClass};
use qontinui_comprehension::input::DiscoveryResult;
use qontinui_comprehension::mapping::degraded_evidence_classes;
use qontinui_comprehension::worker::{assemble_spec, assemble_spec_with_seed};

use qontinui_types::completeness_eval::{enumerate_nodes, evaluate_completeness, CoverageEvidence};
use qontinui_types::functional_spec::{FunctionalSpec, SpecProvenance};

const MANIFEST: &str = include_str!("fixtures/comprehension/connect-runner.awas-manifest.json");
const DISCOVERY: &str = include_str!("fixtures/comprehension/connect-runner.discovery.json");
const INFERRED: &str =
    include_str!("fixtures/comprehension/connect-runner.inferred-overconfident.json");

const SOURCE_URL: &str = "https://app.qontinui.io/connect-runner";
const OBSERVED_AT: &str = "2026-06-14T00:00:00Z";

fn manifest() -> AwasManifest {
    serde_json::from_str(MANIFEST).expect("manifest fixture parses")
}

fn inferred() -> FunctionalSpec {
    serde_json::from_str(INFERRED).expect("inferred fixture parses")
}

fn discovery() -> DiscoveryResult {
    serde_json::from_str(DISCOVERY).expect("discovery fixture parses")
}

/// The same degraded classes `tests/oracle.rs` builds — the explorer's view of
/// connect-runner, which the seed's `AwasDeclared` entries override on collision.
fn degraded_classes() -> BTreeMap<String, EvidenceClass> {
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

fn comprehend_with_seed(inferred: FunctionalSpec) -> FunctionalSpec {
    let (seed, seed_classes) = awas_to_spec_seed(&manifest());
    assemble_spec_with_seed(
        seed,
        &seed_classes,
        inferred,
        &discovery(),
        &degraded_classes(),
        SOURCE_URL,
        Some(OBSERVED_AT),
    )
}

fn op<'a>(spec: &'a FunctionalSpec, name: &str) -> &'a qontinui_types::functional_spec::Operation {
    spec.operations
        .iter()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("operation {name} present"))
}

#[test]
fn manifest_fixture_parses_camel_case_top_level_and_snake_case_actions() {
    let m = manifest();
    assert_eq!(m.schema_version, "1.0");
    assert_eq!(m.app_name, "Qontinui Connect Runner");
    assert_eq!(m.base_url, "https://app.qontinui.io");
    assert_eq!(m.conformance_level.as_deref(), Some("L1"));
    assert_eq!(m.actions.len(), 1);
    let a = &m.actions[0];
    assert_eq!(a.id, "pairConfirm");
    assert_eq!(a.method, "POST");
    assert_eq!(a.endpoint, "/api/runners/pair");
    assert!(a.side_effect, "snake_case side_effect read");
    assert_eq!(a.parameters.len(), 2);
    assert_eq!(a.parameters[0].location, "body");
    assert_eq!(a.parameters[0].param_type, "string");
    assert!(a.parameters[0].required);
    assert_eq!(a.required_scopes, vec!["runner:pair".to_string()]);
    assert!(a.input_schema.is_some(), "snake_case input_schema read");
    let auth = m.auth.as_ref().expect("auth present");
    assert_eq!(auth.auth_type, "api_key");
    assert_eq!(auth.header_name.as_deref(), Some("X-API-Key"));
    assert_eq!(auth.scopes.len(), 1);
    assert_eq!(auth.scopes[0].name, "runner:pair");
}

#[test]
fn manifest_accepts_snake_case_top_level_aliases_too() {
    let m: AwasManifest = serde_json::from_str(
        r#"{"schema_version":"1.0","app_name":"X","base_url":"https://x.test","actions":[]}"#,
    )
    .expect("snake_case aliases parse");
    assert_eq!(m.app_name, "X");
    assert_eq!(m.base_url, "https://x.test");
    // A parameter's type defaults to string when absent, as pydantic's does.
    let a: qontinui_comprehension::awas_seed::AwasParameter =
        serde_json::from_str(r#"{"name":"q","location":"query"}"#).unwrap();
    assert_eq!(a.param_type, "string");
}

#[test]
fn seed_emits_declared_operation_inputs_effect_and_auth() {
    let (seed, classes) = awas_to_spec_seed(&manifest());

    assert_eq!(seed.spec_version, "0");
    assert_eq!(seed.target.source_url, "https://app.qontinui.io");
    assert!(seed.ui_states.is_empty() && seed.navigation.is_empty());
    assert!(seed.assumptions.is_empty(), "the tail collates the ledger");

    let op = op(&seed, "pairConfirm");
    assert_eq!(op.confidence, SpecProvenance::Observed);
    assert_eq!(op.verb, "create");
    assert_eq!(op.entity.as_deref(), Some("Device"));
    assert!(op.credibility.is_none());
    let prov = op.provenance.as_deref().unwrap();
    assert!(
        prov.contains("/api/runners/pair") && prov.contains("POST"),
        "provenance carries the declared endpoint for endpoint_for's override path: {prov}"
    );
    assert_eq!(op.inputs.len(), 2);
    for input in &op.inputs {
        assert!(input.required);
        let v = input
            .validation
            .as_ref()
            .expect("validation from parameter");
        assert_eq!(v.rule, "type:string");
        assert_eq!(v.confidence, SpecProvenance::Observed);
        assert_eq!(
            v.provenance.as_deref(),
            Some(format!("awas:pairConfirm.parameters.{} location=body", input.field).as_str())
        );
    }
    let eff = op.effect.as_ref().expect("effect seeded");
    assert_eq!(
        eff.confidence,
        SpecProvenance::Assumed,
        "a declared side_effect sharpens the assumption text, never the class"
    );
    assert_eq!(
        eff.assumption.as_deref(),
        Some("AWAS declares side_effect=true for POST /api/runners/pair")
    );
    assert!(eff.credibility.is_none());

    // The entity from the input_schema (title → Device).
    assert_eq!(seed.entities.len(), 1);
    let device = &seed.entities[0];
    assert_eq!(device.name, "Device");
    assert_eq!(device.confidence, SpecProvenance::Observed);
    let names: Vec<&str> = device.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["deviceId", "deviceName"]);
    assert!(device
        .fields
        .iter()
        .all(|f| f.confidence == SpecProvenance::Observed
            && f.provenance.as_deref() == Some("awas:pairConfirm.input_schema")));

    let auth = seed.auth.as_ref().expect("auth seeded");
    assert_eq!(auth.model, "api_key");
    assert_eq!(auth.confidence, SpecProvenance::Observed);
    assert!(auth.credibility.is_none());
    assert_eq!(auth.roles.len(), 1);
    assert_eq!(auth.roles[0].name, "runner:pair");
    assert_eq!(auth.roles[0].confidence, SpecProvenance::Observed);

    // Every emitted provenance-bearing node is classed; the declared ones are
    // pinned, the effect is Silent (its class never lifts Assumed).
    for node in enumerate_nodes(&seed) {
        let class = classes
            .get(&node.r#ref)
            .unwrap_or_else(|| panic!("class for {}", node.r#ref));
        if node.r#ref.ends_with(".effect") {
            assert_eq!(*class, EvidenceClass::Silent);
        } else {
            assert_eq!(*class, EvidenceClass::AwasDeclared, "{}", node.r#ref);
            assert!(class.is_pinned());
        }
    }
    assert_eq!(
        classes.get("auth.roles.runner:pair"),
        Some(&EvidenceClass::AwasDeclared)
    );
}

#[test]
fn awas_seed_lifts_confidence_over_the_degraded_path() {
    // The delivered degraded path on the same inputs: auth Inferred ≤ 0.5.
    let degraded = assemble_spec(
        inferred(),
        &discovery(),
        &degraded_classes(),
        SOURCE_URL,
        Some(OBSERVED_AT),
    );
    let d_auth = degraded.auth.as_ref().unwrap();
    assert_eq!(d_auth.confidence, SpecProvenance::Inferred);
    assert!(d_auth.credibility.unwrap() <= 0.5 + 1e-9);
    assert_eq!(
        op(&degraded, "pairConfirm").confidence,
        SpecProvenance::Observed
    );

    // The seeded path: operation Observed, auth Observed with no credibility.
    let seeded = comprehend_with_seed(inferred());
    let s_op = op(&seeded, "pairConfirm");
    assert_eq!(s_op.confidence, SpecProvenance::Observed);
    assert!(s_op.credibility.is_none());
    assert!(
        s_op.provenance.as_deref().unwrap().starts_with("awas:"),
        "the seed node is kept, not the LLM copy"
    );
    let s_auth = seeded.auth.as_ref().unwrap();
    assert_eq!(s_auth.model, "api_key");
    assert_eq!(s_auth.confidence, SpecProvenance::Observed);
    assert!(s_auth.credibility.is_none());
    assert_eq!(s_auth.roles.len(), 1);

    // Exactly one pairConfirm / one Device after the merge; the inferred
    // callback input was unioned in (additive), the seeded two kept.
    assert_eq!(
        seeded
            .operations
            .iter()
            .filter(|o| o.name == "pairConfirm")
            .count(),
        1
    );
    assert_eq!(
        seeded
            .entities
            .iter()
            .filter(|e| e.name == "Device")
            .count(),
        1
    );
    let fields: Vec<&str> = s_op.inputs.iter().map(|i| i.field.as_str()).collect();
    assert_eq!(fields, vec!["deviceName", "deviceId", "callback"]);

    // The explorer still fills what the manifest does not declare — and the
    // clamp still bounds those at the degraded classes (state/callback Deduced).
    let device = seeded.entities.iter().find(|e| e.name == "Device").unwrap();
    let field = |n: &str| device.fields.iter().find(|f| f.name == n).unwrap();
    assert_eq!(field("deviceName").confidence, SpecProvenance::Observed);
    assert_eq!(
        field("deviceName").provenance.as_deref(),
        Some("awas:pairConfirm.input_schema"),
        "seed field kept over the inferred copy"
    );
    assert_eq!(field("state").confidence, SpecProvenance::Inferred);
    assert!(field("state").credibility.unwrap() <= 0.7 + 1e-9);
    assert_eq!(field("callback").confidence, SpecProvenance::Inferred);

    // The IR is still the discovery's.
    assert_eq!(seeded.ui_states.len(), 2);
    assert_eq!(seeded.navigation.len(), 1);
    assert_eq!(seeded.target.source_url, SOURCE_URL);
    assert_eq!(seeded.target.observed_at.as_deref(), Some(OBSERVED_AT));
}

#[test]
fn inferred_copy_at_inferred_cannot_demote_a_seeded_observed_node() {
    let mut weak = inferred();
    for o in &mut weak.operations {
        o.confidence = SpecProvenance::Inferred;
        o.credibility = Some(0.2);
    }
    for e in &mut weak.entities {
        e.confidence = SpecProvenance::Inferred;
        e.credibility = Some(0.2);
        for f in &mut e.fields {
            f.confidence = SpecProvenance::Inferred;
            f.credibility = Some(0.1);
        }
    }
    if let Some(a) = &mut weak.auth {
        a.confidence = SpecProvenance::Assumed;
    }

    let seeded = comprehend_with_seed(weak);
    let op = op(&seeded, "pairConfirm");
    assert_eq!(
        op.confidence,
        SpecProvenance::Observed,
        "seeded Observed survives"
    );
    assert!(op.credibility.is_none());
    let device = seeded.entities.iter().find(|e| e.name == "Device").unwrap();
    assert_eq!(device.confidence, SpecProvenance::Observed);
    for name in ["deviceName", "deviceId"] {
        let f = device.fields.iter().find(|f| f.name == name).unwrap();
        assert_eq!(f.confidence, SpecProvenance::Observed, "{name} pinned");
        assert!(f.credibility.is_none());
    }
    let auth = seeded.auth.as_ref().unwrap();
    assert_eq!(auth.confidence, SpecProvenance::Observed, "seed auth wins");
}

#[test]
fn seeded_effect_is_still_assumed_and_ledgered() {
    let seeded = comprehend_with_seed(inferred());
    let eff = op(&seeded, "pairConfirm").effect.as_ref().unwrap();
    assert_eq!(eff.confidence, SpecProvenance::Assumed);
    assert!(eff.credibility.is_none());
    assert_eq!(seeded.assumptions.len(), 1);
    assert_eq!(seeded.assumptions[0].r#ref, "operations.pairConfirm.effect");
    assert_eq!(
        seeded.assumptions[0].default_applied,
        "AWAS declares side_effect=true for POST /api/runners/pair"
    );
    assert!(seeded.assumptions[0].overridable);
    assert_eq!(collate_assumptions(&seeded), seeded.assumptions);
}

#[test]
fn seeded_spec_round_trips_to_full_coverage() {
    let spec = comprehend_with_seed(inferred());

    let mut covered = BTreeSet::new();
    let mut filled_assumed = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for node in enumerate_nodes(&spec) {
        assert!(
            seen.insert(node.r#ref.clone()),
            "duplicate ref {}",
            node.r#ref
        );
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
        "seeded spec must round-trip to coverage 1.0; got {}",
        verdict.coverage
    );
    assert!(verdict.gaps.is_empty(), "no gaps: {:?}", verdict.gaps);
    assert!((verdict.assumed_fill_rate - 1.0).abs() < 1e-9);
    assert!(verdict.coverage_is_consistent());
}
