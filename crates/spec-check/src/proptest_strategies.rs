//! Proptest strategy generators for adversarial `UIBridgeSnapshot` /
//! `IrPageSpec` inputs. Step 10a of Plan 02.
//!
//! These strategies bias toward small-but-collision-prone inputs so the
//! determinism property test exercises the selectivity-weighted diff path,
//! the tie-break paths, the text-norm path, and degenerate-shape paths
//! (duplicate IDs, NBSP-only text, empty strings, zero-state specs).
//!
//! Compiled `#[cfg(test)]` only; the module is wired into `lib.rs` under
//! the same gate so the integration test surface stays compile-clean.

use proptest::collection::vec;
use proptest::option;
use proptest::prelude::*;
use serde_json::{json, Value as JsonValue};

use qontinui_types::ir::{IrAssertion, IrAssertionTarget, IrPageSpec, IrState};
use qontinui_types::ui_bridge::{
    ElementIdentifier, ElementRect, ElementState, UIBridgeElement, UIBridgeSnapshot,
};

// ===========================================================================
// Dictionaries — tiny on purpose so collisions are likely.
// ===========================================================================

/// Small role vocabulary — biases collisions across many elements.
const ROLES: &[&str] = &[
    "button", "link", "heading", "textbox", "checkbox", "img", "list", "listitem",
];

/// Small tag vocabulary — overlaps with `ROLES` in HTML reality.
const TAGS: &[&str] = &["button", "a", "h1", "h2", "input", "div", "span", "li"];

/// Small label vocabulary — `aria_label` / `accessible_name` text pool.
const LABELS: &[&str] = &["Save", "Cancel", "Submit", "Edit", "Delete", "Open", "Close", "OK"];

/// Visible-text pool — picks short and longer strings to exercise tokenizer
/// branches and includes the NBSP-only string to exercise text-norm.
const TEXTS: &[&str] = &[
    "Save",
    "Cancel",
    "submit",
    "Edit profile",
    "Delete file",
    "Open settings dialog",
    "\u{00A0}\u{00A0}\u{00A0}", // NBSP-only — exercises text-norm collapse
    "",
    "OK",
];

/// Severity pool covered by the AssertionSeverityCounts buckets plus an
/// "unknown" string so the unknown-severity bucket gets exercised.
const SEVERITIES: &[&str] = &["critical", "error", "warning", "info", "blocker"];

// ===========================================================================
// Snapshot strategy
// ===========================================================================

/// Generate a well-formed `UIBridgeSnapshot` with 0..=50 elements drawn from
/// the small dictionaries above. Designed for property testing — small input
/// space + dense collisions = fast convergence on adversarial branches.
pub fn arbitrary_snapshot() -> impl Strategy<Value = UIBridgeSnapshot> {
    // 0..=50 elements, with id-collision rate ~1/4 (Just(0..=2) keeps the
    // distinct-id space small so duplicates land regularly).
    let elements_strat = vec(arbitrary_element(), 0..=50);
    (
        // Timestamp: 0..=10^13 ms (well past epoch, before y10k).
        0i64..10_000_000_000_000,
        elements_strat,
    )
        .prop_map(|(timestamp, elements)| UIBridgeSnapshot {
            timestamp,
            elements,
            components: Vec::new(),
            workflows: Vec::new(),
            modal_stack: None,
            toasts: None,
            undo_redo: None,
            current_route: None,
            segments: Vec::new(),
        })
}

fn arbitrary_element() -> impl Strategy<Value = UIBridgeElement> {
    (
        // ID space restricted so collisions land regularly.
        prop::sample::select(vec!["e0", "e1", "e2", "e3", "e4", "e5", "e6", "e7"]),
        prop::sample::select(TAGS.to_vec()),
        option::of(prop::sample::select(LABELS.to_vec())),
        option::of(prop::sample::select(ROLES.to_vec())),
        option::of(prop::sample::select(TAGS.to_vec())),
        option::of(prop::sample::select(LABELS.to_vec())),
        option::of(prop::sample::select(LABELS.to_vec())),
        option::of(prop::sample::select(TEXTS.to_vec())),
        any::<bool>(),
        any::<bool>(),
        0i64..10_000_000_000_000,
        arbitrary_identifier(),
    )
        .prop_map(
            |(
                id,
                el_type,
                label,
                role,
                tag_name,
                aria_label,
                accessible_name,
                text,
                visible,
                enabled,
                registered_at,
                identifier,
            )| UIBridgeElement {
                id: id.to_string(),
                element_type: el_type.to_string(),
                label: label.map(String::from),
                actions: Vec::new(),
                custom_actions: None,
                identifier,
                state: ElementState {
                    visible,
                    enabled,
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
                },
                registered_at,
                mounted: true,
                bbox: None,
                visible: Some(visible),
                role: role.map(String::from),
                tag_name: tag_name.map(String::from),
                aria_label: aria_label.map(String::from),
                accessible_name: accessible_name.map(String::from),
                text: text.map(String::from),
            },
        )
}

