//! Spec API broadcast channels — per-app fan-out.
//!
//! One `tokio::sync::broadcast` channel per registered `app_id`, lazily
//! created on first emit / first subscribe. The `/apps/:app_id/spec/subscribe`
//! SSE handler subscribes via [`subscribe`] and forwards the events; every
//! write path on the Spec API emits a [`SpecApiEvent`] via [`emit`], which
//! dispatches by `event.app_id()` (see `qontinui_types::spec_api_events`).
//!
//! Subscribers receive ONLY their own app's events — there is no cross-app
//! leakage. Per spec-multi-app Stream C (PLAN.md §C.4) this replaces the
//! single global `OnceLock<broadcast::Sender>`.
//!
//! The event taxonomy itself lives in [`qontinui_types::spec_api_events`]
//! so the Rust enum, the generated TS bindings, and the generated Python
//! Pydantic models stay in sync.
//!
//! ## Emit sites
//!
//! - `SpecChanged` — [`handlers.rs` post_author](super::handlers) emits after
//!   a successful IR + projection write.
//! - `SpecCheckInvoked` / `SpecCheckCompleted` — emitted by
//!   `spec_api/spec_check.rs` HTTP handlers.
//! - `SpecCheckPolicyViolation` — emitted by Plan 03's
//!   `step_executor/spec_check.rs` workflow-step handler when the conjunct
//!   evaluator returns a Fail.
//! - `FlywheelProposalPromoted` / `FlywheelProposalDemoted` — emitted by the
//!   Plan 05 Step 9 sweep handler (`POST /spec/proposals/sweep-pending` +
//!   `storage::promote_pending` / `storage::demote_pending`).

use std::collections::HashMap;
use std::sync::Mutex;

use once_cell::sync::Lazy;
use tokio::sync::broadcast;

pub use qontinui_types::spec_api_events::{SpecApiEvent, SpecChanged};

/// Broadcast capacity per app. Slow subscribers will Lag if they fall this
/// far behind; the SSE handler logs and continues.
const CAPACITY: usize = 256;

/// Per-app channel map. Lazily populated on first `channel_for` call (from
/// either `emit` or `subscribe`). The Mutex is held only for the
/// HashMap insert / lookup — broadcast sends happen outside the lock on
/// the cloned `Sender`.
static CHANNELS: Lazy<Mutex<HashMap<String, broadcast::Sender<SpecApiEvent>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Get or create the per-app broadcast Sender. Cloning a Sender is cheap
/// (Arc bump) and the resulting send happens lock-free.
fn channel_for(app_id: &str) -> broadcast::Sender<SpecApiEvent> {
    let mut guard = CHANNELS.lock().expect("CHANNELS mutex poisoned");
    guard
        .entry(app_id.to_string())
        .or_insert_with(|| broadcast::channel::<SpecApiEvent>(CAPACITY).0)
        .clone()
}

/// Subscribe to the per-app channel for `app_id`. Subscribers see only
/// this app's events; cross-app leakage is impossible by construction.
pub fn subscribe(app_id: &str) -> broadcast::Receiver<SpecApiEvent> {
    channel_for(app_id).subscribe()
}

