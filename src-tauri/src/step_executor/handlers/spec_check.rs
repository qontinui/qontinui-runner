//! Spec-Check Step Handler (Plan 03 Step 9)
//!
//! Thin shim over the runner's own `POST /spec-check` HTTP endpoint
//! (Steps 1-2) plus `qontinui_spec_check::apply_policy`. Persists
//! `{spec_check_result, policy_evaluation}` to
//! `workflow_verification_phase_results.result_json` via the existing
//! verification-phase persistence pipeline (the `output_data` payload here
//! is what gets folded under `details.spec_check`).
//!
//! Behavior per `00-design-context.md` §5.9 / §5.13 / §5.16:
//!   - `fail_when_no_app` + `fail_on` govern SnapshotFetchError handling
//!   - `fail_when_no_spec` governs un-spec'd `page_id` handling
//!   - Step status comes from `policy_evaluation.overall_status` (when a
//!     policy is supplied) or from `summary.match_outcome == FullMatch`
//!     (when no policy is supplied — there is no `Match` variant)
//!
//! `HandlerContext.app_state` is `Arc<AppState>` from `crate::commands`,
//! NOT `Arc<ApiState>`. The handler self-calls the runner's own HTTP API
//! via `get_self_base_url(&context.app_state) + reqwest`, exactly as
//! `ui_bridge_visual_assertion.rs` does for SDK-dependent steps.

use async_trait::async_trait;
use serde_json::json;
use tracing::{info, warn};

use super::{ExecutionStepConfig, HandlerContext, StepHandler, StepHandlerResult};
use crate::spec_api::events;
use qontinui_spec_check as spec_check; // crate name = "qontinui-spec-check"
use qontinui_types::spec_api_events::SpecApiEvent;
use qontinui_types::spec_check::{
    ConjunctRule, MatchOutcome, PolicyConjunct, PolicyEvaluation, PolicyStatus, SpecCheckPolicy,
};

/// Wire name for the `ConjunctRule` variant — mirrors the
/// `#[serde(tag = "kind", rename_all = "snake_case")]` discriminator.
fn rule_kind_str(rule: &ConjunctRule) -> &'static str {
    match rule {
        ConjunctRule::AllPass => "all_pass",
        ConjunctRule::MaxFailures { .. } => "max_failures",
        ConjunctRule::FailureRateBelow { .. } => "failure_rate_below",
        ConjunctRule::StateMatchRateAtLeast { .. } => "state_match_rate_at_least",
        ConjunctRule::AnyStateMatchRateAtLeast { .. } => "any_state_match_rate_at_least",
        ConjunctRule::MatchOutcomeAtLeast { .. } => "match_outcome_at_least",
    }
}

/// Emit one `SpecCheckPolicyViolation` per failing `ConjunctEvaluation`.
/// Looks up each evaluation by `name` in the originating policy so the
/// emit carries the structural `rule_kind` discriminator alongside the
/// evidence string. Indeterminate conjuncts are NOT emitted — only `Fail`.
fn emit_policy_violations(
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
        events::emit(SpecApiEvent::SpecCheckPolicyViolation {
            snapshot_id: snapshot_id.to_string(),
            page_id: page_id.to_string(),
            conjunct_name: conjunct_eval.name.clone(),
            rule_kind: rule_kind.to_string(),
            observed: serde_json::Value::String(conjunct_eval.evidence.clone()),
            at_ms: events::now_ms(),
        });
    }
}

pub struct SpecCheckHandler;

