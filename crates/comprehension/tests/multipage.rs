//! Phase 4 golden tests — multi-page comprehension.
//!
//! The falsifiable claim (plan Phase 4 done-criterion): three fixture pages
//! that each render a `Device` differently (the devices list, a device detail
//! page, and the delivered connect-runner pair) aggregate into ONE coherent
//! spec — one `Device` entity, not three, its fields the union at the highest
//! evidence each page justified; `ui_states` = the DISTINCT state ids across
//! pages; that round-trips through `evaluate_completeness` to coverage 1.0 and
//! whose ledger `collate_assumptions` reproduces.

use std::collections::{BTreeMap, BTreeSet};

use qontinui_comprehension::aggregate::{aggregate_pages, PageObservation};
use qontinui_comprehension::clamp::{collate_assumptions, EvidenceClass};
use qontinui_comprehension::input::{DiscoveryResult, Snapshot};
use qontinui_comprehension::mapping::degraded_evidence_classes;
use qontinui_comprehension::worker::assemble_spec;

use qontinui_types::completeness_eval::{enumerate_nodes, evaluate_completeness, CoverageEvidence};
use qontinui_types::functional_spec::{FunctionalSpec, SpecProvenance};

const DEVICES_SNAPSHOT: &str = include_str!("fixtures/comprehension/site-devices.snapshot.json");
const DEVICES_DISCOVERY: &str = include_str!("fixtures/comprehension/site-devices.discovery.json");
const DETAIL_SNAPSHOT: &str =
    include_str!("fixtures/comprehension/site-device-detail.snapshot.json");
const DETAIL_DISCOVERY: &str =
    include_str!("fixtures/comprehension/site-device-detail.discovery.json");
const CONNECT_SNAPSHOT: &str = include_str!("fixtures/comprehension/connect-runner.snapshot.json");
const CONNECT_DISCOVERY: &str =
    include_str!("fixtures/comprehension/connect-runner.discovery.json");
const INFERRED: &str = include_str!("fixtures/comprehension/site.inferred-overconfident.json");

const SOURCE_URL: &str = "https://app.qontinui.io";
const OBSERVED_AT: &str = "2026-06-14T00:00:00Z";

fn page(snapshot: &str, discovery: &str) -> PageObservation {
    PageObservation {
        snapshot: serde_json::from_str::<Snapshot>(snapshot).expect("snapshot parses"),
        discovery: serde_json::from_str::<DiscoveryResult>(discovery).expect("discovery parses"),
    }
}

fn pages() -> Vec<PageObservation> {
    vec![
        page(DEVICES_SNAPSHOT, DEVICES_DISCOVERY),
        page(DETAIL_SNAPSHOT, DETAIL_DISCOVERY),
        page(CONNECT_SNAPSHOT, CONNECT_DISCOVERY),
    ]
}

/// The degraded classes for the UNION of what the three pages render:
/// deviceName / status (list), deviceId / lastSeen (detail), the two operations
/// a button or a page load evidences; `state` / `callback` / `owner` are only
/// deduced.
fn evidence_classes() -> BTreeMap<String, EvidenceClass> {
    degraded_evidence_classes(
        &[
            "deviceName".into(),
            "deviceId".into(),
            "status".into(),
            "lastSeen".into(),
        ],
        &["Device".into()],
        &["listDevices".into(), "pairConfirm".into()],
        &[
            ("Device".into(), "state".into()),
            ("Device".into(), "callback".into()),
            ("Device".into(), "owner".into()),
        ],
    )
}

fn comprehend_site() -> FunctionalSpec {
    let (snapshot, discovery) = aggregate_pages(&pages());
    let inferred: FunctionalSpec = serde_json::from_str(INFERRED).expect("inferred fixture parses");
    assert_eq!(
        inferred
            .entities
            .iter()
            .filter(|e| e.name == "Device")
            .count(),
        3,
        "the LLM stand-in names Device once per page"
    );
    let _ = snapshot; // the merged snapshot is the LLM's prompt context (runtime-deferred)
    assemble_spec(
        inferred,
        &discovery,
        &evidence_classes(),
        SOURCE_URL,
        Some(OBSERVED_AT),
    )
}

#[test]
fn aggregated_observation_namespaces_ids_and_unions_states() {
    let (snapshot, discovery) = aggregate_pages(&pages());
    assert_eq!(
        snapshot.page,
        "https://app.qontinui.io/devices+https://app.qontinui.io/devices/dev-7f3a+https://app.qontinui.io/connect-runner"
    );
    assert_eq!(snapshot.elements.len(), 4 + 4 + 4);
    assert_eq!(
        snapshot.elements[0].id,
        "https://app.qontinui.io/devices::devices-heading"
    );
    assert_eq!(snapshot.forms.len(), 1);
    assert_eq!(
        snapshot.forms[0].id,
        "https://app.qontinui.io/connect-runner::pair-confirm-form"
    );
    let ids: BTreeSet<&str> = snapshot.elements.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids.len(), snapshot.elements.len(), "no id collides");

    // pairing-confirm is listed by two pages: once in the union, first
    // occurrence wins, checks unioned by id.
    let state_ids: Vec<&str> = discovery.states.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        state_ids,
        vec![
            "devices-list",
            "device-detail",
            "pairing-confirm",
            "pairing-error"
        ]
    );
    let confirm = discovery
        .states
        .iter()
        .find(|s| s.id == "pairing-confirm")
        .unwrap();
    let checks: Vec<&str> = confirm
        .observable_checks
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(
        checks,
        vec![
            "pairing-confirm-connect-button",
            "pairing-confirm-repair-origin",
            "pairing-confirm-authorization-copy"
        ]
    );
    let t_ids: Vec<&str> = discovery
        .transitions
        .iter()
        .map(|t| t.id.as_str())
        .collect();
    assert_eq!(
        t_ids,
        vec!["open-device", "repair-device", "show-pairing-error"]
    );
}