/// Emit an event onto the channel for its owning app. Routing is driven by
/// `event.app_id()` (see the `qontinui_types::spec_api_events::SpecApiEvent`
/// helper). Ignore SendError: it just means there are no subscribers — drop
/// the event silently in that case.
pub fn emit(event: SpecApiEvent) {
    let tx = channel_for(event.app_id());
    let _ = tx.send(event);
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ===========================================================================
// SpecCheckPolicyViolation helpers
// ===========================================================================
//
// The `emit_policy_violations` helper lives here (rather than in either
// adapter) so HTTP (`/spec-check/batch`) and the workflow-step handler
// share one canonical implementation. One emit per `PolicyStatus::Fail`
// conjunct; `Indeterminate` is not emitted (per Plan 06 §5.6 — only
// deliberate violations count as observability signal).

use qontinui_types::spec_check::{
    ConjunctRule, PolicyConjunct, PolicyEvaluation, PolicyStatus, SpecCheckPolicy,
};

/// Wire name for the [`ConjunctRule`] variant — mirrors the
/// `#[serde(tag = "kind", rename_all = "snake_case")]` discriminator.
/// Carried on `SpecCheckPolicyViolation.rule_kind` so subscribers can
/// group violations by rule type without round-tripping the policy.
pub fn rule_kind_str(rule: &ConjunctRule) -> &'static str {
    match rule {
        ConjunctRule::AllPass => "all_pass",
        ConjunctRule::MaxFailures { .. } => "max_failures",
        ConjunctRule::FailureRateBelow { .. } => "failure_rate_below",
        ConjunctRule::StateMatchRateAtLeast { .. } => "state_match_rate_at_least",
        ConjunctRule::AnyStateMatchRateAtLeast { .. } => "any_state_match_rate_at_least",
        ConjunctRule::MatchOutcomeAtLeast { .. } => "match_outcome_at_least",
    }
}

