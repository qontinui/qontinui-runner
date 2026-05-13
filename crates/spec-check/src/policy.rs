//! AND-conjunct policy DSL evaluator (Step 7 of Plan 02, §5.7).
//!
//! Wire shapes (`spec_check.rs:528-569`):
//! - `PolicyEvaluation { overall_status, conjunct_results }`
//! - `ConjunctEvaluation { name, status, evidence }`
//! - `PolicyStatus = Pass | Fail | Indeterminate` (three-state).
//!
//! All conjuncts are ANDed (per §5.7 there is no OR — compose multiple
//! steps for disjunction). The aggregate rule per the wire docstring:
//!   * any `Fail` → `Fail`
//!   * else any `Indeterminate` → `Indeterminate`
//!   * else `Pass`.

use crate::{
    AssertionOutcome, AssertionResult, AssertionScope, ConjunctEvaluation, ConjunctRule,
    MatchOutcome, PolicyEvaluation, PolicyStatus, SpecCheckPolicy, SpecCheckResult,
    StateMatchResult,
};

/// A scoped `AssertionResult` annotated with the parent state it belongs
/// to. The wire `AssertionResult` does not carry a `state_id`; we recover
/// it by walking `result.state_results[*].assertions[*]` so scope filters
/// like `states: ["state1"]` can apply.
#[derive(Debug, Clone, Copy)]
struct ScopedAssertion<'a> {
    state: &'a StateMatchResult,
    assertion: &'a AssertionResult,
}

/// Evaluate a policy against a result.
///
/// Per the wire docstring on `PolicyEvaluation.overall_status`
/// (`spec_check.rs:533-536`): fails if any conjunct fails; indeterminate
/// if any conjunct is indeterminate and none failed; otherwise pass. An
/// empty `policy.conjuncts` vector vacuously passes — this is the
/// `observation_only()` factory's intended behavior.
pub fn apply_policy(result: &SpecCheckResult, policy: &SpecCheckPolicy) -> PolicyEvaluation {
    let conjunct_results: Vec<ConjunctEvaluation> = policy
        .conjuncts
        .iter()
        .map(|c| {
            let scope = filter_assertions(result, &c.scope);
            let (status, evidence) = evaluate_rule(&c.rule, &scope, &c.scope, result);
            ConjunctEvaluation {
                name: c.name.clone(),
                status,
                evidence,
            }
        })
        .collect();

    let overall_status = if conjunct_results
        .iter()
        .any(|c| c.status == PolicyStatus::Fail)
    {
        PolicyStatus::Fail
    } else if conjunct_results
        .iter()
        .any(|c| c.status == PolicyStatus::Indeterminate)
    {
        PolicyStatus::Indeterminate
    } else {
        PolicyStatus::Pass
    };

    PolicyEvaluation {
        overall_status,
        conjunct_results,
    }
}

