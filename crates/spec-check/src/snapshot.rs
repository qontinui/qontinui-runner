//! Indexed snapshot wrapper. Step 2 of Plan 02.
//!
//! Wraps a borrowed [`UIBridgeSnapshot`] in O(N) and exposes O(1)-ish lookups
//! for every field the Phase-1 matcher narrows on. All text-keyed indices
//! route through [`qontinui_types::text_norm::normalize_text`] so authoring
//! `text: "Save"` and a snapshot saying `"  save  "` collide on the same key.
//!
//! Built atop the Addendum A.5 fields on [`UIBridgeElement`]
//! (`role` / `tag_name` / `aria_label` / `accessible_name` / `text`) — no
//! derivation layer.

use std::collections::HashMap;

use qontinui_types::text_norm::normalize_text;
use qontinui_types::ui_bridge::{UIBridgeElement, UIBridgeSnapshot};

use crate::derive;

/// Stable index into [`UIBridgeSnapshot::elements`]. Minted by
/// [`IndexedSnapshot::new`] — never construct manually unless you know what
/// you are doing.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ElementIdx(pub u32);

/// Indexed wrapper around a [`UIBridgeSnapshot`].
///
/// All indices are populated eagerly in [`IndexedSnapshot::new`] in `O(N)`
/// time, where `N = snapshot.elements.len()`. Lookups return borrowed slices
/// without allocation. The `selectivity` field will be plumbed in by the
/// `diff` module in Step 5 of the plan; until then [`Self::selectivity`]
/// panics.
pub struct IndexedSnapshot<'a> {
    /// The underlying snapshot — kept public so consumers can read the
    /// snapshot-level metadata (timestamp, modals, toasts) without going
    /// through this wrapper.
    pub raw: &'a UIBridgeSnapshot,

    by_role: HashMap<String, Vec<ElementIdx>>,
    by_tag: HashMap<String, Vec<ElementIdx>>,
    /// Multi-source ID index. A single element may surface in multiple keys:
    /// each of `element.id`, `identifier.html_id`, `identifier.test_id`,
    /// `identifier.awas_id`, `identifier.ui_id` (when present) is a separate
    /// key pointing back at the element. Vec-valued (not last-write-wins)
    /// because malformed snapshots may collide on the same ID.
    by_id: HashMap<String, Vec<ElementIdx>>,
    by_aria_label: HashMap<String, Vec<ElementIdx>>,
    by_accessible_name: HashMap<String, Vec<ElementIdx>>,
    /// Word-level inverted index over normalized `text`. Tokenized on ASCII
    /// whitespace — `normalize_text` already case-folds, NFC-normalizes, and
    /// collapses runs of whitespace to a single ASCII space, so this is
    /// equivalent to a Unicode-segmentation pass for the inputs we accept.
    text_inverted: HashMap<String, Vec<ElementIdx>>,

    // TF-IDF-inspired selectivity weights computed by
    // `crate::diff::SelectivityIndex` (Step 5 of Plan 02). Lazily built on
    // first access via [`Self::selectivity`]; the evaluator forces the init
    // on the main thread before fan-out so rayon workers never race on the
    // OnceCell.
    selectivity: once_cell::sync::OnceCell<crate::diff::SelectivityIndex>,
}

