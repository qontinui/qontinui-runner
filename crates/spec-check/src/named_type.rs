//! How a NAME a caller wrote (a type or a role in a spec criterion, a query, a
//! filter) is matched against a UI Bridge element — the runner's ONE Rust copy.
//!
//! Mirrors `@qontinui/ui-bridge` `packages/ui-bridge/src/core/element-kind.ts`
//! at ui-bridge `9f373a12` (0.46.0, plan
//! `2026-11-09-an-elements-advertised-verbs-depend-on-which-path-found-it`):
//! `NAMED_TYPE_FAMILIES` / `namedTypeFamily` / `answersToNamedType` and
//! `IMPLIED_ARIA_ROLES` / `impliedAriaRole`. Two rules, kept apart on purpose:
//!
//! - **A named TYPE matches its family** ([`answers_to_named_type`]). The SDK
//!   keeps `input` / `textbox` and `select` / `combobox` / `listbox` apart as
//!   element kinds (their verbs differ) but a person names them
//!   interchangeably, so a type filter answers for the whole family. The SDK
//!   applies this to type / selector / style matching only.
//! - **A named ROLE matches by equality** ([`answers_to_named_role`]), as every
//!   SDK role matcher does (`core/find-filter.ts` among them): the element's
//!   observed ARIA role, its type, or the role its type implies must EQUAL the
//!   declared role. Families do not apply — `listbox` is not `combobox`, and a
//!   "dropdown closed" step asserting no `listbox` must not trip on the
//!   trigger. One backward reach is kept, in one direction only: before 0.46
//!   the registry typed `role="combobox"` / `"listbox"` rows `select` and
//!   `role="textbox"` rows `input`, so a declared `select` also finds a
//!   `combobox` or `listbox` kind and a declared `input` a `textbox` kind.
//!
//! It lives in this crate because it is the lowest crate in the runner
//! workspace that every reader of a named type or role reaches: the
//! spec-check matcher here (`criteria`, the snapshot role index and the
//! selectivity weights in `diff`) and the runner's evaluators in `src-tauri`
//! (the workflow step handler's criteria matcher and
//! `spec_experimentation::accuracy`). Keep the tables in step with the TS
//! source — nothing else enforces it.

/// Kinds a caller names interchangeably (`NAMED_TYPE_FAMILIES` in the SDK).
/// Lower-case, as every comparison below is.
const NAMED_TYPE_FAMILIES: &[&[&str]] =
    &[&["input", "textbox"], &["select", "combobox", "listbox"]];

/// The family a caller-named type belongs to, if it belongs to one
/// (`namedTypeFamily`, which answers the name alone for any other name).
pub fn named_type_family(named: &str) -> Option<&'static [&'static str]> {
    let lower = named.trim().to_lowercase();
    NAMED_TYPE_FAMILIES
        .iter()
        .copied()
        .find(|family| family.contains(&lower.as_str()))
}

/// Whether an element of kind `element_type` answers to the caller-named type
/// `named` (`answersToNamedType`): case-insensitive equality with any member
/// of `named`'s family, or with `named` itself. No type answers to nothing.
pub fn answers_to_named_type(element_type: Option<&str>, named: &str) -> bool {
    let Some(element_type) = element_type else {
        return false;
    };
    let kind = element_type.trim().to_lowercase();
    match named_type_family(named) {
        Some(family) => family.contains(&kind.as_str()),
        None => kind == named.trim().to_lowercase(),
    }
}

/// The ARIA role a kind implies for a node carrying no `role` of its own
/// (`IMPLIED_ARIA_ROLES` / `impliedAriaRole` in the SDK): `input` → `textbox`,
/// `select` → `combobox`, `textarea` → `textbox`, the media kinds → `img`.
pub fn implied_aria_role(element_type: &str) -> Option<&'static str> {
    Some(match element_type.trim().to_lowercase().as_str() {
        "button" => "button",
        "input" | "textarea" | "textbox" => "textbox",
        "select" | "combobox" => "combobox",
        "checkbox" => "checkbox",
        "radio" => "radio",
        "link" => "link",
        "menu" => "menu",
        "menuitem" => "menuitem",
        "tab" => "tab",
        "dialog" => "dialog",
        "disclosure" => "group",
        "switch" => "switch",
        "slider" => "slider",
        "listbox" => "listbox",
        "option" => "option",
        "image" | "svg" | "picture" => "img",
        // form, custom, generic, video, canvas: no implied role.
        _ => return None,
    })
}

