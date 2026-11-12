//! The UI Bridge SDK's named-type family rule — the runner's ONE Rust copy.
//!
//! Mirrors `NAMED_TYPE_FAMILIES`, `namedTypeFamily` and `answersToNamedType`
//! in `@qontinui/ui-bridge` `packages/ui-bridge/src/core/element-kind.ts`
//! (0.46.0, plan
//! `2026-11-09-an-elements-advertised-verbs-depend-on-which-path-found-it`).
//!
//! The SDK keeps `input` / `textbox` and `select` / `combobox` / `listbox`
//! apart as element KINDS, because their verbs differ (a native `<input>`
//! takes `type`, an ARIA `textbox` does not). But a person or an agent NAMES
//! them interchangeably — "the email field" means either — and before 0.46 the
//! registry path reported every `role="textbox"` as `input` and every
//! `role="combobox"` / `"listbox"` as `select`. So:
//!
//! - a reader that MATCHES a type a caller named — a query, a filter, a spec
//!   criterion's `role` or `type`, a selector — matches the name's whole
//!   family ([`answers_to_named_type`]);
//! - a reader that CLASSIFIES an element by its own type reads the precise
//!   kind, and does not call this module.
//!
//! It lives in this crate because it is the lowest crate in the runner
//! workspace that both readers of a named type reach: the spec-check matcher
//! here (`criteria::score_role`, `snapshot::IndexedSnapshot`'s role index) and
//! the runner's own evaluators in `src-tauri` (the workflow step handler's
//! criteria matcher and `spec_experimentation::accuracy`), which depend on
//! this crate. Keep the families in step with the TS table — nothing else
//! enforces it.

/// Kinds a caller names interchangeably (`NAMED_TYPE_FAMILIES` in the SDK).
/// Lower-case, as every comparison below is.
const NAMED_TYPE_FAMILIES: &[&[&str]] =
    &[&["input", "textbox"], &["select", "combobox", "listbox"]];

/// The kinds a caller-named type answers to, lower-cased: its family, else
/// itself alone (`namedTypeFamily`).
pub fn named_type_family(named: &str) -> Vec<String> {
    let lower = named.to_lowercase();
    match NAMED_TYPE_FAMILIES
        .iter()
        .find(|family| family.contains(&lower.as_str()))
    {
        Some(family) => family.iter().map(|kind| (*kind).to_string()).collect(),
        None => vec![lower],
    }
}

/// Whether an element of kind `element_type` answers to the caller-named type
/// `named` (`answersToNamedType`): case-insensitive equality with any member
/// of `named`'s family. An element with no type answers to nothing.
pub fn answers_to_named_type(element_type: Option<&str>, named: &str) -> bool {
    let Some(element_type) = element_type else {
        return false;
    };
    let kind = element_type.to_lowercase();
    named_type_family(named)
        .iter()
        .any(|member| *member == kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_family_member_names_the_whole_family() {
        assert_eq!(named_type_family("input"), vec!["input", "textbox"]);
        assert_eq!(named_type_family("TextBox"), vec!["input", "textbox"]);
        assert_eq!(
            named_type_family("combobox"),
            vec!["select", "combobox", "listbox"]
        );
        assert_eq!(
            named_type_family("listbox"),
            vec!["select", "combobox", "listbox"]
        );
    }

    #[test]
    fn a_name_outside_every_family_names_itself_lower_cased() {
        assert_eq!(named_type_family("Button"), vec!["button"]);
        assert_eq!(named_type_family("generic"), vec!["generic"]);
        assert_eq!(named_type_family(""), vec![""]);
    }

    #[test]
    fn the_kinds_0_46_moved_still_answer_to_their_old_name() {
        // 0.46 re-kinded role="textbox"/contenteditable `input` -> `textbox`
        // and role="combobox" `select` -> `combobox` on the registry path.
        assert!(answers_to_named_type(Some("textbox"), "input"));
        assert!(answers_to_named_type(Some("input"), "textbox"));
        assert!(answers_to_named_type(Some("combobox"), "select"));
        assert!(answers_to_named_type(Some("listbox"), "select"));
        assert!(answers_to_named_type(Some("select"), "combobox"));
        assert!(answers_to_named_type(Some("Combobox"), "LISTBOX"));
    }

    #[test]
    fn the_families_do_not_reach_past_themselves() {
        assert!(!answers_to_named_type(Some("checkbox"), "input"));
        assert!(!answers_to_named_type(Some("textarea"), "input"));
        assert!(!answers_to_named_type(Some("option"), "select"));
        assert!(!answers_to_named_type(Some("button"), "link"));
        assert!(answers_to_named_type(Some("Button"), "button"));
    }

    #[test]
    fn an_element_with_no_type_answers_to_nothing() {
        assert!(!answers_to_named_type(None, "input"));
        assert!(!answers_to_named_type(None, ""));
    }
}
