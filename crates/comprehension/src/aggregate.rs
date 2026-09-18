//! Multi-page aggregation (plan Phase 4): fold N per-page observations into
//! ONE merged observation, and collapse same-named entities / operations so
//! every `enumerate_nodes` ref stays unique.
//!
//! The delivered `Snapshot` is single-page (`page: String`); the site-level
//! input is a list of [`PageObservation`]s. [`aggregate_pages`] produces the
//! one `(Snapshot, DiscoveryResult)` pair the existing `assemble_spec` /
//! `assemble_spec_with_seed` run over — so the LLM is invoked **once per
//! site** over the aggregated context, not once per page followed by an
//! N-way reconcile of entity names the model chose independently each time.
//!
//! [`merge_entities`] is the post-pass that makes the merged spec coherent:
//! entities with the same `name` collapse to one, fields unioned by name with
//! the **highest-evidence copy winning** (`Observed` > `Inferred` > `Assumed`;
//! ties keep the first), relationships unioned by `(to, kind)`, operations
//! deduplicated by `name` the same way. It runs inside the assembly tail
//! (after the clamp, before ledger collation) so the ledger can never carry a
//! ref twice; it is a no-op on a spec whose names are already unique.
//!
//! Everything here is pure, deterministic and order-preserving — like the rest
//! of the crate's substrate, it is golden-testable with no clock and no I/O.
//! The live capture that would *drive* UI Bridge across routes remains
//! runtime-deferred, exactly as Phase 1's single-page capture is.

use qontinui_types::functional_spec::{Entity, FunctionalSpec, Operation};

use crate::clamp::rank;
use crate::input::{DiscoveredState, DiscoveryResult, Snapshot};

/// One page's observation: its UI Bridge snapshot plus the discovery result
/// clustered from it.
#[derive(Debug, Clone, Default)]
pub struct PageObservation {
    pub snapshot: Snapshot,
    pub discovery: DiscoveryResult,
}

/// Merge the pages into one observation:
/// - snapshot `elements` / `forms` are concatenated in page order, each `id`
///   re-namespaced as `<page>::<id>` (an id is left alone when the page's
///   `page` is empty) so nothing collides;
/// - the merged `page` is the non-empty page values joined with `+`;
/// - discovery `states` are unioned by `id` (first occurrence wins; a repeated
///   state contributes any `observable_checks` whose `id` the first lacks,
///   appended in order) and `transitions` by `id` (first wins).
pub fn aggregate_pages(pages: &[PageObservation]) -> (Snapshot, DiscoveryResult) {
    let mut snapshot = Snapshot::default();
    let mut discovery = DiscoveryResult::default();
    let mut page_names: Vec<&str> = Vec::new();

    for page in pages {
        let ns = page.snapshot.page.as_str();
        if !ns.is_empty() {
            page_names.push(ns);
        }
        for el in &page.snapshot.elements {
            let mut el = el.clone();
            el.id = namespaced(ns, &el.id);
            snapshot.elements.push(el);
        }
        for form in &page.snapshot.forms {
            let mut form = form.clone();
            form.id = namespaced(ns, &form.id);
            snapshot.forms.push(form);
        }
        for state in &page.discovery.states {
            union_state(&mut discovery.states, state);
        }
        for t in &page.discovery.transitions {
            if !discovery.transitions.iter().any(|x| x.id == t.id) {
                discovery.transitions.push(t.clone());
            }
        }
    }

    snapshot.page = page_names.join("+");
    (snapshot, discovery)
}

fn namespaced(page: &str, id: &str) -> String {
    if page.is_empty() {
        id.to_string()
    } else {
        format!("{page}::{id}")
    }
}

fn union_state(states: &mut Vec<DiscoveredState>, incoming: &DiscoveredState) {
    match states.iter_mut().find(|s| s.id == incoming.id) {
        Some(existing) => {
            for check in &incoming.observable_checks {
                if !existing.observable_checks.iter().any(|c| c.id == check.id) {
                    existing.observable_checks.push(check.clone());
                }
            }
        }
        None => states.push(incoming.clone()),
    }
}

/// Collapse same-named entities and operations (see the module docs). Node
/// refs are unique afterwards, so `enumerate_nodes` and the ledger agree.
pub fn merge_entities(mut spec: FunctionalSpec) -> FunctionalSpec {
    let mut entities: Vec<Entity> = Vec::with_capacity(spec.entities.len());
    for incoming in spec.entities.drain(..) {
        match entities.iter_mut().find(|e| e.name == incoming.name) {
            Some(target) => merge_into_entity(target, incoming),
            None => entities.push(incoming),
        }
    }
    spec.entities = entities;

    let mut operations: Vec<Operation> = Vec::with_capacity(spec.operations.len());
    for incoming in spec.operations.drain(..) {
        match operations.iter_mut().find(|o| o.name == incoming.name) {
            Some(target) => {
                if rank(incoming.confidence) > rank(target.confidence) {
                    *target = incoming;
                }
            }
            None => operations.push(incoming),
        }
    }
    spec.operations = operations;

    spec
}