#[async_trait]
impl StepHandler for SpecCheckHandler {
    fn step_type(&self) -> &'static str {
        "spec_check"
    }

    fn display_name(&self) -> &'static str {
        "Spec Check"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        // 1. Read config from the flattened ExecutionStepConfig fields.
        let page_id = match &step.spec_check_page_id {
            Some(id) if !id.is_empty() => id.clone(),
            _ => return StepHandlerResult::failure("spec_check step missing spec_check_page_id"),
        };
        let policy: Option<SpecCheckPolicy> = step
            .spec_check_policy
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let fail_when_no_app = step.spec_check_fail_when_no_app.unwrap_or(true);
        let fail_when_no_spec = step.spec_check_fail_when_no_spec.unwrap_or(true);
        let fail_on = step
            .spec_check_fail_on
            .clone()
            .unwrap_or_else(default_fail_on);

        // 2. Self-call the runner's own POST /spec-check (same process,
        //    shares the SDK connection via ApiState). All spec-loading,
        //    snapshot-fetch, and matching logic lives there / in Plan 02.
        let self_base = crate::mcp::types::get_self_base_url(&context.app_state);
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
        {
            Ok(c) => c,
            Err(e) => return StepHandlerResult::failure(format!("reqwest client init: {e}")),
        };
        let http_resp = client
            .post(format!("{}/spec-check", self_base))
            .json(&json!({ "page_id": page_id }))
            .send()
            .await;
        let (status_code, body): (reqwest::StatusCode, serde_json::Value) = match http_resp {
            Ok(r) => {
                let code = r.status();
                let body = r.json::<serde_json::Value>().await.unwrap_or(json!({}));
                (code, body)
            }
            Err(e) => {
                let payload = json!({
                    "spec_check_status": "snapshot-unavailable",
                    "cause": "self-call-failed",
                    "page_id": page_id,
                    "error": e.to_string(),
                });
                return if fail_when_no_app {
                    StepHandlerResult::failure_with_data(
                        "self-call to /spec-check failed".to_string(),
                        payload,
                    )
                } else {
                    StepHandlerResult::success_with_data(payload)
                };
            }
        };

        // 3. Classify HTTP status → step decision, mirroring the §5.9 table.
        if !status_code.is_success() {
            let cause = body
                .get("detail")
                .and_then(|d| d.get("cause"))
                .and_then(|c| c.as_str())
                .unwrap_or("unknown")
                .to_string();
            let variant = http_status_to_variant(status_code, &cause);
            let should_fail = should_fail_for_status(
                status_code,
                &variant,
                fail_when_no_app,
                fail_when_no_spec,
                &fail_on,
            );
            let payload = json!({
                "spec_check_status": body.get("reason"),
                "cause": variant,
                "http_status": status_code.as_u16(),
                "page_id": page_id,
            });
            return if should_fail {
                StepHandlerResult::failure_with_data(
                    format!("spec-check failed: {variant}"),
                    payload,
                )
            } else {
                StepHandlerResult::success_with_data(payload)
            };
        }

        // 4. Parse the success body back into a typed SpecCheckResult.
        let result: qontinui_types::spec_check::SpecCheckResult = match serde_json::from_value(body)
        {
            Ok(r) => r,
            Err(e) => return StepHandlerResult::failure(format!("parse /spec-check body: {e}")),
        };

        // 5. Apply policy (if any) — pure crate call.
        let policy_eval = policy
            .as_ref()
            .map(|p| spec_check::apply_policy(&result, p));

        // 5b. Plan 06: per-conjunct emission of `SpecCheckPolicyViolation`.
        //     One event per `Fail` conjunct; `Indeterminate` is not emitted
        //     (only deliberate violations count as observability signal).
        if let (Some(pol), Some(eval)) = (policy.as_ref(), policy_eval.as_ref()) {
            emit_policy_violations(&result.snapshot_id, &page_id, pol, eval);
        }

        // 6. Status: prefer policy, fall back to summary.match_outcome.
        //    MatchOutcome: FullMatch | PartialMatch | NoMatch (no `Match`).
        //    Step passes only on FullMatch when no policy is supplied.
        let pass = decide_pass(
            policy_eval.as_ref().map(|pe| pe.overall_status),
            result.summary.match_outcome,
        );

        // 7. Build persistence payload. The verification-phase pipeline
        //    writes `output_data` under `details.spec_check`; both blobs
        //    are JSONB-indexable per §5.16 (after CR-5 text→jsonb).
        let payload = json!({
            "spec_check_status": "ok",
            "page_id": page_id,
            "spec_check_result": result,
            "policy_evaluation": policy_eval,
        });

        info!(
            "spec_check step id={:?} page_id={} pass={} match_outcome={:?}",
            step.id, page_id, pass, result.summary.match_outcome
        );

        if pass {
            StepHandlerResult::success_with_data(payload)
        } else {
            StepHandlerResult::failure_with_data(
                format!("spec_check failed for page_id={}", page_id),
                payload,
            )
        }
    }
}

fn default_fail_on() -> Vec<String> {
    vec![
        "not_connected".into(),
        "timeout".into(),
        "network".into(),
        "forbidden".into(),
        "malformed".into(),
    ]
}

