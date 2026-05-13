//! Per-field matching primitives. Step 3 of Plan 02.
//!
//! Phase 1 returns boolean exact-match decisions; Phase 2 returns similarity
//! scores in `[0.0, 1.0]`. All text comparisons route through
//! [`qontinui_types::text_norm::normalize_text`] so authoring `text: "Save"`
//! and a snapshot saying `"  save  "` always collide.
//!
//! ## Two-phase contract
//!
//! - **Phase 1** ([`matches_all`]) — every `Some(_)` field of the criterion
//!   must be exactly satisfied by the same observed element. No partial
//!   credit.
//! - **Phase 2** ([`score_role`] / [`score_text`] / …) — per-field similarity
//!   on a `[0.0, 1.0]` scale, aggregated by the `diff` module with the
//!   TF-IDF-inspired selectivity weights from [`IndexedSnapshot::selectivity`].

use std::collections::BTreeMap;

use qontinui_types::ir::IrElementCriteria;
use qontinui_types::text_norm::normalize_text;
use qontinui_types::ui_bridge::UIBridgeElement;

use crate::role_categories::same_category;
use crate::snapshot::{ElementIdx, IndexedSnapshot};

// ===========================================================================
// Phase 1 — binary exact match
// ===========================================================================

/// True iff every `Some(_)` field of `crit` returns a perfect 1.0 score
/// against `idx`. No partial credit — a single non-perfect field flips the
/// whole criterion to false. This is the hot path: callers narrow the
/// candidate set via [`narrow_candidates`] first.
pub fn matches_all(
    snapshot: &IndexedSnapshot,
    crit: &IrElementCriteria,
    idx: ElementIdx,
) -> bool {
    let el = snapshot.element(idx);

    if let Some(role) = crit.role.as_deref() {
        if score_role(role, &el.role) < 1.0 {
            return false;
        }
    }
    if let Some(tag) = crit.tag_name.as_deref() {
        if score_tag(tag, &el.tag_name) < 1.0 {
            return false;
        }
    }
    if let Some(text) = crit.text.as_deref() {
        if score_text(text, &el.text) < 1.0 {
            return false;
        }
    }
    if let Some(needle) = crit.text_contains.as_deref() {
        if !text_contains(needle, &el.text) {
            return false;
        }
    }
    if let Some(label) = crit.aria_label.as_deref() {
        if score_aria_label(label, &el.aria_label) < 1.0 {
            return false;
        }
    }
    if let Some(name) = crit.accessible_name.as_deref() {
        if score_text(name, &el.accessible_name) < 1.0 {
            return false;
        }
    }
    if let Some(id) = crit.id.as_deref() {
        if !id_matches(id, el) {
            return false;
        }
    }
    if let Some(declared_attrs) = crit.attributes.as_ref() {
        // The wire shape doesn't expose a generic attribute map on
        // UIBridgeElement; treat the observed side as `None` so the scorer
        // semantics are explicit. Empty declared → vacuously satisfied;
        // non-empty declared with no observable attributes → 0.0 → fail.
        if score_attributes(declared_attrs, &None) < 1.0 {
            return false;
        }
    }

    true
}

