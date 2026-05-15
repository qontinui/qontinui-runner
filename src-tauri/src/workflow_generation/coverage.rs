//! Rust port of `ui-bridge-auto/src/state/regression-generator.ts`'s
//! `generateRegressionSuite` + `coverageOf` (Gate 2 of the Stream E flywheel
//! validator).
//!
//! Pure graph-walk over `IrPageSpec.transitions[].from_states` /
//! `activate_states`. The TS reference at
//! `ui-bridge-auto/src/state/regression-generator.ts:523-740` is the
//! authoritative algorithm — this port mirrors it exactly:
//!
//! - **`generate_regression_suite`** emits one [`RegressionCase`] per
//!   transition, sorted by transition id ascending; case ordering is
//!   determined entirely by sorted input (defensive — callers may build IRs
//!   in arbitrary order).
//! - **`coverage_of`** computes `states_covered` by unioning the
//!   `from_states`, `activate_states`, and `exit_states` lists across every
//!   case; `reachable_states` is computed by BFS from `ir.initial_state`
//!   (or from every state if no `initial_state` is declared) over the
//!   transition graph. `reachable_states` / `unreachable_states` are
//!   returned as sorted-id `Vec<String>` (matching the TS contract).
//!
//! The flywheel's coverage gate floors `statesCovered / reachableStates.len()`
//! at the env-configurable threshold (`QONTINUI_SPEC_COVERAGE_FLOOR`,
//! default 0.80) — see `spec_api::validator::gate_coverage`.

use crate::spec_api::types::IrPageSpec;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// One regression case — mirrors the (subset of) TS `RegressionCase` that
/// `coverage_of` actually reads. The flywheel's coverage gate only consumes
/// the state-id columns, so we deliberately omit `assertions[]` here (the
/// full assertion-shape port belongs in a future spec-check streaming-
/// regression module, not this validator gate).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegressionCase {
    pub id: String,
    pub transition_id: String,
    pub from_states: Vec<String>,
    pub activate_states: Vec<String>,
    pub exit_states: Vec<String>,
}

/// Deterministic regression suite — one case per transition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegressionSuite {
    pub id: String,
    pub cases: Vec<RegressionCase>,
}

/// Coverage report — mirror of the TS `CoverageReport`
/// (`ui-bridge-auto/src/state/regression-generator.ts:702`).
///
/// `reachable_states` / `unreachable_states` are arrays of ids (sorted
/// ascending), matching the TS contract. Take `.len()` for counts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoverageReport {
    pub total_states: u32,
    pub total_transitions: u32,
    pub states_covered: u32,
    pub transitions_covered: u32,
    pub reachable_states: Vec<String>,
    pub unreachable_states: Vec<String>,
}

/// Walk the IR and emit a deterministic regression suite — one case per
/// transition, sorted by transition id ascending. Same input → byte-
/// identical suite. (TS reference: `generateRegressionSuite` at
/// `regression-generator.ts:523`.)
pub fn generate_regression_suite(ir: &IrPageSpec) -> RegressionSuite {
    // Sort transitions defensively — callers may build IRs in arbitrary
    // order (insertion order of a Map, ad-hoc construction). We never
    // trust the caller's order.
    let mut sorted_transitions: Vec<&crate::spec_api::types::IrTransition> =
        ir.transitions.iter().collect();
    sorted_transitions.sort_by(|a, b| a.id.cmp(&b.id));

    let cases: Vec<RegressionCase> = sorted_transitions
        .into_iter()
        .map(|t| RegressionCase {
            id: t.id.clone(),
            transition_id: t.id.clone(),
            from_states: sorted_copy(&t.from_states),
            activate_states: sorted_copy(&t.activate_states),
            exit_states: sorted_copy(t.exit_states.as_deref().unwrap_or(&[])),
        })
        .collect();

    RegressionSuite {
        id: format!("{}@suite", ir.id),
        cases,
    }
}