#[test]
fn three_page_site_comprehends_to_one_device_entity() {
    let spec = comprehend_site();

    let devices: Vec<_> = spec
        .entities
        .iter()
        .filter(|e| e.name == "Device")
        .collect();
    assert_eq!(devices.len(), 1, "one Device entity, not three");
    let device = devices[0];
    assert_eq!(device.confidence, SpecProvenance::Observed);

    let names: BTreeSet<&str> = device.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "deviceName",
            "status",
            "lastSeen",
            "deviceId",
            "owner",
            "state",
            "callback"
        ]
        .into_iter()
        .collect(),
        "fields are the union across pages"
    );
    let field = |n: &str| device.fields.iter().find(|f| f.name == n).unwrap();

    // Rendered on some page → Observed, even where another page only inferred it.
    assert_eq!(field("deviceName").confidence, SpecProvenance::Observed);
    assert_eq!(field("status").confidence, SpecProvenance::Observed);
    assert_eq!(field("deviceId").confidence, SpecProvenance::Observed);
    let last_seen = field("lastSeen");
    assert_eq!(
        last_seen.confidence,
        SpecProvenance::Observed,
        "the detail page renders lastSeen; the list page's Inferred copy loses"
    );
    assert_eq!(
        last_seen.provenance.as_deref(),
        Some("detail page renders a last-seen <time>")
    );
    assert!(last_seen.credibility.is_none());

    // Only deduced anywhere → stays Inferred, credibility ≤ 0.7.
    for n in ["owner", "state", "callback"] {
        let f = field(n);
        assert_eq!(f.confidence, SpecProvenance::Inferred, "{n}");
        assert!(
            f.credibility.unwrap() <= 0.7 + 1e-9,
            "{n} credibility ≤ 0.7"
        );
    }

    // Operations deduplicated by name, the Observed copy winning.
    let ops: Vec<&str> = spec.operations.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(ops, vec!["listDevices", "pairConfirm"]);
    let pair = spec
        .operations
        .iter()
        .find(|o| o.name == "pairConfirm")
        .unwrap();
    assert_eq!(pair.confidence, SpecProvenance::Observed);
    assert_eq!(
        pair.inputs.len(),
        1,
        "the Observed copy (with its input) won"
    );
    assert!(pair
        .provenance
        .as_deref()
        .unwrap()
        .contains("/api/v1/devices/pair-confirm"));
    assert_eq!(
        pair.effect.as_ref().unwrap().confidence,
        SpecProvenance::Assumed
    );

    // Auth is still the degraded explorer's: Inferred ≤ 0.5.
    let auth = spec.auth.as_ref().unwrap();
    assert_eq!(auth.confidence, SpecProvenance::Inferred);
    assert!(auth.credibility.unwrap() <= 0.5 + 1e-9);
}

#[test]
fn ui_states_and_navigation_are_the_distinct_ids_across_pages() {
    let spec = comprehend_site();
    let all_state_ids: Vec<String> = pages()
        .iter()
        .flat_map(|p| p.discovery.states.iter().map(|s| s.id.clone()))
        .collect();
    let distinct: BTreeSet<&String> = all_state_ids.iter().collect();
    assert_eq!(
        all_state_ids.len(),
        5,
        "pairing-confirm is listed by two pages"
    );
    assert_eq!(distinct.len(), 4);
    assert_eq!(spec.ui_states.len(), distinct.len());
    let got: BTreeSet<&str> = spec.ui_states.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(got, distinct.iter().map(|s| s.as_str()).collect());
    for st in &spec.ui_states {
        assert!(
            !st.assertions.is_empty(),
            "state {} carries assertions",
            st.id
        );
    }
    let nav: Vec<&str> = spec.navigation.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(
        nav,
        vec!["open-device", "repair-device", "show-pairing-error"]
    );
}

#[test]
fn site_spec_round_trips_to_full_coverage_with_a_consistent_ledger() {
    let spec = comprehend_site();

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
        "site spec must round-trip to coverage 1.0; got {}",
        verdict.coverage
    );
    assert!(verdict.gaps.is_empty(), "no gaps: {:?}", verdict.gaps);
    assert!((verdict.assumed_fill_rate - 1.0).abs() < 1e-9);
    assert!(verdict.coverage_is_consistent());

    assert_eq!(
        spec.assumptions.len(),
        1,
        "one ledger entry per unique effect"
    );
    assert_eq!(spec.assumptions[0].r#ref, "operations.pairConfirm.effect");
    assert_eq!(collate_assumptions(&spec), spec.assumptions);
}
