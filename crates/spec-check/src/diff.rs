//! Selectivity index + top-N miss diagnostics. Step 5 of Plan 02.
//!
//! Two responsibilities:
//!
//! 1. [`SelectivityIndex`] — TF-IDF-inspired per-(field, value) weights
//!    computed once from an [`IndexedSnapshot`]. Rare field-values weight
//!    high; common values weight low. Plain `+1`-smoothed IDF over the
//!    document set (`document := snapshot element`): `log((N + 1) /
//!    (count_of_value + 1))`.
//!
//! 2. [`compute_miss`] — for a single Phase-1-failed criterion, scores every
//!    candidate element via [`score_element_against`], normalizes by total
//!    declared weight so 5-field criteria don't dominate 2-field ones,
//!    returns the top 5 in an [`AssertionMiss`] with structured per-field
//!    diffs and a coarse [`MissReason`].
//!
//! Wire-shape contract (`spec_check.rs:249-327`):
//!
//! - [`AssertionMiss`] = `{ reason, candidates }` — no `criterion` field.
//! - [`CandidateMiss`] = `{ element_id?, role?, text?, path, field_diffs, score }`.
//! - [`FieldDiff`] = `{ field, expected, actual, similarity }` — **no
//!   `weight`**. Selectivity weights are matcher-internal; stashed in a
//!   private `(FieldDiff, f32)` tuple inside the scoring loop and never
//!   serialized.
//! - [`MissReason`] is a unit enum — payloads live on `CandidateMiss`.

use std::collections::{BTreeMap, HashMap};

use qontinui_types::ir::IrElementCriteria;
use qontinui_types::text_norm::normalize_text;
use qontinui_types::ui_bridge::UIBridgeElement;

use crate::criteria::{
    score_aria_label, score_attributes, score_role, score_tag, score_text, score_visibility,
};
use crate::derive;
use crate::snapshot::{ElementIdx, IndexedSnapshot};
use crate::{AssertionMiss, CandidateMiss, FieldDiff, MissReason};

// ===========================================================================
// Constants
// ===========================================================================

/// Cap on the number of candidates returned by [`compute_miss`] per assertion.
pub const MAX_CANDIDATES: usize = 5;

/// Cut-off below which the best scoring candidate is treated as "nothing
/// scored above the floor" → [`MissReason::NoCandidates`].
pub const NO_CANDIDATES_SCORE_FLOOR: f32 = 0.3;

// ===========================================================================
// SelectivityIndex
// ===========================================================================

/// Per-field selectivity weights derived from the snapshot.
///
/// For each (field, value), `weight = log((N + 1) / (count_of_value + 1))`
/// — plain `+1`-smoothed inverse document frequency. The `id` field is
/// treated as max-rare (`log(N + 1)`).
///
/// Computed from the snapshot — never from the spec — so the same snapshot
/// shared across many specs (the `evaluate_batch` hot path) only pays the
/// build cost once.
pub struct SelectivityIndex {
    role_weights: HashMap<String, f32>,
    tag_weights: HashMap<String, f32>,
    aria_label_weights: HashMap<String, f32>,
    /// Per-word weights over the *normalized* word stream. Common stopwords
    /// ("the", "and") get near-zero weight automatically because they appear
    /// in many elements.
    text_word_weights: HashMap<String, f32>,
    /// IDs are meant to be unique — always max-weight per the plan.
    id_weight: f32,
    /// Per-attribute-pair weights (`"key=value"`) over declared attributes
    /// in the snapshot. Reserved for the day `UIBridgeElement` exposes a
    /// generic attribute map; until then this is the empty map and lookups
    /// fall back to `default_value_weight`.
    attribute_weights: HashMap<String, f32>,
    /// Fallback weight for unseen values — `log((N + 1) / 1)` (treats the
    /// value as if it's about to be observed for the first time). Ensures a
    /// declared criterion that doesn't appear anywhere in the snapshot
    /// still has a non-trivial influence on the score.
    default_value_weight: f32,
}