impl<'a> IndexedSnapshot<'a> {
    /// Build all indices in O(N) over `raw.elements`. Normalizes every
    /// text-keyed index through [`normalize_text`] per §5.11.
    pub fn new(raw: &'a UIBridgeSnapshot) -> Self {
        let n = raw.elements.len();
        let mut by_role: HashMap<String, Vec<ElementIdx>> = HashMap::with_capacity(n);
        let mut by_tag: HashMap<String, Vec<ElementIdx>> = HashMap::with_capacity(n);
        let mut by_id: HashMap<String, Vec<ElementIdx>> = HashMap::with_capacity(n * 2);
        let mut by_aria_label: HashMap<String, Vec<ElementIdx>> = HashMap::with_capacity(n);
        let mut by_accessible_name: HashMap<String, Vec<ElementIdx>> = HashMap::with_capacity(n);
        let mut text_inverted: HashMap<String, Vec<ElementIdx>> = HashMap::with_capacity(n * 4);

        for (i, el) in raw.elements.iter().enumerate() {
            let idx = ElementIdx(i as u32);

            // -- role: derived (top-level `el.role` → tag-implicit role →
            //    `element_type` keyword) so a `{role: "heading"}` criterion
            //    finds `<h1>` even when the SDK didn't populate top-level
            //    role. See `crate::derive::role`.
            if let Some(role) = derive::role(el) {
                let key = normalize_text(&role);
                if !key.is_empty() {
                    by_role.entry(key).or_default().push(idx);
                }
            }

            // -- tag_name (Addendum A.5) — no derivation, raw field.
            if let Some(tag) = el.tag_name.as_deref() {
                let key = normalize_text(tag);
                if !key.is_empty() {
                    by_tag.entry(key).or_default().push(idx);
                }
            }

            // -- aria_label: derived (top-level `el.aria_label` → `el.label`).
            if let Some(label) = derive::aria_label(el) {
                let key = normalize_text(&label);
                if !key.is_empty() {
                    by_aria_label.entry(key).or_default().push(idx);
                }
            }

            // -- accessible_name: derived (top-level → `el.label` →
            //    `state.text_content`).
            if let Some(name) = derive::accessible_name(el) {
                let key = normalize_text(&name);
                if !key.is_empty() {
                    by_accessible_name.entry(key).or_default().push(idx);
                }
            }

            // -- id sources: bare id + every Some() identifier.* slot
            insert_id_key(&mut by_id, &el.id, idx);
            if let Some(ref v) = el.identifier.html_id {
                insert_id_key(&mut by_id, v, idx);
            }
            if let Some(ref v) = el.identifier.test_id {
                insert_id_key(&mut by_id, v, idx);
            }
            if let Some(ref v) = el.identifier.awas_id {
                insert_id_key(&mut by_id, v, idx);
            }
            if let Some(ref v) = el.identifier.ui_id {
                insert_id_key(&mut by_id, v, idx);
            }

            // -- text inverted index: derived (top-level `el.text` →
            //    `state.text_content` → `el.label`). Tokenize on ASCII
            //    whitespace post-normalization.
            if let Some(text) = derive::text(el) {
                let normalized = normalize_text(&text);
                for word in normalized.split_ascii_whitespace() {
                    if word.is_empty() {
                        continue;
                    }
                    let entry = text_inverted.entry(word.to_string()).or_default();
                    // Dedup adjacent inserts so an element with repeated
                    // tokens doesn't blow up its own per-word posting list.
                    if entry.last() != Some(&idx) {
                        entry.push(idx);
                    }
                }
            }
        }

        Self {
            raw,
            by_role,
            by_tag,
            by_id,
            by_aria_label,
            by_accessible_name,
            text_inverted,
            selectivity: once_cell::sync::OnceCell::new(),
        }
    }

    /// Resolve `idx` to the underlying element. Panics if the index was not
    /// minted by this snapshot — that's an invariant violation, never a
    /// user-facing condition.
    pub fn element(&self, idx: ElementIdx) -> &UIBridgeElement {
        &self.raw.elements[idx.0 as usize]
    }

    /// Iterate every `(idx, element)` pair in registration order.
    pub fn iter_elements(&self) -> impl Iterator<Item = (ElementIdx, &UIBridgeElement)> {
        self.raw
            .elements
            .iter()
            .enumerate()
            .map(|(i, el)| (ElementIdx(i as u32), el))
    }

    /// Number of elements in the underlying snapshot.
    pub fn element_count(&self) -> usize {
        self.raw.elements.len()
    }

    // -- lookups -----------------------------------------------------------