fn merge_into_entity(target: &mut Entity, incoming: Entity) {
    // Entity-level node: the higher-evidence copy's provenance wins.
    if rank(incoming.confidence) > rank(target.confidence) {
        target.confidence = incoming.confidence;
        target.provenance = incoming.provenance;
        target.credibility = incoming.credibility;
    }
    for field in incoming.fields {
        match target.fields.iter_mut().find(|f| f.name == field.name) {
            Some(existing) => {
                if rank(field.confidence) > rank(existing.confidence) {
                    *existing = field;
                }
            }
            None => target.fields.push(field),
        }
    }
    for rel in incoming.relationships {
        match target
            .relationships
            .iter_mut()
            .find(|r| r.to == rel.to && r.kind == rel.kind)
        {
            Some(existing) => {
                if rank(rel.confidence) > rank(existing.confidence) {
                    *existing = rel;
                }
            }
            None => target.relationships.push(rel),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{DiscoveredTransition, ObservableCheck, SnapshotElement};
    use qontinui_types::functional_spec::{EntityField, Relationship, SpecProvenance, SpecTarget};

    fn page(name: &str, state_ids: &[&str], transition_ids: &[&str]) -> PageObservation {
        PageObservation {
            snapshot: Snapshot {
                page: name.into(),
                elements: vec![SnapshotElement {
                    id: "el".into(),
                    ..Default::default()
                }],
                forms: vec![],
            },
            discovery: DiscoveryResult {
                states: state_ids
                    .iter()
                    .map(|id| DiscoveredState {
                        id: (*id).into(),
                        observable_checks: vec![ObservableCheck {
                            id: format!("{id}-check-{name}"),
                            ..Default::default()
                        }],
                        ..Default::default()
                    })
                    .collect(),
                transitions: transition_ids
                    .iter()
                    .map(|id| DiscoveredTransition {
                        id: (*id).into(),
                        ..Default::default()
                    })
                    .collect(),
            },
        }
    }

    #[test]
    fn aggregate_namespaces_ids_and_unions_states_by_id() {
        let pages = vec![
            page("a", &["s1"], &["t1"]),
            page("b", &["s1", "s2"], &["t1", "t2"]),
            page("", &["s3"], &[]),
        ];
        let (snap, disc) = aggregate_pages(&pages);
        assert_eq!(snap.page, "a+b");
        let ids: Vec<&str> = snap.elements.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["a::el", "b::el", "el"]);
        let state_ids: Vec<&str> = disc.states.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(state_ids, vec!["s1", "s2", "s3"]);
        // s1 appears on two pages: first wins, checks unioned by id in order.
        let s1 = &disc.states[0];
        let checks: Vec<&str> = s1.observable_checks.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(checks, vec!["s1-check-a", "s1-check-b"]);
        let t_ids: Vec<&str> = disc.transitions.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(t_ids, vec!["t1", "t2"]);
    }

    fn field(name: &str, confidence: SpecProvenance, cred: Option<f64>) -> EntityField {
        EntityField {
            name: name.into(),
            field_type: "string".into(),
            values: vec![],
            confidence,
            provenance: Some(format!("{name}@{confidence:?}")),
            credibility: cred,
        }
    }

    #[test]
    fn merge_entities_highest_evidence_wins_ties_keep_first() {
        let spec = FunctionalSpec {
            spec_version: "0".into(),
            target: SpecTarget {
                source_url: "x".into(),
                observed_at: None,
            },
            entities: vec![
                Entity {
                    name: "Device".into(),
                    fields: vec![
                        field("name", SpecProvenance::Observed, None),
                        field("lastSeen", SpecProvenance::Inferred, Some(0.5)),
                    ],
                    relationships: vec![Relationship {
                        to: "Owner".into(),
                        kind: "many-to-one".into(),
                        confidence: SpecProvenance::Inferred,
                        provenance: Some("first".into()),
                        credibility: Some(0.4),
                    }],
                    confidence: SpecProvenance::Inferred,
                    provenance: Some("first entity".into()),
                    credibility: Some(0.6),
                },
                Entity {
                    name: "Device".into(),
                    fields: vec![
                        field("name", SpecProvenance::Observed, None),
                        field("lastSeen", SpecProvenance::Observed, None),
                        field("id", SpecProvenance::Inferred, Some(0.3)),
                    ],
                    relationships: vec![Relationship {
                        to: "Owner".into(),
                        kind: "many-to-one".into(),
                        confidence: SpecProvenance::Inferred,
                        provenance: Some("second".into()),
                        credibility: Some(0.9),
                    }],
                    confidence: SpecProvenance::Observed,
                    provenance: Some("second entity".into()),
                    credibility: None,
                },
            ],
            operations: vec![
                Operation {
                    name: "op".into(),
                    verb: "read".into(),
                    entity: None,
                    inputs: vec![],
                    effect: None,
                    confidence: SpecProvenance::Inferred,
                    provenance: Some("weak".into()),
                    credibility: Some(0.2),
                },
                Operation {
                    name: "op".into(),
                    verb: "read".into(),
                    entity: None,
                    inputs: vec![],
                    effect: None,
                    confidence: SpecProvenance::Observed,
                    provenance: Some("strong".into()),
                    credibility: None,
                },
            ],
            ui_states: vec![],
            navigation: vec![],
            auth: None,
            assumptions: vec![],
        };
        let merged = merge_entities(spec);
        assert_eq!(merged.entities.len(), 1);
        let d = &merged.entities[0];
        assert_eq!(d.confidence, SpecProvenance::Observed);
        assert_eq!(d.provenance.as_deref(), Some("second entity"));
        assert!(d.credibility.is_none());
        let names: Vec<&str> = d.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["name", "lastSeen", "id"], "order preserved");
        let name = d.fields.iter().find(|f| f.name == "name").unwrap();
        assert_eq!(name.provenance.as_deref(), Some("name@Observed"));
        let last = d.fields.iter().find(|f| f.name == "lastSeen").unwrap();
        assert_eq!(
            last.confidence,
            SpecProvenance::Observed,
            "higher copy wins"
        );
        assert_eq!(d.relationships.len(), 1);
        assert_eq!(
            d.relationships[0].provenance.as_deref(),
            Some("first"),
            "equal-rank relationship: first wins"
        );
        assert_eq!(merged.operations.len(), 1);
        assert_eq!(merged.operations[0].provenance.as_deref(), Some("strong"));
    }
}