/// Map the HTTP status from the runner's own `/spec-check` response to the
/// variant strings used by `fail_on`. The reverse mapping of the §5.9 table.
fn http_status_to_variant(status: reqwest::StatusCode, cause: &str) -> String {
    use reqwest::StatusCode as S;
    match status {
        S::FAILED_DEPENDENCY => "not_connected".into(), // 424
        S::GATEWAY_TIMEOUT => "timeout".into(),         // 504
        S::CONFLICT => "partial".into(),                // 409
        S::BAD_GATEWAY => {
            // 502 is a multiplexed bucket. Read the body cause to differentiate.
            match cause {
                "network" => "network".into(),
                "upstream-forbidden" => "forbidden".into(),
                "malformed" => "malformed".into(),
                other => other.to_string(),
            }
        }
        S::NOT_FOUND => "spec_not_found".into(),
        S::BAD_REQUEST => "invalid_request".into(),
        _ => "unknown".into(),
    }
}

/// Decide whether a non-2xx `/spec-check` response should fail the step,
/// per the per-step `fail_*` switches.
fn should_fail_for_status(
    status: reqwest::StatusCode,
    variant: &str,
    fail_when_no_app: bool,
    fail_when_no_spec: bool,
    fail_on: &[String],
) -> bool {
    match status.as_u16() {
        404 => fail_when_no_spec,
        400 => true, // invalid request is always a step failure
        _ => fail_when_no_app && fail_on.iter().any(|v| v == variant),
    }
}

