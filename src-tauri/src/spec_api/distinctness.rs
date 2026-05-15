//! State distinctness validator for IR documents. (Plan 04, Step 1.)
//!
//! Enforces three rules on every `IrPageSpec`:
//!
//!   1. **Empty-criteria** — no `IrElementCriteria` parsed from
//!      `assertion.target.criteria` may have every field `None` / empty. An
//!      unparseable criteria payload also counts as empty.
//!   2. **Identical states** — no two states may share the same canonical
//!      key-set after text normalization + multiset dedup.
//!   3. **Subset domination** — no state's key-set may be a proper subset of
//!      another state's key-set.
//!
//! Normalization of text-bearing fields (`text`, `textContains`, `ariaLabel`,
//! `accessibleName`, and attribute *values*) delegates to
//! `qontinui_types::text_norm::normalize_text`. `role`, `tagName`, `id`, and
//! attribute *keys* are case-sensitive in the IR contract (design context
//! §5.11) and are NOT folded.
//!
//! ## Scope notes
//!
//! - `excludedElements` is intentionally NOT scanned for empty-criterion
//!   violations: an empty `excludedElements` entry is a no-op, not a footgun
//!   (opposite semantic from `assertions`).
//! - `parent` is NOT a field on `IrElementCriteria` today (verified against
//!   `qontinui-schemas/rust/src/ir.rs:42`). If the schema grows a `parent`
//!   field, the `field_set_exhaustive` test below fails to compile —
//!   re-examine `is_empty_criterion` + `canonical_criterion_key` at that
//!   point. Non-empty `parent` should count as a real (non-empty) criterion.
//! - The plan-internal `DistinctnessReport` carries the per-violation
//!   discriminator. The landed `qontinui_types::spec_check::SpecValidation`
//!   does not; use `to_spec_validation` to roll up for the
//!   `GET /spec/list` surface.

use super::types::{IrAssertion, IrElementCriteria, IrPageSpec, IrState};
use qontinui_types::spec_check::SpecValidation;
use qontinui_types::text_norm::normalize_text;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

// ============================================================================
// Public API
// ============================================================================

/// Entry point. `Ok(())` if the document is distinct; otherwise the
/// structured report.
pub fn validate(doc: &IrPageSpec) -> Result<(), DistinctnessReport> {
    let r = report(doc);
    if r.ok {
        Ok(())
    } else {
        Err(r)
    }
}

/// Always returns a report; `ok == true` when there are no violations.
pub fn report(doc: &IrPageSpec) -> DistinctnessReport {
    let mut violations: Vec<DistinctnessViolation> = Vec::new();

    // Check 1 — empty criteria
    for state in &doc.states {
        for (i, assertion) in state.assertions.iter().enumerate() {
            let empty = match try_parse_criteria(&assertion.target.criteria) {
                Some(c) => is_empty_criterion(&c),
                // Unparseable payload — treat as empty for the validator.
                None => true,
            };
            if empty {
                violations.push(DistinctnessViolation::EmptyCriteria {
                    state_id: state.id.clone(),
                    assertion_index: i,
                });
            }
        }
    }

    // Build canonical state keys once (skip empty-criterion-only states from
    // identical/subset checks — Check 1 already flagged them).
    let state_keys: Vec<StateKey> = doc.states.iter().map(canonical_state_key).collect();

    // Check 2 — identical states
    let mut by_key: BTreeMap<BTreeSet<CriterionKey>, Vec<String>> = BTreeMap::new();
    for sk in &state_keys {
        if sk.keys.is_empty() {
            // Empty-criteria-only state — Check 1 covers it.
            continue;
        }
        by_key
            .entry(sk.keys.clone())
            .or_default()
            .push(sk.state_id.clone());
    }
    for (keys, state_ids) in &by_key {
        if state_ids.len() >= 2 {
            let mut ids = state_ids.clone();
            ids.sort();
            violations.push(DistinctnessViolation::IdenticalStates {
                state_ids: ids,
                key_size: keys.len(),
            });
        }
    }

    // Check 3 — subset domination (all-pairs, O(N^2)).
    for i in 0..state_keys.len() {
        for j in 0..state_keys.len() {
            if i == j {
                continue;
            }
            let a = &state_keys[i];
            let b = &state_keys[j];
            if a.keys.is_empty() {
                // Empty already reported.
                continue;
            }
            if a.keys == b.keys {
                // Identical, already covered by Check 2.
                continue;
            }
            if a.keys.is_subset(&b.keys) {
                // Proper subset (equality filtered above).
                violations.push(DistinctnessViolation::SubsetDomination {
                    subset_state_id: a.state_id.clone(),
                    superset_state_id: b.state_id.clone(),
                    missing_in_subset: b.keys.len() - a.keys.len(),
                });
            }
        }
    }

    DistinctnessReport {
        ok: violations.is_empty(),
        violations,
    }
}

