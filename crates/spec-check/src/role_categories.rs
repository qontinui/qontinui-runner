//! ARIA role → category map (§5.5). Step 3 of Plan 02.
//!
//! Same-category mismatches score 0.5 in Phase 2 (e.g. asserting
//! `role="button"` against a snapshot element with `role="link"` — both
//! `Widget`).
//!
//! Authoritative source: W3C ARIA 1.2 role definitions —
//! <https://www.w3.org/TR/wai-aria-1.2/#role_definitions>. Roles not listed
//! here resolve to [`AriaCategory::Other`].

/// Top-level ARIA role categories. Mirrors the W3C ARIA 1.2 taxonomy with
/// `Live` split out from `Widget`/`Document` for finer-grained fallback
/// scoring (status/alert/log all share live-region semantics that are
/// distinct from interactive widgets).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AriaCategory {
    /// Interactive widgets (button, link, textbox, checkbox, slider, …).
    Widget,
    /// Document structure (article, heading, list, paragraph, table, …).
    Document,
    /// Page landmarks (banner, navigation, main, complementary, …).
    Landmark,
    /// Application windows (dialog, alertdialog).
    Window,
    /// Live region roles (alert, status, log, timer, marquee).
    Live,
    /// Unknown / unmapped role.
    Other,
}

/// Sorted-by-role lookup table. Curated from ARIA 1.2 §5.4 "Definition of
/// Roles" (the abstract roles `widget` / `structure` / `landmark` / `window`
/// are intentionally absent — only concrete roles appear).
static ROLE_TO_CATEGORY: &[(&str, AriaCategory)] = &[
    // --- Widget roles ----------------------------------------------------
    ("button", AriaCategory::Widget),
    ("checkbox", AriaCategory::Widget),
    ("combobox", AriaCategory::Widget),
    ("gridcell", AriaCategory::Widget),
    ("link", AriaCategory::Widget),
    ("menuitem", AriaCategory::Widget),
    ("menuitemcheckbox", AriaCategory::Widget),
    ("menuitemradio", AriaCategory::Widget),
    ("option", AriaCategory::Widget),
    ("progressbar", AriaCategory::Widget),
    ("radio", AriaCategory::Widget),
    ("scrollbar", AriaCategory::Widget),
    ("searchbox", AriaCategory::Widget),
    ("separator", AriaCategory::Widget), // focusable separator only; non-focusable belongs to Document, but disambiguation requires state we don't have here
    ("slider", AriaCategory::Widget),
    ("spinbutton", AriaCategory::Widget),
    ("switch", AriaCategory::Widget),
    ("tab", AriaCategory::Widget),
    ("tabpanel", AriaCategory::Widget),
    ("textbox", AriaCategory::Widget),
    ("treeitem", AriaCategory::Widget),
    // --- Document-structure roles ---------------------------------------
    ("article", AriaCategory::Document),
    ("blockquote", AriaCategory::Document),
    ("caption", AriaCategory::Document),
    ("cell", AriaCategory::Document),
    ("code", AriaCategory::Document),
    ("columnheader", AriaCategory::Document),
    ("definition", AriaCategory::Document),
    ("deletion", AriaCategory::Document),
    ("directory", AriaCategory::Document),
    ("document", AriaCategory::Document),
    ("emphasis", AriaCategory::Document),
    ("feed", AriaCategory::Document),
    ("figure", AriaCategory::Document),
    ("generic", AriaCategory::Document),
    ("group", AriaCategory::Document),
    ("heading", AriaCategory::Document),
    ("img", AriaCategory::Document),
    ("insertion", AriaCategory::Document),
    ("list", AriaCategory::Document),
    ("listitem", AriaCategory::Document),
    ("math", AriaCategory::Document),
    ("meter", AriaCategory::Document),
    ("none", AriaCategory::Document),
    ("note", AriaCategory::Document),
    ("paragraph", AriaCategory::Document),
    ("presentation", AriaCategory::Document),
    ("row", AriaCategory::Document),
    ("rowgroup", AriaCategory::Document),
    ("rowheader", AriaCategory::Document),
    ("strong", AriaCategory::Document),
    ("subscript", AriaCategory::Document),
    ("superscript", AriaCategory::Document),
    ("table", AriaCategory::Document),
    ("term", AriaCategory::Document),
    ("time", AriaCategory::Document),
    ("toolbar", AriaCategory::Document),
    ("tooltip", AriaCategory::Document),
    // --- Landmark roles --------------------------------------------------
    ("banner", AriaCategory::Landmark),
    ("complementary", AriaCategory::Landmark),
    ("contentinfo", AriaCategory::Landmark),
    ("form", AriaCategory::Landmark),
    ("main", AriaCategory::Landmark),
    ("navigation", AriaCategory::Landmark),
    ("region", AriaCategory::Landmark),
    ("search", AriaCategory::Landmark),
    // --- Window roles ----------------------------------------------------
    ("alertdialog", AriaCategory::Window),
    ("dialog", AriaCategory::Window),
    // --- Live-region roles ----------------------------------------------
    ("alert", AriaCategory::Live),
    ("log", AriaCategory::Live),
    ("marquee", AriaCategory::Live),
    ("status", AriaCategory::Live),
    ("timer", AriaCategory::Live),
];

