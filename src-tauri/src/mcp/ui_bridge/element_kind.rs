//! How the runner READS an element row's kind — the reader-side half of
//! `@qontinui/ui-bridge` `packages/ui-bridge/src/core/element-kind.ts`
//! (0.46.0, plan
//! `2026-11-09-an-elements-advertised-verbs-depend-on-which-path-found-it`).
//!
//! Three rules a runner reader of a serialized element (a snapshot, discover or
//! find row) has to apply the way the SDK does:
//!
//! - **A named TYPE matches its family.** A criterion, query or filter that
//!   names a type (`input`, `select`, …) answers for the kinds the SDK keeps
//!   apart but people name interchangeably (`input`↔`textbox`,
//!   `select`↔`combobox`↔`listbox`) — [`answers_to_named_type`].
//! - **A named ROLE matches by equality** with the element's observed ARIA
//!   role, its kind, or the role its kind implies, plus a one-way reach for
//!   the two kind names `select` / `input` — [`answers_to_named_role`] and,
//!   for a substring matcher, [`role_names_answered`]. Families do not apply:
//!   `listbox` is not `combobox`.
//!
//!   Both are implemented ONCE, in [`qontinui_spec_check::named_type`],
//!   because the spec-check matcher needs them too and that crate cannot
//!   depend on this one; they are re-exported here so the runner's own
//!   readers import them from the module that names the rules.
//! - **"Has actions" does not mean "is a control".** Since 0.46 every kind
//!   advertises the verbs every element takes (`EVERY_ELEMENT`: `click`,
//!   `sendKeys`, `scroll`, …) — a media row included, because a canvas takes
//!   keyboard input — so a non-empty `actions` list no longer tells an image
//!   from a button. `actions: []` is carried by content rows and by any
//!   registration that declares it (the terminal proxy pane in
//!   `TerminalBridgeProxies.tsx` does; any `useUIElement` caller can). A
//!   reader that wants the page's interactive affordances therefore excludes
//!   content and media rows by CLASSIFICATION ([`is_content_or_media_row`]),
//!   exactly as the SDK's own `core/find-filter.ts` `interactive_only`
//!   predicate does.

pub(crate) use qontinui_spec_check::named_type::{
    answers_to_named_role, answers_to_named_type, role_names_answered,
};

/// Whether a serialized element row is a content or a media row — the SDK's
/// `isContentRow(el) || isMediaRow(el)` in `core/find-filter.ts`: a `category`
/// of `content` or `media`, or a `kind` of `content` (the snapshot serializer
/// mirrors `category` into `kind` for content and leaves it unset for media).
///
/// Such a row is not an interactive affordance by virtue of its built-in
/// `actions`, which for a media row is the full every-element list. A custom
/// action it declares (`customActions`) is still an affordance — an app put it
/// there to be invoked — so a reader counting affordances checks that
/// separately.
pub(crate) fn is_content_or_media_row(element: &serde_json::Value) -> bool {
    let field = |key: &str| element.get(key).and_then(|v| v.as_str());
    matches!(field("category"), Some("content" | "media")) || field("kind") == Some("content")
}

/// The `actions` `@qontinui/ui-bridge` 0.46.0 serves for runner elements,
/// MEASURED by the frontend tests against the real SDK mounted the way
/// `App.tsx` mounts it (`src/lib/ui-bridge/autoRegisterVerbs.dom.test.tsx`,
/// `src/components/terminal/sessionAddressability.test.ts`) and read by the
/// Rust tests from the same file — so the SDK serving a different list fails
/// there, and a fixture edited to follow it re-runs every Rust test that reads
/// it (the pre-IPC gate in `elements.rs`, the vocabulary in
/// `action_vocabulary.rs`) on the new list.
#[cfg(test)]
pub(crate) const SERVED_ACTIONS_0_46_JSON: &str =
    include_str!("../../../../src/lib/ui-bridge/servedActions.0.46.json");

/// One named list from [`SERVED_ACTIONS_0_46_JSON`].
#[cfg(test)]
pub(crate) fn served_actions_0_46(name: &str) -> Vec<String> {
    let fixture: serde_json::Value =
        serde_json::from_str(SERVED_ACTIONS_0_46_JSON).expect("served-actions fixture parses");
    fixture[name]
        .as_array()
        .unwrap_or_else(|| panic!("fixture list {name}"))
        .iter()
        .map(|v| v.as_str().expect("verb").to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The every-element verbs 0.46 serves on EVERY kind, media included
    /// (`EVERY_ELEMENT` in `core/element-kind.ts` at ui-bridge `9f373a12`).
    const EVERY_ELEMENT_0_46: &[&str] = &[
        "click",
        "hoverClick",
        "doubleClick",
        "rightClick",
        "middleClick",
        "sendKeys",
        "focus",
        "blur",
        "hover",
        "unhover",
        "scroll",
        "scrollIntoView",
        "drag",
    ];

    #[test]
    fn media_and_content_rows_are_classified_by_category_not_actions() {
        for media_type in ["image", "video", "canvas", "svg", "picture"] {
            let row = json!({"id": "m", "type": media_type, "category": "media",
                "actions": EVERY_ELEMENT_0_46});
            assert!(is_content_or_media_row(&row), "{media_type}");
        }
        assert!(is_content_or_media_row(
            &json!({"id": "c", "type": "heading", "category": "content", "actions": []})
        ));
        assert!(is_content_or_media_row(
            &json!({"id": "c", "type": "generic", "kind": "content", "actions": []})
        ));
    }

    #[test]
    fn interactive_and_unclassified_rows_are_not_content_or_media() {
        assert!(!is_content_or_media_row(
            &json!({"id": "b", "type": "button", "category": "interactive", "kind": "interactive"})
        ));
        // A pre-category serializer: nothing to classify by, so not excluded.
        assert!(!is_content_or_media_row(
            &json!({"id": "b", "type": "button"})
        ));
        // A canvas the APP registered as a control keeps its interactive
        // category — the classification is the registration's, not the tag's.
        assert!(!is_content_or_media_row(
            &json!({"id": "pad", "type": "canvas", "category": "interactive"})
        ));
    }

    #[test]
    fn the_named_type_and_role_rules_are_reachable_from_here() {
        assert!(answers_to_named_type(Some("textbox"), "input"));
        assert!(answers_to_named_role("select", None, Some("combobox")));
        assert!(!answers_to_named_role("listbox", None, Some("combobox")));
        assert_eq!(
            role_names_answered(None, Some("textarea")),
            vec!["textarea", "textbox"]
        );
    }
}