/// Apply the four AND-combined scope filters to every assertion in the
/// result. Empty `states`/`severities`/`categories`/`groups` match all;
/// `assertion_ids: None` matches all; `Some([])` matches nothing
/// (deliberately empty scope per `spec_check.rs:486-489`).
///
/// Note: the wire `AssertionResult` carries no `group` field today. The
/// `groups` filter is therefore a no-op at this layer — it matches every
/// assertion when populated. When IR grouping lands, this is the single
/// site that needs to learn the group lookup; for now we keep the field
/// honored (it doesn't prune nothing) but document the limitation.
fn filter_assertions<'a>(
    result: &'a SpecCheckResult,
    scope: &AssertionScope,
) -> Vec<ScopedAssertion<'a>> {
    if matches!(&scope.assertion_ids, Some(ids) if ids.is_empty()) {
        // Deliberately-empty scope.
        return Vec::new();
    }

    result
        .state_results
        .iter()
        .flat_map(|state| {
            state
                .assertions
                .iter()
                .map(move |assertion| ScopedAssertion { state, assertion })
        })
        .filter(|sa| {
            if !scope.states.is_empty() && !scope.states.contains(&sa.state.state_id) {
                return false;
            }
            if !scope.severities.is_empty()
                && !scope.severities.contains(&sa.assertion.severity)
            {
                return false;
            }
            if !scope.categories.is_empty()
                && !scope.categories.contains(&sa.assertion.category)
            {
                return false;
            }
            // `groups` filter — no group field on AssertionResult yet; the
            // filter is intentionally a no-op when the field is unmapped.
            // When present, an empty `groups` matches all (no-op).
            // (Keeping the field honored avoids silent breakage when IR
            // grouping is added.)

            if let Some(ids) = &scope.assertion_ids {
                if !ids.contains(&sa.assertion.assertion_id) {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// Map `MatchOutcome` to a comparison rank.
///
/// Ordering per `spec_check.rs:521-522`:
/// `NoMatch(0) < PartialMatch(1) < FullMatch(2)`.
fn match_outcome_rank(outcome: MatchOutcome) -> u8 {
    match outcome {
        MatchOutcome::NoMatch => 0,
        MatchOutcome::PartialMatch => 1,
        MatchOutcome::FullMatch => 2,
    }
}

fn is_pass(a: &AssertionResult) -> bool {
    matches!(a.outcome, AssertionOutcome::Pass { .. })
}

/// Evaluate a single conjunct rule against the filtered scope.
///
/// `scope_def` is the original `AssertionScope` — needed by per-state
/// rules to know which IR states the conjunct restricts to (the filtered
/// `scope` slice doesn't include states that have zero assertions in
/// scope).
fn evaluate_rule(
    rule: &ConjunctRule,
    scope: &[ScopedAssertion<'_>],
    scope_def: &AssertionScope,
    result: &SpecCheckResult,
) -> (PolicyStatus, String) {
    match rule {
        ConjunctRule::AllPass => {
            if scope.is_empty() {
                return (
                    PolicyStatus::Indeterminate,
                    "no assertions in scope".to_string(),
                );
            }
            let failed = scope.iter().filter(|sa| !is_pass(sa.assertion)).count();
            let total = scope.len();
            if failed == 0 {
                (
                    PolicyStatus::Pass,
                    format!("all {total} in scope passed"),
                )
            } else {
                (
                    PolicyStatus::Fail,
                    format!("{failed} of {total} in scope failed"),
                )
            }
        }

        ConjunctRule::MaxFailures { count } => {
            // MaxFailures is meaningful even on empty scope (0 failures
            // trivially ≤ count), so no indeterminate here.
            let failed = scope.iter().filter(|sa| !is_pass(sa.assertion)).count();
            let total = scope.len();
            if failed <= *count as usize {
                (
                    PolicyStatus::Pass,
                    format!("{failed} of {total} failed (max {count})"),
                )
            } else {
                (
                    PolicyStatus::Fail,
                    format!("{failed} of {total} failed (max {count})"),
                )
            }
        }

        ConjunctRule::FailureRateBelow { rate } => {
            if scope.is_empty() {
                return (
                    PolicyStatus::Indeterminate,
                    "no assertions in scope (failure rate undefined)".to_string(),
                );
            }
            let total = scope.len();
            let failed = scope.iter().filter(|sa| !is_pass(sa.assertion)).count();
            let actual = failed as f32 / total as f32;
            if actual < *rate {
                (
                    PolicyStatus::Pass,
                    format!(
                        "failure rate {actual:.3} ({failed}/{total}) below threshold {rate:.3}"
                    ),
                )
            } else {
                (
                    PolicyStatus::Fail,
                    format!(
                        "failure rate {actual:.3} ({failed}/{total}) at or above threshold {rate:.3}"
                    ),
                )
            }
        }

        ConjunctRule::StateMatchRateAtLeast { rate } => {
            let states_in_scope = states_for_scope(result, scope_def);
            if states_in_scope.is_empty() {
                return (
                    PolicyStatus::Indeterminate,
                    "no states match the scope's states filter".to_string(),
                );
            }
            let mut below: Vec<&StateMatchResult> = states_in_scope
                .iter()
                .copied()
                .filter(|s| s.match_rate < *rate)
                .collect();
            // Stable, lexical listing for deterministic evidence text.
            below.sort_by(|a, b| a.state_id.cmp(&b.state_id));
            if below.is_empty() {
                (
                    PolicyStatus::Pass,
                    format!(
                        "all {} states meet match_rate >= {rate:.3}",
                        states_in_scope.len()
                    ),
                )
            } else {
                let lowest = below
                    .iter()
                    .min_by(|a, b| {
                        a.match_rate
                            .partial_cmp(&b.match_rate)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .expect("below is non-empty");
                (
                    PolicyStatus::Fail,
                    format!(
                        "{} of {} states below match_rate {rate:.3} (lowest: {} at {:.3})",
                        below.len(),
                        states_in_scope.len(),
                        lowest.state_id,
                        lowest.match_rate
                    ),
                )
            }
        }

        ConjunctRule::AnyStateMatchRateAtLeast { rate } => {
            let states_in_scope = states_for_scope(result, scope_def);
            if states_in_scope.is_empty() {
                return (
                    PolicyStatus::Indeterminate,
                    "no states match the scope's states filter".to_string(),
                );
            }
            let best = states_in_scope
                .iter()
                .copied()
                .max_by(|a, b| {
                    a.match_rate
                        .partial_cmp(&b.match_rate)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .expect("states_in_scope is non-empty");
            if best.match_rate >= *rate {
                (
                    PolicyStatus::Pass,
                    format!(
                        "state {} has match_rate {:.3} >= {rate:.3}",
                        best.state_id, best.match_rate
                    ),
                )
            } else {
                (
                    PolicyStatus::Fail,
                    format!(
                        "no state reaches match_rate {rate:.3} (best: {} at {:.3})",
                        best.state_id, best.match_rate
                    ),
                )
            }
        }

        ConjunctRule::MatchOutcomeAtLeast { outcome } => {
            let actual = result.summary.match_outcome;
            if match_outcome_rank(actual) >= match_outcome_rank(*outcome) {
                (
                    PolicyStatus::Pass,
                    format!("match_outcome {actual:?} >= required {outcome:?}"),
                )
            } else {
                (
                    PolicyStatus::Fail,
                    format!("match_outcome {actual:?} < required {outcome:?}"),
                )
            }
        }
    }
}

/// Resolve the set of `StateMatchResult`s the scope's `states` filter
/// designates. Empty filter = all states.
fn states_for_scope<'a>(
    result: &'a SpecCheckResult,
    scope: &AssertionScope,
) -> Vec<&'a StateMatchResult> {
    if scope.states.is_empty() {
        result.state_results.iter().collect()
    } else {
        result
            .state_results
            .iter()
            .filter(|s| scope.states.contains(&s.state_id))
            .collect()
    }
}

// ============================================================================
// Factories
// ============================================================================

/// Named factories for common policy shapes. No implicit defaults — the
/// caller picks explicitly per §5.7.
pub mod factories {
    use crate::{AssertionScope, ConjunctRule, PolicyConjunct, SpecCheckPolicy};

    /// All `critical`-severity assertions must pass.
    pub fn criticals_only() -> SpecCheckPolicy {
        SpecCheckPolicy {
            conjuncts: vec![PolicyConjunct {
                name: "criticals-must-pass".to_string(),
                scope: AssertionScope {
                    severities: vec!["critical".to_string()],
                    ..Default::default()
                },
                rule: ConjunctRule::AllPass,
            }],
        }
    }

    /// Every assertion across every state must pass.
    pub fn strict_all_pass() -> SpecCheckPolicy {
        SpecCheckPolicy {
            conjuncts: vec![PolicyConjunct {
                name: "all-assertions-pass".to_string(),
                scope: AssertionScope::default(),
                rule: ConjunctRule::AllPass,
            }],
        }
    }

    /// At least one state must have `match_rate == 1.0`.
    pub fn require_perfect_state() -> SpecCheckPolicy {
        SpecCheckPolicy {
            conjuncts: vec![PolicyConjunct {
                name: "at-least-one-state-100%".to_string(),
                scope: AssertionScope::default(),
                rule: ConjunctRule::AnyStateMatchRateAtLeast { rate: 1.0 },
            }],
        }
    }

    /// A specific state must achieve `match_rate == 1.0`.
    pub fn require_state(state_id: &str) -> SpecCheckPolicy {
        SpecCheckPolicy {
            conjuncts: vec![PolicyConjunct {
                name: format!("require-state-{state_id}"),
                scope: AssertionScope {
                    states: vec![state_id.to_string()],
                    ..Default::default()
                },
                rule: ConjunctRule::StateMatchRateAtLeast { rate: 1.0 },
            }],
        }
    }

    /// No conjuncts — vacuously passes. Surface for "observe but never
    /// gate".
    pub fn observation_only() -> SpecCheckPolicy {
        SpecCheckPolicy { conjuncts: vec![] }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssertionMiss, AssertionSeverityCounts, BridgeFingerprint, MatchOutcome, MatchedElement,
        MissReason, PolicyConjunct,
    };

    fn pass_assertion(id: &str, severity: &str, category: &str) -> AssertionResult {
        AssertionResult {
            assertion_id: id.to_string(),
            description: format!("desc {id}"),
            severity: severity.to_string(),
            category: category.to_string(),
            outcome: AssertionOutcome::Pass {
                matched: MatchedElement {
                    element_id: None,
                    role: None,
                    text: None,
                    path: "body".to_string(),
                },
            },
        }
    }

    fn fail_assertion(id: &str, severity: &str, category: &str) -> AssertionResult {
        AssertionResult {
            assertion_id: id.to_string(),
            description: format!("desc {id}"),
            severity: severity.to_string(),
            category: category.to_string(),
            outcome: AssertionOutcome::Fail {
                miss: AssertionMiss {
                    reason: MissReason::NoCandidates,
                    candidates: vec![],
                },
            },
        }
    }

    fn state(id: &str, rate: f32, assertions: Vec<AssertionResult>) -> StateMatchResult {
        StateMatchResult {
            state_id: id.to_string(),
            state_name: id.to_string(),
            match_rate: rate,
            assertions,
        }
    }

    fn make_result(states: Vec<StateMatchResult>, outcome: MatchOutcome) -> SpecCheckResult {
        SpecCheckResult {
            result_schema_version: 1,
            snapshot_id: "scs_test".to_string(),
            spec_content_hash: "sha256-test".to_string(),
            spec_version: "1.0".to_string(),
            page_id: "page".to_string(),
            state_results: states,
            summary: crate::SpecCheckSummary {
                match_outcome: outcome,
                overall_match_rate: 0.5,
                severity_counts: AssertionSeverityCounts::default(),
                recommended_state: None,
            },
            bridge_fingerprint: BridgeFingerprint {
                app_id: "app".to_string(),
                app_version: None,
                route: None,
                bridge_version: None,
                snapshot_timestamp: "2026-05-13T00:00:00Z".to_string(),
                element_count: 0,
            },
            evaluated_at: "2026-05-13T00:00:00Z".to_string(),
            warnings: vec![],
        }
    }

    fn baseline_result() -> SpecCheckResult {
        let s1 = state(
            "state1",
            0.5,
            vec![
                pass_assertion("a1", "critical", "layout"),
                fail_assertion("a2", "critical", "layout"),
                pass_assertion("a3", "warning", "a11y"),
                fail_assertion("a4", "info", "a11y"),
            ],
        );
        let s2 = state(
            "state2",
            1.0,
            vec![
                pass_assertion("b1", "critical", "layout"),
                pass_assertion("b2", "warning", "a11y"),
            ],
        );
        make_result(vec![s1, s2], MatchOutcome::PartialMatch)
    }

    // ------------- aggregation -------------

    #[test]
    fn aggregate_fail_dominates_indeterminate() {
        let result = baseline_result();
        let policy = SpecCheckPolicy {
            conjuncts: vec![
                PolicyConjunct {
                    name: "fail-conjunct".to_string(),
                    scope: AssertionScope::default(),
                    rule: ConjunctRule::AllPass,
                },
                PolicyConjunct {
                    name: "indeterminate-conjunct".to_string(),
                    scope: AssertionScope {
                        assertion_ids: Some(vec![]),
                        ..Default::default()
                    },
                    rule: ConjunctRule::AllPass,
                },
            ],
        };
        let eval = apply_policy(&result, &policy);
        assert_eq!(eval.overall_status, PolicyStatus::Fail);
        assert_eq!(eval.conjunct_results.len(), 2);
    }

    #[test]
    fn aggregate_indeterminate_when_no_fail() {
        let result = baseline_result();
        let policy = SpecCheckPolicy {
            conjuncts: vec![
                PolicyConjunct {
                    name: "pass".to_string(),
                    scope: AssertionScope {
                        states: vec!["state2".to_string()],
                        ..Default::default()
                    },
                    rule: ConjunctRule::AllPass,
                },
                PolicyConjunct {
                    name: "indet".to_string(),
                    scope: AssertionScope {
                        assertion_ids: Some(vec![]),
                        ..Default::default()
                    },
                    rule: ConjunctRule::AllPass,
                },
            ],
        };
        let eval = apply_policy(&result, &policy);
        assert_eq!(eval.overall_status, PolicyStatus::Indeterminate);
    }

    #[test]
    fn observation_only_passes_vacuously() {
        let result = baseline_result();
        let policy = factories::observation_only();
        let eval = apply_policy(&result, &policy);
        assert_eq!(eval.overall_status, PolicyStatus::Pass);
        assert!(eval.conjunct_results.is_empty());
    }

    // ------------- ConjunctRule::AllPass -------------

    #[test]
    fn all_pass_passes_when_no_failures() {
        let result = baseline_result();
        let policy = SpecCheckPolicy {
            conjuncts: vec![PolicyConjunct {
                name: "n".to_string(),
                scope: AssertionScope {
                    states: vec!["state2".to_string()],
                    ..Default::default()
                },
                rule: ConjunctRule::AllPass,
            }],
        };
        let eval = apply_policy(&result, &policy);
        assert_eq!(eval.overall_status, PolicyStatus::Pass);
        assert!(eval.conjunct_results[0].evidence.contains("passed"));
    }

    #[test]
    fn all_pass_fails_when_any_failure() {
        let result = baseline_result();
        let policy = factories::strict_all_pass();
        let eval = apply_policy(&result, &policy);
        assert_eq!(eval.overall_status, PolicyStatus::Fail);
    }

    #[test]
    fn all_pass_indeterminate_on_empty_scope() {
        let result = baseline_result();
        let policy = SpecCheckPolicy {
            conjuncts: vec![PolicyConjunct {
                name: "n".to_string(),
                scope: AssertionScope {
                    assertion_ids: Some(vec![]),
                    ..Default::default()
                },
                rule: ConjunctRule::AllPass,
            }],
        };
        let eval = apply_policy(&result, &policy);
        assert_eq!(eval.overall_status, PolicyStatus::Indeterminate);
    }

    // ------------- ConjunctRule::MaxFailures -------------

    #[test]
    fn max_failures_passes_under_limit() {
        let result = baseline_result();
        let policy = SpecCheckPolicy {
            conjuncts: vec![PolicyConjunct {
                name: "n".to_string(),
                scope: AssertionScope::default(),
                rule: ConjunctRule::MaxFailures { count: 5 },
            }],
        };
        let eval = apply_policy(&result, &policy);
        assert_eq!(eval.overall_status, PolicyStatus::Pass);
    }

    #[test]
    fn max_failures_fails_over_limit() {
        let result = baseline_result();
        let policy = SpecCheckPolicy {
            conjuncts: vec![PolicyConjunct {
                name: "n".to_string(),
                scope: AssertionScope::default(),
                rule: ConjunctRule::MaxFailures { count: 0 },
            }],
        };
        let eval = apply_policy(&result, &policy);
        assert_eq!(eval.overall_status, PolicyStatus::Fail);
    }

    // ------------- ConjunctRule::FailureRateBelow -------------

    #[test]
    fn failure_rate_below_passes() {
        let result = baseline_result();
        let policy = SpecCheckPolicy {
            conjuncts: vec![PolicyConjunct {
                name: "n".to_string(),
                scope: AssertionScope::default(),
                rule: ConjunctRule::FailureRateBelow { rate: 0.5 },
            }],
        };
        // 2 failures of 6 = 0.333 < 0.5
        let eval = apply_policy(&result, &policy);
        assert_eq!(eval.overall_status, PolicyStatus::Pass);
    }

    #[test]
    fn failure_rate_below_fails() {
        let result = baseline_result();
        let policy = SpecCheckPolicy {
            conjuncts: vec![PolicyConjunct {
                name: "n".to_string(),
                scope: AssertionScope::default(),
                rule: ConjunctRule::FailureRateBelow { rate: 0.1 },
            }],
        };
        let eval = apply_policy(&result, &policy);
        assert_eq!(eval.overall_status, PolicyStatus::Fail);
    }

    #[test]
    fn failure_rate_below_indeterminate_on_empty() {
        let result = baseline_result();
        let policy = SpecCheckPolicy {
            conjuncts: vec![PolicyConjunct {
                name: "n".to_string(),
                scope: AssertionScope {
                    assertion_ids: Some(vec![]),
                    ..Default::default()
                },
                rule: ConjunctRule::FailureRateBelow { rate: 0.5 },
            }],
        };
        let eval = apply_policy(&result, &policy);
        assert_eq!(eval.overall_status, PolicyStatus::Indeterminate);
    }

    // ------------- ConjunctRule::StateMatchRateAtLeast -------------

    #[test]
    fn state_match_rate_at_least_passes() {
        let result = baseline_result();
        let policy = SpecCheckPolicy {
            conjuncts: vec![PolicyConjunct {
                name: "n".to_string(),
                scope: AssertionScope {
                    states: vec!["state2".to_string()],
                    ..Default::default()
                },
                rule: ConjunctRule::StateMatchRateAtLeast { rate: 0.9 },
            }],
        };
        let eval = apply_policy(&result, &policy);
        assert_eq!(eval.overall_status, PolicyStatus::Pass);
    }

    #[test]
    fn state_match_rate_at_least_fails() {
        let result = baseline_result();
        let policy = SpecCheckPolicy {
            conjuncts: vec![PolicyConjunct {
                name: "n".to_string(),
                scope: AssertionScope::default(),
                rule: ConjunctRule::StateMatchRateAtLeast { rate: 0.9 },
            }],
        };
        // state1 is 0.5, so this fails.
        let eval = apply_policy(&result, &policy);
        assert_eq!(eval.overall_status, PolicyStatus::Fail);
        assert!(eval.conjunct_results[0].evidence.contains("state1"));
    }

    #[test]
    fn state_match_rate_at_least_indeterminate_when_state_missing() {
        let result = baseline_result();
        let policy = SpecCheckPolicy {
            conjuncts: vec![PolicyConjunct {
                name: "n".to_string(),
                scope: AssertionScope {
                    states: vec!["state_unknown".to_string()],
                    ..Default::default()
                },
                rule: ConjunctRule::StateMatchRateAtLeast { rate: 0.5 },
            }],
        };
        let eval = apply_policy(&result, &policy);
        assert_eq!(eval.overall_status, PolicyStatus::Indeterminate);
    }

    // ------------- ConjunctRule::AnyStateMatchRateAtLeast -------------

    #[test]
    fn any_state_match_rate_at_least_passes() {
        let result = baseline_result();
        let policy = factories::require_perfect_state();
        // state2 has match_rate 1.0
        let eval = apply_policy(&result, &policy);
        assert_eq!(eval.overall_status, PolicyStatus::Pass);
    }

    #[test]
    fn any_state_match_rate_at_least_fails() {
        let result = baseline_result();
        let policy = SpecCheckPolicy {
            conjuncts: vec![PolicyConjunct {
                name: "n".to_string(),
                scope: AssertionScope::default(),
                rule: ConjunctRule::AnyStateMatchRateAtLeast { rate: 1.5 },
            }],
        };
        let eval = apply_policy(&result, &policy);
        assert_eq!(eval.overall_status, PolicyStatus::Fail);
    }

    #[test]
    fn any_state_match_rate_at_least_indeterminate_on_unknown_state() {
        let result = baseline_result();
        let policy = SpecCheckPolicy {
            conjuncts: vec![PolicyConjunct {
                name: "n".to_string(),
                scope: AssertionScope {
                    states: vec!["nonesuch".to_string()],
                    ..Default::default()
                },
                rule: ConjunctRule::AnyStateMatchRateAtLeast { rate: 0.5 },
            }],
        };
        let eval = apply_policy(&result, &policy);
        assert_eq!(eval.overall_status, PolicyStatus::Indeterminate);
    }

    // ------------- ConjunctRule::MatchOutcomeAtLeast -------------

    #[test]
    fn match_outcome_at_least_passes() {
        let result = baseline_result(); // PartialMatch
        let policy = SpecCheckPolicy {
            conjuncts: vec![PolicyConjunct {
                name: "n".to_string(),
                scope: AssertionScope::default(),
                rule: ConjunctRule::MatchOutcomeAtLeast {
                    outcome: MatchOutcome::PartialMatch,
                },
            }],
        };
        let eval = apply_policy(&result, &policy);
        assert_eq!(eval.overall_status, PolicyStatus::Pass);
    }

    #[test]
    fn match_outcome_at_least_fails_when_too_low() {
        let mut result = baseline_result();
        result.summary.match_outcome = MatchOutcome::NoMatch;
        let policy = SpecCheckPolicy {
            conjuncts: vec![PolicyConjunct {
                name: "n".to_string(),
                scope: AssertionScope::default(),
                rule: ConjunctRule::MatchOutcomeAtLeast {
                    outcome: MatchOutcome::FullMatch,
                },
            }],
        };
        let eval = apply_policy(&result, &policy);
        assert_eq!(eval.overall_status, PolicyStatus::Fail);
    }

    #[test]
    fn match_outcome_rank_ordering() {
        assert!(match_outcome_rank(MatchOutcome::NoMatch) < match_outcome_rank(MatchOutcome::PartialMatch));
        assert!(match_outcome_rank(MatchOutcome::PartialMatch) < match_outcome_rank(MatchOutcome::FullMatch));
    }

    // ------------- Scope filter — AND across populated fields -------------

    #[test]
    fn scope_filter_empty_matches_all() {
        let result = baseline_result();
        let filtered = filter_assertions(&result, &AssertionScope::default());
        assert_eq!(filtered.len(), 6);
    }

    #[test]
    fn scope_filter_assertion_ids_none_matches_all() {
        let result = baseline_result();
        let filtered = filter_assertions(
            &result,
            &AssertionScope {
                assertion_ids: None,
                ..Default::default()
            },
        );
        assert_eq!(filtered.len(), 6);
    }

    #[test]
    fn scope_filter_assertion_ids_empty_matches_nothing() {
        let result = baseline_result();
        let filtered = filter_assertions(
            &result,
            &AssertionScope {
                assertion_ids: Some(vec![]),
                ..Default::default()
            },
        );
        assert!(filtered.is_empty());
    }

    #[test]
    fn scope_filter_states_only() {
        let result = baseline_result();
        let filtered = filter_assertions(
            &result,
            &AssertionScope {
                states: vec!["state1".to_string()],
                ..Default::default()
            },
        );
        assert_eq!(filtered.len(), 4);
        assert!(filtered.iter().all(|sa| sa.state.state_id == "state1"));
    }

    #[test]
    fn scope_filter_severities_only() {
        let result = baseline_result();
        let filtered = filter_assertions(
            &result,
            &AssertionScope {
                severities: vec!["critical".to_string()],
                ..Default::default()
            },
        );
        // a1, a2 in state1 + b1 in state2 = 3 criticals.
        assert_eq!(filtered.len(), 3);
    }

    #[test]
    fn scope_filter_chain_state_and_severity_and() {
        let result = baseline_result();
        let filtered = filter_assertions(
            &result,
            &AssertionScope {
                states: vec!["state1".to_string()],
                severities: vec!["critical".to_string()],
                ..Default::default()
            },
        );
        // a1, a2 are critical on state1; b1 critical on state2 is excluded.
        assert_eq!(filtered.len(), 2);
        let ids: Vec<&str> = filtered
            .iter()
            .map(|sa| sa.assertion.assertion_id.as_str())
            .collect();
        assert!(ids.contains(&"a1"));
        assert!(ids.contains(&"a2"));
    }

    #[test]
    fn scope_filter_categories_and_assertion_ids_combine() {
        let result = baseline_result();
        let filtered = filter_assertions(
            &result,
            &AssertionScope {
                categories: vec!["a11y".to_string()],
                assertion_ids: Some(vec!["a3".to_string(), "b2".to_string()]),
                ..Default::default()
            },
        );
        assert_eq!(filtered.len(), 2);
    }

    // ------------- Factories -------------

    #[test]
    fn factory_criticals_only_shape() {
        let p = factories::criticals_only();
        assert_eq!(p.conjuncts.len(), 1);
        assert_eq!(p.conjuncts[0].name, "criticals-must-pass");
        assert_eq!(p.conjuncts[0].scope.severities, vec!["critical".to_string()]);
        assert!(matches!(p.conjuncts[0].rule, ConjunctRule::AllPass));
    }

    #[test]
    fn factory_criticals_only_fails_when_critical_fails() {
        let result = baseline_result(); // state1 has critical a2 failing
        let eval = apply_policy(&result, &factories::criticals_only());
        assert_eq!(eval.overall_status, PolicyStatus::Fail);
    }

    #[test]
    fn factory_strict_all_pass_shape() {
        let p = factories::strict_all_pass();
        assert_eq!(p.conjuncts.len(), 1);
        assert_eq!(p.conjuncts[0].name, "all-assertions-pass");
        assert!(p.conjuncts[0].scope.states.is_empty());
        assert!(p.conjuncts[0].scope.severities.is_empty());
        assert!(matches!(p.conjuncts[0].rule, ConjunctRule::AllPass));
    }

    #[test]
    fn factory_require_perfect_state_shape() {
        let p = factories::require_perfect_state();
        assert_eq!(p.conjuncts.len(), 1);
        match p.conjuncts[0].rule {
            ConjunctRule::AnyStateMatchRateAtLeast { rate } => assert_eq!(rate, 1.0),
            ref other => panic!("unexpected rule: {other:?}"),
        }
    }

    #[test]
    fn factory_require_state_shape() {
        let p = factories::require_state("login");
        assert_eq!(p.conjuncts.len(), 1);
        assert_eq!(p.conjuncts[0].name, "require-state-login");
        assert_eq!(p.conjuncts[0].scope.states, vec!["login".to_string()]);
        match p.conjuncts[0].rule {
            ConjunctRule::StateMatchRateAtLeast { rate } => assert_eq!(rate, 1.0),
            ref other => panic!("unexpected rule: {other:?}"),
        }
    }

    #[test]
    fn factory_observation_only_shape() {
        let p = factories::observation_only();
        assert!(p.conjuncts.is_empty());
    }

    // ------------- Round-trip serialization -------------

    #[test]
    fn round_trip_all_factories() {
        let policies = vec![
            factories::criticals_only(),
            factories::strict_all_pass(),
            factories::require_perfect_state(),
            factories::require_state("page-main"),
            factories::observation_only(),
        ];
        for p in policies {
            let s = serde_json::to_string(&p).expect("serialize");
            let back: SpecCheckPolicy = serde_json::from_str(&s).expect("deserialize");
            let s2 = serde_json::to_string(&back).expect("re-serialize");
            assert_eq!(s, s2);
        }
    }

    #[test]
    fn round_trip_every_conjunct_rule_variant() {
        let rules: Vec<ConjunctRule> = vec![
            ConjunctRule::AllPass,
            ConjunctRule::MaxFailures { count: 3 },
            ConjunctRule::FailureRateBelow { rate: 0.125 },
            ConjunctRule::StateMatchRateAtLeast { rate: 0.5 },
            ConjunctRule::AnyStateMatchRateAtLeast { rate: 0.75 },
            ConjunctRule::MatchOutcomeAtLeast {
                outcome: MatchOutcome::PartialMatch,
            },
        ];
        for rule in rules {
            let s = serde_json::to_string(&rule).expect("serialize");
            let back: ConjunctRule = serde_json::from_str(&s).expect("deserialize");
            let s2 = serde_json::to_string(&back).expect("re-serialize");
            assert_eq!(s, s2);
        }
    }
}
