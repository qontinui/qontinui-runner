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
//! ties keep the first), relationships unioned by `to`, operations collapsed by
//! `name` **additively**. It runs inside the assembly tail (after the clamp,
//! before ledger collation); it is a no-op on a spec whose names are already
//! unique.
//!
//! Two deliberate choices in that merge, both because the *ref* convention —
//! not the struct — decides what "duplicate" means:
//!
//! - **Relationships dedupe on `to` alone, not `(to, kind)`.** `enumerate_nodes`
//!   keys a relationship `entities.{entity}.relationships.{to}` with no `kind`
//!   component (`completeness_eval.rs`), and `clamp_provenance` uses the same
//!   key. Keeping `Device→Owner (many-to-one)` beside `Device→Owner
//!   (one-to-one)` would emit that ref twice: `CoverageEvidence.covered` is a
//!   set, so the duplicate collapses on the numerator while the denominator
//!   counts it twice, and coverage silently drops below 1.0 on a spec with no
//!   real gap.
//! - **Operations merge additively.** An earlier draft replaced the whole
//!   struct with the higher-ranked copy, which dropped the loser's `inputs`
//!   and — the sharp case — its `effect`. An operation whose winning copy has
//!   `effect: None` would lose an assumption that `collate_assumptions` had
//!   ledgered, in a crate whose premise is that assumptions are mechanically
//!   ledgered and never dropped. `merge_into_operation` therefore unions
//!   `inputs` by `field` and never lets a present `effect` lose to an absent
//!   one.
//!
//! Ref uniqueness after this pass is a claim about **entities and operations
//! only**. `enumerate_nodes` also emits `uiStates.{id}` and `navigation.{id}`,
//! which the assembly tail does not deduplicate — their uniqueness is the
//! caller's obligation, met by [`aggregate_pages`] and not by the tail.
//!
//! Everything here is pure, deterministic and order-preserving — like the rest
//! of the crate's substrate, it is golden-testable with no clock and no I/O.
//! The live capture that would *drive* UI Bridge across routes remains
//! runtime-deferred, exactly as Phase 1's single-page capture is.

use std::collections::BTreeMap;

use qontinui_types::functional_spec::{Entity, FunctionalSpec, Operation};

use crate::clamp::rank;
use crate::input::{DiscoveredState, DiscoveryResult, Snapshot};

/// The separator between a page namespace and an id. The namespace half is
/// guaranteed free of it (see [`page_namespace`]), so the FIRST occurrence is
/// always the separator and [`split_namespaced`] is an exact inverse even when
/// the id itself contains one.
pub const NS_SEP: &str = "::";

/// One page's observation: its UI Bridge snapshot plus the discovery result
/// clustered from it.
#[derive(Debug, Clone, Default)]
pub struct PageObservation {
    pub snapshot: Snapshot,
    pub discovery: DiscoveryResult,
}

