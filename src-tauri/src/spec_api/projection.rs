//! Rust port of `projectIRToBundledPage`
//! (`qontinui-schemas/ts/src/ui-bridge-ir/projection.ts`).
//!
//! Same intent: pure function from `IrDocument` (+ optional notes) to the
//! legacy `*.spec.uibridge.json` shape. Same byte-stable output rule —
//! object keys are sorted lexicographically at the final step, arrays
//! preserve input order, no timestamps / random IDs.
//!
//! Tested against the TS projection's output for the same fixture (see
//! `spec_api/tests.rs::projection_matches_typescript_fixture`).

use serde_json::{json, Map, Value};

use super::types::{
    IrAssertion, IrDocument, IrElementCriteria, IrGroup, IrState, IrTransition, IrTransitionAction,
    LegacyAssertion, LegacyAssertionTarget, LegacyGroup, LegacyProcessStep, LegacySpec,
    LegacyStateMachine, LegacyStateMachineState, LegacyTransition,
};

/// Recursively sort object keys lexicographically. Arrays preserve input
/// order. Mirrors the TS `sortKeys` pass in `projection.ts`.
fn sort_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            // BTreeMap iterates in lexicographic order; serde_json's `Map`
            // does not, so we collect into a btree to enforce sort, then
            // back into a `Map` (which preserves insertion order on
            // serialization with the default `preserve_order` feature off
            // — and even with it on, our sorted insertion is what counts).
            let mut sorted: std::collections::BTreeMap<String, Value> =
                std::collections::BTreeMap::new();
            for (k, v) in map {
                sorted.insert(k, sort_keys(v));
            }
            let mut out = Map::new();
            for (k, v) in sorted {
                out.insert(k, v);
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(sort_keys).collect()),
        other => other,
    }
}

/// Convert IR criteria to legacy criteria. Mirrors `convertCriteria` in
/// the TS projection.
fn convert_criteria(criteria: &IrElementCriteria) -> Value {
    let mut out = Map::new();
    if let Some(role) = &criteria.role {
        out.insert("role".to_string(), Value::String(role.clone()));
    }
    if let Some(tag_name) = &criteria.tag_name {
        out.insert("tagName".to_string(), Value::String(tag_name.clone()));
    }
    if let Some(text) = &criteria.text {
        out.insert("textContent".to_string(), Value::String(text.clone()));
    }
    if let Some(text_contains) = &criteria.text_contains {
        out.insert(
            "textContains".to_string(),
            Value::String(text_contains.clone()),
        );
    }
    // Prefer explicit accessibleName when present; fall back to ariaLabel.
    if let Some(name) = &criteria.accessible_name {
        out.insert("accessibleName".to_string(), Value::String(name.clone()));
    } else if let Some(aria_label) = &criteria.aria_label {
        out.insert(
            "accessibleName".to_string(),
            Value::String(aria_label.clone()),
        );
    }
    if let Some(id) = &criteria.id {
        out.insert("id".to_string(), Value::String(id.clone()));
    }
    if let Some(attrs) = &criteria.attributes {
        let mut data_attrs = Map::new();
        for (k, v) in attrs {
            data_attrs.insert(k.clone(), Value::String(v.clone()));
        }
        out.insert("dataAttributes".to_string(), Value::Object(data_attrs));
    }
    Value::Object(out)
}

/// Build a single legacy assertion. Mirrors `buildAssertion` in the TS
/// projection — including the placeholder fallback when criteria is `None`.
fn build_assertion(
    state: &IrState,
    index: usize,
    criteria: Option<&IrElementCriteria>,
) -> LegacyAssertion {
    let description = state
        .metadata
        .as_ref()
        .and_then(|m| m.description.clone())
        .unwrap_or_else(|| format!("Required element {} for state {}", index, state.name));
    let target_criteria = match criteria {
        Some(c) => convert_criteria(c),
        None => Value::Object(Map::new()),
    };
    LegacyAssertion {
        id: format!("{}-elem-{}", state.id, index),
        description,
        category: "element-presence".to_string(),
        severity: "critical".to_string(),
        assertion_type: "exists".to_string(),
        target: LegacyAssertionTarget {
            kind: "search".to_string(),
            criteria: target_criteria,
            label: format!("Required element for {}", state.name),
        },
        source: "ai-generated".to_string(),
        reviewed: false,
        enabled: true,
        precondition: state.precondition.clone(),
    }
}

fn build_group(state: &IrState) -> LegacyGroup {
    let assertions: Vec<LegacyAssertion> = if state.required_elements.is_empty() {
        vec![build_assertion(state, 0, None)]
    } else {
        state
            .required_elements
            .iter()
            .enumerate()
            .map(|(i, c)| build_assertion(state, i, Some(c)))
            .collect()
    };
    let description = state
        .description
        .clone()
        .or_else(|| state.metadata.as_ref().and_then(|m| m.description.clone()))
        .unwrap_or_default();
    let source = state
        .provenance
        .as_ref()
        .map(|p| p.source.clone())
        .unwrap_or_else(|| "ai-generated".to_string());
    LegacyGroup {
        id: state.id.clone(),
        name: state.name.clone(),
        description,
        category: "element-presence".to_string(),
        assertions,
        source,
    }
}

