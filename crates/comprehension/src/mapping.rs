//! Deterministic snapshot/discovery → IR + spec-seed mapping (plan Phase 1.2d /
//! Phase 2). This is the **golden-testable** half of comprehension: no LLM, no
//! clock, no I/O — a pure function from the input fixtures to (a) the IR
//! (`ui_states` / `navigation`, with **populated `IrAssertion`s** — the #3-owns-
//! assertions seam) and (b) the known **evidence-class map** the clamp consumes.
//!
//! The LLM's job (runtime-deferred) is to infer the `entities` / `operations` /
//! `auth` *content* (names, fields, verbs) from the same snapshot; this mapping
//! owns the parts that must be deterministic to keep honesty machine-checkable:
//! the IR nodes (always `Observed` per the rubric) and the per-node evidence
//! class that bounds what the LLM is *allowed* to claim.

use std::collections::BTreeMap;

use qontinui_types::ir::{
    IrAssertion, IrAssertionTarget, IrProvenance, IrState, IrTransition, IrTransitionAction,
};

use crate::clamp::EvidenceClass;
use crate::input::{DiscoveredState, DiscoveryResult, ObservableCheck};

/// The real discovery module path the IR provenance must cite (plan: "the
/// `provenance.file` the oracle must cite is the real module path").
pub const DISCOVERY_FILE: &str = "discovery/state_discovery/strategies/fingerprint.py";

/// Map a discovery result's clustered states → `IrState`s, each carrying
/// non-empty `assertions` derived from its observable checks (the
/// `IrState.assertions` ownership the cross-plan seam assigns to #3) and a
/// `discovery` `IrProvenance` citing the fingerprint strategy module.
pub fn states_to_ir(discovery: &DiscoveryResult) -> Vec<IrState> {
    discovery.states.iter().map(state_to_ir).collect()
}

fn state_to_ir(s: &DiscoveredState) -> IrState {
    IrState {
        id: s.id.clone(),
        name: s.name.clone(),
        description: s.description.clone(),
        assertions: s.observable_checks.iter().map(check_to_assertion).collect(),
        is_initial: if s.is_initial { Some(true) } else { None },
        provenance: Some(IrProvenance {
            source: "discovery".into(),
            file: Some(DISCOVERY_FILE.into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn check_to_assertion(c: &ObservableCheck) -> IrAssertion {
    IrAssertion {
        id: c.id.clone(),
        description: c.description.clone(),
        category: c.category.clone(),
        severity: c.severity.clone(),
        assertion_type: "exists".into(),
        target: IrAssertionTarget {
            kind: "search".into(),
            criteria: c.criteria.clone(),
            label: c.label.clone(),
        },
        source: "discovery".into(),
        reviewed: false,
        enabled: true,
        precondition: None,
    }
}

/// Map a discovery result's clustered transitions → `IrTransition`s.
pub fn transitions_to_ir(discovery: &DiscoveryResult) -> Vec<IrTransition> {
    discovery
        .transitions
        .iter()
        .map(|t| IrTransition {
            id: t.id.clone(),
            name: t.name.clone(),
            from_states: t.from_state_ids.clone(),
            activate_states: t.to_state_ids.clone(),
            actions: vec![IrTransitionAction {
                kind: if t.action_type.is_empty() {
                    "click".into()
                } else {
                    t.action_type.clone()
                },
                target: serde_json::from_value(t.trigger.clone()).unwrap_or_default(),
                params: None,
                wait_after: None,
            }],
            ..Default::default()
        })
        .collect()
}

/// Build the known evidence-class map for the **degraded explorer path** (no
/// AWAS manifest), keyed by the `enumerate_nodes` dotted ref. This encodes the
/// honesty contract for connect-runner directly from what each source can
/// justify — it is the bound the clamp enforces against whatever the LLM claims.
///
/// The classes are derived structurally from the inputs, not hand-listed per
/// page:
/// - an entity field whose name appears as a **rendered element text** or a
///   **form field** → [`EvidenceClass::Rendered`]; otherwise → `Deduced`
///   (opaque token / relationship the LLM had to deduce).
/// - the entity itself → `Rendered` when the page renders it.
/// - an operation whose name matches a **button/action** → `Rendered`
///   existence; its `effect` is forced `Assumed` by the clamp regardless.
/// - `auth` → [`EvidenceClass::AuthShellInfer`] (the page sits behind the shell).
pub fn degraded_evidence_classes(
    rendered_field_names: &[String],
    rendered_entity_names: &[String],
    observed_operation_names: &[String],
    deduced_field_names: &[(String, String)], // (entity, field)
) -> BTreeMap<String, EvidenceClass> {
    let mut m = BTreeMap::new();

    for entity in rendered_entity_names {
        m.insert(format!("entities.{entity}"), EvidenceClass::Rendered);
        for field in rendered_field_names {
            m.insert(
                format!("entities.{entity}.fields.{field}"),
                EvidenceClass::Rendered,
            );
        }
    }
    for (entity, field) in deduced_field_names {
        m.insert(
            format!("entities.{entity}.fields.{field}"),
            EvidenceClass::Deduced,
        );
    }
    for op in observed_operation_names {
        m.insert(format!("operations.{op}"), EvidenceClass::Rendered);
        // The effect node is forced Assumed by the clamp; mark it Silent for
        // completeness so collate/clamp agree on its class.
        m.insert(format!("operations.{op}.effect"), EvidenceClass::Silent);
    }
    m.insert("auth".into(), EvidenceClass::AuthShellInfer);

    m
}