/// Compute coverage of the IR by the suite. `transitions_covered` is just
/// `suite.cases.len()` (one case per transition). `reachable_states` is
/// computed via a local BFS from `ir.initial_state` if present, else from
/// every state in the IR (so coverage isn't gated on a declared start node).
///
/// TS reference: `coverageOf` at `regression-generator.ts:673`.
pub fn coverage_of(ir: &IrPageSpec, suite: &RegressionSuite) -> CoverageReport {
    // All declared state ids.
    let all_state_ids: BTreeSet<String> = ir.states.iter().map(|s| s.id.clone()).collect();

    // States touched by at least one case (any of from/activate/exit).
    let mut touched: BTreeSet<String> = BTreeSet::new();
    for c in &suite.cases {
        for s in &c.from_states {
            touched.insert(s.clone());
        }
        for s in &c.activate_states {
            touched.insert(s.clone());
        }
        for s in &c.exit_states {
            touched.insert(s.clone());
        }
    }

    // Reachability seeds: prefer `initial_state`, else every declared state.
    let seeds: Vec<String> = match ir.initial_state.as_ref() {
        Some(s) => vec![s.clone()],
        None => all_state_ids.iter().cloned().collect(),
    };
    let reachable = reachable_from(ir, &seeds);

    let mut reachable_states: Vec<String> = Vec::new();
    let mut unreachable_states: Vec<String> = Vec::new();
    for id in &all_state_ids {
        if reachable.contains(id) {
            reachable_states.push(id.clone());
        } else {
            unreachable_states.push(id.clone());
        }
    }

    CoverageReport {
        total_states: ir.states.len() as u32,
        total_transitions: ir.transitions.len() as u32,
        states_covered: touched.len() as u32,
        transitions_covered: suite.cases.len() as u32,
        reachable_states,
        unreachable_states,
    }
}

/// Local BFS over the IR transition graph. A transition is "available"
/// once all its `from_states` are reachable; firing it grows the reachable
/// set with every `activate_state`. Iterate to fixpoint.
///
/// Mirrors the TS `reachableFrom` at `regression-generator.ts:637`.
fn reachable_from(ir: &IrPageSpec, seeds: &[String]) -> BTreeSet<String> {
    let mut reachable: BTreeSet<String> = seeds.iter().cloned().collect();

    let mut transitions_sorted: Vec<&crate::spec_api::types::IrTransition> =
        ir.transitions.iter().collect();
    transitions_sorted.sort_by(|a, b| a.id.cmp(&b.id));

    let mut changed = true;
    while changed {
        changed = false;
        for t in &transitions_sorted {
            // A transition is "available" once all its from_states are
            // reachable.
            let available = t.from_states.iter().all(|s| reachable.contains(s));
            if !available {
                continue;
            }
            for s in &t.activate_states {
                if reachable.insert(s.clone()) {
                    changed = true;
                }
            }
        }
    }
    reachable
}

