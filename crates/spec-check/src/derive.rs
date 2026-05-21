//! Bridge `IrElementCriteria` field names to the values the UI Bridge
//! snapshot actually carries.
//!
//! ## Why this module exists
//!
//! The IR uses W3C-ARIA-style criteria (`role: "heading"`, `text: "General"`,
//! `accessibleName: "Sign In"`) but the live snapshot's top-level
//! `role` / `text` / `accessibleName` fields on [`UIBridgeElement`] are
//! almost always `None`. The SDK emits the same information in different
//! places: `tagName` carries the implicit ARIA role, `state.textContent`
//! carries the visible text, `label` carries the accessible name.
//!
//! Hand-fixing the SDK to populate the top-level fields would alter the
//! wire contract for every consumer. The right place to bridge the gap is
//! the matcher — `qontinui-spec-check` is the only consumer that needs
//! the IR↔snapshot translation, so we resolve it here.
//!
//! ## Precedence
//!
//! Each derivation prefers the direct field (so a future SDK that *does*
//! populate top-level `role` keeps working) and falls back to alternative
//! locations only when the direct field is `None` or empty.
//!
//! | Derived         | 1st choice          | 2nd                       | 3rd                            |
//! |-----------------|---------------------|---------------------------|--------------------------------|
//! | role            | `el.role`           | implicit role from `tag_name` | `element_type` keyword     |
//! | accessible_name | `el.accessible_name`| `el.label`                | `el.state.text_content`        |
//! | text            | `el.text`           | `el.state.text_content`   | `el.label`                     |
//! | aria_label      | `el.aria_label`     | `el.label`                | —                              |

use qontinui_types::ui_bridge::UIBridgeElement;

/// Effective ARIA role for matching purposes.
///
/// 1. `el.role` if non-empty (a future SDK that populates this wins).
/// 2. Implicit ARIA role from `el.tag_name` per the table below.
/// 3. `el.element_type` when it looks like an ARIA role keyword
///    (covers SDK-emitted semantic types like `"heading"`, `"button"`,
///    `"link"`, `"paragraph"` even when the tag name is generic).
pub fn role(el: &UIBridgeElement) -> Option<String> {
    if let Some(r) = non_empty(el.role.as_deref()) {
        return Some(r.to_string());
    }
    if let Some(tag) = el.tag_name.as_deref() {
        if let Some(role) = role_from_tag(tag, el.element_type.as_str()) {
            return Some(role.to_string());
        }
    }
    let et = el.element_type.trim().to_ascii_lowercase();
    if !et.is_empty() && et != "div" && et != "span" && et != "input" {
        // Allow-list the SDK's semantic type names that already read as
        // ARIA roles. We exclude "input" because the W3C role for `<input>`
        // depends on `type=` (textbox / searchbox / button / checkbox /
        // radio / spinbutton …) — `role_from_tag` handles the common
        // cases above and we'd rather return None than mis-tag.
        return Some(et);
    }
    None
}

/// Effective accessible name for matching purposes.
///
/// 1. `el.accessible_name` if non-empty.
/// 2. `el.label` — the SDK's human-readable label, populated whenever the
///    accessible-name algorithm produces a result.
/// 3. `el.state.text_content` — visible text as a last resort, since for
///    most non-form-control elements `accessibleName === textContent`.
pub fn accessible_name(el: &UIBridgeElement) -> Option<String> {
    non_empty(el.accessible_name.as_deref())
        .map(str::to_string)
        .or_else(|| non_empty(el.label.as_deref()).map(str::to_string))
        .or_else(|| non_empty(el.state.text_content.as_deref()).map(str::to_string))
}

/// Effective visible text for matching purposes.
///
/// 1. `el.text` if non-empty (SDK-canonical innerText slot).
/// 2. `el.state.text_content` — same slot under the legacy nested name.
/// 3. `el.label` — covers content elements where the SDK chose to expose
///    the visible text only via the label.
pub fn text(el: &UIBridgeElement) -> Option<String> {
    non_empty(el.text.as_deref())
        .map(str::to_string)
        .or_else(|| non_empty(el.state.text_content.as_deref()).map(str::to_string))
        .or_else(|| non_empty(el.label.as_deref()).map(str::to_string))
}