/// Roll up the plan-internal report to the landed `SpecValidation` schema
/// surfaced on `GET /spec/list`. Empty-criteria and subset-dominated subset
/// state IDs go into `degenerate_state_ids`; identical-state members go in
/// both `degenerate_state_ids` (so the consumer can flag them) and
/// `indistinguishable_state_pairs` (sorted lexicographically, deduped).
pub fn to_spec_validation(page_id: &str, r: &DistinctnessReport) -> SpecValidation {
    let mut degenerate: BTreeSet<String> = BTreeSet::new();
    let mut pairs: BTreeSet<[String; 2]> = BTreeSet::new();
    for v in &r.violations {
        match v {
            DistinctnessViolation::EmptyCriteria { state_id, .. } => {
                degenerate.insert(state_id.clone());
            }
            DistinctnessViolation::IdenticalStates { state_ids, .. } => {
                degenerate.extend(state_ids.iter().cloned());
                for i in 0..state_ids.len() {
                    for j in (i + 1)..state_ids.len() {
                        let mut pair = [state_ids[i].clone(), state_ids[j].clone()];
                        pair.sort();
                        pairs.insert(pair);
                    }
                }
            }
            DistinctnessViolation::SubsetDomination {
                subset_state_id, ..
            } => {
                degenerate.insert(subset_state_id.clone());
            }
        }
    }
    SpecValidation {
        page_id: page_id.to_string(),
        degenerate_state_ids: degenerate.into_iter().collect(),
        indistinguishable_state_pairs: pairs.into_iter().collect(),
    }
}

/// Canonical state key. Public so the diagnostic CLI (Plan 04 Step 8) and
/// integration tests can render `(state_id, key)` tables.
pub fn canonical_state_key(state: &IrState) -> StateKey {
    let keys: BTreeSet<CriterionKey> = state
        .assertions
        .iter()
        .filter_map(|a| try_parse_criteria(&a.target.criteria))
        .filter(|c| !is_empty_criterion(c))
        .map(|c| canonical_criterion_key(&c))
        .collect();
    StateKey {
        state_id: state.id.clone(),
        keys,
    }
}

/// Canonical criterion key. Public so the diagnostic CLI can render each
/// criterion-element for human eyes. Text-bearing fields pass through
/// `normalize_text`; structural fields (`role`, `tagName`, `id`, attribute
/// keys) are preserved verbatim — they're case-sensitive in the IR contract.
pub fn canonical_criterion_key(c: &IrElementCriteria) -> CriterionKey {
    CriterionKey {
        role: c.role.clone(),
        tag_name: c.tag_name.clone(),
        text: c.text.as_deref().map(normalize_text),
        text_contains: c.text_contains.as_deref().map(normalize_text),
        aria_label: c.aria_label.as_deref().map(normalize_text),
        accessible_name: c.accessible_name.as_deref().map(normalize_text),
        id: c.id.clone(),
        attributes: c
            .attributes
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| (k, normalize_text(&v)))
            .collect(),
    }
}

// ============================================================================
// Types
// ============================================================================

/// Canonical key for one `IrElementCriteria`. All text-bearing fields are
/// post-`normalize_text`. The `Ord` derivation defines a stable total order
/// across criteria — used by `BTreeSet` membership in the state-level key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CriterionKey {
    pub role: Option<String>,
    pub tag_name: Option<String>,
    pub text: Option<String>,
    pub text_contains: Option<String>,
    pub aria_label: Option<String>,
    pub accessible_name: Option<String>,
    pub id: Option<String>,
    pub attributes: BTreeMap<String, String>,
}