/// Return a sorted copy of a list of ids. Pure helper to mirror
/// `sortedCopy` from the TS reference (`regression-generator.ts`).
fn sorted_copy(xs: &[String]) -> Vec<String> {
    let mut out = xs.to_vec();
    out.sort();
    out
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec_api::types::{IrPageSpec, IrState, IrTransition};

    /// Construct a minimal `IrPageSpec` with the given states + transitions.
    /// Every field outside the test's interest is defaulted to keep call
    /// sites short.
    fn mk_ir(
        id: &str,
        state_ids: &[&str],
        transitions: Vec<(&str, Vec<&str>, Vec<&str>)>,
        initial: Option<&str>,
    ) -> IrPageSpec {
        let states: Vec<IrState> = state_ids
            .iter()
            .map(|sid| IrState {
                id: sid.to_string(),
                name: sid.to_string(),
                description: None,
                assertions: Vec::new(),
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
            })
            .collect();

        let transitions: Vec<IrTransition> = transitions
            .into_iter()
            .map(|(tid, from, to)| IrTransition {
                id: tid.to_string(),
                name: tid.to_string(),
                description: None,
                from_states: from.iter().map(|s| s.to_string()).collect(),
                activate_states: to.iter().map(|s| s.to_string()).collect(),
                exit_states: None,
                actions: Vec::new(),
                path_cost: None,
                bidirectional: None,
                effect: None,
                metadata: None,
                provenance: None,
                cross_refs: None,
            })
            .collect();

        IrPageSpec {
            version: "1.0".into(),
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            metadata: None,
            provenance: None,
            states,
            transitions,
            synthesized_groups: None,
            initial_state: initial.map(|s| s.to_string()),
        }
    }

    #[test]
    fn empty_ir_yields_empty_suite_and_zero_coverage() {
        let ir = mk_ir("empty", &[], vec![], None);
        let suite = generate_regression_suite(&ir);
        assert_eq!(suite.cases.len(), 0);
        assert_eq!(suite.id, "empty@suite");

        let report = coverage_of(&ir, &suite);
        assert_eq!(report.total_states, 0);
        assert_eq!(report.total_transitions, 0);
        assert_eq!(report.states_covered, 0);
        assert_eq!(report.transitions_covered, 0);
        assert!(report.reachable_states.is_empty());
        assert!(report.unreachable_states.is_empty());
    }

    #[test]
    fn fully_reachable_chain_marks_every_state_reachable() {
        // a → b → c, no initial declared (seeds = every state).
        let ir = mk_ir(
            "chain",
            &["a", "b", "c"],
            vec![("t1", vec!["a"], vec!["b"]), ("t2", vec!["b"], vec!["c"])],
            None,
        );
        let suite = generate_regression_suite(&ir);
        let report = coverage_of(&ir, &suite);
        assert_eq!(report.total_states, 3);
        assert_eq!(report.total_transitions, 2);
        assert_eq!(report.states_covered, 3);
        assert_eq!(report.transitions_covered, 2);
        assert_eq!(report.reachable_states, vec!["a", "b", "c"]);
        assert!(report.unreachable_states.is_empty());
    }

    #[test]
    fn fully_reachable_with_initial_state() {
        // a → b → c, initial = a.
        let ir = mk_ir(
            "chain",
            &["a", "b", "c"],
            vec![("t1", vec!["a"], vec!["b"]), ("t2", vec!["b"], vec!["c"])],
            Some("a"),
        );
        let suite = generate_regression_suite(&ir);
        let report = coverage_of(&ir, &suite);
        assert_eq!(report.reachable_states, vec!["a", "b", "c"]);
        assert!(report.unreachable_states.is_empty());
    }

    #[test]
    fn disconnected_subgraph_yields_unreachable_states() {
        // Initial = a; a → b; isolated state x with no incoming.
        let ir = mk_ir(
            "disconnect",
            &["a", "b", "x"],
            vec![("t1", vec!["a"], vec!["b"])],
            Some("a"),
        );
        let suite = generate_regression_suite(&ir);
        let report = coverage_of(&ir, &suite);
        // BFS from `a` only — `x` is unreachable.
        assert_eq!(report.reachable_states, vec!["a", "b"]);
        assert_eq!(report.unreachable_states, vec!["x"]);
        // states_covered counts every state TOUCHED by a case (`from` or
        // `activate`). Only `t1` exists, touching a + b. x is not touched.
        assert_eq!(report.states_covered, 2);
    }

    #[test]
    fn states_covered_counts_union_across_cases() {
        // Two transitions: a→b and b→c. Total states touched = {a,b,c} = 3.
        let ir = mk_ir(
            "fan",
            &["a", "b", "c", "d"],
            vec![("t1", vec!["a"], vec!["b"]), ("t2", vec!["b"], vec!["c"])],
            None,
        );
        let suite = generate_regression_suite(&ir);
        let report = coverage_of(&ir, &suite);
        // d is declared but no transition touches it.
        assert_eq!(report.states_covered, 3);
        assert_eq!(report.total_states, 4);
    }

    #[test]
    fn transitions_sort_deterministically_regardless_of_input_order() {
        // Insertion order [t2, t1, t3] must produce a suite ordered
        // [t1, t2, t3] — ordering is a function of sorted ids, not input order.
        let ir = mk_ir(
            "sort",
            &["a", "b", "c", "d"],
            vec![
                ("t2", vec!["b"], vec!["c"]),
                ("t1", vec!["a"], vec!["b"]),
                ("t3", vec!["c"], vec!["d"]),
            ],
            None,
        );
        let suite = generate_regression_suite(&ir);
        let ids: Vec<&str> = suite.cases.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["t1", "t2", "t3"]);

        // Same IR, different insertion order — suite ids must be byte-identical.
        let ir2 = mk_ir(
            "sort",
            &["a", "b", "c", "d"],
            vec![
                ("t3", vec!["c"], vec!["d"]),
                ("t1", vec!["a"], vec!["b"]),
                ("t2", vec!["b"], vec!["c"]),
            ],
            None,
        );
        let suite2 = generate_regression_suite(&ir2);
        let ids2: Vec<&str> = suite2.cases.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids2, vec!["t1", "t2", "t3"]);
        assert_eq!(suite, suite2);
    }
}
