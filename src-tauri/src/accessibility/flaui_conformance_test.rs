//! FlaUI-compatibility conformance tests for the accessibility query layer.
//!
//! These are fast, pure-Rust unit tests. They build in-memory `UnifiedNode`
//! trees and exercise `QueryBuilder` without touching any real OS
//! accessibility stack — **no `#[ignore]`**, so they run on every
//! `cargo test`. Port targets: the selector idioms in FlaUI's
//! `FlaUI.Core.UITests.ConditionFactoryTests`, narrowed to cases the
//! existing unit tests in `query::tests` don't already cover.

#![cfg(test)]

use crate::accessibility::model::{
    ControlType, NodeSource, TriBool, UnifiedBounds, UnifiedNode, UnifiedRole, UnifiedState,
};
use crate::accessibility::query::{
    try_as_button, try_as_checkbox, try_as_combobox, try_as_textbox, try_as_window, By,
    QueryBuilder, StateMatcher,
};

// ─── Fixture: a richer tree than query::tests::make_tree ────────────────────

/// Mini dialog mimicking a FlaUI-style target app: a top-level window with
/// a nav tab, a settings pane (buttons + edit + checkbox + combo box), and
/// a hidden/disabled tooltip so state-matcher tests have something to bite
/// on. Ref IDs are sequential to make assertions easy to read.
fn make_rich_tree() -> UnifiedNode {
    UnifiedNode {
        ref_id: "@e0".into(),
        role: UnifiedRole::Window,
        name: Some("SampleApp".into()),
        description: Some("Main window".into()),
        bounds: Some(UnifiedBounds {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        }),
        automation_id: Some("win_root".into()),
        class_name: Some("Window".into()),
        source: NodeSource::Uia,
        platform_handle: Some(1),
        children: vec![
            // TitleBar — non-interactive, has descriptive text
            UnifiedNode {
                ref_id: "@e1".into(),
                role: UnifiedRole::Titlebar,
                name: Some("SampleApp - Untitled".into()),
                ..Default::default()
            },
            // OK button with Invoke pattern
            UnifiedNode {
                ref_id: "@e2".into(),
                role: UnifiedRole::Button,
                name: Some("OK".into()),
                automation_id: Some("btnOk".into()),
                class_name: Some("Button".into()),
                is_interactive: true,
                platform_handle: Some(2),
                supported_patterns: vec![crate::accessibility::model::InteractionPattern::Invoke],
                ..Default::default()
            },
            // Cancel button with Invoke pattern
            UnifiedNode {
                ref_id: "@e3".into(),
                role: UnifiedRole::Button,
                name: Some("Cancel".into()),
                automation_id: Some("btnCancel".into()),
                class_name: Some("Button".into()),
                is_interactive: true,
                platform_handle: Some(3),
                ..Default::default()
            },
            // Email textbox — focused + editable
            UnifiedNode {
                ref_id: "@e4".into(),
                role: UnifiedRole::Textbox,
                name: Some("Email".into()),
                value: Some("alice@example.com".into()),
                description: Some("User email address".into()),
                automation_id: Some("txtEmail".into()),
                class_name: Some("TextBox".into()),
                is_interactive: true,
                platform_handle: Some(4),
                state: UnifiedState {
                    is_focused: true,
                    is_editable: true,
                    ..Default::default()
                },
                supported_patterns: vec![crate::accessibility::model::InteractionPattern::Value],
                ..Default::default()
            },
            // Remember-me checkbox — checked
            UnifiedNode {
                ref_id: "@e5".into(),
                role: UnifiedRole::Checkbox,
                name: Some("Remember me".into()),
                automation_id: Some("chkRemember".into()),
                is_interactive: true,
                platform_handle: Some(5),
                state: UnifiedState {
                    is_checked: TriBool::True,
                    ..Default::default()
                },
                supported_patterns: vec![crate::accessibility::model::InteractionPattern::Toggle],
                ..Default::default()
            },
            // Country combobox with two option children
            UnifiedNode {
                ref_id: "@e6".into(),
                role: UnifiedRole::Combobox,
                name: Some("Country".into()),
                value: Some("USA".into()),
                automation_id: Some("cmbCountry".into()),
                is_interactive: true,
                platform_handle: Some(6),
                children: vec![
                    UnifiedNode {
                        ref_id: "@e7".into(),
                        role: UnifiedRole::Listitem,
                        name: Some("USA".into()),
                        ..Default::default()
                    },
                    UnifiedNode {
                        ref_id: "@e8".into(),
                        role: UnifiedRole::Listitem,
                        name: Some("Canada".into()),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            // Disabled tooltip (hidden) — used to test NOT / StateMatcher
            UnifiedNode {
                ref_id: "@e9".into(),
                role: UnifiedRole::Tooltip,
                name: Some("Help".into()),
                is_interactive: false,
                state: UnifiedState {
                    is_disabled: true,
                    is_hidden: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            // An Edit control (UIA-style) with no advertised patterns
            UnifiedNode {
                ref_id: "@e10".into(),
                role: UnifiedRole::Edit,
                name: Some("Notes".into()),
                value: Some("".into()),
                class_name: Some("Edit".into()),
                is_interactive: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

// ─── by_role / by_roles ─────────────────────────────────────────────────────

#[test]
fn by_role_matches_only_exact_role() {
    let tree = make_rich_tree();
    let results = QueryBuilder::new()
        .by_role(UnifiedRole::Button)
        .find_all(&tree);
    assert_eq!(results.len(), 2, "should find exactly OK and Cancel");
    let names: Vec<_> = results.iter().filter_map(|n| n.name.as_deref()).collect();
    assert!(names.contains(&"OK"));
    assert!(names.contains(&"Cancel"));
}

#[test]
fn by_roles_accepts_union() {
    let tree = make_rich_tree();
    let results = QueryBuilder::new()
        .by_roles(vec![UnifiedRole::Textbox, UnifiedRole::Edit])
        .find_all(&tree);
    assert_eq!(results.len(), 2, "should find Email textbox + Notes edit");
}

// ─── by_label / by_label_contains / by_text ─────────────────────────────────

#[test]
fn by_label_exact_match() {
    let tree = make_rich_tree();
    let results = QueryBuilder::new().by_label("OK").find_all(&tree);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ref_id, "@e2");
}

#[test]
fn by_label_contains_is_substring() {
    let tree = make_rich_tree();
    let results = QueryBuilder::new()
        .by_label_contains("Remember")
        .find_all(&tree);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ref_id, "@e5");
}

#[test]
fn by_text_searches_name_value_and_description() {
    let tree = make_rich_tree();

    // Match on `name` only
    let by_name = QueryBuilder::new().by_text("Email").find_all(&tree);
    assert!(by_name.iter().any(|n| n.ref_id == "@e4"));

    // Match on `value` only — "alice@example.com" lives in value
    let by_value = QueryBuilder::new().by_text("alice@").find_all(&tree);
    assert_eq!(by_value.len(), 1);
    assert_eq!(by_value[0].ref_id, "@e4");

    // Match on `description` only — "User email address"
    let by_desc = QueryBuilder::new()
        .by_text("email address")
        .case_insensitive()
        .find_all(&tree);
    assert!(by_desc.iter().any(|n| n.ref_id == "@e4"));

    // Substring that exists in none of the three fields
    let no_match = QueryBuilder::new().by_text("zzz-unknown").find_all(&tree);
    assert!(no_match.is_empty());
}

#[test]
fn by_text_differs_from_by_label_contains_on_value_field() {
    let tree = make_rich_tree();
    // "alice@" lives only in the `value` field of @e4 — by_label_contains
    // hits `name` only and should miss it.
    assert!(QueryBuilder::new()
        .by_label_contains("alice@")
        .find_all(&tree)
        .is_empty());
    // by_text widens to value/description and should hit.
    assert_eq!(
        QueryBuilder::new().by_text("alice@").find_all(&tree).len(),
        1
    );
}

// ─── by_automation_id / by_class_name ───────────────────────────────────────

#[test]
fn by_automation_id_matches_exactly() {
    let tree = make_rich_tree();
    let results = QueryBuilder::new()
        .by_automation_id("txtEmail")
        .find_all(&tree);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].role, UnifiedRole::Textbox);
}

#[test]
fn by_class_name_finds_all_button_class() {
    let tree = make_rich_tree();
    let results = QueryBuilder::new().by_class_name("Button").find_all(&tree);
    assert_eq!(results.len(), 2, "OK and Cancel both class=Button");
}

// ─── by_control_type ────────────────────────────────────────────────────────

#[test]
fn by_control_type_button() {
    let tree = make_rich_tree();
    let results = QueryBuilder::new()
        .by_control_type(ControlType::Button)
        .find_all(&tree);
    assert_eq!(results.len(), 2);
}

#[test]
fn by_control_type_checkbox_and_combo() {
    let tree = make_rich_tree();
    let cb = QueryBuilder::new()
        .by_control_type(ControlType::CheckBox)
        .find_all(&tree);
    assert_eq!(cb.len(), 1);
    assert_eq!(cb[0].ref_id, "@e5");

    let combo = QueryBuilder::new()
        .by_control_type(ControlType::ComboBox)
        .find_all(&tree);
    assert_eq!(combo.len(), 1);
    assert_eq!(combo[0].ref_id, "@e6");
}

#[test]
fn by_control_type_edit_matches_only_edit_role_not_textbox() {
    // ControlType::Edit maps to UnifiedRole::Edit specifically;
    // UnifiedRole::Textbox does not match by_control_type(Edit) — it's a
    // separate ARIA flavour. (Use TextBox typed wrapper if you want both.)
    let tree = make_rich_tree();
    let results = QueryBuilder::new()
        .by_control_type(ControlType::Edit)
        .find_all(&tree);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ref_id, "@e10");
}

#[test]
fn by_control_type_window_matches_root() {
    let tree = make_rich_tree();
    let results = QueryBuilder::new()
        .by_control_type(ControlType::Window)
        .find_all(&tree);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ref_id, "@e0");
}

#[test]
fn control_type_roundtrip_through_uia_ids() {
    // Covers the UIA_CONTROLTYPE_ID <-> ControlType mapping end-to-end.
    for (id, expected) in &[
        (50000, ControlType::Button),
        (50002, ControlType::CheckBox),
        (50004, ControlType::Edit),
        (50015, ControlType::Slider),
        (50032, ControlType::Window),
    ] {
        let got = ControlType::from_uia_control_type_id(*id).expect("id maps");
        assert_eq!(got, *expected);
        assert_eq!(got.to_uia_control_type_id(), *id);
    }
    // Unknown ID returns None
    assert!(ControlType::from_uia_control_type_id(12345).is_none());
}

// ─── AND / OR / NOT composition ─────────────────────────────────────────────

#[test]
fn and_filter_narrows_multiple_conditions() {
    let tree = make_rich_tree();
    let results = QueryBuilder::new()
        .filter(By::And(vec![
            By::Role(UnifiedRole::Button),
            By::Label("OK".into()),
        ]))
        .find_all(&tree);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ref_id, "@e2");
}

#[test]
fn or_filter_unions_roles() {
    let tree = make_rich_tree();
    let results = QueryBuilder::new()
        .filter(By::Or(vec![
            By::Role(UnifiedRole::Checkbox),
            By::Role(UnifiedRole::Combobox),
        ]))
        .find_all(&tree);
    assert_eq!(results.len(), 2);
}

#[test]
fn not_filter_inverts_match() {
    let tree = make_rich_tree();
    // Everything that isn't a Button
    let results = QueryBuilder::new()
        .filter(By::Not(Box::new(By::Role(UnifiedRole::Button))))
        .find_all(&tree);
    let button_count = results
        .iter()
        .filter(|n| n.role == UnifiedRole::Button)
        .count();
    assert_eq!(button_count, 0);
    // Should include window + titlebar + textbox + checkbox + combo +
    // 2 options + tooltip + edit = 9 total
    assert!(!results.is_empty());
}

// ─── interactive() convenience ──────────────────────────────────────────────

#[test]
fn interactive_filter_only_returns_interactive_nodes() {
    let tree = make_rich_tree();
    let results = QueryBuilder::new().interactive().find_all(&tree);
    assert!(results.iter().all(|n| n.is_interactive));
    // OK, Cancel, Email textbox, Remember checkbox, Country combo, Notes edit
    assert_eq!(results.len(), 6);
}

// ─── by_state / StateMatcher ────────────────────────────────────────────────

#[test]
fn by_state_matches_checked_and_focused() {
    let tree = make_rich_tree();

    let checked = QueryBuilder::new()
        .by_state(StateMatcher {
            checked: Some(TriBool::True),
            ..Default::default()
        })
        .find_all(&tree);
    assert_eq!(checked.len(), 1);
    assert_eq!(checked[0].ref_id, "@e5");

    let focused = QueryBuilder::new()
        .by_state(StateMatcher {
            focused: Some(true),
            ..Default::default()
        })
        .find_all(&tree);
    assert_eq!(focused.len(), 1);
    assert_eq!(focused[0].ref_id, "@e4");
}

#[test]
fn by_state_hidden_filters_tooltip() {
    let tree = make_rich_tree();
    let hidden = QueryBuilder::new()
        .by_state(StateMatcher {
            hidden: Some(true),
            ..Default::default()
        })
        .find_all(&tree);
    assert_eq!(hidden.len(), 1);
    assert_eq!(hidden[0].ref_id, "@e9");
}

// ─── Edge cases ─────────────────────────────────────────────────────────────

#[test]
fn empty_tree_returns_no_results() {
    let empty = UnifiedNode {
        ref_id: "@e0".into(),
        role: UnifiedRole::Window,
        ..Default::default()
    };
    let buttons = QueryBuilder::new()
        .by_role(UnifiedRole::Button)
        .find_all(&empty);
    assert!(buttons.is_empty());

    let any_text = QueryBuilder::new().by_text("whatever").find_all(&empty);
    assert!(any_text.is_empty());
}

#[test]
fn missing_optional_fields_dont_spuriously_match() {
    // A node with no name/value/description should never match by_text.
    let bare = UnifiedNode {
        ref_id: "@e0".into(),
        role: UnifiedRole::Pane,
        name: None,
        value: None,
        description: None,
        ..Default::default()
    };
    assert!(QueryBuilder::new()
        .by_text("anything")
        .find_all(&bare)
        .is_empty());
    assert!(QueryBuilder::new()
        .by_label("anything")
        .find_all(&bare)
        .is_empty());
    assert!(QueryBuilder::new()
        .by_automation_id("anything")
        .find_all(&bare)
        .is_empty());
    // Empty substring matches any present string, but still not None fields.
    assert!(QueryBuilder::new()
        .by_label_contains("")
        .find_all(&bare)
        .is_empty());
}

#[test]
fn stale_element_refs_do_not_panic() {
    // Simulate a tree where a previously captured ref is no longer present.
    // `find_first` must simply return `None`, never panic on missing data.
    let tree = make_rich_tree();
    let gone = tree.find_by_ref("@e999");
    assert!(gone.is_none());

    // And a query targeting a nonexistent automation_id is silent.
    let results = QueryBuilder::new()
        .by_automation_id("does_not_exist")
        .find_all(&tree);
    assert!(results.is_empty());
}

// ─── Typed accessors ────────────────────────────────────────────────────────

#[test]
fn try_as_button_on_button_succeeds() {
    let tree = make_rich_tree();
    let ok = tree.find_by_ref("@e2").unwrap();
    let btn = try_as_button(ok).expect("@e2 is a Button");
    assert_eq!(btn.name(), Some("OK"));
    let req = btn.invoke();
    assert_eq!(
        req.pattern,
        crate::accessibility::model::InteractionPattern::Invoke
    );
}

#[test]
fn try_as_button_on_non_button_fails() {
    let tree = make_rich_tree();
    let email = tree.find_by_ref("@e4").unwrap();
    assert!(try_as_button(email).is_none());
}

#[test]
fn try_as_textbox_accepts_edit_and_textbox_roles() {
    let tree = make_rich_tree();
    // @e4 is role = Textbox
    assert!(try_as_textbox(tree.find_by_ref("@e4").unwrap()).is_some());
    // @e10 is role = Edit
    assert!(try_as_textbox(tree.find_by_ref("@e10").unwrap()).is_some());
    // @e2 is role = Button, should reject
    assert!(try_as_textbox(tree.find_by_ref("@e2").unwrap()).is_none());
}

#[test]
fn try_as_checkbox_reads_checked_state() {
    let tree = make_rich_tree();
    let remember = tree.find_by_ref("@e5").unwrap();
    let cb = try_as_checkbox(remember).expect("@e5 is a Checkbox");
    assert_eq!(cb.is_checked(), Some(true));
}

#[test]
fn try_as_combobox_reads_option_names() {
    let tree = make_rich_tree();
    let country = tree.find_by_ref("@e6").unwrap();
    let combo = try_as_combobox(country).expect("@e6 is a ComboBox");
    assert_eq!(combo.selected(), Some("USA"));
    assert_eq!(combo.option_names(), vec!["USA", "Canada"]);
}

#[test]
fn try_as_window_reads_title() {
    let tree = make_rich_tree();
    let w = try_as_window(&tree).expect("root is a Window");
    assert_eq!(w.title(), Some("SampleApp"));
    assert_eq!(w.class_name(), Some("Window"));
    assert!(!w.is_modal());
}