/// Every role name an element answers to for a ROLE criterion, lower-cased
/// and deduplicated: its observed ARIA role, its type, the role its type
/// implies, plus the one-way backward reach (`select` for a select-family
/// kind, `input` for an input-family kind). [`answers_to_named_role`] is
/// membership in this set, and an index keyed on it finds exactly what that
/// matcher accepts.
pub fn role_names_answered(observed_role: Option<&str>, element_type: Option<&str>) -> Vec<String> {
    let mut names: Vec<String> = Vec::with_capacity(4);
    let mut push = |name: &str| {
        let name = name.trim().to_lowercase();
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    };
    if let Some(role) = observed_role {
        push(role);
    }
    if let Some(kind) = element_type {
        push(kind);
        if let Some(implied) = implied_aria_role(kind) {
            push(implied);
        }
        match named_type_family(kind) {
            Some(family) if family.contains(&"select") => push("select"),
            Some(family) if family.contains(&"input") => push("input"),
            _ => {}
        }
    }
    names
}

/// Whether an element answers to a ROLE a caller declared: the declared role
/// equals the element's observed ARIA role, its type, or the role its type
/// implies (case-insensitive), or it is `select` / `input` and the type is of
/// that family (the pre-0.46 registry spelling).
pub fn answers_to_named_role(
    declared: &str,
    observed_role: Option<&str>,
    element_type: Option<&str>,
) -> bool {
    let declared = declared.trim().to_lowercase();
    role_names_answered(observed_role, element_type).contains(&declared)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_family_member_names_the_whole_family() {
        assert_eq!(named_type_family("input"), Some(&["input", "textbox"][..]));
        assert_eq!(
            named_type_family("TextBox"),
            Some(&["input", "textbox"][..])
        );
        assert_eq!(
            named_type_family("listbox"),
            Some(&["select", "combobox", "listbox"][..])
        );
        assert_eq!(named_type_family("Button"), None);
    }

    #[test]
    fn a_named_type_answers_for_its_family() {
        // 0.46 types role="combobox" `combobox` (the 0.38 registry: `select`)
        // and role="textbox" `textbox` (the 0.38 registry: `input`).
        assert!(answers_to_named_type(Some("textbox"), "input"));
        assert!(answers_to_named_type(Some("input"), "textbox"));
        assert!(answers_to_named_type(Some("combobox"), "select"));
        assert!(answers_to_named_type(Some("listbox"), "Select"));
        assert!(answers_to_named_type(Some("Button"), "button"));
        // ...and stops at the family's edge.
        assert!(!answers_to_named_type(Some("checkbox"), "input"));
        assert!(!answers_to_named_type(Some("textarea"), "input"));
        assert!(!answers_to_named_type(Some("option"), "select"));
        assert!(!answers_to_named_type(None, "input"));
    }

    #[test]
    fn a_named_role_matches_by_equality_with_the_role_the_kind_implies() {
        // Positive: observed role, type, implied role.
        assert!(answers_to_named_role(
            "listbox",
            Some("listbox"),
            Some("generic")
        ));
        assert!(answers_to_named_role("combobox", None, Some("select")));
        assert!(answers_to_named_role("textbox", None, Some("textarea")));
        assert!(answers_to_named_role("textbox", None, Some("input")));
        assert!(answers_to_named_role("img", None, Some("svg")));
        // Negative: a family is not a role.
        assert!(!answers_to_named_role(
            "listbox",
            Some("combobox"),
            Some("combobox")
        ));
        assert!(!answers_to_named_role("listbox", None, Some("select")));
        assert!(!answers_to_named_role(
            "combobox",
            Some("listbox"),
            Some("listbox")
        ));
        assert!(!answers_to_named_role("textbox", None, Some("checkbox")));
    }

    #[test]
    fn the_two_kind_names_reach_back_one_way_only() {
        assert!(answers_to_named_role("select", None, Some("combobox")));
        assert!(answers_to_named_role("select", None, Some("listbox")));
        assert!(answers_to_named_role("input", None, Some("textbox")));
        // Not the other way: declared `combobox` does not find a `listbox`.
        assert!(!answers_to_named_role("combobox", None, Some("listbox")));
        assert!(!answers_to_named_role("input", None, Some("checkbox")));
    }

    #[test]
    fn role_names_are_deduplicated_and_lower_cased() {
        assert_eq!(
            role_names_answered(Some("Combobox"), Some("combobox")),
            vec!["combobox", "select"]
        );
        assert!(role_names_answered(None, None).is_empty());
    }
}