impl SelectivityIndex {
    /// Build the selectivity index in O(N·F) over the indexed snapshot
    /// (N = element count, F = number of indexed fields). Iterates every
    /// element once; per-field counts are merged into hashmaps without
    /// allocation in the hot loop.
    ///
    /// Weight formula: `1.0 + ln((N + 1) / (count_of_value + 1))`. The +1.0
    /// baseline ensures the dullest common value still carries a unit of
    /// weight (so a perfect-match candidate always normalizes to 1.0); the
    /// log term provides the IDF rarity boost on top.
    pub fn build(indexed: &IndexedSnapshot) -> Self {
        let n = indexed.element_count();
        let n_plus_one = (n as f32 + 1.0).max(2.0); // floor at 2 so log > 0 for n=1

        // Unseen values: treat count as 0 → 1.0 + ln((N+1)/1).
        let default_value_weight = 1.0 + n_plus_one.ln();

        // Per-element value counts. We collect into hashmaps first then
        // transform to weights in a second pass so we don't pay log() inside
        // the hot loop.
        let mut role_counts: HashMap<String, u32> = HashMap::new();
        let mut tag_counts: HashMap<String, u32> = HashMap::new();
        let mut aria_label_counts: HashMap<String, u32> = HashMap::new();
        let mut text_word_counts: HashMap<String, u32> = HashMap::new();

        // Selectivity weights must reflect the same value-space the matcher
        // operates on — use the derive layer so a `{role: heading}` criterion
        // sees the IDF weight of the *derived* role population, not the
        // top-level (almost-always-None) `el.role` distribution.
        for (_idx, el) in indexed.iter_elements() {
            if let Some(role) = derive::role(el) {
                let key = normalize_text(&role);
                if !key.is_empty() {
                    *role_counts.entry(key).or_insert(0) += 1;
                }
            }
            if let Some(tag) = el.tag_name.as_deref() {
                let key = normalize_text(tag);
                if !key.is_empty() {
                    *tag_counts.entry(key).or_insert(0) += 1;
                }
            }
            if let Some(label) = derive::aria_label(el) {
                let key = normalize_text(&label);
                if !key.is_empty() {
                    *aria_label_counts.entry(key).or_insert(0) += 1;
                }
            }
            if let Some(text) = derive::text(el) {
                let normalized = normalize_text(&text);
                // Per-element dedup so an element with repeated tokens
                // doesn't inflate the count for its own posting.
                let mut seen_in_el: std::collections::HashSet<&str> =
                    std::collections::HashSet::new();
                for word in normalized.split_ascii_whitespace() {
                    if word.is_empty() || !seen_in_el.insert(word) {
                        continue;
                    }
                    *text_word_counts.entry(word.to_string()).or_insert(0) += 1;
                }
            }
        }

        Self {
            role_weights: weights_from_counts(&role_counts, n_plus_one),
            tag_weights: weights_from_counts(&tag_counts, n_plus_one),
            aria_label_weights: weights_from_counts(&aria_label_counts, n_plus_one),
            text_word_weights: weights_from_counts(&text_word_counts, n_plus_one),
            id_weight: 1.0 + n_plus_one.ln(),
            attribute_weights: HashMap::new(),
            default_value_weight,
        }
    }

    /// Aggregate weight of every declared field in `crit`. Used as the
    /// denominator when normalizing the per-element score so 5-field
    /// criteria don't dominate 2-field ones.
    pub fn total_declared_weight(&self, crit: &IrElementCriteria) -> f32 {
        let mut total = 0.0;
        if let Some(role) = crit.role.as_deref() {
            total += self.weight_for_role(role);
        }
        if let Some(tag) = crit.tag_name.as_deref() {
            total += self.weight_for_tag(tag);
        }
        if let Some(label) = crit.aria_label.as_deref() {
            total += self.weight_for_aria_label(label);
        }
        if let Some(name) = crit.accessible_name.as_deref() {
            // accessible_name shares the text-token weight pool — sum the
            // per-word weights of the declared phrase.
            total += self.weight_for_text_phrase(name);
        }
        if let Some(text) = crit.text.as_deref() {
            total += self.weight_for_text_phrase(text);
        }
        if let Some(needle) = crit.text_contains.as_deref() {
            total += self.weight_for_text_phrase(needle);
        }
        if crit.id.is_some() {
            total += self.id_weight;
        }
        if let Some(declared_attrs) = crit.attributes.as_ref() {
            for (k, v) in declared_attrs {
                total += self.weight_for_attribute(k, v);
            }
        }
        total
    }