/// Canonical key for one state — the deduped *set* (not multiset) of
/// non-empty criterion keys. Two duplicate `{role: "button"}` entries collapse
/// to one — they don't add discriminator power.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateKey {
    pub state_id: String,
    pub keys: BTreeSet<CriterionKey>,
}

/// Plan-internal validation report. Carries the per-violation discriminator
/// (`reason: emptyCriteria | identicalStates | subsetDomination`) that the
/// landed `qontinui_types::spec_check::SpecValidation` schema lacks.
///
/// NOT in `qontinui-schemas` — only the runner-internal `POST /spec/validate`
/// surface and the diagnostic CLI consume this shape. Use
/// [`to_spec_validation`] to roll up for cross-language consumers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DistinctnessReport {
    pub ok: bool,
    pub violations: Vec<DistinctnessViolation>,
}

/// Discriminated violation variant. Tagged on the wire as
/// `{ "reason": "emptyCriteria" | "identicalStates" | "subsetDomination", ... }`.
/// Per-variant fields ship as camelCase via `rename_all_fields` (serde ≥ 1.0.157).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "reason",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DistinctnessViolation {
    EmptyCriteria {
        state_id: String,
        assertion_index: usize,
    },
    IdenticalStates {
        state_ids: Vec<String>,
        key_size: usize,
    },
    SubsetDomination {
        subset_state_id: String,
        superset_state_id: String,
        missing_in_subset: usize,
    },
}

// ============================================================================
// Internals
// ============================================================================

/// `IrAssertionTarget.criteria` is free-form JSON (`serde_json::Value`); the
/// validator parses it lazily into the typed `IrElementCriteria`. A parse
/// failure (unknown discriminator, type mismatch, etc.) counts as empty —
/// see `report()`.
fn try_parse_criteria(v: &serde_json::Value) -> Option<IrElementCriteria> {
    serde_json::from_value(v.clone()).ok()
}