    /// Elements whose `role` (normalized) equals the (normalized) query.
    /// Returns an empty slice when nothing matches.
    pub fn lookup_by_role(&self, role: &str) -> &[ElementIdx] {
        slice_or_empty(&self.by_role, &normalize_text(role))
    }

    /// Elements whose `tag_name` (normalized) equals the (normalized) query.
    pub fn lookup_by_tag(&self, tag: &str) -> &[ElementIdx] {
        slice_or_empty(&self.by_tag, &normalize_text(tag))
    }

    /// All elements whose `element.id` or any populated `identifier.*` slot
    /// equals the (normalized) query. Vec-valued (not Option) so a malformed
    /// snapshot with duplicate IDs doesn't silently lose candidates.
    pub fn lookup_by_id(&self, id: &str) -> &[ElementIdx] {
        slice_or_empty(&self.by_id, &normalize_text(id))
    }

    /// Elements whose `aria_label` (normalized) equals the (normalized)
    /// query.
    pub fn lookup_by_aria_label(&self, label: &str) -> &[ElementIdx] {
        slice_or_empty(&self.by_aria_label, &normalize_text(label))
    }

    /// Elements whose `accessible_name` (normalized) equals the (normalized)
    /// query.
    pub fn lookup_by_accessible_name(&self, name: &str) -> &[ElementIdx] {
        slice_or_empty(&self.by_accessible_name, &normalize_text(name))
    }

    /// Elements whose `text` (normalized + tokenized) contains the
    /// (normalized) query word. Returns an empty slice for empty queries or
    /// multi-word queries — callers must split into per-word lookups.
    pub fn lookup_text_word(&self, word: &str) -> &[ElementIdx] {
        let normalized = normalize_text(word);
        // Multi-word queries are caller error — bail out empty rather than
        // returning a misleading posting list for the first word only.
        if normalized.split_ascii_whitespace().count() != 1 {
            return &[];
        }
        slice_or_empty(&self.text_inverted, normalized.trim())
    }

    /// Lazy access to the per-field selectivity weights.
    ///
    /// First call builds the [`crate::diff::SelectivityIndex`] in O(N·F) over
    /// the underlying snapshot; subsequent calls return the cached reference.
    /// The evaluator calls this once on the main thread (via
    /// [`crate::evaluate`]) so rayon workers never race on the OnceCell.
    pub fn selectivity(&self) -> &crate::diff::SelectivityIndex {
        self.selectivity
            .get_or_init(|| crate::diff::SelectivityIndex::build(self))
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

fn slice_or_empty<'m>(
    map: &'m HashMap<String, Vec<ElementIdx>>,
    key: &str,
) -> &'m [ElementIdx] {
    if key.is_empty() {
        return &[];
    }
    map.get(key).map(Vec::as_slice).unwrap_or(&[])
}