/// Merge the pages into one observation:
/// - snapshot `elements` / `forms` are concatenated in page order, each `id`
///   re-namespaced as `<page-namespace>::<id>` (see [`page_namespace`]) so
///   nothing collides;
/// - discovery `transitions` are namespaced the same way, then deduped by the
///   namespaced id;
/// - the merged `page` is the non-empty page values joined with `+`;
/// - discovery `states` are unioned by `id` — NOT namespaced — first occurrence
///   winning, a repeated state contributing any `observable_checks` whose `id`
///   the first lacks, appended in order.
///
/// **Why states and transitions are treated differently.** State ids are
/// content-derived upstream (`fp:<hash>` in the discovery strategy), so the
/// same id on two pages means the same state and unioning is the intent.
/// Transition ids are **positional** when the source carries none — the
/// strategy falls back to `trans_{i}` — so two independently-run per-page
/// discoveries both emit `trans_0`, `trans_1`, …, and a union by raw id would
/// keep page A's transitions and silently DELETE page B's, trigger and
/// endpoints with them. Namespacing makes a positional collision impossible;
/// the cost is that a genuinely shared transition appears once per page, which
/// over-counts rather than deletes. Because `from_state_ids` / `to_state_ids`
/// point at the un-namespaced state ids, they are deliberately left alone.
///
/// (The plan's phrase "transitions … merged with the higher-confidence copy
/// winning" is unimplementable as written: `DiscoveredTransition` carries no
/// confidence field. Namespacing is the resolution, recorded here rather than
/// silently doing something else.)
pub fn aggregate_pages(pages: &[PageObservation]) -> (Snapshot, DiscoveryResult) {
    let mut snapshot = Snapshot::default();
    let mut discovery = DiscoveryResult::default();
    let mut page_names: Vec<&str> = Vec::new();
    // id -> index, so a long site crawl stays O(n log n) rather than O(n²).
    let mut state_at: BTreeMap<String, usize> = BTreeMap::new();
    let mut transition_at: BTreeMap<String, usize> = BTreeMap::new();

    for (index, page) in pages.iter().enumerate() {
        let raw = page.snapshot.page.as_str();
        if !raw.is_empty() {
            page_names.push(raw);
        }
        let ns = page_namespace(raw, index);
        for el in &page.snapshot.elements {
            let mut el = el.clone();
            el.id = namespace_id(&ns, &el.id);
            snapshot.elements.push(el);
        }
        for form in &page.snapshot.forms {
            let mut form = form.clone();
            form.id = namespace_id(&ns, &form.id);
            snapshot.forms.push(form);
        }
        for state in &page.discovery.states {
            match state_at.get(&state.id).copied() {
                Some(at) => union_checks(&mut discovery.states[at], state),
                None => {
                    state_at.insert(state.id.clone(), discovery.states.len());
                    discovery.states.push(state.clone());
                }
            }
        }
        for t in &page.discovery.transitions {
            let mut t = t.clone();
            t.id = namespace_id(&ns, &t.id);
            if !transition_at.contains_key(&t.id) {
                transition_at.insert(t.id.clone(), discovery.transitions.len());
                discovery.transitions.push(t);
            }
        }
    }

    snapshot.page = page_names.join("+");
    (snapshot, discovery)
}

/// The `::`-free namespace for a page, so [`split_namespaced`] is exact.
///
/// A page value that is empty — the serde default, so any snapshot missing the
/// key — or that itself contains [`NS_SEP`] would make the joined id ambiguous
/// (page `a` + id `b::c` and page `a::b` + id `c` both render `a::b::c`), and
/// an empty one would leave the id bare and colliding with every other bare
/// one. Both fall back to the page's positional namespace.
pub fn page_namespace(page: &str, index: usize) -> String {
    if page.is_empty() || page.contains(NS_SEP) {
        format!("page-{index}")
    } else {
        page.to_string()
    }
}

/// Join a `::`-free namespace to an id. Inverse: [`split_namespaced`].
pub fn namespace_id(namespace: &str, id: &str) -> String {
    format!("{namespace}{NS_SEP}{id}")
}

/// Split a namespaced id back into `(namespace, id)`, or `None` when the string
/// carries no namespace. Exact for every output of [`namespace_id`] whose
/// namespace came from [`page_namespace`], id included when the id itself
/// contains [`NS_SEP`].
pub fn split_namespaced(namespaced: &str) -> Option<(&str, &str)> {
    namespaced.split_once(NS_SEP)
}

fn union_checks(existing: &mut DiscoveredState, incoming: &DiscoveredState) {
    for check in &incoming.observable_checks {
        if !existing.observable_checks.iter().any(|c| c.id == check.id) {
            existing.observable_checks.push(check.clone());
        }
    }
}