fn arbitrary_identifier() -> impl Strategy<Value = ElementIdentifier> {
    // html_id space overlaps the element-id space so the snapshot's by_id
    // index sees multi-source collisions.
    let html_pool = vec!["e0", "e1", "e2", "alt-a", "alt-b"];
    (
        option::of(prop::sample::select(html_pool.clone())),
        option::of(prop::sample::select(html_pool.clone())),
        option::of(prop::sample::select(html_pool.clone())),
        option::of(prop::sample::select(html_pool)),
    )
        .prop_map(|(html_id, test_id, awas_id, ui_id)| ElementIdentifier {
            ui_id: ui_id.map(String::from),
            test_id: test_id.map(String::from),
            awas_id: awas_id.map(String::from),
            html_id: html_id.map(String::from),
            xpath: String::new(),
            selector: String::new(),
        })
}

// ===========================================================================
// Spec strategy
// ===========================================================================

/// Generate a well-formed `IrPageSpec` with 0..=5 states, each with 0..=5
/// assertions. Criteria are synthesized as partially-populated
/// `IrElementCriteria` JSON objects drawn from the same dictionaries used by
/// the snapshot strategy so both pass and fail paths fire.
pub fn arbitrary_spec() -> impl Strategy<Value = IrPageSpec> {
    let states_strat = vec(arbitrary_state(), 0..=5);
    states_strat.prop_map(|states| IrPageSpec {
        version: "1.0".to_string(),
        id: "page-prop".to_string(),
        name: "Property-test page".to_string(),
        description: None,
        metadata: None,
        provenance: None,
        states,
        transitions: Vec::new(),
        synthesized_groups: None,
        initial_state: None,
    })
}

fn arbitrary_state() -> impl Strategy<Value = IrState> {
    (
        "s[0-9]",
        vec(arbitrary_assertion(), 0..=5),
        option::of(any::<bool>()),
    )
        .prop_map(|(id, assertions, is_initial)| IrState {
            id: id.clone(),
            name: format!("{id}-name"),
            description: None,
            assertions,
            excluded_elements: None,
            conditions: None,
            is_initial,
            is_terminal: None,
            blocking: None,
            group: None,
            path_cost: None,
            precondition: None,
            element_ids: None,
            incoming_transitions: None,
            metadata: None,
            provenance: None,
            cross_refs: None,
        })
}

fn arbitrary_assertion() -> impl Strategy<Value = IrAssertion> {
    (
        "a[0-9]",
        prop::sample::select(SEVERITIES.to_vec()),
        any::<bool>(),
        arbitrary_criteria(),
    )
        .prop_map(|(id, severity, enabled, criteria)| IrAssertion {
            id: id.clone(),
            description: format!("desc-{id}"),
            category: "a11y".to_string(),
            severity: severity.to_string(),
            assertion_type: "exists".to_string(),
            target: IrAssertionTarget {
                kind: "search".to_string(),
                criteria,
                label: format!("label-{id}"),
            },
            source: "proptest".to_string(),
            reviewed: false,
            enabled,
            precondition: None,
        })
}

/// Synthesize a partially-populated criteria object as a `serde_json::Value`
/// so the evaluator's loose-criteria parsing path is exercised. Each field is
/// independently coin-flipped — yields the full Cartesian product of
/// criterion shapes from "empty/anything" through "fully-constrained".
fn arbitrary_criteria() -> impl Strategy<Value = JsonValue> {
    (
        option::of(prop::sample::select(ROLES.to_vec())),
        option::of(prop::sample::select(TAGS.to_vec())),
        option::of(prop::sample::select(TEXTS.to_vec())),
        option::of(prop::sample::select(LABELS.to_vec())),
        option::of(prop::sample::select(LABELS.to_vec())),
    )
        .prop_map(
            |(role, tag_name, text, aria_label, accessible_name)| {
                let mut obj = serde_json::Map::new();
                if let Some(v) = role {
                    obj.insert("role".to_string(), json!(v));
                }
                if let Some(v) = tag_name {
                    obj.insert("tagName".to_string(), json!(v));
                }
                if let Some(v) = text {
                    obj.insert("text".to_string(), json!(v));
                }
                if let Some(v) = aria_label {
                    obj.insert("ariaLabel".to_string(), json!(v));
                }
                if let Some(v) = accessible_name {
                    obj.insert("accessibleName".to_string(), json!(v));
                }
                JsonValue::Object(obj)
            },
        )
}