fn insert_id_key(map: &mut HashMap<String, Vec<ElementIdx>>, raw: &str, idx: ElementIdx) {
    let key = normalize_text(raw);
    if key.is_empty() {
        return;
    }
    let entry = map.entry(key).or_default();
    if entry.last() != Some(&idx) {
        entry.push(idx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_types::ui_bridge::{ElementIdentifier, ElementRect, ElementState};

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

    fn element(
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

    fn make_snapshot(elements: Vec<UIBridgeElement>) -> UIBridgeSnapshot {
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

    #[test]
    fn empty_snapshot_does_not_panic() {
        let snap = make_snapshot(Vec::new());
        let idx = IndexedSnapshot::new(&snap);

        assert_eq!(idx.element_count(), 0);
        assert!(idx.lookup_by_role("button").is_empty());
        assert!(idx.lookup_by_tag("div").is_empty());
        assert!(idx.lookup_by_id("anything").is_empty());
        assert!(idx.lookup_by_aria_label("anything").is_empty());
        assert!(idx.lookup_by_accessible_name("anything").is_empty());
        assert!(idx.lookup_text_word("hello").is_empty());
        assert_eq!(idx.iter_elements().count(), 0);
    }

    #[test]
    fn lookup_by_role_matches_normalized() {
        let snap = make_snapshot(vec![
            element("e0", Some("Button"), None, None, None, None, empty_identifier()),
            element("e1", Some("link"), None, None, None, None, empty_identifier()),
            element("e2", Some("button"), None, None, None, None, empty_identifier()),
        ]);
        let idx = IndexedSnapshot::new(&snap);

        // Case-folded via normalize_text.
        let hits = idx.lookup_by_role("BUTTON");
        assert_eq!(hits.len(), 2);
        assert!(hits.contains(&ElementIdx(0)));
        assert!(hits.contains(&ElementIdx(2)));

        assert_eq!(idx.lookup_by_role("link").len(), 1);
        assert!(idx.lookup_by_role("missing").is_empty());
    }

    #[test]
    fn lookup_by_tag_matches_normalized() {
        let snap = make_snapshot(vec![
            element("e0", None, Some("button"), None, None, None, empty_identifier()),
            element("e1", None, Some("BUTTON"), None, None, None, empty_identifier()),
            element("e2", None, Some("input"), None, None, None, empty_identifier()),
        ]);
        let idx = IndexedSnapshot::new(&snap);

        assert_eq!(idx.lookup_by_tag("button").len(), 2);
        assert_eq!(idx.lookup_by_tag("input").len(), 1);
        assert!(idx.lookup_by_tag("nope").is_empty());
    }

    #[test]
    fn lookup_by_id_collects_from_all_identifier_sources() {
        let mut ident = empty_identifier();
        ident.html_id = Some("html-1".to_string());
        ident.test_id = Some("test-1".to_string());
        ident.awas_id = Some("awas-1".to_string());
        ident.ui_id = Some("ui-1".to_string());

        let snap = make_snapshot(vec![element(
            "bare-1",
            None,
            None,
            None,
            None,
            None,
            ident,
        )]);
        let idx = IndexedSnapshot::new(&snap);

        assert_eq!(idx.lookup_by_id("bare-1"), &[ElementIdx(0)]);
        assert_eq!(idx.lookup_by_id("html-1"), &[ElementIdx(0)]);
        assert_eq!(idx.lookup_by_id("test-1"), &[ElementIdx(0)]);
        assert_eq!(idx.lookup_by_id("awas-1"), &[ElementIdx(0)]);
        assert_eq!(idx.lookup_by_id("ui-1"), &[ElementIdx(0)]);
        assert!(idx.lookup_by_id("nope").is_empty());
    }

    #[test]
    fn lookup_by_id_handles_duplicates_without_loss() {
        let mut a = empty_identifier();
        a.html_id = Some("same-id".to_string());
        let mut b = empty_identifier();
        b.html_id = Some("same-id".to_string());

        let snap = make_snapshot(vec![
            element("e0", None, None, None, None, None, a),
            element("e1", None, None, None, None, None, b),
        ]);
        let idx = IndexedSnapshot::new(&snap);

        let hits = idx.lookup_by_id("same-id");
        assert_eq!(hits.len(), 2);
        assert!(hits.contains(&ElementIdx(0)));
        assert!(hits.contains(&ElementIdx(1)));
    }

    #[test]
    fn lookup_by_aria_label_and_accessible_name() {
        let snap = make_snapshot(vec![
            element("e0", None, None, Some("Save"), Some("Save document"), None, empty_identifier()),
            element("e1", None, None, Some("Cancel"), Some("Cancel changes"), None, empty_identifier()),
        ]);
        let idx = IndexedSnapshot::new(&snap);

        assert_eq!(idx.lookup_by_aria_label("save"), &[ElementIdx(0)]);
        assert_eq!(idx.lookup_by_aria_label("CANCEL"), &[ElementIdx(1)]);
        assert_eq!(idx.lookup_by_accessible_name("save document"), &[ElementIdx(0)]);
        assert!(idx.lookup_by_aria_label("missing").is_empty());
    }

    #[test]
    fn text_inverted_tokenizes_on_whitespace() {
        let snap = make_snapshot(vec![
            element("e0", None, None, None, None, Some("Save and Quit"), empty_identifier()),
            element("e1", None, None, None, None, Some("Quit without saving"), empty_identifier()),
            element("e2", None, None, None, None, Some("Discard changes"), empty_identifier()),
        ]);
        let idx = IndexedSnapshot::new(&snap);

        // "save" appears only in e0 (e1 says "saving").
        assert_eq!(idx.lookup_text_word("save"), &[ElementIdx(0)]);

        // "quit" appears in both e0 and e1.
        let quit = idx.lookup_text_word("quit");
        assert_eq!(quit.len(), 2);
        assert!(quit.contains(&ElementIdx(0)));
        assert!(quit.contains(&ElementIdx(1)));

        // Case-insensitive via normalize_text.
        assert_eq!(idx.lookup_text_word("QUIT").len(), 2);

        // Multi-word query → empty (callers split per-word).
        assert!(idx.lookup_text_word("save quit").is_empty());
        assert!(idx.lookup_text_word("").is_empty());
    }

    #[test]
    fn text_inverted_dedups_repeated_tokens_per_element() {
        // Same word repeated 5 times in a single element should produce a
        // single posting (not five).
        let snap = make_snapshot(vec![element(
            "e0",
            None,
            None,
            None,
            None,
            Some("hello hello hello hello hello"),
            empty_identifier(),
        )]);
        let idx = IndexedSnapshot::new(&snap);
        assert_eq!(idx.lookup_text_word("hello"), &[ElementIdx(0)]);
    }

    #[test]
    fn iter_elements_yields_every_element_in_order() {
        let snap = make_snapshot(vec![
            element("e0", Some("button"), None, None, None, None, empty_identifier()),
            element("e1", Some("link"), None, None, None, None, empty_identifier()),
        ]);
        let idx = IndexedSnapshot::new(&snap);

        let collected: Vec<u32> = idx.iter_elements().map(|(i, _)| i.0).collect();
        assert_eq!(collected, vec![0, 1]);
    }

    #[test]
    fn element_resolves_idx_to_underlying_element() {
        let snap = make_snapshot(vec![
            element("first", Some("button"), None, None, None, None, empty_identifier()),
            element("second", Some("link"), None, None, None, None, empty_identifier()),
        ]);
        let idx = IndexedSnapshot::new(&snap);

        assert_eq!(idx.element(ElementIdx(0)).id, "first");
        assert_eq!(idx.element(ElementIdx(1)).id, "second");
    }

    #[test]
    fn build_is_linear_on_100_elements() {
        // Acceptance bullet: terminates on a 100-element fixture. We're not
        // measuring wall-clock here — the existence of the test asserts the
        // build doesn't accidentally become quadratic via panicking on a
        // pathological pattern.
        let elements: Vec<UIBridgeElement> = (0..100)
            .map(|i| {
                let mut ident = empty_identifier();
                ident.html_id = Some(format!("html-{i}"));
                element(
                    &format!("e{i}"),
                    Some(if i % 2 == 0 { "button" } else { "link" }),
                    Some("div"),
                    Some(&format!("label-{}", i % 10)),
                    Some(&format!("accessible-{}", i % 10)),
                    Some(&format!("word{} shared", i)),
                    ident,
                )
            })
            .collect();

        let snap = make_snapshot(elements);
        let idx = IndexedSnapshot::new(&snap);
        assert_eq!(idx.element_count(), 100);
        assert_eq!(idx.lookup_text_word("shared").len(), 100);
        assert_eq!(idx.lookup_by_role("button").len(), 50);
        assert_eq!(idx.lookup_by_role("link").len(), 50);
    }
}