    pub fn weight_for_role(&self, role: &str) -> f32 {
        self.role_weights
            .get(&normalize_text(role))
            .copied()
            .unwrap_or(self.default_value_weight)
    }

    pub fn weight_for_tag(&self, tag: &str) -> f32 {
        self.tag_weights
            .get(&normalize_text(tag))
            .copied()
            .unwrap_or(self.default_value_weight)
    }

    pub fn weight_for_aria_label(&self, label: &str) -> f32 {
        self.aria_label_weights
            .get(&normalize_text(label))
            .copied()
            .unwrap_or(self.default_value_weight)
    }

    pub fn weight_for_text_word(&self, word: &str) -> f32 {
        let key = normalize_text(word);
        if key.is_empty() {
            return 0.0;
        }
        self.text_word_weights
            .get(&key)
            .copied()
            .unwrap_or(self.default_value_weight)
    }

    /// Sum of per-word weights over the normalized whitespace-tokenized
    /// phrase. Used for `text` / `text_contains` / `accessible_name` — the
    /// phrase's selectivity is the sum of its parts' selectivities.
    pub fn weight_for_text_phrase(&self, phrase: &str) -> f32 {
        let normalized = normalize_text(phrase);
        let mut total = 0.0;
        for w in normalized.split_ascii_whitespace() {
            total += self.weight_for_text_word(w);
        }
        total
    }

    pub fn weight_for_attribute(&self, key: &str, value: &str) -> f32 {
        let composite = format!("{}={}", normalize_text(key), normalize_text(value));
        self.attribute_weights
            .get(&composite)
            .copied()
            .unwrap_or(self.default_value_weight)
    }

    pub fn id_weight(&self) -> f32 {
        self.id_weight
    }
}

fn weights_from_counts(counts: &HashMap<String, u32>, n_plus_one: f32) -> HashMap<String, f32> {
    counts
        .iter()
        .map(|(k, &c)| (k.clone(), 1.0 + (n_plus_one / (c as f32 + 1.0)).ln()))
        .collect()
}

// ===========================================================================
// compute_miss — top-N candidate diagnostics
// ===========================================================================

/// Build the Phase-2 miss diagnostic for a Phase-1-failed criterion.
///
/// Iterates every element via [`IndexedSnapshot::iter_elements`], scores it
/// against the criterion using [`score_element_against`], normalizes the
/// score by [`SelectivityIndex::total_declared_weight`], sorts descending
/// (with element-path tiebreak for determinism), truncates to N=5, and
/// classifies the miss reason.
pub fn compute_miss(indexed: &IndexedSnapshot, crit: &IrElementCriteria) -> AssertionMiss {
    let selectivity = indexed.selectivity();
    let total_weight = selectivity.total_declared_weight(crit).max(1e-6);

    // Score every element. `scored[i] = (idx, normalized_score, diffs)`.
    let mut scored: Vec<(ElementIdx, f32, Vec<FieldDiff>)> = indexed
        .iter_elements()
        .map(|(idx, el)| {
            let (raw_score, diffs) = score_element_against(crit, el, selectivity);
            let normalized = (raw_score / total_weight).clamp(0.0, 1.0);
            (idx, normalized, diffs)
        })
        .collect();

    // Sort descending by score; tiebreak by element_path ascending for
    // determinism (§5.4 acceptance bullet). The path is the wire-stable
    // CSS selector, always populated by the SDK.
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| path_of(indexed, a.0).cmp(&path_of(indexed, b.0)))
    });

    // Classify before truncation so visibility-mismatch detection sees the
    // best raw scoring candidate even if the floor truncates it.
    let reason = classify_miss(&scored, crit, indexed);

    scored.truncate(MAX_CANDIDATES);

    // Drop sub-floor candidates entirely — surfacing 5 low-confidence
    // matches when nothing scored above 0.3 is misleading.
    let candidates: Vec<CandidateMiss> = scored
        .into_iter()
        .filter(|(_, score, _)| *score >= NO_CANDIDATES_SCORE_FLOOR)
        .map(|(idx, score, diffs)| into_candidate_miss(indexed, idx, score, diffs))
        .collect();

    AssertionMiss { reason, candidates }
}