/// Narrow the search to the most-selective declared field, in priority order:
///
/// 1. `id` (uniquest)
/// 2. `aria_label`
/// 3. `role` ∩ `text` first-word
/// 4. `role`
/// 5. `tag_name`
/// 6. Fallback: iterate every element
///
/// Returned vec preserves the natural index ordering of the underlying
/// snapshot; downstream callers should still run [`matches_all`] to
/// disambiguate.
pub fn narrow_candidates(
    snapshot: &IndexedSnapshot,
    crit: &IrElementCriteria,
) -> Vec<ElementIdx> {
    // (1) id — strongest signal; if the user declared an ID and we have a
    //     hit, that's the candidate set, period.
    if let Some(id) = crit.id.as_deref() {
        return snapshot.lookup_by_id(id).to_vec();
    }

    // (2) aria_label
    if let Some(label) = crit.aria_label.as_deref() {
        return snapshot.lookup_by_aria_label(label).to_vec();
    }

    // (3) role + text first-word intersection. Tighter than role alone when
    //     the spec includes both.
    if let (Some(role), Some(text)) = (crit.role.as_deref(), crit.text.as_deref()) {
        let role_hits = snapshot.lookup_by_role(role);
        if role_hits.is_empty() {
            return Vec::new();
        }
        let normalized_text = normalize_text(text);
        if let Some(first_word) = normalized_text.split_ascii_whitespace().next() {
            let text_hits = snapshot.lookup_text_word(first_word);
            if text_hits.is_empty() {
                // Word didn't appear anywhere → candidate set is empty even
                // though role matched some elements (the AND fails).
                return Vec::new();
            }
            return intersect_sorted_unique(role_hits, text_hits);
        }
        return role_hits.to_vec();
    }

    // (4) role alone
    if let Some(role) = crit.role.as_deref() {
        return snapshot.lookup_by_role(role).to_vec();
    }

    // (5) tag_name alone
    if let Some(tag) = crit.tag_name.as_deref() {
        return snapshot.lookup_by_tag(tag).to_vec();
    }

    // (6) Fallback: every element. Phase-1 will filter via matches_all.
    snapshot.iter_elements().map(|(i, _)| i).collect()
}

// ===========================================================================
// Phase 2 — per-field fuzzy scoring
// ===========================================================================

/// 1.0 on exact match (post-normalization), 0.5 when declared and observed
/// roles share an ARIA category but differ literally, 0.0 otherwise (or when
/// observed is absent).
pub fn score_role(declared: &str, observed: &Option<String>) -> f32 {
    let Some(obs) = observed else {
        return 0.0;
    };
    let d = normalize_text(declared);
    let o = normalize_text(obs);
    if d.is_empty() && o.is_empty() {
        return 1.0;
    }
    if d == o {
        1.0
    } else if same_category(&d, &o) {
        0.5
    } else {
        0.0
    }
}

/// 1.0 on exact match, 0.0 otherwise. HTML tag names are categorical — no
/// graded similarity.
pub fn score_tag(declared: &str, observed: &Option<String>) -> f32 {
    let Some(obs) = observed else {
        return 0.0;
    };
    let d = normalize_text(declared);
    let o = normalize_text(obs);
    if d == o {
        1.0
    } else {
        0.0
    }
}

/// Max of three signals over the normalized strings:
///
/// 1. Substring containment — 1.0 if observed contains declared (or vice
///    versa, since both directions are semantically interesting).
/// 2. Jaccard similarity over whitespace-tokenized word sets.
/// 3. `1 - normalized_levenshtein` (edit-distance similarity).
pub fn score_text(declared: &str, observed: &Option<String>) -> f32 {
    let Some(obs) = observed else {
        return 0.0;
    };
    let d = normalize_text(declared);
    let o = normalize_text(obs);
    if d.is_empty() && o.is_empty() {
        return 1.0;
    }
    if d.is_empty() || o.is_empty() {
        return 0.0;
    }
    if d == o {
        return 1.0;
    }

    let substring = if o.contains(&d) || d.contains(&o) { 1.0 } else { 0.0 };
    let jaccard = jaccard_words(&d, &o);
    let lev = 1.0 - strsim::normalized_levenshtein(&d, &o) as f32;
    let lev_sim = 1.0 - lev;

    max_f32(substring, max_f32(jaccard, lev_sim))
}

/// Same machinery as [`score_text`], routed via a distinct name so call
/// sites read clearly.
pub fn score_aria_label(declared: &str, observed: &Option<String>) -> f32 {
    score_text(declared, observed)
}

