//! Effect-Check Step Handler (D3 effect-calculus Phase 3c)
//!
//! Triggers a UI Bridge action against the connected SDK app and asserts on the
//! D3 effect-calculus verification the SDK attaches to the `ActionResponse`
//! (`effectVerification`). The runner is a pure RELAY here: it deserializes only
//! the sub-shape it asserts on (`outcome` / `cause` / `containment` /
//! `durationMs`) and ships the full raw verification into `result_json` for
//! diagnostics. It does NOT recompute on `predicted` / `observed`.
//!
//! ## Triggering the action
//! Mirrors the `spec_check` self-call pattern: this handler self-calls the
//! runner's own `POST /ui-bridge/sdk/element/{id}/action` endpoint
//! (`crate::mcp::sdk_client::handle_element_action`) via
//! `get_self_base_url(&context.app_state) + reqwest`, exactly as `spec_check.rs`
//! and `ui_bridge_visual_assertion.rs` do for SDK-dependent steps. That endpoint
//! returns the SDK's `ActionResponse` JSON (possibly wrapped under a `data`
//! envelope on the IPC-fallback path), from which we read `effectVerification`.
//!
//! ## Pass / fail logic
//!   - `effectVerification` ABSENT  → step fails (`effect_check_status =
//!     "unavailable"`): the SDK must have effect verification enabled and a
//!     handler signature must resolve for this `(action, element)`.
//!   - `expected_outcome` SET       → pass iff `observed == expected`.
//!   - `expected_outcome` UNSET     → pass iff observed ∈ {Confirmed, Surprise,
//!     Partial}. `Failure` and `Contradiction` fail (Contradiction is an
//!     emergency-stop signal).
//!
//! ## Known integration dependency
//! `effect_check` can only PASS when the connected SDK app has effect
//! verification enabled AND a signature resolves for the targeted
//! `(action, element)`. Absent that, every run reports `unavailable` and fails —
//! this is by design (the step's whole purpose is to assert the verification
//! exists and matches), not a runner bug.

use async_trait::async_trait;
use serde_json::json;
use tracing::{info, warn};

use super::{ExecutionStepConfig, HandlerContext, StepHandler, StepHandlerResult};

/// The five effect-calculus outcomes that pass when no `expected_outcome` is
/// pinned. `Failure` and `Contradiction` are deliberately excluded.
const SOFT_PASS_OUTCOMES: [&str; 3] = ["Confirmed", "Surprise", "Partial"];

/// Local view of the `effectVerification.containment` sub-object. Deserialized
/// from the opaque `effect_verification` JSON the SDK attaches — deliberately
/// NOT added to `qontinui-types` (the wire stays lean/opaque; only the runner
/// types the slice it asserts on).
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct ContainmentView {
    predicted_subset_observed: bool,
    observed_subset_predicted: bool,
    active_negation: bool,
    coverage: f64,
}

/// Local view of the `effectVerification` object — only the fields the runner
/// asserts on. The full raw value is relayed separately into `result_json`.
#[derive(serde::Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct EffectVerificationView {
    outcome: String,
    cause: String,
    containment: ContainmentView,
    duration_ms: u64,
}

pub struct EffectCheckHandler;

