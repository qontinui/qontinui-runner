//! How the runner READS an element row's kind — the reader-side half of
//! `@qontinui/ui-bridge` `packages/ui-bridge/src/core/element-kind.ts`
//! (0.46.0, plan
//! `2026-11-09-an-elements-advertised-verbs-depend-on-which-path-found-it`).
//!
//! Two rules a runner reader of a serialized element (a snapshot, discover or
//! find row) has to apply the way the SDK does:
//!
//! - **A named type matches its family.** A criterion, query or filter that
//!   NAMES a type (`input`, `select`, …) answers for the kinds the SDK keeps
//!   apart but people name interchangeably (`input`↔`textbox`,
//!   `select`↔`combobox`↔`listbox`). The rule is implemented ONCE, in
//!   [`qontinui_spec_check::named_type`], because the spec-check matcher needs
//!   it too and that crate cannot depend on this one; it is re-exported here so
//!   the runner's own readers import it from the module that names the rule.
//! - **"Has actions" does not mean "is a control".** Since 0.46 every kind
//!   advertises the verbs every element takes (`EVERY_ELEMENT`: `click`,
//!   `sendKeys`, `scroll`, …) — a media row included, because a canvas takes
//!   keyboard input — so a non-empty `actions` list no longer tells an image
//!   from a button. Only a content row still carries `actions: []`. A reader
//!   that wants the page's interactive affordances therefore excludes content
//!   and media rows by CLASSIFICATION ([`is_content_or_media_row`]), exactly as
//!   the SDK's own `core/find-filter.ts` `interactive_only` predicate does.

pub(crate) use qontinui_spec_check::named_type::{answers_to_named_type, named_type_family};

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
    fn the_named_type_family_is_reachable_from_here() {
        assert!(answers_to_named_type(Some("textbox"), "input"));
        assert_eq!(named_type_family("select").len(), 3);
    }
}
