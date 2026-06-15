//! The comprehension worker: load inputs → (LLM infer) → assemble IR + clamp →
//! collate ledger → write spec (plan Phase 1.2).
//!
//! Phase 1 splits this into:
//! - [`assemble_spec`] — the **deterministic** assembly: overlay the IR
//!   (`ui_states`/`navigation` from discovery) onto the LLM-inferred
//!   entities/operations/auth, clamp provenance against the known evidence
//!   classes, and collate the assumptions ledger. Golden-tested.
//! - [`comprehend`] — the full pipeline including the LLM call + file write,
//!   runtime-deferred (the LLM and live capture are not faked).

use std::collections::BTreeMap;

use qontinui_types::functional_spec::{FunctionalSpec, SpecTarget};

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
/// Steps: clamp the LLM spec → replace its IR with the discovery-derived IR →
/// collate the ledger from the (clamped) `Assumed` nodes. The result is the
/// honest, well-formed spec. This is pure (no clock, no I/O, no LLM).
pub fn assemble_spec(
    inferred: FunctionalSpec,
    discovery: &DiscoveryResult,
    evidence_classes: &BTreeMap<String, EvidenceClass>,
    source_url: &str,
    observed_at: Option<&str>,
) -> FunctionalSpec {
    // 1. Clamp the LLM-inferred entities/operations/auth to honest provenance.
    let mut spec = clamp_provenance(inferred, evidence_classes);

    // 2. The deterministic substrate OWNS the IR — replace whatever the LLM
    //    emitted with the discovery-derived states/transitions (with populated
    //    assertions + discovery provenance).
    spec.ui_states = states_to_ir(discovery);
    spec.navigation = transitions_to_ir(discovery);

    // 3. Stamp the target.
    spec.spec_version = "0".into();
    spec.target = SpecTarget {
        source_url: source_url.into(),
        observed_at: observed_at.map(str::to_string),
    };

    // 4. Collate the assumptions ledger mechanically from the clamped spec so it
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