/// Emit one [`SpecApiEvent::SpecCheckPolicyViolation`] per failing
/// [`qontinui_types::spec_check::ConjunctEvaluation`]. Looks up each
/// evaluation by `name` in the originating policy so the emit carries
/// the structural `rule_kind` discriminator alongside the evidence
/// string. `Indeterminate` conjuncts are NOT emitted — only `Fail`.
///
/// Called from both `/spec-check/batch` (when `sharedPolicy` /
/// `policyOverride` apply) and the workflow-step handler (which applies
/// policy locally after self-calling `/spec-check`). No double-emit risk
/// since the workflow handler self-calls the **single** endpoint, which
/// doesn't run policy.
pub fn emit_policy_violations(
    app_id: &str,
    snapshot_id: &str,
    page_id: &str,
    policy: &SpecCheckPolicy,
    eval: &PolicyEvaluation,
) {
    for conjunct_eval in &eval.conjunct_results {
        if conjunct_eval.status != PolicyStatus::Fail {
            continue;
        }
        let rule_kind = policy
            .conjuncts
            .iter()
            .find(|c: &&PolicyConjunct| c.name == conjunct_eval.name)
            .map(|c| rule_kind_str(&c.rule))
            .unwrap_or("unknown");
        emit(SpecApiEvent::SpecCheckPolicyViolation {
            app_id: app_id.to_string(),
            snapshot_id: snapshot_id.to_string(),
            page_id: page_id.to_string(),
            conjunct_name: conjunct_eval.name.clone(),
            rule_kind: rule_kind.to_string(),
            observed: serde_json::Value::String(conjunct_eval.evidence.clone()),
            at_ms: now_ms(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_types::spec_check::{
        AssertionScope, ConjunctEvaluation, MatchOutcome, PolicyConjunct, SpecCheckPolicy,
    };

    #[test]
    fn rule_kind_str_covers_every_variant() {
        // Locks the snake_case discriminator strings against accidental
        // drift from `#[serde(tag = "kind", rename_all = "snake_case")]`.
        assert_eq!(rule_kind_str(&ConjunctRule::AllPass), "all_pass");
        assert_eq!(
            rule_kind_str(&ConjunctRule::MaxFailures { count: 0 }),
            "max_failures"
        );
        assert_eq!(
            rule_kind_str(&ConjunctRule::FailureRateBelow { rate: 0.0 }),
            "failure_rate_below"
        );
        assert_eq!(
            rule_kind_str(&ConjunctRule::StateMatchRateAtLeast { rate: 0.0 }),
            "state_match_rate_at_least"
        );
        assert_eq!(
            rule_kind_str(&ConjunctRule::AnyStateMatchRateAtLeast { rate: 0.0 }),
            "any_state_match_rate_at_least"
        );
        assert_eq!(
            rule_kind_str(&ConjunctRule::MatchOutcomeAtLeast {
                outcome: MatchOutcome::FullMatch
            }),
            "match_outcome_at_least"
        );
    }

    #[test]
    fn emit_policy_violations_publishes_one_per_failing_conjunct() {
        let app_id = "qontinui-runner";
        let mut rx = subscribe(app_id);
        let marker = "scs_test_policy_violation_emit_shared_1";
        let page_id = "settings-test-violation-shared";
        let policy = SpecCheckPolicy {
            conjuncts: vec![
                PolicyConjunct {
                    name: "critical-all-pass".into(),
                    scope: AssertionScope::default(),
                    rule: ConjunctRule::AllPass,
                },
                PolicyConjunct {
                    name: "max-three-failures".into(),
                    scope: AssertionScope::default(),
                    rule: ConjunctRule::MaxFailures { count: 3 },
                },
                PolicyConjunct {
                    name: "passing-conjunct".into(),
                    scope: AssertionScope::default(),
                    rule: ConjunctRule::AllPass,
                },
            ],
        };
        let eval = PolicyEvaluation {
            overall_status: PolicyStatus::Fail,
            conjunct_results: vec![
                ConjunctEvaluation {
                    name: "critical-all-pass".into(),
                    status: PolicyStatus::Fail,
                    evidence: "2 of 17 assertions failed".into(),
                },
                ConjunctEvaluation {
                    name: "max-three-failures".into(),
                    status: PolicyStatus::Fail,
                    evidence: "5 failures > 3 budget".into(),
                },
                ConjunctEvaluation {
                    name: "passing-conjunct".into(),
                    status: PolicyStatus::Pass,
                    evidence: "all green".into(),
                },
            ],
        };
        emit_policy_violations(app_id, marker, page_id, &policy, &eval);
        let mut violations = Vec::new();
        for _ in 0..128 {
            match rx.try_recv() {
                Ok(SpecApiEvent::SpecCheckPolicyViolation {
                    snapshot_id,
                    page_id: pid,
                    conjunct_name,
                    rule_kind,
                    ..
                }) if snapshot_id == marker => {
                    violations.push((conjunct_name, rule_kind, pid));
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        assert_eq!(violations.len(), 2, "expected exactly 2 violation emits");
        // Order = policy declaration order; first failing conjunct first.
        assert_eq!(violations[0].0, "critical-all-pass");
        assert_eq!(violations[0].1, "all_pass");
        assert_eq!(violations[0].2, page_id);
        assert_eq!(violations[1].0, "max-three-failures");
        assert_eq!(violations[1].1, "max_failures");
    }

    #[test]
    fn emit_policy_violations_skips_pass_and_indeterminate_conjuncts() {
        let app_id = "qontinui-runner";
        let mut rx = subscribe(app_id);
        let marker = "scs_test_policy_violation_emit_skip_1";
        let policy = SpecCheckPolicy {
            conjuncts: vec![PolicyConjunct {
                name: "single".into(),
                scope: AssertionScope::default(),
                rule: ConjunctRule::AllPass,
            }],
        };
        // Indeterminate must NOT emit (per the §5.6 contract — only
        // deliberate Fail violations count as observability signal).
        let eval = PolicyEvaluation {
            overall_status: PolicyStatus::Indeterminate,
            conjunct_results: vec![ConjunctEvaluation {
                name: "single".into(),
                status: PolicyStatus::Indeterminate,
                evidence: "no in-scope assertions".into(),
            }],
        };
        emit_policy_violations(app_id, marker, "page-x", &policy, &eval);
        // Drain; expect zero matching events.
        let mut hits = 0;
        for _ in 0..32 {
            match rx.try_recv() {
                Ok(SpecApiEvent::SpecCheckPolicyViolation { snapshot_id, .. })
                    if snapshot_id == marker =>
                {
                    hits += 1;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        assert_eq!(hits, 0, "Indeterminate must not emit");
    }
}
