//! Phase 2 anti-overpromise proof for the provenance clamp in isolation.
//!
//! Feeds a deliberately over-confident LLM output (every node `Observed`) and
//! asserts the clamp downgrades EXACTLY the nodes whose evidence class forbids
//! `Observed`, caps credibility per the §3a table, forces `OperationEffect` to
//! `Assumed`, and that `collate_assumptions` reproduces the ledger.

use std::collections::BTreeMap;

use qontinui_comprehension::clamp::{clamp_provenance, collate_assumptions, EvidenceClass};
use qontinui_types::functional_spec::{
    AuthModel, Entity, EntityField, FunctionalSpec, Operation, OperationEffect, SpecProvenance,
    SpecTarget,
};

fn over_confident_spec() -> FunctionalSpec {
    FunctionalSpec {
        spec_version: "0".into(),
        target: SpecTarget {
            source_url: "https://x.test".into(),
            observed_at: None,
        },
        entities: vec![Entity {
            name: "Device".into(),
            fields: vec![
                EntityField {
                    name: "deviceName".into(),
                    field_type: "string".into(),
                    values: vec![],
                    confidence: SpecProvenance::Observed,
                    provenance: None,
                    credibility: None,
                },
                EntityField {
                    name: "state".into(),
                    field_type: "string".into(),
                    values: vec![],
                    // LLM over-claims a deduced token as Observed with high cred.
                    confidence: SpecProvenance::Observed,
                    provenance: None,
                    credibility: Some(0.95),
                },
            ],
            relationships: vec![],
            confidence: SpecProvenance::Observed,
            provenance: None,
            credibility: None,
        }],
        operations: vec![Operation {
            name: "pairConfirm".into(),
            verb: "create".into(),
            entity: Some("Device".into()),
            inputs: vec![],
            effect: Some(OperationEffect {
                confidence: SpecProvenance::Observed,
                assumption: Some("persists pairing".into()),
                provenance: None,
                credibility: Some(0.99),
            }),
            confidence: SpecProvenance::Observed,
            provenance: None,
            credibility: None,
        }],
        ui_states: vec![],
        navigation: vec![],
        auth: Some(AuthModel {
            model: "session".into(),
            confidence: SpecProvenance::Observed,
            roles: vec![],
            provenance: None,
            credibility: Some(0.95),
        }),
        assumptions: vec![],
    }
}

fn classes() -> BTreeMap<String, EvidenceClass> {
    let mut m = BTreeMap::new();
    m.insert("entities.Device".into(), EvidenceClass::Rendered);
    m.insert(
        "entities.Device.fields.deviceName".into(),
        EvidenceClass::Rendered,
    );
    m.insert(
        "entities.Device.fields.state".into(),
        EvidenceClass::Deduced,
    );
    m.insert("operations.pairConfirm".into(), EvidenceClass::Rendered);
    m.insert("auth".into(), EvidenceClass::AuthShellInfer);
    m
}

#[test]
fn clamp_downgrades_exactly_the_forbidden_nodes() {
    let spec = clamp_provenance(over_confident_spec(), &classes());
    let device = &spec.entities[0];

    // Rendered entity + field stay Observed.
    assert_eq!(device.confidence, SpecProvenance::Observed);
    let dn = device
        .fields
        .iter()
        .find(|f| f.name == "deviceName")
        .unwrap();
    assert_eq!(dn.confidence, SpecProvenance::Observed);
    assert!(dn.credibility.is_none(), "Observed carries no credibility");

    // Deduced field clamped to Inferred, credibility capped at 0.7.
    let st = device.fields.iter().find(|f| f.name == "state").unwrap();
    assert_eq!(st.confidence, SpecProvenance::Inferred);
    assert_eq!(st.credibility, Some(0.7), "0.95 capped to 0.7");

    // Operation existence stays Observed; effect forced Assumed.
    let op = &spec.operations[0];
    assert_eq!(op.confidence, SpecProvenance::Observed);
    let eff = op.effect.as_ref().unwrap();
    assert_eq!(eff.confidence, SpecProvenance::Assumed);
    assert!(eff.credibility.is_none());

    // Auth clamped to Inferred, credibility capped at 0.5.
    let auth = spec.auth.as_ref().unwrap();
    assert_eq!(auth.confidence, SpecProvenance::Inferred);
    assert_eq!(auth.credibility, Some(0.5), "0.95 capped to 0.5");
}

#[test]
fn collate_assumptions_reproduces_the_ledger_from_assumed_nodes() {
    let spec = clamp_provenance(over_confident_spec(), &classes());
    let ledger = collate_assumptions(&spec);
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].r#ref, "operations.pairConfirm.effect");
    assert_eq!(ledger[0].default_applied, "persists pairing");
    assert!(ledger[0].overridable);
}

#[test]
fn awas_declared_nodes_are_pinned_against_downgrade() {
    // An AWAS-declared node must never be clamped even if its claimed provenance
    // is Observed and another evidence source would cap it lower (§3d).
    let mut m = BTreeMap::new();
    m.insert(
        "entities.Device.fields.state".into(),
        EvidenceClass::AwasDeclared,
    );
    // Mark the field Observed with a high credibility; AwasDeclared keeps it.
    let clamped = clamp_provenance(over_confident_spec(), &m);
    let st = clamped.entities[0]
        .fields
        .iter()
        .find(|f| f.name == "state")
        .unwrap();
    assert_eq!(
        st.confidence,
        SpecProvenance::Observed,
        "AWAS-declared node is pinned Observed"
    );
    assert_eq!(
        st.credibility,
        Some(0.95),
        "pinned node credibility untouched"
    );
}
