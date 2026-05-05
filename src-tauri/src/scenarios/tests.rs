//! Unit tests for the static scenario projection. Mirrors the
//! determinism + label-emission rules tested on the TS side.

use serde_json::json;

use crate::spec_api::types::{
    IrDocument, IrElementCriteria, IrState, IrTransition, IrTransitionAction,
};

use super::projection::project_scenarios;

fn empty_state(id: &str, name: &str) -> IrState {
    IrState {
        id: id.to_string(),
        name: name.to_string(),
        description: None,
        required_elements: vec![],
        excluded_elements: None,
        conditions: None,
        is_initial: None,
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
    }
}

fn click_action() -> IrTransitionAction {
    IrTransitionAction {
        kind: "click".to_string(),
        target: IrElementCriteria {
            role: Some("button".to_string()),
            ..IrElementCriteria::default()
        },
        params: None,
        wait_after: None,
    }
}

fn empty_transition(
    id: &str,
    name: &str,
    from: Vec<&str>,
    activate: Vec<&str>,
    actions: Vec<IrTransitionAction>,
) -> IrTransition {
    IrTransition {
        id: id.to_string(),
        name: name.to_string(),
        description: None,
        from_states: from.into_iter().map(String::from).collect(),
        activate_states: activate.into_iter().map(String::from).collect(),
        exit_states: None,
        actions,
        path_cost: None,
        bidirectional: None,
        effect: None,
        metadata: None,
        provenance: None,
        cross_refs: None,
    }
}

fn make_ir() -> IrDocument {
    IrDocument {
        version: "1.0".to_string(),
        id: "test-page".to_string(),
        name: "Test Page".to_string(),
        description: None,
        metadata: None,
        provenance: None,
        states: vec![
            empty_state("state-b", "state-b"),
            // Out of order — projection must sort.
            empty_state("state-a", "State A"),
        ],
        transitions: vec![
            empty_transition(
                "t-2",
                "t-2",
                vec!["state-a"],
                vec!["state-b", "state-a"], // Unsorted activate_states.
                vec![click_action(), click_action()],
            ),
            empty_transition(
                "t-1",
                "Click Foo",
                vec!["state-a"],
                vec!["state-b"],
                vec![click_action()],
            ),
        ],
        initial_state: None,
    }
}

#[test]
fn project_scenarios_sorts_states_by_id_ascending() {
    let ir = make_ir();
    let proj = project_scenarios(&ir);
    let ids: Vec<&str> = proj.states.iter().map(|s| s.state_id.as_str()).collect();
    assert_eq!(ids, vec!["state-a", "state-b"]);
}

#[test]
fn project_scenarios_sorts_outbound_transitions_by_id() {
    let ir = make_ir();
    let proj = project_scenarios(&ir);
    let state_a = proj
        .states
        .iter()
        .find(|s| s.state_id == "state-a")
        .unwrap();
    let trans_ids: Vec<&str> = state_a
        .outbound_transitions
        .iter()
        .map(|t| t.transition_id.as_str())
        .collect();
    assert_eq!(trans_ids, vec!["t-1", "t-2"]);
}

#[test]
fn project_scenarios_sorts_target_state_ids_ascending() {
    let ir = make_ir();
    let proj = project_scenarios(&ir);
    let state_a = proj
        .states
        .iter()
        .find(|s| s.state_id == "state-a")
        .unwrap();
    let t2 = state_a
        .outbound_transitions
        .iter()
        .find(|t| t.transition_id == "t-2")
        .unwrap();
    assert_eq!(t2.target_state_ids, vec!["state-a", "state-b"]);
}

#[test]
fn project_scenarios_omits_label_when_equal_to_id() {
    let ir = make_ir();
    let proj = project_scenarios(&ir);

    // state-b has name == id → no label.
    let state_b = proj
        .states
        .iter()
        .find(|s| s.state_id == "state-b")
        .unwrap();
    assert!(state_b.label.is_none());

    // state-a has name "State A" != "state-a" → label populated.
    let state_a = proj
        .states
        .iter()
        .find(|s| s.state_id == "state-a")
        .unwrap();
    assert_eq!(state_a.label.as_deref(), Some("State A"));
}

#[test]
fn project_scenarios_includes_action_count_and_required_element_count() {
    let ir = make_ir();
    let proj = project_scenarios(&ir);
    let state_a = proj
        .states
        .iter()
        .find(|s| s.state_id == "state-a")
        .unwrap();
    assert_eq!(state_a.required_element_count, 0);

    let t2 = state_a
        .outbound_transitions
        .iter()
        .find(|t| t.transition_id == "t-2")
        .unwrap();
    assert_eq!(t2.action_count, 2);

    let t1 = state_a
        .outbound_transitions
        .iter()
        .find(|t| t.transition_id == "t-1")
        .unwrap();
    assert_eq!(t1.action_count, 1);
    assert_eq!(t1.label.as_deref(), Some("Click Foo"));
}

#[test]
fn project_scenarios_is_deterministic_across_runs() {
    let ir = make_ir();
    let a = serde_json::to_string(&project_scenarios(&ir)).unwrap();
    for _ in 0..10 {
        let b = serde_json::to_string(&project_scenarios(&ir)).unwrap();
        assert_eq!(a, b, "projection must be byte-identical across runs");
    }
}

#[test]
fn project_scenarios_marks_deterministic_true() {
    let ir = make_ir();
    let proj = project_scenarios(&ir);
    let v = serde_json::to_value(&proj).unwrap();
    assert_eq!(v["deterministic"], json!(true));
}

#[test]
fn project_scenarios_handles_empty_ir() {
    let ir = IrDocument {
        version: "1.0".to_string(),
        id: "empty".to_string(),
        name: "empty".to_string(),
        description: None,
        metadata: None,
        provenance: None,
        states: vec![],
        transitions: vec![],
        initial_state: None,
    };
    let proj = project_scenarios(&ir);
    assert!(proj.states.is_empty());
    assert!(proj.deterministic);
}
