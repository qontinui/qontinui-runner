//! The comprehension worker: load inputs → (LLM infer) → assemble IR + clamp →
//! collate ledger → write spec (plan Phase 1.2).
//!
//! The deterministic half lives here:
//! - [`assemble_spec`] — the no-manifest (degraded explorer) assembly: overlay
//!   the IR (`ui_states`/`navigation` from discovery) onto the LLM-inferred
//!   entities/operations/auth, clamp provenance against the known evidence
//!   classes, merge same-named nodes, and collate the assumptions ledger.
//!   Golden-tested.
//! - [`assemble_spec_with_seed`] — the AWAS-first sibling (plan Phase 3, §3d):
//!   the manifest seed goes first, the inferred spec fills the rest and never
//!   downgrades a seeded node, then the same tail runs.
//!
//! The full pipeline (LLM call + live capture + file write) is runtime-deferred
//! and lives outside this crate's tests (`examples/comprehend_live.rs`).

use std::collections::BTreeMap;

use qontinui_types::functional_spec::{FunctionalSpec, SpecTarget};

use crate::aggregate::merge_entities;
use crate::clamp::{clamp_provenance, collate_assumptions, EvidenceClass};
use crate::input::DiscoveryResult;
use crate::mapping::{states_to_ir, transitions_to_ir};

/// Assemble the final `FunctionalSpec` deterministically from:
/// - `inferred`: the entities/operations/auth the LLM proposed (its IR and
///   ledger are ignored/overwritten — the deterministic substrate owns those),
/// - `discovery`: the clustered states/transitions → IR,
/// - `evidence_classes`: the known per-node evidence class → clamp bound,
/// - `source_url` / `observed_at`: the spec target stamp.
///
/// Steps: clamp the LLM spec → merge same-named nodes → replace its IR with the
/// discovery-derived IR → collate the ledger from the (clamped) `Assumed`
/// nodes. The result is the honest, well-formed spec. This is pure (no clock,
/// no I/O, no LLM). It is the **no-manifest** path; a site that serves an AWAS
/// manifest goes through [`assemble_spec_with_seed`].
pub fn assemble_spec(
    inferred: FunctionalSpec,
    discovery: &DiscoveryResult,
    evidence_classes: &BTreeMap<String, EvidenceClass>,
    source_url: &str,
    observed_at: Option<&str>,
) -> FunctionalSpec {
    finish(
        inferred,
        discovery,
        evidence_classes,
        source_url,
        observed_at,
    )
}

/// The AWAS-first assembly (plan Phase 3.3, §3d): AWAS and the explorer are two
/// evidence sources merged into ONE spec, AWAS winning on the nodes it declares.
///
/// - Start from `seed` (from `awas_seed::awas_to_spec_seed`).
/// - For each `inferred` entity whose name the seed carries, **keep the seed
///   node** and union in the fields / relationships the seed lacks, at the
///   inferred copy's own claimed provenance (which the clamp then bounds via
///   `degraded_classes`). For each inferred operation the seed carries, keep
///   the seed node and union in inputs the seed lacks (by field) — additive
///   only, nothing seeded is ever replaced or downgraded.
/// - Append every other inferred entity / operation.
/// - Merge the class maps, the seed's entries winning on collision, so the
///   declared set stays pinned (`EvidenceClass::is_pinned`).
/// - Take `auth` from the seed when it has one, else from `inferred`.
/// - Then the shared tail: clamp → merge → discovery-IR overlay → stamp →
///   collate, exactly as [`assemble_spec`].
///
/// Pure: no clock, no I/O, no LLM.
pub fn assemble_spec_with_seed(
    seed: FunctionalSpec,
    seed_classes: &BTreeMap<String, EvidenceClass>,
    inferred: FunctionalSpec,
    discovery: &DiscoveryResult,
    degraded_classes: &BTreeMap<String, EvidenceClass>,
    source_url: &str,
    observed_at: Option<&str>,
) -> FunctionalSpec {
    let mut spec = seed;

    for entity in inferred.entities {
        match spec.entities.iter_mut().find(|e| e.name == entity.name) {
            Some(seeded) => {
                for field in entity.fields {
                    if !seeded.fields.iter().any(|f| f.name == field.name) {
                        seeded.fields.push(field);
                    }
                }
                for rel in entity.relationships {
                    if !seeded
                        .relationships
                        .iter()
                        .any(|r| r.to == rel.to && r.kind == rel.kind)
                    {
                        seeded.relationships.push(rel);
                    }
                }
            }
            None => spec.entities.push(entity),
        }
    }

    for op in inferred.operations {
        match spec.operations.iter_mut().find(|o| o.name == op.name) {
            Some(seeded) => {
                for input in op.inputs {
                    if !seeded.inputs.iter().any(|i| i.field == input.field) {
                        seeded.inputs.push(input);
                    }
                }
            }
            None => spec.operations.push(op),
        }
    }

    if spec.auth.is_none() {
        spec.auth = inferred.auth;
    }

    let mut classes = degraded_classes.clone();
    for (key, class) in seed_classes {
        classes.insert(key.clone(), *class);
    }

    finish(spec, discovery, &classes, source_url, observed_at)
}