#[async_trait]
impl StepHandler for EffectCheckHandler {
    fn step_type(&self) -> &'static str {
        "effect_check"
    }

    fn display_name(&self) -> &'static str {
        "Effect Check"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        // 1. Read + validate config.
        let element_id = match step.effect_check_element_id.as_deref() {
            Some(id) if !id.trim().is_empty() => id.trim().to_string(),
            _ => {
                return StepHandlerResult::failure(
                    "effect_check step missing effect_check_element_id",
                )
            }
        };
        let action = match step.effect_check_action.as_deref() {
            Some(a) if !a.trim().is_empty() => a.trim().to_string(),
            _ => {
                return StepHandlerResult::failure("effect_check step missing effect_check_action")
            }
        };
        let expected_outcome = step
            .effect_check_expected_outcome
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        // Validate expected_outcome (when present) against the known set so a
        // typo fails fast rather than silently never-matching.
        if let Some(ref exp) = expected_outcome {
            if !is_valid_outcome(exp) {
                return StepHandlerResult::failure(format!(
                    "effect_check expected_outcome '{}' is not a valid outcome \
                     (expected one of Confirmed, Surprise, Failure, Contradiction, Partial)",
                    exp
                ));
            }
        }

        // 2. Trigger the action via the runner's own SDK action endpoint.
        let self_base = crate::mcp::types::get_self_base_url(&context.app_state);
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
        {
            Ok(c) => c,
            Err(e) => return StepHandlerResult::failure(format!("reqwest client init: {e}")),
        };

        let action_url = format!(
            "{}/ui-bridge/sdk/element/{}/action",
            self_base,
            urlencoding::encode(&element_id)
        );
        let mut body = json!({ "action": action });
        if let Some(params) = &step.effect_check_params {
            body["params"] = params.clone();
        }

        let http_resp = client.post(&action_url).json(&body).send().await;
        let response_json: serde_json::Value = match http_resp {
            Ok(r) => match r.json::<serde_json::Value>().await {
                Ok(v) => v,
                Err(e) => {
                    return StepHandlerResult::failure(format!(
                        "effect_check: failed to parse action response: {e}"
                    ))
                }
            },
            Err(e) => {
                return StepHandlerResult::failure(format!(
                    "effect_check: action request to {} failed: {e}",
                    action_url
                ))
            }
        };

        // 3. Read `effectVerification` — top-level on the `ActionResponse`, or
        //    nested under a `data` envelope (the IPC-fallback wrap path in
        //    `handle_element_action`). The SDK emits camelCase (`effectVerification`).
        let effect_verification = extract_effect_verification(&response_json);

        let effect_verification = match effect_verification {
            Some(v) => v,
            None => {
                warn!(
                    "effect_check step id={:?} element_id={} action={}: \
                     effectVerification absent on ActionResponse",
                    step.id, element_id, action
                );
                let payload = json!({
                    "effect_check_status": "unavailable",
                    "action": action,
                    "element_id": element_id,
                });
                return StepHandlerResult::failure_with_data(
                    "effect_check: effect verification not present on the response \
                     (the SDK must have effect verification enabled and a signature \
                     must resolve for this action+element)"
                        .to_string(),
                    payload,
                );
            }
        };

        // 4. Deserialize the asserted sub-shape.
        let view: EffectVerificationView = match serde_json::from_value(effect_verification.clone())
        {
            Ok(v) => v,
            Err(e) => {
                let payload = json!({
                    "effect_check_status": "malformed",
                    "action": action,
                    "element_id": element_id,
                    "full_verification": effect_verification,
                });
                return StepHandlerResult::failure_with_data(
                    format!("effect_check: malformed effect verification shape: {e}"),
                    payload,
                );
            }
        };

        // 5. Decide pass / fail.
        let pass = match &expected_outcome {
            Some(exp) => view.outcome == *exp,
            None => SOFT_PASS_OUTCOMES.contains(&view.outcome.as_str()),
        };

        // 6. Persist the diagnostics payload (full raw verification goes into
        //    result_json; the runner relays, it does not recompute).
        let payload = json!({
            "effect_check_status": if pass { "ok" } else { "mismatch" },
            "action": action,
            "element_id": element_id,
            "expected_outcome": expected_outcome,
            "observed_outcome": view.outcome,
            "cause": view.cause,
            "containment": view.containment,
            "duration_ms": view.duration_ms,
            "full_verification": effect_verification,
        });

        info!(
            "effect_check step id={:?} element_id={} action={} expected={:?} observed={} cause={} pass={}",
            step.id, element_id, action, expected_outcome, view.outcome, view.cause, pass
        );

        if pass {
            StepHandlerResult::success_with_data(payload)
        } else {
            let msg = match &expected_outcome {
                Some(exp) => format!(
                    "effect_check failed: expected outcome '{}' but observed '{}'",
                    exp, view.outcome
                ),
                None => format!(
                    "effect_check failed: observed outcome '{}' (Failure/Contradiction do not pass)",
                    view.outcome
                ),
            };
            StepHandlerResult::failure_with_data(msg, payload)
        }
    }
}

/// Whether `s` is one of the five canonical effect-calculus outcomes.
fn is_valid_outcome(s: &str) -> bool {
    matches!(
        s,
        "Confirmed" | "Surprise" | "Failure" | "Contradiction" | "Partial"
    )
}