/// Score one element against a criterion, returning the raw selectivity-
/// weighted sum **plus** the per-field diffs (only emitted for fields whose
/// similarity is below 1.0 — perfect-match fields aren't a "diff").
///
/// The returned score is in `[0, total_declared_weight]`. The caller
/// normalizes by `total_declared_weight` to map onto `[0, 1]`.
pub(crate) fn score_element_against(
    crit: &IrElementCriteria,
    el: &UIBridgeElement,
    sel: &SelectivityIndex,
) -> (f32, Vec<FieldDiff>) {
    let mut score = 0.0;
    let mut diffs: Vec<FieldDiff> = Vec::new();

    if let Some(role) = crit.role.as_deref() {
        let observed = derive::role(el);
        let sim = score_role(role, &observed);
        let w = sel.weight_for_role(role);
        score += sim * w;
        if sim < 1.0 {
            diffs.push(FieldDiff {
                field: "role".to_string(),
                expected: serde_json::Value::String(role.to_string()),
                actual: observed
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null),
                similarity: sim,
            });
        }
    }

    if let Some(tag) = crit.tag_name.as_deref() {
        let sim = score_tag(tag, &el.tag_name);
        let w = sel.weight_for_tag(tag);
        score += sim * w;
        if sim < 1.0 {
            diffs.push(FieldDiff {
                field: "tagName".to_string(),
                expected: serde_json::Value::String(tag.to_string()),
                actual: el
                    .tag_name
                    .as_ref()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .unwrap_or(serde_json::Value::Null),
                similarity: sim,
            });
        }
    }

    if let Some(text) = crit.text.as_deref() {
        let observed = derive::text(el);
        let sim = score_text(text, &observed);
        let w = sel.weight_for_text_phrase(text);
        score += sim * w;
        if sim < 1.0 {
            diffs.push(FieldDiff {
                field: "text".to_string(),
                expected: serde_json::Value::String(text.to_string()),
                actual: observed
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null),
                similarity: sim,
            });
        }
    }

    if let Some(needle) = crit.text_contains.as_deref() {
        // text_contains is asymmetric — emit 1.0 if the observed text
        // contains the needle, else use score_text similarity for the diff.
        let observed = derive::text(el);
        let observed_lower = observed
            .as_deref()
            .map(normalize_text)
            .unwrap_or_default();
        let needle_lower = normalize_text(needle);
        let contains = !needle_lower.is_empty() && observed_lower.contains(&needle_lower);
        let sim = if contains { 1.0 } else { score_text(needle, &observed) };
        let w = sel.weight_for_text_phrase(needle);
        score += sim * w;
        if sim < 1.0 {
            diffs.push(FieldDiff {
                field: "textContains".to_string(),
                expected: serde_json::Value::String(needle.to_string()),
                actual: observed
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null),
                similarity: sim,
            });
        }
    }

    if let Some(label) = crit.aria_label.as_deref() {
        let observed = derive::aria_label(el);
        let sim = score_aria_label(label, &observed);
        let w = sel.weight_for_aria_label(label);
        score += sim * w;
        if sim < 1.0 {
            diffs.push(FieldDiff {
                field: "ariaLabel".to_string(),
                expected: serde_json::Value::String(label.to_string()),
                actual: observed
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null),
                similarity: sim,
            });
        }
    }

    if let Some(name) = crit.accessible_name.as_deref() {
        let observed = derive::accessible_name(el);
        let sim = score_text(name, &observed);
        let w = sel.weight_for_text_phrase(name);
        score += sim * w;
        if sim < 1.0 {
            diffs.push(FieldDiff {
                field: "accessibleName".to_string(),
                expected: serde_json::Value::String(name.to_string()),
                actual: observed
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null),
                similarity: sim,
            });
        }
    }

    if let Some(id) = crit.id.as_deref() {
        let sim = if id_matches_element(id, el) { 1.0 } else { 0.0 };
        let w = sel.id_weight();
        score += sim * w;
        if sim < 1.0 {
            diffs.push(FieldDiff {
                field: "id".to_string(),
                expected: serde_json::Value::String(id.to_string()),
                actual: serde_json::Value::String(el.id.clone()),
                similarity: sim,
            });
        }
    }

    if let Some(declared_attrs) = crit.attributes.as_ref() {
        // UIBridgeElement doesn't expose a generic attribute map; treat
        // observed as None, so `score_attributes` falls through to 0.0
        // (or 1.0 when declared is empty). Per-attribute diffs are emitted
        // individually so the AI debugging UI can see exactly which keys
        // didn't match.
        let observed: Option<BTreeMap<String, String>> = None;
        let sim = score_attributes(declared_attrs, &observed);
        let aggregated_weight: f32 = declared_attrs
            .iter()
            .map(|(k, v)| sel.weight_for_attribute(k, v))
            .sum();
        score += sim * aggregated_weight;
        if sim < 1.0 {
            for (k, v) in declared_attrs {
                diffs.push(FieldDiff {
                    field: format!("attribute:{k}"),
                    expected: serde_json::Value::String(v.clone()),
                    actual: serde_json::Value::Null,
                    similarity: 0.0,
                });
            }
        }
    }

    // Visibility is a synthetic field — not on `IrElementCriteria` directly,
    // but the wire `MissReason::VisibilityMismatch` exists precisely to
    // call out elements that scored well on every declared field yet
    // weren't visible. Emit a diff (no weight, since `crit` doesn't
    // declare visibility) so policy UIs can see why a high-score
    // candidate still failed.
    if !el.state.visible {
        // Only emit when the criterion otherwise matched perfectly —
        // i.e. no diffs above except (possibly) attributes. This keeps
        // VisibilityMismatch from polluting routine partial-match misses.
        if diffs.is_empty() {
            let sim = score_visibility(true, false);
            diffs.push(FieldDiff {
                field: "state.visible".to_string(),
                expected: serde_json::Value::Bool(true),
                actual: serde_json::Value::Bool(false),
                similarity: sim,
            });
        }
    }

    (score, diffs)
}