/// Convert an `IrAssertion` (synthesis-produced) into the legacy assertion
/// shape. Mirrors the TS `synthesizedAssertionToLegacy` helper. The criteria
/// pass through verbatim — `LegacyAssertionTarget.criteria` is already
/// `serde_json::Value`, so no re-shaping happens.
fn synthesized_assertion_to_legacy(assertion: &IrAssertion) -> LegacyAssertion {
    LegacyAssertion {
        id: assertion.id.clone(),
        description: assertion.description.clone(),
        category: assertion.category.clone(),
        severity: assertion.severity.clone(),
        assertion_type: assertion.assertion_type.clone(),
        target: LegacyAssertionTarget {
            kind: assertion.target.kind.clone(),
            criteria: assertion.target.criteria.clone(),
            label: assertion.target.label.clone(),
        },
        source: assertion.source.clone(),
        reviewed: assertion.reviewed,
        enabled: assertion.enabled,
        precondition: assertion.precondition.clone(),
    }
}

/// Convert a synthesized `IrGroup` into a `LegacyGroup`. Mirrors the TS
/// `synthesizedGroupToLegacy` helper. Falls back to `"ai-generated"` for the
/// rare case where the IR group omits `source`.
fn synthesized_group_to_legacy(group: &IrGroup) -> LegacyGroup {
    LegacyGroup {
        id: group.id.clone(),
        name: group.name.clone(),
        description: group.description.clone(),
        category: group.category.clone(),
        assertions: group
            .assertions
            .iter()
            .map(synthesized_assertion_to_legacy)
            .collect(),
        source: group
            .source
            .clone()
            .unwrap_or_else(|| "ai-generated".to_string()),
    }
}

fn build_process_step(action: &IrTransitionAction) -> LegacyProcessStep {
    LegacyProcessStep {
        action: action.kind.clone(),
        target: convert_criteria(&action.target),
        wait_after: action.wait_after.clone(),
    }
}

fn build_transition(transition: &IrTransition) -> LegacyTransition {
    let exit_states = transition.exit_states.clone().unwrap_or_default();
    let stays_visible = exit_states.is_empty();
    LegacyTransition {
        id: transition.id.clone(),
        name: transition.name.clone(),
        activate_states: transition.activate_states.clone(),
        deactivate_states: exit_states,
        stays_visible,
        process: transition.actions.iter().map(build_process_step).collect(),
    }
}

fn build_state_machine_state(
    state: &IrState,
    transitions: &[IrTransition],
    doc: &IrDocument,
) -> LegacyStateMachineState {
    let outgoing: Vec<LegacyTransition> = transitions
        .iter()
        .filter(|t| t.from_states.contains(&state.id))
        .map(build_transition)
        .collect();
    let is_initial = match state.is_initial {
        Some(v) => v,
        None => doc.initial_state.as_deref() == Some(state.id.as_str()),
    };
    LegacyStateMachineState {
        id: state.id.clone(),
        name: state.name.clone(),
        description: state.description.clone().unwrap_or_default(),
        elements: state
            .required_elements
            .iter()
            .map(convert_criteria)
            .collect(),
        is_initial,
        transitions: outgoing,
    }
}

/// Project an IR document into the legacy bundled-page spec shape.
///
/// Pure / deterministic: same input always produces structurally identical
/// output. The output is returned as a `serde_json::Value` so callers can
/// rely on the lex-sorted key order applied at the final step.
pub fn project_ir_to_bundled_page(doc: &IrDocument, notes: Option<&str>) -> Value {
    let base_description = doc.description.clone().unwrap_or_else(|| doc.name.clone());
    let description = match notes {
        Some(n) if !n.is_empty() => format!("{}\n\n{}", base_description, n),
        _ => base_description,
    };

    let component = doc
        .metadata
        .as_ref()
        .and_then(|m| m.purpose.clone())
        .unwrap_or_else(|| doc.id.clone());

    let mut metadata = Map::new();
    metadata.insert("component".to_string(), Value::String(component));
    if let Some(meta) = &doc.metadata {
        if let Some(tags) = &meta.tags {
            metadata.insert(
                "tags".to_string(),
                Value::Array(tags.iter().map(|t| Value::String(t.clone())).collect()),
            );
        }
    }

    let mut groups: Vec<LegacyGroup> = doc.states.iter().map(build_group).collect();
    if let Some(synth) = &doc.synthesized_groups {
        // Append synthesis-produced groups AFTER state-derived ones, in
        // declared order. Byte-stable output requires this exact ordering.
        for g in synth {
            groups.push(synthesized_group_to_legacy(g));
        }
    }
    let sm_states: Vec<LegacyStateMachineState> = doc
        .states
        .iter()
        .map(|s| build_state_machine_state(s, &doc.transitions, doc))
        .collect();

    let spec = LegacySpec {
        version: "1.0.0".to_string(),
        description,
        groups,
        state_machine: LegacyStateMachine { states: sm_states },
        metadata: Value::Object(metadata),
    };

    // Round-trip via serde_json::Value, then lex-sort all object keys to
    // produce byte-stable output regardless of struct field order.
    let raw = serde_json::to_value(spec).expect("LegacySpec must serialize");
    sort_keys(raw)
}

/// Convenience: project + serialize to pretty JSON with 2-space indent and
/// trailing newline (matches the Node CLI's output exactly so the two paths
/// can be byte-diffed).
pub fn project_to_pretty_json(doc: &IrDocument, notes: Option<&str>) -> String {
    let value = project_ir_to_bundled_page(doc, notes);
    let mut s = serde_json::to_string_pretty(&value).expect("Value must serialize");
    s.push('\n');
    s
}

// Avoid "unused" warning on the json! import in production builds — used
// in tests but referenced here so it doesn't get pruned.
#[allow(dead_code)]
fn _ensure_json_macro_in_scope() -> Value {
    json!({})
}
