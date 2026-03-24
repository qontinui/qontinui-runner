//! Stall intervention strategies for the orchestration loop.
//!
//! When a stall is detected, this module builds prompts for the AI to suggest
//! an intervention, and parses the response into an actionable intervention.

use serde::{Deserialize, Serialize};
use tracing::info;

use super::stall_detector::StallPattern;

/// Action to take when a stall is detected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InterventionAction {
    RetryWithAlternativePrompt(String),
    SkipCurrentStep,
    ResetToCheckpoint(String),
    StopWithDiagnosis(String),
}

/// A stall intervention containing the detected pattern and suggested action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StallIntervention {
    pub pattern: String,
    pub alternative_strategy: String,
    pub suggested_action: InterventionAction,
}

/// Build a prompt for the AI to suggest an intervention given a stall pattern.
pub fn build_intervention_prompt(pattern: &StallPattern, recent_actions: &[String]) -> String {
    let actions_str = recent_actions.join("\n  - ");
    format!(
        "An automated workflow has stalled with the following pattern:\n\
        \n\
        Pattern: {}\n\
        \n\
        Recent actions:\n  - {}\n\
        \n\
        Suggest ONE alternative strategy to break out of this stall. \
        Be specific and actionable. Respond with a JSON object:\n\
        {{\n\
          \"strategy\": \"description of the alternative approach\",\n\
          \"action\": \"retry_alternative\" | \"skip\" | \"stop\"\n\
        }}",
        pattern, actions_str
    )
}

/// Parse the AI response into a StallIntervention.
pub fn parse_intervention_response(
    pattern: &StallPattern,
    response: &str,
) -> StallIntervention {
    // Try to parse JSON from response
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(response) {
        let strategy = parsed
            .get("strategy")
            .and_then(|v| v.as_str())
            .unwrap_or("No strategy provided")
            .to_string();

        let action = match parsed.get("action").and_then(|v| v.as_str()) {
            Some("skip") => InterventionAction::SkipCurrentStep,
            Some("stop") => InterventionAction::StopWithDiagnosis(strategy.clone()),
            _ => InterventionAction::RetryWithAlternativePrompt(strategy.clone()),
        };

        info!(
            "Stall intervention parsed: strategy='{}', action={:?}",
            strategy, action
        );

        StallIntervention {
            pattern: pattern.to_string(),
            alternative_strategy: strategy,
            suggested_action: action,
        }
    } else {
        // Fallback: use the raw response as the strategy
        StallIntervention {
            pattern: pattern.to_string(),
            alternative_strategy: response.to_string(),
            suggested_action: InterventionAction::StopWithDiagnosis(format!(
                "Stall detected: {}. AI suggestion: {}",
                pattern, response
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pattern() -> StallPattern {
        StallPattern::RepeatedIdenticalAction {
            action_signature: "click_button".to_string(),
            count: 5,
        }
    }

    #[test]
    fn build_intervention_prompt_includes_pattern_and_actions() {
        let pattern = test_pattern();
        let actions = vec!["click_button".to_string(), "wait".to_string()];
        let prompt = build_intervention_prompt(&pattern, &actions);
        assert!(prompt.contains("click_button"));
        assert!(prompt.contains("repeated"));
        assert!(prompt.contains("wait"));
        assert!(prompt.contains("alternative strategy"));
    }

    #[test]
    fn parse_intervention_response_valid_json_retry() {
        let pattern = test_pattern();
        let response = r#"{"strategy": "Try a different selector", "action": "retry_alternative"}"#;
        let intervention = parse_intervention_response(&pattern, response);
        assert_eq!(intervention.alternative_strategy, "Try a different selector");
        match intervention.suggested_action {
            InterventionAction::RetryWithAlternativePrompt(s) => {
                assert_eq!(s, "Try a different selector");
            }
            other => panic!("Expected RetryWithAlternativePrompt, got {:?}", other),
        }
    }

    #[test]
    fn parse_intervention_response_skip_action() {
        let pattern = test_pattern();
        let response = r#"{"strategy": "Skip this step", "action": "skip"}"#;
        let intervention = parse_intervention_response(&pattern, response);
        match intervention.suggested_action {
            InterventionAction::SkipCurrentStep => {}
            other => panic!("Expected SkipCurrentStep, got {:?}", other),
        }
    }

    #[test]
    fn parse_intervention_response_stop_action() {
        let pattern = test_pattern();
        let response = r#"{"strategy": "Cannot proceed", "action": "stop"}"#;
        let intervention = parse_intervention_response(&pattern, response);
        match intervention.suggested_action {
            InterventionAction::StopWithDiagnosis(s) => {
                assert_eq!(s, "Cannot proceed");
            }
            other => panic!("Expected StopWithDiagnosis, got {:?}", other),
        }
    }

    #[test]
    fn parse_intervention_response_invalid_json_falls_back() {
        let pattern = test_pattern();
        let response = "Just try something else entirely";
        let intervention = parse_intervention_response(&pattern, response);
        assert_eq!(intervention.alternative_strategy, response);
        match intervention.suggested_action {
            InterventionAction::StopWithDiagnosis(s) => {
                assert!(s.contains("Just try something else"));
                assert!(s.contains("Stall detected"));
            }
            other => panic!("Expected StopWithDiagnosis fallback, got {:?}", other),
        }
    }
}