/// Classify the dominant miss reason for a sorted-by-score `scored` slice.
///
/// Priority order when the best candidate has multiple divergent fields:
/// **Role > Text > Visibility > Attribute**. Single-field divergence
/// short-circuits to that field's [`MissReason`]. Empty slice or sub-floor
/// best → [`MissReason::NoCandidates`]. ≥2 candidates tied at the top score
/// → [`MissReason::MultipleMatches`].
fn classify_miss(
    scored: &[(ElementIdx, f32, Vec<FieldDiff>)],
    _crit: &IrElementCriteria,
    _indexed: &IndexedSnapshot,
) -> MissReason {
    if scored.is_empty() {
        return MissReason::NoCandidates;
    }

    let best = &scored[0];
    if best.1 < NO_CANDIDATES_SCORE_FLOOR {
        return MissReason::NoCandidates;
    }

    // MultipleMatches: 2+ candidates tied at the top score AND the top
    // candidate has no field diffs (i.e. they'd all match-all per Phase 1,
    // but the assertion expected a unique target). If the top candidate
    // has diffs, then "tied at top" just means tied-near-the-best, not
    // tied-perfect-match — fall through to single-field classification.
    if scored.len() >= 2 {
        let second = &scored[1];
        let tied = (best.1 - second.1).abs() < f32::EPSILON;
        if tied && best.2.is_empty() {
            return MissReason::MultipleMatches;
        }
    }

    // Single-field divergence on the best candidate's diffs.
    let dominant = dominant_diff_kind(&best.2);
    match dominant {
        Some(DiffKind::Role) => MissReason::RoleMismatch,
        Some(DiffKind::Text) => MissReason::TextMismatch,
        Some(DiffKind::Visibility) => MissReason::VisibilityMismatch,
        Some(DiffKind::Attribute) => MissReason::AttributeMismatch,
        // No diffs at all but score < 1.0 and no tie → fall back to
        // NoCandidates (everything we declared matched, but the score is
        // still below 1.0 because of unweighted normalization; nothing
        // useful to report).
        None => MissReason::NoCandidates,
    }
}

/// Coarse classification of a [`FieldDiff`]. Used to pick the dominant
/// miss reason via priority order `Role > Text > Visibility > Attribute`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum DiffKind {
    Role,
    Text,
    Visibility,
    Attribute,
}

fn classify_field(field: &str) -> DiffKind {
    // Match by prefix so `attribute:data-state` and friends route correctly.
    if field == "role" {
        DiffKind::Role
    } else if field == "text" || field == "textContains" || field == "ariaLabel" || field == "accessibleName" {
        DiffKind::Text
    } else if field == "state.visible" {
        DiffKind::Visibility
    } else if field.starts_with("attribute:") || field == "tagName" || field == "id" {
        // tagName/id surface here too — they're not in the wire-enum's
        // primary classification (role / text / visibility / attribute),
        // so we lump them with the generic `AttributeMismatch` bucket
        // (the lowest-priority surface that's still informative).
        DiffKind::Attribute
    } else {
        DiffKind::Attribute
    }
}