/// Effective aria-label for matching purposes.
///
/// 1. `el.aria_label` if non-empty.
/// 2. `el.label` — when the SDK didn't surface an explicit `aria-label`
///    attribute but did expose a human-readable label, that label is the
///    closest equivalent.
pub fn aria_label(el: &UIBridgeElement) -> Option<String> {
    non_empty(el.aria_label.as_deref())
        .map(str::to_string)
        .or_else(|| non_empty(el.label.as_deref()).map(str::to_string))
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// Implicit ARIA role for a given HTML tag.
///
/// Curated from the WAI-ARIA-in-HTML mapping
/// (<https://www.w3.org/TR/html-aria/>) — only the elements that carry an
/// unambiguous implicit role with no attribute disambiguation are listed.
/// `<input>` is delegated to `element_type` because its role depends on
/// the `type=` attribute, which the snapshot doesn't expose directly but
/// the SDK has already resolved into the semantic `element_type` string
/// (`"input"` / `"button"` / `"checkbox"` / `"radio"` / `"search"` / …).
fn role_from_tag(tag: &str, element_type: &str) -> Option<&'static str> {
    let t = tag.trim().to_ascii_lowercase();
    match t.as_str() {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Some("heading"),
        "button" => Some("button"),
        "a" => Some("link"),
        "select" => Some("combobox"),
        "textarea" => Some("textbox"),
        "input" => role_from_input_type(element_type),
        "nav" => Some("navigation"),
        "main" => Some("main"),
        "header" => Some("banner"),
        "footer" => Some("contentinfo"),
        "aside" => Some("complementary"),
        "article" => Some("article"),
        "section" => Some("region"),
        "form" => Some("form"),
        "table" => Some("table"),
        "thead" | "tbody" | "tfoot" => Some("rowgroup"),
        "tr" => Some("row"),
        "td" => Some("cell"),
        "th" => Some("columnheader"),
        "p" => Some("paragraph"),
        "ul" | "ol" | "menu" => Some("list"),
        "li" => Some("listitem"),
        "dialog" => Some("dialog"),
        "img" => Some("img"),
        "hr" => Some("separator"),
        "fieldset" => Some("group"),
        "figure" => Some("figure"),
        _ => None,
    }
}

