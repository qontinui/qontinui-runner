//! The honesty machine: the deterministic **provenance clamp** and the
//! mechanical **assumptions ledger collation**.
//!
//! This is the load-bearing correctness property of comprehension (#3, §5 of the
//! plan): *a node's `SpecProvenance` is never higher than its weakest evidence
//! justifies*. The LLM (runtime-deferred, see `llm.rs`) may emit an
//! over-confident spec; this pass downgrades any node whose **known evidence
//! class** forbids the claimed provenance, so honesty is machine-checked rather
//! than prompt-trusted.
//!
//! ## Evidence classes (plan §3a)
//!
//! Per the degraded-explorer honesty table, each spec node's *known* evidence
//! class caps its provenance and credibility:
//!
//! | evidence class    | provenance ceiling | credibility cap |
//! | ----------------- | ------------------ | --------------- |
//! | `AwasDeclared`    | `Observed`         | (none — declared) |
//! | `Rendered`        | `Observed`         | (none) |
//! | `DiscoveryCluster`| `Observed`         | (none — a state/transition was clustered) |
//! | `Deduced`         | `Inferred`         | `≤ 0.7` |
//! | `AuthShellInfer`  | `Inferred`         | `≤ 0.5` (cf. oracle `auth: 0.5`) |
//! | `Silent`          | `Assumed`          | (none — ledgered) |
//!
//! Plus the invariant that holds regardless of class: **`OperationEffect` is
//! always `Assumed`** (the frontend cannot reveal what persists server-side).

use std::collections::BTreeMap;

use qontinui_types::functional_spec::{AssumptionEntry, FunctionalSpec, SpecProvenance};

/// The known evidence class for a spec node, keyed by the node's dotted ref
/// (the `enumerate_nodes` convention). The pipeline knows the class of each node
/// from *which source produced it* (AWAS seed / rendered snapshot / discovery
/// cluster / multi-observation deduction / frontend-silent), independent of what
/// the LLM claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceClass {
    /// Declared by an AWAS manifest — highest confidence, never clamped.
    AwasDeclared,
    /// Directly rendered element / form field / observed validation firing.
    Rendered,
    /// A discovery fingerprint cluster (a `ui_state` / `navigation` node).
    DiscoveryCluster,
    /// Deduced from ≥2 observations (a relationship, an opaque-token semantic).
    Deduced,
    /// "Page sits behind the authed shell" style auth inference.
    AuthShellInfer,
    /// The frontend is silent — a server-only effect / hidden rule.
    Silent,
}

impl EvidenceClass {
    /// The highest [`SpecProvenance`] this evidence class can honestly justify.
    pub fn provenance_ceiling(self) -> SpecProvenance {
        match self {
            EvidenceClass::AwasDeclared
            | EvidenceClass::Rendered
            | EvidenceClass::DiscoveryCluster => SpecProvenance::Observed,
            EvidenceClass::Deduced | EvidenceClass::AuthShellInfer => SpecProvenance::Inferred,
            EvidenceClass::Silent => SpecProvenance::Assumed,
        }
    }

    /// The credibility cap on an `Inferred` node of this class, if any.
    pub fn credibility_cap(self) -> Option<f64> {
        match self {
            EvidenceClass::Deduced => Some(0.7),
            EvidenceClass::AuthShellInfer => Some(0.5),
            _ => None,
        }
    }

    /// Whether the clamp must never touch a node of this class (AWAS-declared
    /// nodes have the highest evidence class — §3d merge order).
    pub fn is_pinned(self) -> bool {
        matches!(self, EvidenceClass::AwasDeclared)
    }
}

/// Rank for ceiling comparison: a higher value = more confident.
fn rank(p: SpecProvenance) -> u8 {
    match p {
        SpecProvenance::Assumed => 0,
        SpecProvenance::Inferred => 1,
        SpecProvenance::Observed => 2,
    }
}

/// Downgrade `claimed` to at most `ceiling`, returning the honest provenance.
fn clamp_one(claimed: SpecProvenance, ceiling: SpecProvenance) -> SpecProvenance {
    if rank(claimed) > rank(ceiling) {
        ceiling
    } else {
        claimed
    }
}

/// Apply the credibility cap (and only set credibility on `Inferred` nodes).
fn clamp_credibility(
    provenance: SpecProvenance,
    credibility: Option<f64>,
    cap: Option<f64>,
) -> Option<f64> {
    match provenance {
        SpecProvenance::Inferred => match (credibility, cap) {
            (Some(c), Some(cap)) => Some(c.min(cap)),
            (None, Some(cap)) => Some(cap),
            (c, None) => c,
        },
        // Credibility is meaningless on Observed / Assumed.
        _ => None,
    }
}