/// The shared assembly tail: clamp → merge same-named nodes → the deterministic
/// substrate's IR replaces whatever the LLM emitted → stamp the target →
/// collate the ledger mechanically so it can never disagree with the body.
fn finish(
    spec: FunctionalSpec,
    discovery: &DiscoveryResult,
    evidence_classes: &BTreeMap<String, EvidenceClass>,
    source_url: &str,
    observed_at: Option<&str>,
) -> FunctionalSpec {
    // 1. Clamp the entities/operations/auth to honest provenance.
    let spec = clamp_provenance(spec, evidence_classes);

    // 2. Collapse same-named entities / operations (highest evidence wins) so
    //    every ledger ref is unique. A no-op on an already-unique spec.
    let mut spec = merge_entities(spec);

    // 3. The deterministic substrate OWNS the IR — replace whatever the LLM
    //    emitted with the discovery-derived states/transitions (with populated
    //    assertions + discovery provenance).
    spec.ui_states = states_to_ir(discovery);
    spec.navigation = transitions_to_ir(discovery);

    // 4. Stamp the target.
    spec.spec_version = "0".into();
    spec.target = SpecTarget {
        source_url: source_url.into(),
        observed_at: observed_at.map(str::to_string),
    };

    // 5. Collate the assumptions ledger mechanically from the clamped spec so it
    //    can never disagree with the spec body.
    spec.assumptions = collate_assumptions(&spec);

    spec
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_types::functional_spec::SpecProvenance;

    #[test]
    fn assemble_overwrites_llm_ir_with_discovery_ir() {
        // Even if the LLM hallucinated a ui_state, the deterministic IR wins.
        let inferred = FunctionalSpec {
            spec_version: "0".into(),
            target: SpecTarget {
                source_url: "x".into(),
                observed_at: None,
            },
            entities: vec![],
            operations: vec![],
            ui_states: vec![qontinui_types::ir::IrState {
                id: "hallucinated".into(),
                ..Default::default()
            }],
            navigation: vec![],
            auth: None,
            assumptions: vec![],
        };
        let discovery = DiscoveryResult::default();
        let spec = assemble_spec(inferred, &discovery, &BTreeMap::new(), "u", None);
        assert!(
            spec.ui_states.is_empty(),
            "LLM-hallucinated ui_state must be replaced by the (empty) discovery IR"
        );
    }

    #[test]
    fn assemble_forces_effect_assumed_and_collates_ledger() {
        use qontinui_types::functional_spec::{Operation, OperationEffect};
        let inferred = FunctionalSpec {
            spec_version: "0".into(),
            target: SpecTarget {
                source_url: "x".into(),
                observed_at: None,
            },
            entities: vec![],
            operations: vec![Operation {
                name: "pairConfirm".into(),
                verb: "create".into(),
                entity: Some("Device".into()),
                inputs: vec![],
                effect: Some(OperationEffect {
                    // LLM over-claims the effect as Observed.
                    confidence: SpecProvenance::Observed,
                    assumption: Some("persists pairing".into()),
                    provenance: None,
                    credibility: Some(0.9),
                }),
                confidence: SpecProvenance::Observed,
                provenance: None,
                credibility: None,
            }],
            ui_states: vec![],
            navigation: vec![],
            auth: None,
            assumptions: vec![],
        };
        let spec = assemble_spec(
            inferred,
            &DiscoveryResult::default(),
            &BTreeMap::new(),
            "u",
            None,
        );
        let eff = spec.operations[0].effect.as_ref().unwrap();
        assert_eq!(
            eff.confidence,
            SpecProvenance::Assumed,
            "effect forced Assumed"
        );
        assert!(eff.credibility.is_none());
        assert_eq!(spec.assumptions.len(), 1);
        assert_eq!(spec.assumptions[0].r#ref, "operations.pairConfirm.effect");
    }
}