/// Map the SDK's semantic `element_type` for `<input>` elements onto an
/// ARIA role. The SDK normalizes `<input type="X">` into one of a small
/// set of strings (`"input"`, `"button"`, `"checkbox"`, `"radio"`,
/// `"search"`, …) — we route each to the W3C ARIA-in-HTML mapping.
fn role_from_input_type(element_type: &str) -> Option<&'static str> {
    match element_type.trim().to_ascii_lowercase().as_str() {
        // Generic / text-flavored inputs all map to "textbox" in ARIA.
        "input" | "text" | "email" | "password" | "tel" | "url" | "number" | "date"
        | "datetime-local" | "month" | "time" | "week" | "color" | "file" => Some("textbox"),
        "search" => Some("searchbox"),
        "button" | "submit" | "reset" => Some("button"),
        "checkbox" => Some("checkbox"),
        "radio" => Some("radio"),
        "range" => Some("slider"),
        _ => Some("textbox"),
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

    fn el(
        element_type: &str,
        tag: Option<&str>,
        role: Option<&str>,
        aria_label: Option<&str>,
        accessible_name: Option<&str>,
        label: Option<&str>,
        text: Option<&str>,
        text_content: Option<&str>,
    ) -> UIBridgeElement {
        let mut state = empty_state();
        state.text_content = text_content.map(String::from);
        UIBridgeElement {
            id: "e0".to_string(),
            element_type: element_type.to_string(),
            label: label.map(String::from),
            actions: Vec::new(),
            custom_actions: None,
            identifier: empty_identifier(),
            state,
            registered_at: 0,
            mounted: true,
            bbox: None,
            visible: Some(true),
            role: role.map(String::from),
            tag_name: tag.map(String::from),
            aria_label: aria_label.map(String::from),
            accessible_name: accessible_name.map(String::from),
            text: text.map(String::from),
        }
    }

    // -- role -------------------------------------------------------------

    #[test]
    fn role_prefers_explicit_top_level() {
        let e = el(
            "heading",
            Some("h1"),
            Some("button"), // explicit role wins even when wrong-looking
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(role(&e).as_deref(), Some("button"));
    }

    #[test]
    fn role_falls_back_to_tag_for_headings() {
        for tag in ["h1", "h2", "h3", "h4", "h5", "h6"] {
            let e = el("heading", Some(tag), None, None, None, None, None, None);
            assert_eq!(role(&e).as_deref(), Some("heading"), "tag={tag}");
        }
    }

    #[test]
    fn role_falls_back_to_tag_for_common_widgets() {
        let cases = [
            ("button", "button"),
            ("a", "link"),
            ("select", "combobox"),
            ("textarea", "textbox"),
            ("p", "paragraph"),
            ("ul", "list"),
            ("li", "listitem"),
            ("nav", "navigation"),
            ("main", "main"),
            ("dialog", "dialog"),
        ];
        for (tag, expected) in cases {
            let e = el("misc", Some(tag), None, None, None, None, None, None);
            assert_eq!(role(&e).as_deref(), Some(expected), "tag={tag}");
        }
    }

    #[test]
    fn role_for_input_uses_element_type() {
        let cases = [
            ("input", "textbox"),     // plain text input
            ("email", "textbox"),
            ("password", "textbox"),
            ("search", "searchbox"),
            ("button", "button"),
            ("checkbox", "checkbox"),
            ("radio", "radio"),
            ("range", "slider"),
        ];
        for (et, expected) in cases {
            let e = el(et, Some("input"), None, None, None, None, None, None);
            assert_eq!(role(&e).as_deref(), Some(expected), "element_type={et}");
        }
    }

    #[test]
    fn role_falls_back_to_element_type_when_tag_is_generic() {
        // `<div role="...">` style — generic tag, but element_type is semantic.
        let e = el(
            "heading",
            Some("div"),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(role(&e).as_deref(), Some("heading"));
    }

    #[test]
    fn role_returns_none_for_pure_generic_div() {
        let e = el("div", Some("div"), None, None, None, None, None, None);
        assert_eq!(role(&e), None);
    }

    // -- accessible_name --------------------------------------------------

    #[test]
    fn accessible_name_prefers_top_level() {
        let e = el(
            "button",
            Some("button"),
            None,
            None,
            Some("Submit form"),
            Some("Submit"),       // label loses to accessible_name
            None,
            Some("Submit form"),
        );
        assert_eq!(accessible_name(&e).as_deref(), Some("Submit form"));
    }

    #[test]
    fn accessible_name_falls_back_to_label() {
        let e = el(
            "button",
            Some("button"),
            None,
            None,
            None,
            Some("Sign In"),
            None,
            None,
        );
        assert_eq!(accessible_name(&e).as_deref(), Some("Sign In"));
    }

    #[test]
    fn accessible_name_falls_back_to_text_content() {
        let e = el(
            "heading",
            Some("h1"),
            None,
            None,
            None,
            None,
            None,
            Some("Qontinui"),
        );
        assert_eq!(accessible_name(&e).as_deref(), Some("Qontinui"));
    }

    #[test]
    fn accessible_name_none_when_all_sources_absent() {
        let e = el("div", Some("div"), None, None, None, None, None, None);
        assert_eq!(accessible_name(&e), None);
    }

    // -- text -------------------------------------------------------------

    #[test]
    fn text_prefers_top_level() {
        let e = el(
            "heading",
            Some("h1"),
            None,
            None,
            None,
            None,
            Some("Top-level"),
            Some("Nested"),
        );
        assert_eq!(text(&e).as_deref(), Some("Top-level"));
    }

    #[test]
    fn text_falls_back_to_text_content() {
        let e = el(
            "heading",
            Some("h1"),
            None,
            None,
            None,
            None,
            None,
            Some("General"),
        );
        assert_eq!(text(&e).as_deref(), Some("General"));
    }

    #[test]
    fn text_falls_back_to_label() {
        let e = el(
            "paragraph",
            Some("p"),
            None,
            None,
            None,
            Some("Some prose"),
            None,
            None,
        );
        assert_eq!(text(&e).as_deref(), Some("Some prose"));
    }

    // -- aria_label -------------------------------------------------------

    #[test]
    fn aria_label_prefers_top_level() {
        let e = el(
            "button",
            Some("button"),
            None,
            Some("Save"),
            None,
            Some("Save changes"), // ignored
            None,
            None,
        );
        assert_eq!(aria_label(&e).as_deref(), Some("Save"));
    }

    #[test]
    fn aria_label_falls_back_to_label() {
        let e = el(
            "button",
            Some("button"),
            None,
            None,
            None,
            Some("Save"),
            None,
            None,
        );
        assert_eq!(aria_label(&e).as_deref(), Some("Save"));
    }

    // -- non_empty edge cases --------------------------------------------

    #[test]
    fn whitespace_only_strings_are_treated_as_absent() {
        // role: "   " should NOT short-circuit; we should fall through to
        // the tag-derived role.
        let e = el("heading", Some("h1"), Some("   "), None, None, None, None, None);
        assert_eq!(role(&e).as_deref(), Some("heading"));
    }
}