/// Step pass decision: prefer the policy verdict, fall back to a bare
/// `FullMatch` requirement. There is no `Match` variant on `MatchOutcome`.
fn decide_pass(policy_status: Option<PolicyStatus>, outcome: MatchOutcome) -> bool {
    match policy_status {
        Some(s) => matches!(s, PolicyStatus::Pass),
        None => matches!(outcome, MatchOutcome::FullMatch),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::step_executor::handlers::HandlerRegistry;

    #[test]
    fn handler_registers_under_spec_check() {
        let mut registry = HandlerRegistry::new();
        registry.register(SpecCheckHandler);
        assert!(registry.has_handler("spec_check"));
        assert!(registry.get("spec_check").is_some());
    }

    #[test]
    fn default_fail_on_excludes_partial_and_stale() {
        let d = default_fail_on();
        assert!(d.contains(&"not_connected".to_string()));
        assert!(d.contains(&"timeout".to_string()));
        assert!(d.contains(&"network".to_string()));
        assert!(d.contains(&"forbidden".to_string()));
        assert!(d.contains(&"malformed".to_string()));
        assert!(!d.contains(&"partial".to_string()));
        assert!(!d.contains(&"stale".to_string()));
    }

    #[test]
    fn http_status_maps_to_fail_on_variants() {
        use reqwest::StatusCode as S;
        assert_eq!(
            http_status_to_variant(S::FAILED_DEPENDENCY, ""),
            "not_connected"
        );
        assert_eq!(http_status_to_variant(S::GATEWAY_TIMEOUT, ""), "timeout");
        assert_eq!(http_status_to_variant(S::CONFLICT, ""), "partial");
        assert_eq!(http_status_to_variant(S::BAD_GATEWAY, "network"), "network");
        assert_eq!(
            http_status_to_variant(S::BAD_GATEWAY, "upstream-forbidden"),
            "forbidden"
        );
        assert_eq!(
            http_status_to_variant(S::BAD_GATEWAY, "malformed"),
            "malformed"
        );
        assert_eq!(http_status_to_variant(S::NOT_FOUND, ""), "spec_not_found");
        assert_eq!(
            http_status_to_variant(S::BAD_REQUEST, ""),
            "invalid_request"
        );
    }

    #[test]
    fn should_fail_respects_no_spec_switch() {
        use reqwest::StatusCode as S;
        // 404 with fail_when_no_spec=false → soft pass
        assert!(!should_fail_for_status(
            S::NOT_FOUND,
            "spec_not_found",
            true,
            false,
            &default_fail_on()
        ));
        // 404 with fail_when_no_spec=true → fail
        assert!(should_fail_for_status(
            S::NOT_FOUND,
            "spec_not_found",
            true,
            true,
            &default_fail_on()
        ));
    }

    #[test]
    fn should_fail_respects_no_app_and_fail_on() {
        use reqwest::StatusCode as S;
        // 424 not_connected, in default fail_on, fail_when_no_app=true → fail
        assert!(should_fail_for_status(
            S::FAILED_DEPENDENCY,
            "not_connected",
            true,
            true,
            &default_fail_on()
        ));
        // Same, but fail_when_no_app=false → soft pass
        assert!(!should_fail_for_status(
            S::FAILED_DEPENDENCY,
            "not_connected",
            false,
            true,
            &default_fail_on()
        ));
        // 409 partial is NOT in default fail_on → soft pass
        assert!(!should_fail_for_status(
            S::CONFLICT,
            "partial",
            true,
            true,
            &default_fail_on()
        ));
        // 400 invalid request always fails
        assert!(should_fail_for_status(
            S::BAD_REQUEST,
            "invalid_request",
            false,
            false,
            &[]
        ));
    }

    #[test]
    fn decide_pass_prefers_policy_then_full_match() {
        // Policy present → mirrors policy status
        assert!(decide_pass(Some(PolicyStatus::Pass), MatchOutcome::NoMatch));
        assert!(!decide_pass(
            Some(PolicyStatus::Fail),
            MatchOutcome::FullMatch
        ));
        assert!(!decide_pass(
            Some(PolicyStatus::Indeterminate),
            MatchOutcome::FullMatch
        ));
        // No policy → only FullMatch passes
        assert!(decide_pass(None, MatchOutcome::FullMatch));
        assert!(!decide_pass(None, MatchOutcome::PartialMatch));
        assert!(!decide_pass(None, MatchOutcome::NoMatch));
    }

    #[test]
    fn config_deserializes_camel_and_snake_case() {
        // camelCase
        let camel: ExecutionStepConfig = serde_json::from_value(json!({
            "type": "spec_check",
            "specCheckPageId": "settings-general",
            "specCheckPolicy": { "conjuncts": [] },
            "specCheckFailWhenNoApp": false,
            "specCheckFailWhenNoSpec": false,
            "specCheckFailOn": ["timeout"]
        }))
        .unwrap();
        assert_eq!(
            camel.spec_check_page_id.as_deref(),
            Some("settings-general")
        );
        assert!(camel.spec_check_policy.is_some());
        assert_eq!(camel.spec_check_fail_when_no_app, Some(false));
        assert_eq!(camel.spec_check_fail_when_no_spec, Some(false));
        assert_eq!(camel.spec_check_fail_on, Some(vec!["timeout".to_string()]));

        // snake_case
        let snake: ExecutionStepConfig = serde_json::from_value(json!({
            "type": "spec_check",
            "spec_check_page_id": "active-runs",
            "spec_check_fail_on": ["network", "malformed"]
        }))
        .unwrap();
        assert_eq!(snake.spec_check_page_id.as_deref(), Some("active-runs"));
        assert_eq!(
            snake.spec_check_fail_on,
            Some(vec!["network".to_string(), "malformed".to_string()])
        );
    }

    #[test]
    fn config_default_has_all_spec_check_fields_none() {
        let d = ExecutionStepConfig::default();
        assert!(d.spec_check_page_id.is_none());
        assert!(d.spec_check_policy.is_none());
        assert!(d.spec_check_fail_when_no_app.is_none());
        assert!(d.spec_check_fail_when_no_spec.is_none());
        assert!(d.spec_check_fail_on.is_none());
    }

    // ------------------------------------------------------------------------
    // Plan 06 — emit-callsite helpers
    // ------------------------------------------------------------------------

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
        use qontinui_types::spec_api_events::SpecApiEvent;
        use qontinui_types::spec_check::{AssertionScope, ConjunctEvaluation, PolicyConjunct};
        let mut rx = events::subscribe();
        let marker = "scs_test_policy_violation_emit_1";
        let page_id = "settings-test-violation";
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
        let eval = qontinui_types::spec_check::PolicyEvaluation {
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
        emit_policy_violations(marker, page_id, &policy, &eval);
        // Two failing conjuncts → expect two emits with our marker.
        // Drain up to 64 events from the broadcast, filter by marker.
        let mut violations = Vec::new();
        for _ in 0..64 {
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
        // Order is policy declaration order; first failing conjunct first.
        assert_eq!(violations[0].0, "critical-all-pass");
        assert_eq!(violations[0].1, "all_pass");
        assert_eq!(violations[0].2, page_id);
        assert_eq!(violations[1].0, "max-three-failures");
        assert_eq!(violations[1].1, "max_failures");
    }
}
