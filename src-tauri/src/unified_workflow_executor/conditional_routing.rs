//! Conditional routing engine for per-phase model selection.
//!
//! Evaluates routing rules against runtime context (iteration count,
//! verification failures, stage index) to dynamically select models.
//! Rules are evaluated in order; the first matching rule wins.

use tracing::debug;

use crate::unified_workflows::{ModelOverrideConfig, RoutingRule};

/// Runtime context available for routing rule evaluation.
#[derive(Debug, Clone, Default)]
pub struct RoutingContext {
    /// Current iteration number (1-indexed).
    pub iteration: u32,
    /// Number of verification failures so far (across all stages).
    pub verification_failures: u32,
    /// Current stage index (0-indexed), if running a multi-stage workflow.
    pub stage_index: Option<u32>,
}

/// Resolved model configuration after evaluating routing rules.
#[derive(Debug, Clone)]
pub struct ResolvedModelConfig {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

impl From<&ModelOverrideConfig> for ResolvedModelConfig {
    fn from(config: &ModelOverrideConfig) -> Self {
        Self {
            model: config.model.clone(),
            provider: config.provider.clone(),
            temperature: config.temperature,
            max_tokens: config.max_tokens,
        }
    }
}

/// Evaluate routing rules against the given context.
///
/// Returns a `ResolvedModelConfig` from the first matching rule,
/// or falls back to the static fields on `default` if no rules match.
pub fn evaluate_routing_rules(
    rules: &[RoutingRule],
    ctx: &RoutingContext,
    default: &ModelOverrideConfig,
) -> ResolvedModelConfig {
    for rule in rules {
        if evaluate_condition(&rule.condition, ctx) {
            debug!(
                "Routing rule matched: '{}' → model={:?}, provider={:?}",
                rule.condition, rule.model, rule.provider
            );
            return ResolvedModelConfig {
                model: rule.model.clone().or_else(|| default.model.clone()),
                provider: rule.provider.clone().or_else(|| default.provider.clone()),
                temperature: rule.temperature.or(default.temperature),
                max_tokens: rule.max_tokens.or(default.max_tokens),
            };
        }
    }

    // No rule matched — use the static defaults
    ResolvedModelConfig::from(default)
}

/// Parse and evaluate a simple condition expression.
///
/// Syntax: `"<variable> <op> <value>"`
///
/// Variables: `verification_failures`, `iteration`, `stage_index`
/// Operators: `>=`, `>`, `<=`, `<`, `==`, `!=`
fn evaluate_condition(condition: &str, ctx: &RoutingContext) -> bool {
    let parts: Vec<&str> = condition.split_whitespace().collect();
    if parts.len() != 3 {
        debug!(
            "Invalid routing condition (expected 3 parts): '{}'",
            condition
        );
        return false;
    }

    let variable = parts[0];
    let operator = parts[1];
    let value_str = parts[2];

    let value: u32 = match value_str.parse() {
        Ok(v) => v,
        Err(_) => {
            debug!(
                "Invalid routing condition value (expected u32): '{}'",
                value_str
            );
            return false;
        }
    };

    let actual = match variable {
        "verification_failures" => ctx.verification_failures,
        "iteration" => ctx.iteration,
        "stage_index" => ctx.stage_index.unwrap_or(0),
        _ => {
            debug!("Unknown routing condition variable: '{}'", variable);
            return false;
        }
    };

    match operator {
        ">=" => actual >= value,
        ">" => actual > value,
        "<=" => actual <= value,
        "<" => actual < value,
        "==" => actual == value,
        "!=" => actual != value,
        _ => {
            debug!("Unknown routing condition operator: '{}'", operator);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evaluate_condition_verification_failures() {
        let ctx = RoutingContext {
            iteration: 3,
            verification_failures: 2,
            stage_index: Some(1),
        };

        assert!(evaluate_condition("verification_failures >= 2", &ctx));
        assert!(!evaluate_condition("verification_failures >= 3", &ctx));
        assert!(evaluate_condition("verification_failures == 2", &ctx));
        assert!(evaluate_condition("verification_failures != 0", &ctx));
        assert!(evaluate_condition("iteration > 2", &ctx));
        assert!(!evaluate_condition("iteration > 3", &ctx));
        assert!(evaluate_condition("stage_index == 1", &ctx));
    }

    #[test]
    fn test_evaluate_condition_invalid() {
        let ctx = RoutingContext::default();
        assert!(!evaluate_condition("bad", &ctx));
        assert!(!evaluate_condition("unknown_var >= 1", &ctx));
        assert!(!evaluate_condition("iteration badop 1", &ctx));
        assert!(!evaluate_condition("iteration >= not_a_number", &ctx));
    }

    #[test]
    fn test_evaluate_routing_rules_first_match_wins() {
        let rules = vec![
            RoutingRule {
                condition: "verification_failures >= 3".to_string(),
                model: Some("claude-opus-4-20250514".to_string()),
                provider: None,
                temperature: None,
                max_tokens: None,
            },
            RoutingRule {
                condition: "verification_failures >= 1".to_string(),
                model: Some("claude-sonnet-4-20250514".to_string()),
                provider: None,
                temperature: Some(0.5),
                max_tokens: None,
            },
        ];
        let default = ModelOverrideConfig::default();

        let ctx = RoutingContext {
            verification_failures: 2,
            ..Default::default()
        };
        let resolved = evaluate_routing_rules(&rules, &ctx, &default);
        assert_eq!(resolved.model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(resolved.temperature, Some(0.5));

        let ctx = RoutingContext {
            verification_failures: 3,
            ..Default::default()
        };
        let resolved = evaluate_routing_rules(&rules, &ctx, &default);
        assert_eq!(resolved.model.as_deref(), Some("claude-opus-4-20250514"));
    }

    #[test]
    fn test_evaluate_routing_rules_no_match_uses_default() {
        let rules = vec![RoutingRule {
            condition: "verification_failures >= 10".to_string(),
            model: Some("claude-opus-4-20250514".to_string()),
            provider: None,
            temperature: None,
            max_tokens: None,
        }];
        let default = ModelOverrideConfig {
            model: Some("claude-sonnet-4-20250514".to_string()),
            temperature: Some(0.3),
            ..Default::default()
        };

        let ctx = RoutingContext::default();
        let resolved = evaluate_routing_rules(&rules, &ctx, &default);
        assert_eq!(resolved.model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(resolved.temperature, Some(0.3));
    }
}