/// Collapse same-named entities and operations (see the module docs).
/// `entities.*` and `operations.*` refs are unique afterwards; `uiStates.*` /
/// `navigation.*` uniqueness is the caller's (see the module docs).
pub fn merge_entities(mut spec: FunctionalSpec) -> FunctionalSpec {
    let mut entities: Vec<Entity> = Vec::with_capacity(spec.entities.len());
    let mut entity_at: BTreeMap<String, usize> = BTreeMap::new();
    for incoming in spec.entities.drain(..) {
        match entity_at.get(&incoming.name).copied() {
            Some(at) => merge_into_entity(&mut entities[at], incoming),
            None => {
                entity_at.insert(incoming.name.clone(), entities.len());
                entities.push(incoming);
            }
        }
    }
    spec.entities = entities;

    let mut operations: Vec<Operation> = Vec::with_capacity(spec.operations.len());
    let mut operation_at: BTreeMap<String, usize> = BTreeMap::new();
    for incoming in spec.operations.drain(..) {
        match operation_at.get(&incoming.name).copied() {
            Some(at) => merge_into_operation(&mut operations[at], incoming),
            None => {
                operation_at.insert(incoming.name.clone(), operations.len());
                operations.push(incoming);
            }
        }
    }
    spec.operations = operations;

    spec
}