/// True when every field is unset. `attributes: Some({})` is equivalent to
/// `None`.
fn is_empty_criterion(c: &IrElementCriteria) -> bool {
    c.role.is_none()
        && c.tag_name.is_none()
        && c.text.is_none()
        && c.text_contains.is_none()
        && c.aria_label.is_none()
        && c.accessible_name.is_none()
        && c.id.is_none()
        && c.attributes.as_ref().map(|m| m.is_empty()).unwrap_or(true)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_types::ir::{IrAssertion, IrAssertionTarget, IrPageSpec, IrState};
    use serde_json::json;

    // ------------------------------------------------------------------------
    // Fixture helpers
    // ------------------------------------------------------------------------

    fn make_state(id: &str, criteria: Vec<serde_json::Value>) -> IrState {
        IrState {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            assertions: criteria
                .into_iter()
                .enumerate()
                .map(|(i, c)| IrAssertion {
                    id: format!("{id}-elem-{i}"),
                    description: "test".to_string(),
                    category: "element-presence".to_string(),
                    severity: "critical".to_string(),
                    assertion_type: "exists".to_string(),
                    target: IrAssertionTarget {
                        kind: "search".to_string(),
                        criteria: c,
                        label: "".to_string(),
                    },
                    source: "test".to_string(),
                    reviewed: false,
                    enabled: true,
                    precondition: None,
                })
                .collect(),
            excluded_elements: None,
            conditions: None,
            is_initial: None,
            is_terminal: None,
            blocking: None,
            group: None,
            path_cost: None,
            precondition: None,
            element_ids: None,
            incoming_transitions: None,
            metadata: None,
            provenance: None,
            cross_refs: None,
        }
    }

    fn make_doc(states: Vec<IrState>) -> IrPageSpec {
        IrPageSpec {
            version: "1.0".to_string(),
            id: "test-page".to_string(),
            name: "Test".to_string(),
            description: None,
            metadata: None,
            provenance: None,
            states,
            transitions: vec![],
            synthesized_groups: None,
            initial_state: None,
        }
    }

    fn count_violations<F>(r: &DistinctnessReport, pred: F) -> usize
    where
        F: Fn(&DistinctnessViolation) -> bool,
    {
        r.violations.iter().filter(|v| pred(v)).count()
    }

    // ------------------------------------------------------------------------
    // 1. empty_criterion_detection
    // ------------------------------------------------------------------------

    #[test]
    fn empty_criterion_detection() {
        // `{}` → violation
        let doc_empty = make_doc(vec![make_state("s1", vec![json!({})])]);
        let r = report(&doc_empty);
        assert!(!r.ok);
        assert_eq!(
            count_violations(&r, |v| matches!(
                v,
                DistinctnessViolation::EmptyCriteria { .. }
            )),
            1
        );

        // `{role: "button"}` → no violation
        let doc_role = make_doc(vec![make_state("s1", vec![json!({"role": "button"})])]);
        let r = report(&doc_role);
        assert!(r.ok, "expected no violations, got {:?}", r.violations);

        // `{attributes: {}}` → violation (empty attributes map ≡ None)
        let doc_empty_attrs = make_doc(vec![make_state("s1", vec![json!({"attributes": {}})])]);
        let r = report(&doc_empty_attrs);
        assert!(!r.ok);
        assert_eq!(
            count_violations(&r, |v| matches!(
                v,
                DistinctnessViolation::EmptyCriteria { .. }
            )),
            1
        );

        // Unparseable criteria payload (wrong field type) → violation
        let doc_bad = make_doc(vec![make_state(
            "s1",
            vec![json!({"role": 42})], // role must be string
        )]);
        let r = report(&doc_bad);
        assert!(!r.ok);
        assert!(matches!(
            r.violations[0],
            DistinctnessViolation::EmptyCriteria { .. }
        ));
    }

    // ------------------------------------------------------------------------
    // 2. whitespace_collapse_to_identical
    // ------------------------------------------------------------------------

    #[test]
    fn whitespace_collapse_to_identical() {
        // Two states with text that normalizes to the same canonical form.
        let s1 = make_state("lib-dashboard", vec![json!({"text": "Library Dashboard"})]);
        let s2 = make_state(
            "lib-item",
            vec![json!({"text": "  library  dashboard\u{00A0} "})],
        );
        let doc = make_doc(vec![s1, s2]);
        let r = report(&doc);
        assert!(!r.ok, "expected violations, got none");

        // Exactly one identical-states violation pairing both ids.
        let identical = r
            .violations
            .iter()
            .filter_map(|v| match v {
                DistinctnessViolation::IdenticalStates {
                    state_ids,
                    key_size,
                } => Some((state_ids.clone(), *key_size)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(identical.len(), 1);
        let (ids, ks) = &identical[0];
        assert_eq!(ks, &1);
        assert_eq!(
            ids,
            &vec!["lib-dashboard".to_string(), "lib-item".to_string()]
        );
    }

    // ------------------------------------------------------------------------
    // 3. multiset_dedup_to_identical
    // ------------------------------------------------------------------------

    #[test]
    fn multiset_dedup_to_identical() {
        // State 1: two button criteria — collapses to a single-element set.
        let s1 = make_state(
            "s1",
            vec![json!({"role": "button"}), json!({"role": "button"})],
        );
        // State 2: one button criterion.
        let s2 = make_state("s2", vec![json!({"role": "button"})]);
        let doc = make_doc(vec![s1, s2]);
        let r = report(&doc);
        assert!(!r.ok);

        let identical_count = count_violations(&r, |v| {
            matches!(v, DistinctnessViolation::IdenticalStates { .. })
        });
        assert_eq!(
            identical_count, 1,
            "BTreeSet dedup should collapse to identical key sets"
        );
    }

    // ------------------------------------------------------------------------
    // 4. proper_subset_only
    // ------------------------------------------------------------------------

    #[test]
    fn proper_subset_only() {
        // A = {x}, B = {x, y} — subset violation expected.
        let a = make_state("a", vec![json!({"role": "button"})]);
        let b = make_state(
            "b",
            vec![json!({"role": "button"}), json!({"role": "link"})],
        );
        let doc = make_doc(vec![a, b]);
        let r = report(&doc);
        assert!(!r.ok);
        let subset = r
            .violations
            .iter()
            .filter_map(|v| match v {
                DistinctnessViolation::SubsetDomination {
                    subset_state_id,
                    superset_state_id,
                    missing_in_subset,
                } => Some((
                    subset_state_id.clone(),
                    superset_state_id.clone(),
                    *missing_in_subset,
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(subset.len(), 1);
        assert_eq!(subset[0].0, "a");
        assert_eq!(subset[0].1, "b");
        assert_eq!(subset[0].2, 1);

        // A == B — only an identical violation (NOT subset).
        let a = make_state("a", vec![json!({"role": "button"})]);
        let b = make_state("b", vec![json!({"role": "button"})]);
        let doc = make_doc(vec![a, b]);
        let r = report(&doc);
        assert!(!r.ok);
        let subset_count = count_violations(&r, |v| {
            matches!(v, DistinctnessViolation::SubsetDomination { .. })
        });
        let identical_count = count_violations(&r, |v| {
            matches!(v, DistinctnessViolation::IdenticalStates { .. })
        });
        assert_eq!(
            subset_count, 0,
            "equality should NOT be reported as subset domination"
        );
        assert_eq!(identical_count, 1);
    }

    // ------------------------------------------------------------------------
    // 5. three_way_dominance
    // ------------------------------------------------------------------------

    #[test]
    fn three_way_dominance() {
        // A = {x} ⊂ B = {x, y} ⊂ C = {x, y, z}
        let a = make_state("a", vec![json!({"role": "button"})]);
        let b = make_state(
            "b",
            vec![json!({"role": "button"}), json!({"role": "link"})],
        );
        let c = make_state(
            "c",
            vec![
                json!({"role": "button"}),
                json!({"role": "link"}),
                json!({"role": "list"}),
            ],
        );
        let doc = make_doc(vec![a, b, c]);
        let r = report(&doc);
        assert!(!r.ok);
        let subset_count = count_violations(&r, |v| {
            matches!(v, DistinctnessViolation::SubsetDomination { .. })
        });
        // (a, b), (a, c), (b, c) — three pair-violations, no minimization.
        assert_eq!(
            subset_count, 3,
            "expected (a,b), (a,c), (b,c); got {:?}",
            r.violations
        );
    }

    // ------------------------------------------------------------------------
    // 6. excluded_elements_ignored
    // ------------------------------------------------------------------------

    #[test]
    fn excluded_elements_ignored() {
        // A state with a fully-populated assertion AND an empty entry in
        // excludedElements — should NOT produce an empty-criterion violation.
        let mut s1 = make_state("s1", vec![json!({"role": "button"})]);
        s1.excluded_elements = Some(vec![IrElementCriteria::default()]);
        // Add a second state so we have a distinct corpus.
        let s2 = make_state("s2", vec![json!({"role": "link"})]);
        let doc = make_doc(vec![s1, s2]);
        let r = report(&doc);
        assert!(
            r.ok,
            "excludedElements with empty entry should not produce a violation; got {:?}",
            r.violations
        );
    }

    // ------------------------------------------------------------------------
    // 7. case_sensitive_role
    // ------------------------------------------------------------------------

    #[test]
    fn case_sensitive_role() {
        let s1 = make_state("s1", vec![json!({"role": "BUTTON"})]);
        let s2 = make_state("s2", vec![json!({"role": "button"})]);
        let doc = make_doc(vec![s1, s2]);
        let r = report(&doc);
        assert!(
            r.ok,
            "role is case-sensitive — BUTTON and button should be distinct"
        );
    }

    // ------------------------------------------------------------------------
    // 8. id_field_case_sensitive
    // ------------------------------------------------------------------------

    #[test]
    fn id_field_case_sensitive() {
        let s1 = make_state("s1", vec![json!({"id": "Save"})]);
        let s2 = make_state("s2", vec![json!({"id": "save"})]);
        let doc = make_doc(vec![s1, s2]);
        let r = report(&doc);
        assert!(
            r.ok,
            "id is case-sensitive — Save and save should be distinct"
        );
    }

    // ------------------------------------------------------------------------
    // 9. field_set_exhaustive — fail-compile if IrElementCriteria gains a field
    // ------------------------------------------------------------------------

    #[test]
    fn field_set_exhaustive() {
        // Destructure exhaustively — if IrElementCriteria gains a field, this
        // fails to compile and the developer must update is_empty_criterion
        // and canonical_criterion_key.
        let c = IrElementCriteria::default();
        let IrElementCriteria {
            role,
            tag_name,
            text,
            text_contains,
            aria_label,
            accessible_name,
            id,
            attributes,
        } = &c;
        assert!(role.is_none());
        assert!(tag_name.is_none());
        assert!(text.is_none());
        assert!(text_contains.is_none());
        assert!(aria_label.is_none());
        assert!(accessible_name.is_none());
        assert!(id.is_none());
        assert!(attributes.is_none());

        // Default ≡ empty.
        assert!(is_empty_criterion(&c));

        // Each field, individually populated → NOT empty.
        let with_role = IrElementCriteria {
            role: Some("button".into()),
            ..Default::default()
        };
        assert!(!is_empty_criterion(&with_role));

        let with_tag = IrElementCriteria {
            tag_name: Some("div".into()),
            ..Default::default()
        };
        assert!(!is_empty_criterion(&with_tag));

        let with_text = IrElementCriteria {
            text: Some("Hello".into()),
            ..Default::default()
        };
        assert!(!is_empty_criterion(&with_text));

        let with_tc = IrElementCriteria {
            text_contains: Some("Hel".into()),
            ..Default::default()
        };
        assert!(!is_empty_criterion(&with_tc));

        let with_aria = IrElementCriteria {
            aria_label: Some("Save button".into()),
            ..Default::default()
        };
        assert!(!is_empty_criterion(&with_aria));

        let with_acc = IrElementCriteria {
            accessible_name: Some("Save".into()),
            ..Default::default()
        };
        assert!(!is_empty_criterion(&with_acc));

        let with_id = IrElementCriteria {
            id: Some("save-btn".into()),
            ..Default::default()
        };
        assert!(!is_empty_criterion(&with_id));

        let mut attrs = BTreeMap::new();
        attrs.insert("data-testid".to_string(), "save".to_string());
        let with_attrs = IrElementCriteria {
            attributes: Some(attrs),
            ..Default::default()
        };
        assert!(!is_empty_criterion(&with_attrs));
    }

    // ------------------------------------------------------------------------
    // 10. to_spec_validation_rolls_up_correctly
    // ------------------------------------------------------------------------

    #[test]
    fn to_spec_validation_rolls_up_correctly() {
        // Build a corpus with one of each violation type:
        // - "empty" → state with an empty criterion {}
        // - "dup-a", "dup-b" → identical
        // - "sub" → subset-dominated by "sup"
        let empty_state = make_state("empty", vec![json!({})]);
        let dup_a = make_state("dup-a", vec![json!({"role": "heading"})]);
        let dup_b = make_state("dup-b", vec![json!({"role": "heading"})]);
        let sub = make_state("sub", vec![json!({"role": "navigation"})]);
        let sup = make_state(
            "sup",
            vec![json!({"role": "navigation"}), json!({"role": "main"})],
        );
        let doc = make_doc(vec![empty_state, dup_a, dup_b, sub, sup]);
        let r = report(&doc);
        assert!(!r.ok);

        let sv = to_spec_validation("test-page", &r);
        assert_eq!(sv.page_id, "test-page");

        // degenerate_state_ids: empty + dup-a + dup-b + sub (NOT sup —
        // sup is the dominator, not the dominated).
        let degenerate: BTreeSet<String> = sv.degenerate_state_ids.iter().cloned().collect();
        assert!(degenerate.contains("empty"));
        assert!(degenerate.contains("dup-a"));
        assert!(degenerate.contains("dup-b"));
        assert!(degenerate.contains("sub"));
        assert!(
            !degenerate.contains("sup"),
            "the dominator should not be reported as degenerate"
        );

        // indistinguishable_state_pairs: sorted, deduped, one entry.
        assert_eq!(sv.indistinguishable_state_pairs.len(), 1);
        let pair = &sv.indistinguishable_state_pairs[0];
        assert_eq!(pair, &["dup-a".to_string(), "dup-b".to_string()]);
    }
}