/// Pick the dominant diff kind via the priority order
/// `Role > Text > Visibility > Attribute`. Returns `None` for an empty
/// slice.
fn dominant_diff_kind(diffs: &[FieldDiff]) -> Option<DiffKind> {
    if diffs.is_empty() {
        return None;
    }
    let mut best: Option<DiffKind> = None;
    for d in diffs {
        let k = classify_field(&d.field);
        best = match best {
            None => Some(k),
            Some(prev) if k < prev => Some(k),
            _ => best,
        };
    }
    best
}

fn into_candidate_miss(
    indexed: &IndexedSnapshot,
    idx: ElementIdx,
    score: f32,
    field_diffs: Vec<FieldDiff>,
) -> CandidateMiss {
    let el = indexed.element(idx);
    CandidateMiss {
        element_id: Some(el.id.clone()).filter(|s| !s.is_empty()),
        // Surface derived values for parity with the matcher's effective view
        // — top-level `el.role` / `el.text` are almost always None on the
        // live wire (the SDK populates `tag_name` / `state.text_content`
        // instead). See `evaluator::matched_from` for the same reasoning.
        role: derive::role(el).filter(|s| !s.is_empty()),
        text: derive::text(el).filter(|s| !s.is_empty()),
        path: el.identifier.selector.clone(),
        field_diffs,
        score,
    }
}

fn path_of(indexed: &IndexedSnapshot, idx: ElementIdx) -> String {
    indexed.element(idx).identifier.selector.clone()
}