/// Case-insensitive role → category lookup. Unknown roles return
/// [`AriaCategory::Other`].
pub fn category_of(role: &str) -> AriaCategory {
    let needle = role.trim().to_ascii_lowercase();
    for (k, v) in ROLE_TO_CATEGORY {
        if *k == needle.as_str() {
            return *v;
        }
    }
    AriaCategory::Other
}

/// Two roles share a category iff both resolve to the same non-`Other`
/// [`AriaCategory`]. `Other`/`Other` is **not** treated as same-category —
/// pairing two unknown roles should never produce a fallback score.
pub fn same_category(a: &str, b: &str) -> bool {
    let ca = category_of(a);
    let cb = category_of(b);
    ca == cb && ca != AriaCategory::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widget_roles_map_to_widget() {
        assert_eq!(category_of("button"), AriaCategory::Widget);
        assert_eq!(category_of("link"), AriaCategory::Widget);
        assert_eq!(category_of("textbox"), AriaCategory::Widget);
        assert_eq!(category_of("checkbox"), AriaCategory::Widget);
        assert_eq!(category_of("slider"), AriaCategory::Widget);
    }

    #[test]
    fn document_roles_map_to_document() {
        assert_eq!(category_of("article"), AriaCategory::Document);
        assert_eq!(category_of("heading"), AriaCategory::Document);
        assert_eq!(category_of("list"), AriaCategory::Document);
    }

    #[test]
    fn landmark_roles_map_to_landmark() {
        assert_eq!(category_of("main"), AriaCategory::Landmark);
        assert_eq!(category_of("navigation"), AriaCategory::Landmark);
    }

    #[test]
    fn window_roles_map_to_window() {
        assert_eq!(category_of("dialog"), AriaCategory::Window);
        assert_eq!(category_of("alertdialog"), AriaCategory::Window);
    }

    #[test]
    fn live_roles_map_to_live() {
        assert_eq!(category_of("alert"), AriaCategory::Live);
        assert_eq!(category_of("status"), AriaCategory::Live);
        assert_eq!(category_of("timer"), AriaCategory::Live);
    }

    #[test]
    fn unknown_role_maps_to_other() {
        assert_eq!(category_of("nope-not-a-role"), AriaCategory::Other);
        assert_eq!(category_of(""), AriaCategory::Other);
    }

    #[test]
    fn lookup_is_case_insensitive() {
        assert_eq!(category_of("BUTTON"), AriaCategory::Widget);
        assert_eq!(category_of("Button"), AriaCategory::Widget);
        assert_eq!(category_of("  button  "), AriaCategory::Widget);
    }

    #[test]
    fn same_category_widget_widget() {
        assert!(same_category("button", "link"));
        assert!(same_category("textbox", "slider"));
    }

    #[test]
    fn same_category_cross_category_false() {
        assert!(!same_category("button", "dialog")); // Widget vs Window
        assert!(!same_category("article", "main"));  // Document vs Landmark
    }

    #[test]
    fn same_category_other_other_is_false() {
        // Two unknown roles must not be treated as "matching" via the
        // fallback path — the contract is "shared *known* category".
        assert!(!same_category("fooble", "wibble"));
    }
}
