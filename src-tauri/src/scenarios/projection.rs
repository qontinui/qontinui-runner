//! Pure Rust port of `projectScenarios` from
//! `ui-bridge-auto/src/state/scenario-projection.ts`.
//!
//! Determinism rules (non-negotiable, mirror the TS implementation):
//!   - No clock reads, no RNG, no I/O.
//!   - Sort states by `state_id` ascending.
//!   - Sort each state's `outbound_transitions` by `transition_id` ascending.
//!   - Sort each transition's `target_state_ids` ascending.
//!   - Never mutate caller-supplied data — always sort fresh copies.
//!
//! The Rust port keeps the same field-emission rules as the TS source
//! (e.g. `label` only emitted when distinct from the id) so the JSON
//! output is byte-identical to what `projectScenarios(ir)` would emit.

use std::collections::BTreeMap;

use crate::spec_api::types::{IrDocument, IrTransition};

use super::types::{ProjectedState, ProjectedTransition, ScenarioProjection};

/// Bucket transitions by their `from_states` entries. A transition with
/// multiple preconditions appears under EVERY fromState — matching the
/// TS implementation's "what can I do from state X" semantic. We use
/// `BTreeMap` so the from-state iteration order is itself deterministic
/// (the TS version re-sorts after collection; using a sorted map here is
/// equivalent and avoids a needless allocation).
fn index_transitions_by_from_state<'a>(
    ir: &'a IrDocument,
) -> BTreeMap<&'a str, Vec<&'a IrTransition>> {
    let mut out: BTreeMap<&'a str, Vec<&'a IrTransition>> = BTreeMap::new();
    for t in &ir.transitions {
        for from in &t.from_states {
            out.entry(from.as_str()).or_default().push(t);
        }
    }
    out
}

/// Sorted copy of a `&[String]` — never mutates the caller-owned slice.
fn sorted_copy(values: &[String]) -> Vec<String> {
    let mut v: Vec<String> = values.to_vec();
    v.sort();
    v
}

/// Mirror of `projectTransition` in the TS source.
fn project_transition(t: &IrTransition) -> ProjectedTransition {
    let label = if !t.name.is_empty() && t.name != t.id {
        Some(t.name.clone())
    } else {
        None
    };
    ProjectedTransition {
        transition_id: t.id.clone(),
        label,
        target_state_ids: sorted_copy(&t.activate_states),
        action_count: t.actions.len(),
    }
}

/// Walk the IR and emit a deterministic projection of every state with
/// its outbound transitions. Same `IrDocument` input → byte-identical
/// output as the TS `projectScenarios`.
///
/// Cost is `O(|states| log |states| + |transitions| log |transitions|)`
/// for the sorts plus `O(|transitions|)` for the bucketing.
pub fn project_scenarios(ir: &IrDocument) -> ScenarioProjection {
    let by_from_state = index_transitions_by_from_state(ir);

    // Defensive sort — callers may build IRs in arbitrary order.
    let mut sorted_state_indices: Vec<usize> = (0..ir.states.len()).collect();
    sorted_state_indices.sort_by(|&i, &j| ir.states[i].id.cmp(&ir.states[j].id));

    let mut states: Vec<ProjectedState> = Vec::with_capacity(ir.states.len());
    for idx in sorted_state_indices {
        let s = &ir.states[idx];
        let bucket = by_from_state
            .get(s.id.as_str())
            .map(|v| v.as_slice())
            .unwrap_or(&[]);

        // Sort each bucket by transition id ascending.
        let mut sorted_bucket: Vec<&IrTransition> = bucket.to_vec();
        sorted_bucket.sort_by(|a, b| a.id.cmp(&b.id));

        let outbound: Vec<ProjectedTransition> =
            sorted_bucket.iter().map(|t| project_transition(t)).collect();

        let label = if !s.name.is_empty() && s.name != s.id {
            Some(s.name.clone())
        } else {
            None
        };

        states.push(ProjectedState {
            state_id: s.id.clone(),
            label,
            required_element_count: s.required_elements.len(),
            outbound_transitions: outbound,
        });
    }

    ScenarioProjection {
        states,
        deterministic: true,
    }
}