/// Fraction of declared `(key, value)` pairs that exactly match (post-
/// normalization). Empty declared set → 1.0 (vacuously satisfied).
pub fn score_attributes(
    declared: &BTreeMap<String, String>,
    observed: &Option<BTreeMap<String, String>>,
) -> f32 {
    if declared.is_empty() {
        return 1.0;
    }
    let Some(obs) = observed else {
        return 0.0;
    };
    let mut matched = 0usize;
    for (k, v) in declared {
        let nk = normalize_text(k);
        let nv = normalize_text(v);
        // Look up by normalized key — observed map may have the literal key.
        let found = obs.iter().any(|(ok, ov)| {
            normalize_text(ok) == nk && normalize_text(ov) == nv
        });
        if found {
            matched += 1;
        }
    }
    matched as f32 / declared.len() as f32
}

/// 1.0 if `declared == observed`, 0.0 otherwise. The IR doesn't currently
/// expose a visibility field on `IrElementCriteria`, so this is reserved for
/// `excluded_elements` and `IrStateCondition` use cases — kept here so the
/// scoring surface is symmetric.
pub fn score_visibility(declared: bool, observed: bool) -> f32 {
    if declared == observed {
        1.0
    } else {
        0.0
    }
}

// ===========================================================================
// Private helpers
// ===========================================================================

/// `crit.text_contains` is asymmetric — declared is a *substring* of the
/// observed text (no edit-distance fallback). Used by `matches_all` only.
fn text_contains(declared: &str, observed: &Option<String>) -> bool {
    let Some(obs) = observed else { return false };
    let d = normalize_text(declared);
    let o = normalize_text(obs);
    if d.is_empty() {
        return true;
    }
    o.contains(&d)
}

/// Exact-ID match against the bare element id and every populated
/// `identifier.*` slot. Mirrors the multi-source semantics of
/// [`IndexedSnapshot::lookup_by_id`].
fn id_matches(declared: &str, el: &UIBridgeElement) -> bool {
    let d = normalize_text(declared);
    if d.is_empty() {
        return false;
    }
    if normalize_text(&el.id) == d {
        return true;
    }
    let ident = &el.identifier;
    [
        ident.html_id.as_deref(),
        ident.test_id.as_deref(),
        ident.awas_id.as_deref(),
        ident.ui_id.as_deref(),
    ]
    .iter()
    .flatten()
    .any(|s| normalize_text(s) == d)
}

fn jaccard_words(a: &str, b: &str) -> f32 {
    let aw: std::collections::HashSet<&str> = a.split_ascii_whitespace().collect();
    let bw: std::collections::HashSet<&str> = b.split_ascii_whitespace().collect();
    if aw.is_empty() && bw.is_empty() {
        return 1.0;
    }
    let inter = aw.intersection(&bw).count();
    let union = aw.union(&bw).count();
    if union == 0 {
        return 0.0;
    }
    inter as f32 / union as f32
}