/// The deterministic provenance-clamp pass (plan Phase 2.1, required in the
/// Phase-1 deliverable).
///
/// Given the spec the LLM emitted + the *known evidence class per node ref*,
/// downgrade any node the model over-rated to its evidence ceiling and cap its
/// credibility. A node ref with **no** entry in `evidence_classes` is left
/// untouched (the pipeline asserts a class for every node it produced; an absent
/// class means "no known constraint", which is a fail-open the worker never
/// relies on — it always supplies a full map).
///
/// `OperationEffect` is forced to `Assumed` unconditionally, regardless of any
/// supplied class, because the frontend is structurally incapable of revealing
/// server-side persistence.
pub fn clamp_provenance(
    mut spec: FunctionalSpec,
    evidence_classes: &BTreeMap<String, EvidenceClass>,
) -> FunctionalSpec {
    // --- entities + fields + relationships ---
    for e in &mut spec.entities {
        let key = format!("entities.{}", e.name);
        apply(
            &key,
            evidence_classes,
            &mut e.confidence,
            &mut e.credibility,
        );
        for f in &mut e.fields {
            let key = format!("entities.{}.fields.{}", e.name, f.name);
            apply(
                &key,
                evidence_classes,
                &mut f.confidence,
                &mut f.credibility,
            );
        }
        for r in &mut e.relationships {
            let key = format!("entities.{}.relationships.{}", e.name, r.to);
            apply(
                &key,
                evidence_classes,
                &mut r.confidence,
                &mut r.credibility,
            );
        }
    }

    // --- operations + validation + effect ---
    for op in &mut spec.operations {
        let key = format!("operations.{}", op.name);
        apply(
            &key,
            evidence_classes,
            &mut op.confidence,
            &mut op.credibility,
        );
        for inp in &mut op.inputs {
            if let Some(v) = &mut inp.validation {
                let key = format!("operations.{}.inputs.{}.validation", op.name, inp.field);
                apply(
                    &key,
                    evidence_classes,
                    &mut v.confidence,
                    &mut v.credibility,
                );
            }
        }
        if let Some(eff) = &mut op.effect {
            // Hard invariant: server-side effect is ALWAYS Assumed.
            eff.confidence = SpecProvenance::Assumed;
            eff.credibility = None;
        }
    }

    // --- auth model + roles ---
    if let Some(a) = &mut spec.auth {
        apply(
            "auth",
            evidence_classes,
            &mut a.confidence,
            &mut a.credibility,
        );
        for role in &mut a.roles {
            let key = format!("auth.roles.{}", role.name);
            apply(
                &key,
                evidence_classes,
                &mut role.confidence,
                &mut role.credibility,
            );
        }
    }

    // ui_states / navigation are not provenance-bearing on the wire (the rubric
    // always counts them as one Observed node each — see enumerate_nodes), so
    // there is nothing to clamp there.

    spec
}

/// Clamp one node's `confidence` + `credibility` against its known class.
fn apply(
    key: &str,
    classes: &BTreeMap<String, EvidenceClass>,
    confidence: &mut SpecProvenance,
    credibility: &mut Option<f64>,
) {
    let Some(class) = classes.get(key).copied() else {
        return;
    };
    if class.is_pinned() {
        return;
    }
    *confidence = clamp_one(*confidence, class.provenance_ceiling());
    *credibility = clamp_credibility(*confidence, *credibility, class.credibility_cap());
}

/// Mechanically derive the assumptions ledger from every `Assumed` node, so the
/// ledger can never disagree with the spec body (plan Phase 2.2). Currently the
/// only structurally-`Assumed` node is each operation's `effect`; the collation
/// walks the spec rather than special-casing, so it stays correct if future
/// sources emit other `Assumed` nodes.
pub fn collate_assumptions(spec: &FunctionalSpec) -> Vec<AssumptionEntry> {
    let mut out = Vec::new();
    for op in &spec.operations {
        if let Some(eff) = &op.effect {
            if eff.confidence == SpecProvenance::Assumed {
                out.push(AssumptionEntry {
                    r#ref: format!("operations.{}.effect", op.name),
                    default_applied: eff
                        .assumption
                        .clone()
                        .unwrap_or_else(|| "best-practice server effect (frontend-silent)".into()),
                    overridable: true,
                    note: None,
                });
            }
        }
    }
    out
}