/// Additive operation merge: the higher-evidence copy's own claims win, but
/// nothing the loser carried is discarded. In particular an `effect` is never
/// dropped — see the module docs for why that case is the sharp one.
fn merge_into_operation(target: &mut Operation, incoming: Operation) {
    if rank(incoming.confidence) > rank(target.confidence) {
        target.confidence = incoming.confidence;
        target.provenance = incoming.provenance;
        target.credibility = incoming.credibility;
        target.verb = incoming.verb;
        if incoming.entity.is_some() {
            target.entity = incoming.entity;
        }
        if incoming.effect.is_some() {
            target.effect = incoming.effect;
        }
    } else if target.effect.is_none() {
        // The loser is the only copy carrying the assumption. Dropping it here
        // would unledger an effect `collate_assumptions` had already recorded.
        target.effect = incoming.effect;
    }
    for input in incoming.inputs {
        if !target.inputs.iter().any(|i| i.field == input.field) {
            target.inputs.push(input);
        }
    }
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
    // Keyed on `to` ALONE — the `enumerate_nodes` ref carries no `kind`, so two
    // kinds to the same target would emit one ref twice (module docs).
    for rel in incoming.relationships {
        match target.relationships.iter_mut().find(|r| r.to == rel.to) {
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
    use qontinui_types::functional_spec::{
        EntityField, OperationEffect, OperationInput, Relationship, SpecProvenance, SpecTarget,
    };
    use std::collections::BTreeSet;

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
        assert_eq!(
            ids,
            vec!["a::el", "b::el", "page-2::el"],
            "an empty page falls back to its positional namespace, never bare"
        );
        let state_ids: Vec<&str> = disc.states.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(state_ids, vec!["s1", "s2", "s3"], "states union by raw id");
        // s1 appears on two pages: first wins, checks unioned by id in order.
        let s1 = &disc.states[0];
        let checks: Vec<&str> = s1.observable_checks.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(checks, vec!["s1-check-a", "s1-check-b"]);
        let t_ids: Vec<&str> = disc.transitions.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(t_ids, vec!["a::t1", "b::t1", "b::t2"]);
    }

    /// The upstream discovery strategy names an id-less transition positionally
    /// (`trans_{i}`), so two independently-discovered pages BOTH emit `trans_0`.
    /// A union by raw id would keep page a's and delete page b's outright.
    #[test]
    fn positionally_named_transitions_from_two_pages_both_survive() {
        let pages = vec![
            page("a", &["s1"], &["trans_0", "trans_1"]),
            page("b", &["s2"], &["trans_0"]),
        ];
        let (_, disc) = aggregate_pages(&pages);
        let t_ids: Vec<&str> = disc.transitions.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(t_ids, vec!["a::trans_0", "a::trans_1", "b::trans_0"]);
        let distinct: BTreeSet<&str> = t_ids.iter().copied().collect();
        assert_eq!(distinct.len(), t_ids.len(), "navigation refs stay unique");
    }

    #[test]
    fn namespacing_round_trips_even_when_the_id_carries_the_separator() {
        for (page, index, id) in [
            ("a", 0usize, "b::c"),
            ("", 3, "plain"),
            ("a::b", 1, "c"),
            ("https://app.example/x", 0, "el-1"),
        ] {
            let ns = page_namespace(page, index);
            assert!(!ns.contains(NS_SEP), "namespace must be separator-free");
            let joined = namespace_id(&ns, id);
            assert_eq!(
                split_namespaced(&joined),
                Some((ns.as_str(), id)),
                "round trip for ({page:?}, {id:?})"
            );
        }
        assert_eq!(split_namespaced("no-namespace-here"), None);
    }

    /// page `a` + id `b::c` and page `a::b` + id `c` both rendered `a::b::c`
    /// under the first draft's scheme.
    #[test]
    fn the_ambiguous_page_id_pair_no_longer_collides() {
        let one = namespace_id(&page_namespace("a", 0), "b::c");
        let two = namespace_id(&page_namespace("a::b", 1), "c");
        assert_ne!(one, two);
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

    fn op(confidence: SpecProvenance, inputs: &[&str], effect: bool) -> Operation {
        Operation {
            name: "op".into(),
            verb: "create".into(),
            entity: None,
            inputs: inputs
                .iter()
                .map(|f| OperationInput {
                    field: (*f).into(),
                    required: false,
                    validation: None,
                })
                .collect(),
            effect: effect.then(|| OperationEffect {
                confidence: SpecProvenance::Assumed,
                assumption: Some("persists".into()),
                provenance: Some("awas:op".into()),
                credibility: None,
            }),
            confidence,
            provenance: Some(format!("{confidence:?}")),
            credibility: None,
        }
    }

    fn spec_with(entities: Vec<Entity>, operations: Vec<Operation>) -> FunctionalSpec {
        FunctionalSpec {
            spec_version: "0".into(),
            target: SpecTarget {
                source_url: "x".into(),
                observed_at: None,
            },
            entities,
            operations,
            ui_states: vec![],
            navigation: vec![],
            auth: None,
            assumptions: vec![],
        }
    }

    /// The sharp case: the WINNING copy has no effect. Replacing the struct
    /// wholesale would unledger an assumption `collate_assumptions` recorded.
    #[test]
    fn an_effect_is_never_dropped_when_the_higher_ranked_copy_lacks_one() {
        let merged = merge_entities(spec_with(
            vec![],
            vec![
                op(SpecProvenance::Inferred, &["a"], true),
                op(SpecProvenance::Observed, &["b"], false),
            ],
        ));
        assert_eq!(merged.operations.len(), 1);
        let only = &merged.operations[0];
        assert_eq!(only.confidence, SpecProvenance::Observed, "stronger wins");
        assert_eq!(only.provenance.as_deref(), Some("Observed"));
        let effect = only
            .effect
            .as_ref()
            .expect("the loser's effect survives the merge");
        assert_eq!(effect.assumption.as_deref(), Some("persists"));
        let fields: Vec<&str> = only.inputs.iter().map(|i| i.field.as_str()).collect();
        assert_eq!(fields, vec!["a", "b"], "inputs union, order preserved");
    }

    /// `enumerate_nodes` keys a relationship on `to` with no `kind`, so two
    /// kinds to one target must collapse or the ref is emitted twice.
    #[test]
    fn two_relationship_kinds_to_the_same_target_collapse_to_one_ref() {
        let rel = |kind: &str, confidence, provenance: &str| Relationship {
            to: "Owner".into(),
            kind: kind.into(),
            confidence,
            provenance: Some(provenance.into()),
            credibility: None,
        };
        let entity = |kind: &str, confidence, provenance: &str| Entity {
            name: "Device".into(),
            fields: vec![],
            relationships: vec![rel(kind, confidence, provenance)],
            confidence: SpecProvenance::Observed,
            provenance: None,
            credibility: None,
        };
        let merged = merge_entities(spec_with(
            vec![
                entity("many-to-one", SpecProvenance::Inferred, "weak"),
                entity("one-to-one", SpecProvenance::Observed, "strong"),
            ],
            vec![],
        ));
        assert_eq!(merged.entities.len(), 1);
        let rels = &merged.entities[0].relationships;
        assert_eq!(rels.len(), 1, "one ref per target, whatever the kind");
        assert_eq!(rels[0].provenance.as_deref(), Some("strong"));
        assert_eq!(rels[0].kind, "one-to-one");
    }
}