/// Inlined ID-match mirroring `criteria::id_matches` (private there).
fn id_matches_element(declared: &str, el: &UIBridgeElement) -> bool {
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

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_types::ui_bridge::{
        ElementIdentifier, ElementRect, ElementState, UIBridgeElement, UIBridgeSnapshot,
    };

    // ----- helpers -----

    fn state(visible: bool) -> ElementState {
        ElementState {
            visible,
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

    fn ident(selector: &str) -> ElementIdentifier {
        ElementIdentifier {
            ui_id: None,
            test_id: None,
            awas_id: None,
            html_id: None,
            xpath: String::new(),
            selector: selector.to_string(),
        }
    }

    fn elem(
        id: &str,
        selector: &str,
        role: Option<&str>,
        tag: Option<&str>,
        aria: Option<&str>,
        accessible: Option<&str>,
        text: Option<&str>,
        visible: bool,
    ) -> UIBridgeElement {
        UIBridgeElement {
            id: id.to_string(),
            element_type: "div".to_string(),
            label: None,
            actions: Vec::new(),
            custom_actions: None,
            identifier: ident(selector),
            state: state(visible),
            registered_at: 0,
            mounted: true,
            bbox: None,
            visible: Some(visible),
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

    // ----- SelectivityIndex -----

    #[test]
    fn selectivity_rare_role_outweighs_common() {
        // 9 buttons + 1 dialog → "dialog" should be much more selective.
        let mut elements = Vec::new();
        for i in 0..9 {
            elements.push(elem(
                &format!("e{i}"),
                &format!("#e{i}"),
                Some("button"),
                None,
                None,
                None,
                None,
                true,
            ));
        }
        elements.push(elem("dlg", "#dlg", Some("dialog"), None, None, None, None, true));

        let s = snap(elements);
        let idx = IndexedSnapshot::new(&s);
        let sel = SelectivityIndex::build(&idx);

        let w_button = sel.weight_for_role("button");
        let w_dialog = sel.weight_for_role("dialog");
        assert!(
            w_dialog > w_button,
            "dialog ({w_dialog}) must outweigh button ({w_button})"
        );
    }

    #[test]
    fn selectivity_id_weight_is_max() {
        let s = snap(vec![elem(
            "only", "#only", Some("button"), None, None, None, None, true,
        )]);
        let idx = IndexedSnapshot::new(&s);
        let sel = SelectivityIndex::build(&idx);

        // For N=1, id_weight = 1.0 + ln(2) ≈ 1.693
        let expected = 1.0 + (1.0_f32 + 1.0).max(2.0).ln();
        assert!(
            (sel.id_weight() - expected).abs() < 1e-5,
            "id_weight={}, expected≈{}",
            sel.id_weight(),
            expected
        );
    }

    #[test]
    fn selectivity_empty_snapshot_does_not_panic() {
        let s = snap(vec![]);
        let idx = IndexedSnapshot::new(&s);
        let sel = SelectivityIndex::build(&idx);
        // No crash; default weight is positive.
        assert!(sel.default_value_weight > 0.0);
        // Unknown role uses default.
        assert!(sel.weight_for_role("button") > 0.0);
    }

    #[test]
    fn total_declared_weight_aggregates_across_fields() {
        let s = snap(vec![elem(
            "e0", "#e0", Some("button"), None, None, None, Some("hello"), true,
        )]);
        let idx = IndexedSnapshot::new(&s);
        let sel = SelectivityIndex::build(&idx);

        let crit = IrElementCriteria {
            role: Some("button".to_string()),
            text: Some("hello".to_string()),
            ..Default::default()
        };
        let total = sel.total_declared_weight(&crit);
        let expected = sel.weight_for_role("button") + sel.weight_for_text_phrase("hello");
        assert!((total - expected).abs() < 1e-6);
    }

    // ----- compute_miss -----

    #[test]
    fn compute_miss_no_elements_is_no_candidates() {
        let s = snap(vec![]);
        let idx = IndexedSnapshot::new(&s);
        let crit = IrElementCriteria {
            role: Some("button".to_string()),
            ..Default::default()
        };
        let miss = compute_miss(&idx, &crit);
        assert_eq!(miss.reason, MissReason::NoCandidates);
        assert!(miss.candidates.is_empty());
    }

    #[test]
    fn compute_miss_role_only_mismatch_button_vs_link_is_role_mismatch_with_similarity_half() {
        // Plan §Step 5 acceptance: a spec with only role mismatch (button
        // vs link, same category) emits RoleMismatch with similarity 0.5.
        let s = snap(vec![elem(
            "e0", "#e0", Some("link"), None, None, None, None, true,
        )]);
        let idx = IndexedSnapshot::new(&s);
        let crit = IrElementCriteria {
            role: Some("button".to_string()),
            ..Default::default()
        };
        let miss = compute_miss(&idx, &crit);
        assert_eq!(miss.reason, MissReason::RoleMismatch);
        assert!(!miss.candidates.is_empty());
        let c = &miss.candidates[0];
        assert_eq!(c.field_diffs.len(), 1);
        let d = &c.field_diffs[0];
        assert_eq!(d.field, "role");
        assert_eq!(d.similarity, 0.5);
        assert_eq!(d.expected, serde_json::Value::String("button".into()));
        assert_eq!(d.actual, serde_json::Value::String("link".into()));
    }

    #[test]
    fn compute_miss_returns_at_most_5_candidates() {
        let mut elements = Vec::new();
        for i in 0..50 {
            elements.push(elem(
                &format!("e{i}"),
                &format!("#e{i}"),
                Some("button"),
                None,
                None,
                None,
                Some("partial match"),
                true,
            ));
        }
        let s = snap(elements);
        let idx = IndexedSnapshot::new(&s);
        let crit = IrElementCriteria {
            role: Some("button".to_string()),
            text: Some("partial match".to_string()),
            ..Default::default()
        };
        let miss = compute_miss(&idx, &crit);
        assert!(
            miss.candidates.len() <= MAX_CANDIDATES,
            "candidates.len()={}",
            miss.candidates.len()
        );
    }

    #[test]
    fn compute_miss_path_tiebreak_is_stable_across_reruns() {
        // Two elements scoring exactly the same → element-path ascending
        // determines order, and that order must hold across reruns.
        let s = snap(vec![
            elem("e0", "#zeta", Some("link"), None, None, None, None, true),
            elem("e1", "#alpha", Some("link"), None, None, None, None, true),
        ]);
        let idx = IndexedSnapshot::new(&s);
        let crit = IrElementCriteria {
            role: Some("button".to_string()),
            ..Default::default()
        };

        let mut first_order: Option<Vec<String>> = None;
        for _ in 0..100 {
            let miss = compute_miss(&idx, &crit);
            let paths: Vec<String> = miss.candidates.iter().map(|c| c.path.clone()).collect();
            if let Some(prev) = &first_order {
                assert_eq!(*prev, paths, "non-deterministic candidate order");
            } else {
                first_order = Some(paths.clone());
                // #alpha must come before #zeta.
                assert!(
                    paths.iter().position(|p| p == "#alpha")
                        < paths.iter().position(|p| p == "#zeta"),
                    "#alpha must precede #zeta after path tiebreak; got {paths:?}"
                );
            }
        }
    }

    #[test]
    fn compute_miss_multiple_matches_on_ties_without_diffs() {
        // Two elements that perfectly match the criterion → MultipleMatches.
        let s = snap(vec![
            elem("a", "#a", Some("button"), None, None, None, Some("Save"), true),
            elem("b", "#b", Some("button"), None, None, None, Some("Save"), true),
        ]);
        let idx = IndexedSnapshot::new(&s);
        let crit = IrElementCriteria {
            role: Some("button".to_string()),
            text: Some("Save".to_string()),
            ..Default::default()
        };
        let miss = compute_miss(&idx, &crit);
        assert_eq!(miss.reason, MissReason::MultipleMatches);
    }

    #[test]
    fn compute_miss_visibility_mismatch_when_otherwise_perfect_but_hidden() {
        let s = snap(vec![elem(
            "e0", "#e0", Some("button"), None, None, None, Some("Save"), false,
        )]);
        let idx = IndexedSnapshot::new(&s);
        let crit = IrElementCriteria {
            role: Some("button".to_string()),
            text: Some("Save".to_string()),
            ..Default::default()
        };
        let miss = compute_miss(&idx, &crit);
        assert_eq!(miss.reason, MissReason::VisibilityMismatch);
        assert!(!miss.candidates.is_empty());
        let has_visibility_diff = miss.candidates[0]
            .field_diffs
            .iter()
            .any(|d| d.field == "state.visible");
        assert!(has_visibility_diff);
    }

    #[test]
    fn compute_miss_dominant_priority_role_over_text() {
        // Element has wrong role (link, same Widget category as button → 0.5)
        // AND wrong-but-similar text ("Save changes" shares the "save" token
        // with "Save and quit"). Both fields produce diffs; priority order
        // Role > Text → RoleMismatch.
        let s = snap(vec![elem(
            "e0",
            "#e0",
            Some("link"),
            None,
            None,
            None,
            Some("Save changes"),
            true,
        )]);
        let idx = IndexedSnapshot::new(&s);
        let crit = IrElementCriteria {
            role: Some("button".to_string()),
            text: Some("Save and quit".to_string()),
            ..Default::default()
        };
        let miss = compute_miss(&idx, &crit);
        // Both role and text mismatch → priority Role > Text → RoleMismatch.
        assert_eq!(miss.reason, MissReason::RoleMismatch);
        // Both fields should appear in field_diffs.
        let c = &miss.candidates[0];
        let fields: Vec<&str> = c.field_diffs.iter().map(|d| d.field.as_str()).collect();
        assert!(fields.contains(&"role"), "expected role diff, got {fields:?}");
        assert!(fields.contains(&"text"), "expected text diff, got {fields:?}");
    }

    #[test]
    fn compute_miss_below_floor_when_nothing_scores() {
        // Spec asks for an attribute we can't observe; selectivity weights
        // are non-trivial so normalized score may stay below the floor.
        // The exact floor behavior depends on selectivity, so just verify
        // we don't crash and the result is sane.
        let s = snap(vec![elem(
            "e0", "#e0", None, None, None, None, None, true,
        )]);
        let idx = IndexedSnapshot::new(&s);
        let mut attrs = BTreeMap::new();
        attrs.insert("data-state".to_string(), "open".to_string());
        let crit = IrElementCriteria {
            attributes: Some(attrs),
            ..Default::default()
        };
        let miss = compute_miss(&idx, &crit);
        // Some reason must be set; the test guards against panic.
        match miss.reason {
            MissReason::NoCandidates
            | MissReason::AttributeMismatch
            | MissReason::MultipleMatches => {} // any is acceptable
            other => panic!("unexpected reason for attribute-only miss: {other:?}"),
        }
    }
}