#[inline]
fn max_f32(a: f32, b: f32) -> f32 {
    if a >= b {
        a
    } else {
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_types::ui_bridge::{
        ElementIdentifier, ElementRect, ElementState, UIBridgeSnapshot,
    };

    fn empty_state() -> ElementState {
        ElementState {
            visible: true,
            enabled: true,
            focused: false,
            rect: ElementRect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
                left: 0.0,
            },
            value: None,
            checked: None,
            selected_options: None,
            text_content: None,
        }
    }

    fn empty_identifier() -> ElementIdentifier {
        ElementIdentifier {
            ui_id: None,
            test_id: None,
            awas_id: None,
            html_id: None,
            xpath: String::new(),
            selector: String::new(),
        }
    }

    fn elem(
        id: &str,
        role: Option<&str>,
        tag: Option<&str>,
        aria: Option<&str>,
        accessible: Option<&str>,
        text: Option<&str>,
        identifier: ElementIdentifier,
    ) -> UIBridgeElement {
        UIBridgeElement {
            id: id.to_string(),
            element_type: "div".to_string(),
            label: None,
            actions: Vec::new(),
            custom_actions: None,
            identifier,
            state: empty_state(),
            registered_at: 0,
            mounted: true,
            bbox: None,
            visible: Some(true),
            role: role.map(String::from),
            tag_name: tag.map(String::from),
            aria_label: aria.map(String::from),
            accessible_name: accessible.map(String::from),
            text: text.map(String::from),
        }
    }

    fn snap(elements: Vec<UIBridgeElement>) -> UIBridgeSnapshot {
        UIBridgeSnapshot {
            timestamp: 0,
            elements,
            components: Vec::new(),
            workflows: Vec::new(),
            modal_stack: None,
            toasts: None,
            undo_redo: None,
            current_route: None,
            segments: Vec::new(),
        }
    }

    // -- score_role ------------------------------------------------------

    #[test]
    fn score_role_exact_match() {
        assert_eq!(score_role("button", &Some("button".into())), 1.0);
    }

    #[test]
    fn score_role_case_insensitive_exact() {
        assert_eq!(score_role("BUTTON", &Some("Button".into())), 1.0);
    }

    #[test]
    fn score_role_same_widget_category_scores_half() {
        assert_eq!(score_role("button", &Some("link".into())), 0.5);
    }

    #[test]
    fn score_role_cross_category_scores_zero() {
        assert_eq!(score_role("button", &Some("dialog".into())), 0.0);
    }

    #[test]
    fn score_role_missing_observed_is_zero() {
        assert_eq!(score_role("button", &None), 0.0);
    }

    // -- score_tag -------------------------------------------------------

    #[test]
    fn score_tag_exact_or_zero() {
        assert_eq!(score_tag("button", &Some("button".into())), 1.0);
        assert_eq!(score_tag("button", &Some("div".into())), 0.0);
        assert_eq!(score_tag("button", &None), 0.0);
        assert_eq!(score_tag("BUTTON", &Some("button".into())), 1.0); // normalization
    }

    // -- score_text ------------------------------------------------------

    #[test]
    fn score_text_exact_match() {
        assert_eq!(score_text("Save", &Some("save".into())), 1.0);
    }

    #[test]
    fn score_text_substring_signals_one() {
        // "save" is a substring of "save changes".
        assert_eq!(score_text("save", &Some("save changes".into())), 1.0);
    }

    #[test]
    fn score_text_jaccard_partial() {
        // Tokens: {"save", "changes"} vs {"save", "and", "quit"}
        // jaccard = 1 / 4 = 0.25; substring asymmetric (no containment); lev partial
        let s = score_text("save changes", &Some("save and quit".into()));
        assert!(s > 0.0 && s < 1.0, "got {s}");
    }

    #[test]
    fn score_text_missing_observed() {
        assert_eq!(score_text("save", &None), 0.0);
    }

    #[test]
    fn score_text_both_empty_is_one() {
        assert_eq!(score_text("", &Some("".into())), 1.0);
    }

    // -- score_aria_label -----------------------------------------------

    #[test]
    fn score_aria_label_delegates_to_score_text() {
        assert_eq!(score_aria_label("Save", &Some("save".into())), 1.0);
        assert_eq!(score_aria_label("save", &None), 0.0);
    }

    // -- score_attributes -----------------------------------------------

    #[test]
    fn score_attributes_empty_declared_is_one() {
        let declared: BTreeMap<String, String> = BTreeMap::new();
        assert_eq!(score_attributes(&declared, &None), 1.0);
        assert_eq!(score_attributes(&declared, &Some(BTreeMap::new())), 1.0);
    }

    #[test]
    fn score_attributes_full_match() {
        let mut declared = BTreeMap::new();
        declared.insert("type".to_string(), "button".to_string());
        declared.insert("disabled".to_string(), "false".to_string());

        let mut observed = BTreeMap::new();
        observed.insert("type".to_string(), "button".to_string());
        observed.insert("disabled".to_string(), "false".to_string());

        assert_eq!(score_attributes(&declared, &Some(observed)), 1.0);
    }

    #[test]
    fn score_attributes_partial_fraction() {
        let mut declared = BTreeMap::new();
        declared.insert("type".to_string(), "button".to_string());
        declared.insert("disabled".to_string(), "false".to_string());

        let mut observed = BTreeMap::new();
        observed.insert("type".to_string(), "button".to_string());
        observed.insert("disabled".to_string(), "true".to_string()); // mismatch

        assert_eq!(score_attributes(&declared, &Some(observed)), 0.5);
    }

    #[test]
    fn score_attributes_none_observed_is_zero() {
        let mut declared = BTreeMap::new();
        declared.insert("type".to_string(), "button".to_string());
        assert_eq!(score_attributes(&declared, &None), 0.0);
    }

    // -- score_visibility -----------------------------------------------

    #[test]
    fn score_visibility_exact() {
        assert_eq!(score_visibility(true, true), 1.0);
        assert_eq!(score_visibility(false, false), 1.0);
        assert_eq!(score_visibility(true, false), 0.0);
    }

    // -- matches_all -----------------------------------------------------

    #[test]
    fn matches_all_with_no_declared_fields_matches_any_element() {
        let s = snap(vec![elem("e0", Some("button"), None, None, None, None, empty_identifier())]);
        let idx = IndexedSnapshot::new(&s);
        let crit = IrElementCriteria::default();
        assert!(matches_all(&idx, &crit, ElementIdx(0)));
    }

    #[test]
    fn matches_all_role_only() {
        let s = snap(vec![
            elem("e0", Some("button"), None, None, None, None, empty_identifier()),
            elem("e1", Some("link"), None, None, None, None, empty_identifier()),
        ]);
        let idx = IndexedSnapshot::new(&s);
        let crit = IrElementCriteria {
            role: Some("button".to_string()),
            ..Default::default()
        };
        assert!(matches_all(&idx, &crit, ElementIdx(0)));
        assert!(!matches_all(&idx, &crit, ElementIdx(1)));
    }

    #[test]
    fn matches_all_text_contains() {
        let s = snap(vec![
            elem("e0", None, None, None, None, Some("Save changes and quit"), empty_identifier()),
            elem("e1", None, None, None, None, Some("Cancel"), empty_identifier()),
        ]);
        let idx = IndexedSnapshot::new(&s);
        let crit = IrElementCriteria {
            text_contains: Some("changes".to_string()),
            ..Default::default()
        };
        assert!(matches_all(&idx, &crit, ElementIdx(0)));
        assert!(!matches_all(&idx, &crit, ElementIdx(1)));
    }

    #[test]
    fn matches_all_id_via_identifier_slot() {
        let mut ident = empty_identifier();
        ident.html_id = Some("save-btn".to_string());

        let s = snap(vec![elem(
            "internal-id-1",
            Some("button"),
            None,
            None,
            None,
            None,
            ident,
        )]);
        let idx = IndexedSnapshot::new(&s);

        let crit = IrElementCriteria {
            id: Some("save-btn".to_string()),
            ..Default::default()
        };
        assert!(matches_all(&idx, &crit, ElementIdx(0)));

        let crit_bare = IrElementCriteria {
            id: Some("internal-id-1".to_string()),
            ..Default::default()
        };
        assert!(matches_all(&idx, &crit_bare, ElementIdx(0)));

        let crit_miss = IrElementCriteria {
            id: Some("nope".to_string()),
            ..Default::default()
        };
        assert!(!matches_all(&idx, &crit_miss, ElementIdx(0)));
    }

    #[test]
    fn matches_all_requires_every_some_field() {
        // role matches, text doesn't → matches_all = false.
        let s = snap(vec![elem(
            "e0",
            Some("button"),
            None,
            None,
            None,
            Some("Cancel"),
            empty_identifier(),
        )]);
        let idx = IndexedSnapshot::new(&s);
        let crit = IrElementCriteria {
            role: Some("button".to_string()),
            text: Some("Save".to_string()),
            ..Default::default()
        };
        assert!(!matches_all(&idx, &crit, ElementIdx(0)));
    }

    // -- narrow_candidates ----------------------------------------------

    #[test]
    fn narrow_by_id_returns_unique_match() {
        let mut ident = empty_identifier();
        ident.html_id = Some("save-1".to_string());

        let s = snap(vec![
            elem("e0", Some("button"), None, None, None, None, ident),
            elem("e1", Some("button"), None, None, None, None, empty_identifier()),
        ]);
        let idx = IndexedSnapshot::new(&s);
        let crit = IrElementCriteria {
            id: Some("save-1".to_string()),
            ..Default::default()
        };

        assert_eq!(narrow_candidates(&idx, &crit), vec![ElementIdx(0)]);
    }

    #[test]
    fn narrow_by_nonexistent_id_returns_empty() {
        let s = snap(vec![elem("e0", Some("button"), None, None, None, None, empty_identifier())]);
        let idx = IndexedSnapshot::new(&s);
        let crit = IrElementCriteria {
            id: Some("nonexistent".to_string()),
            role: Some("button".to_string()), // shouldn't fall back to this
            ..Default::default()
        };
        assert!(narrow_candidates(&idx, &crit).is_empty());
    }

    #[test]
    fn narrow_by_aria_label() {
        let s = snap(vec![
            elem("e0", Some("button"), None, Some("Save"), None, None, empty_identifier()),
            elem("e1", Some("button"), None, Some("Cancel"), None, None, empty_identifier()),
        ]);
        let idx = IndexedSnapshot::new(&s);
        let crit = IrElementCriteria {
            aria_label: Some("save".to_string()),
            ..Default::default()
        };
        assert_eq!(narrow_candidates(&idx, &crit), vec![ElementIdx(0)]);
    }

    #[test]
    fn narrow_by_role_and_text_intersects() {
        let s = snap(vec![
            // role match but no "save" word
            elem("e0", Some("button"), None, None, None, Some("Cancel"), empty_identifier()),
            // role match + "save" → candidate
            elem("e1", Some("button"), None, None, None, Some("Save changes"), empty_identifier()),
            // wrong role
            elem("e2", Some("link"), None, None, None, Some("Save link"), empty_identifier()),
        ]);
        let idx = IndexedSnapshot::new(&s);
        let crit = IrElementCriteria {
            role: Some("button".to_string()),
            text: Some("Save".to_string()),
            ..Default::default()
        };
        assert_eq!(narrow_candidates(&idx, &crit), vec![ElementIdx(1)]);
    }

    #[test]
    fn narrow_by_role_alone() {
        let s = snap(vec![
            elem("e0", Some("button"), None, None, None, None, empty_identifier()),
            elem("e1", Some("link"), None, None, None, None, empty_identifier()),
        ]);
        let idx = IndexedSnapshot::new(&s);
        let crit = IrElementCriteria {
            role: Some("button".to_string()),
            ..Default::default()
        };
        assert_eq!(narrow_candidates(&idx, &crit), vec![ElementIdx(0)]);
    }

    #[test]
    fn narrow_falls_back_to_all_when_no_fields() {
        let s = snap(vec![
            elem("e0", None, None, None, None, None, empty_identifier()),
            elem("e1", None, None, None, None, None, empty_identifier()),
        ]);
        let idx = IndexedSnapshot::new(&s);
        let crit = IrElementCriteria::default();
        let candidates = narrow_candidates(&idx, &crit);
        assert_eq!(candidates.len(), 2);
    }

    // -- property: matches_all → score = 1.0 for every Some field ------

    #[test]
    fn property_matches_all_implies_score_one_per_field() {
        let mut ident = empty_identifier();
        ident.html_id = Some("save-btn".to_string());

        let s = snap(vec![elem(
            "e0",
            Some("button"),
            Some("button"),
            Some("Save"),
            Some("Save document"),
            Some("Save changes now"),
            ident,
        )]);
        let idx = IndexedSnapshot::new(&s);

        let crit = IrElementCriteria {
            role: Some("button".to_string()),
            tag_name: Some("button".to_string()),
            aria_label: Some("Save".to_string()),
            accessible_name: Some("Save document".to_string()),
            text_contains: Some("changes".to_string()),
            id: Some("save-btn".to_string()),
            ..Default::default()
        };

        // Phase 1 pass.
        assert!(matches_all(&idx, &crit, ElementIdx(0)));

        // For every Some field in `crit`, the corresponding score_* must be 1.0.
        let el = idx.element(ElementIdx(0));
        if let Some(ref role) = crit.role {
            assert_eq!(score_role(role, &el.role), 1.0, "role");
        }
        if let Some(ref tag) = crit.tag_name {
            assert_eq!(score_tag(tag, &el.tag_name), 1.0, "tag");
        }
        if let Some(ref label) = crit.aria_label {
            assert_eq!(score_aria_label(label, &el.aria_label), 1.0, "aria_label");
        }
        if let Some(ref name) = crit.accessible_name {
            assert_eq!(score_text(name, &el.accessible_name), 1.0, "accessible_name");
        }
    }

    // -- property test (proptest) — randomized variant of the above -----

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn proptest_matches_all_implies_score_one(
            role in "[a-z]{3,10}",
            tag in "[a-z]{2,8}",
            aria in "[A-Za-z ]{2,15}",
            text in "[A-Za-z ]{3,30}",
        ) {
            // Build a snapshot containing exactly the element we'll point at.
            let s = snap(vec![elem(
                "e0",
                Some(&role),
                Some(&tag),
                Some(&aria),
                Some(&aria),
                Some(&text),
                empty_identifier(),
            )]);
            let idx = IndexedSnapshot::new(&s);
            let crit = IrElementCriteria {
                role: Some(role.clone()),
                tag_name: Some(tag.clone()),
                aria_label: Some(aria.clone()),
                accessible_name: Some(aria.clone()),
                text: Some(text.clone()),
                ..Default::default()
            };

            // matches_all must hold for this construction-by-mirror.
            prop_assert!(matches_all(&idx, &crit, ElementIdx(0)));

            // Each Some-field's score_* must be 1.0.
            let el = idx.element(ElementIdx(0));
            prop_assert_eq!(score_role(&role, &el.role), 1.0);
            prop_assert_eq!(score_tag(&tag, &el.tag_name), 1.0);
            prop_assert_eq!(score_aria_label(&aria, &el.aria_label), 1.0);
            prop_assert_eq!(score_text(&aria, &el.accessible_name), 1.0);
            prop_assert_eq!(score_text(&text, &el.text), 1.0);
        }
    }
}