/// Extract the `effectVerification` object from an action-endpoint response.
///
/// The runner's `/ui-bridge/sdk/element/:id/action` endpoint returns either the
/// SDK's `ActionResponse` directly (WS/HTTP dispatch path → `effectVerification`
/// at the top level) or a `{ "success": true, "data": <ActionResponse> }`
/// envelope (IPC-fallback path → `effectVerification` nested under `data`). Try
/// both. Returns the object only when it is a non-null JSON value.
fn extract_effect_verification(response: &serde_json::Value) -> Option<serde_json::Value> {
    let candidate = response.get("effectVerification").or_else(|| {
        response
            .get("data")
            .and_then(|d| d.get("effectVerification"))
    });
    match candidate {
        Some(v) if !v.is_null() => Some(v.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::step_executor::handlers::HandlerRegistry;

    #[test]
    fn handler_registers_under_effect_check() {
        let mut registry = HandlerRegistry::new();
        registry.register(EffectCheckHandler);
        assert!(registry.has_handler("effect_check"));
        assert!(registry.get("effect_check").is_some());
    }

    #[test]
    fn valid_outcomes_recognised() {
        for o in [
            "Confirmed",
            "Surprise",
            "Failure",
            "Contradiction",
            "Partial",
        ] {
            assert!(is_valid_outcome(o), "{o} should be valid");
        }
        assert!(!is_valid_outcome("confirmed")); // case-sensitive
        assert!(!is_valid_outcome("Bogus"));
        assert!(!is_valid_outcome(""));
    }

    #[test]
    fn soft_pass_set_excludes_failure_and_contradiction() {
        assert!(SOFT_PASS_OUTCOMES.contains(&"Confirmed"));
        assert!(SOFT_PASS_OUTCOMES.contains(&"Surprise"));
        assert!(SOFT_PASS_OUTCOMES.contains(&"Partial"));
        assert!(!SOFT_PASS_OUTCOMES.contains(&"Failure"));
        assert!(!SOFT_PASS_OUTCOMES.contains(&"Contradiction"));
    }

    #[test]
    fn extract_effect_verification_top_level() {
        let resp = json!({
            "success": true,
            "durationMs": 12,
            "timestamp": 1,
            "effectVerification": { "outcome": "Confirmed" }
        });
        let ev = extract_effect_verification(&resp).expect("top-level present");
        assert_eq!(
            ev.get("outcome").and_then(|v| v.as_str()),
            Some("Confirmed")
        );
    }

    #[test]
    fn extract_effect_verification_nested_under_data() {
        let resp = json!({
            "success": true,
            "data": { "effectVerification": { "outcome": "Surprise" } }
        });
        let ev = extract_effect_verification(&resp).expect("nested present");
        assert_eq!(ev.get("outcome").and_then(|v| v.as_str()), Some("Surprise"));
    }

    #[test]
    fn extract_effect_verification_absent() {
        let resp = json!({ "success": true, "durationMs": 5, "timestamp": 1 });
        assert!(extract_effect_verification(&resp).is_none());
        // Null is treated as absent.
        let resp_null = json!({ "effectVerification": null });
        assert!(extract_effect_verification(&resp_null).is_none());
    }

    #[test]
    fn effect_verification_view_deserializes_full_shape() {
        let raw = json!({
            "outcome": "Confirmed",
            "predicted": { "anything": 1 },
            "observed": { "anything": 2 },
            "containment": {
                "predictedSubsetObserved": true,
                "observedSubsetPredicted": false,
                "activeNegation": false,
                "coverage": 0.875
            },
            "cause": "causal",
            "durationMs": 42
        });
        let view: EffectVerificationView = serde_json::from_value(raw).expect("deserialize");
        assert_eq!(view.outcome, "Confirmed");
        assert_eq!(view.cause, "causal");
        assert_eq!(view.duration_ms, 42);
        assert!(view.containment.predicted_subset_observed);
        assert!(!view.containment.observed_subset_predicted);
        assert!(!view.containment.active_negation);
        assert_eq!(view.containment.coverage, 0.875);
    }

    #[test]
    fn config_deserializes_camel_and_snake_case() {
        // camelCase
        let camel: ExecutionStepConfig = serde_json::from_value(json!({
            "type": "effect_check",
            "effectCheckElementId": "button-submit-0",
            "effectCheckAction": "click",
            "effectCheckParams": { "value": "hi" },
            "effectCheckExpectedOutcome": "Confirmed"
        }))
        .unwrap();
        assert_eq!(
            camel.effect_check_element_id.as_deref(),
            Some("button-submit-0")
        );
        assert_eq!(camel.effect_check_action.as_deref(), Some("click"));
        assert!(camel.effect_check_params.is_some());
        assert_eq!(
            camel.effect_check_expected_outcome.as_deref(),
            Some("Confirmed")
        );

        // snake_case
        let snake: ExecutionStepConfig = serde_json::from_value(json!({
            "type": "effect_check",
            "effect_check_element_id": "input-name-1",
            "effect_check_action": "type"
        }))
        .unwrap();
        assert_eq!(
            snake.effect_check_element_id.as_deref(),
            Some("input-name-1")
        );
        assert_eq!(snake.effect_check_action.as_deref(), Some("type"));
        assert!(snake.effect_check_expected_outcome.is_none());
    }

    #[test]
    fn config_default_has_all_effect_check_fields_none() {
        let d = ExecutionStepConfig::default();
        assert!(d.effect_check_element_id.is_none());
        assert!(d.effect_check_action.is_none());
        assert!(d.effect_check_params.is_none());
        assert!(d.effect_check_expected_outcome.is_none());
    }
}