// ===========================================================================
// Misc utilities (private to crate, but `pub(crate)` so evaluator can reuse)
// ===========================================================================

/// Intersect two slice-views of `ElementIdx`, returning the elements that
/// appear in both. Linear in `|a| + |b|` after a copy + sort of `b`
/// (typically `b` is the role hit list — small constant factors).
pub(crate) fn intersect_sorted_unique(a: &[ElementIdx], b: &[ElementIdx]) -> Vec<ElementIdx> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    // Both posting lists are produced in insertion-order (the natural
    // element index order) by `IndexedSnapshot::new`, so they're already
    // sorted ascending — verify defensively before two-pointer merge.
    let sorted_a = is_sorted(a);
    let sorted_b = is_sorted(b);

    if sorted_a && sorted_b {
        let mut out = Vec::with_capacity(a.len().min(b.len()));
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            match a[i].cmp(&b[j]) {
                std::cmp::Ordering::Equal => {
                    if out.last() != Some(&a[i]) {
                        out.push(a[i]);
                    }
                    i += 1;
                    j += 1;
                }
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
            }
        }
        out
    } else {
        let set: std::collections::HashSet<ElementIdx> = a.iter().copied().collect();
        let mut out: Vec<ElementIdx> = b.iter().copied().filter(|x| set.contains(x)).collect();
        out.sort();
        out.dedup();
        out
    }
}

fn is_sorted(v: &[ElementIdx]) -> bool {
    v.windows(2).all(|w| w[0] <= w[1])
}
