//! Schema Compliance metric — deterministic scorer.
//!
//! Evaluates how well agent outputs conform to their expected JSON Schema
//! on the first attempt, tracking validation retries and coercions needed.
//!
//! Scoring:
//! - 1.0 — Valid on first attempt, no coercions
//! - 0.8 — Valid on first attempt with coercions
//! - 0.5 — Valid after 1 retry
//! - 0.2 — Valid after 2 retries
//! - 0.1 — Valid after 3+ retries
//! - 0.0 — Never valid / exhausted retries

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{AgenticMetric, MetricResult};

/// Input data for computing schema compliance score.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SchemaComplianceInput {
    /// Whether the output was valid on first attempt.
    pub valid_first_attempt: bool,
    /// Number of validation retries needed.
    pub validation_retries: u32,
    /// Number of coercions applied.
    pub coercions_count: u32,
    /// Whether the output was eventually valid.
    pub eventually_valid: bool,
}

/// Compute the schema compliance score.
///
/// Scoring:
/// - 1.0: Valid on first attempt, no coercions
/// - 0.8: Valid on first attempt with coercions
/// - 0.5: Valid after 1 retry
/// - 0.2: Valid after 2 retries
/// - 0.1: Valid after 3+ retries
/// - 0.0: Never valid / exhausted retries
pub fn compute_schema_compliance_score(input: &SchemaComplianceInput) -> f64 {
    if !input.eventually_valid {
        return 0.0;
    }
    if input.valid_first_attempt {
        if input.coercions_count == 0 {
            1.0
        } else {
            0.8
        }
    } else {
        match input.validation_retries {
            1 => 0.5,
            2 => 0.2,
            _ => 0.1,
        }
    }
}

/// Compute the schema compliance MetricResult from per-trace compliance inputs.
///
/// If no compliance data is available, returns a neutral score with low confidence.
pub fn score(inputs: &[SchemaComplianceInput]) -> MetricResult {
    if inputs.is_empty() {
        return MetricResult {
            metric: AgenticMetric::SchemaCompliance,
            score: 0.5,
            confidence: 0.1,
            rationale: "No schema compliance data available — neutral score".into(),
            is_llm_judged: false,
            model_used: None,
        };
    }

    let scores: Vec<f64> = inputs
        .iter()
        .map(|i| compute_schema_compliance_score(i))
        .collect();

    let avg_score = scores.iter().sum::<f64>() / scores.len() as f64;
    let confidence = 0.7 + (inputs.len().min(6) as f64 * 0.05); // More data = more confidence

    let first_attempt_count = inputs.iter().filter(|i| i.valid_first_attempt).count();
    let eventually_valid_count = inputs.iter().filter(|i| i.eventually_valid).count();
    let total_coercions: u32 = inputs.iter().map(|i| i.coercions_count).sum();
    let total_retries: u32 = inputs.iter().map(|i| i.validation_retries).sum();

    let rationale = format!(
        "{}/{} valid first attempt, {}/{} eventually valid, {} total coercions, {} total retries across {} traces",
        first_attempt_count,
        inputs.len(),
        eventually_valid_count,
        inputs.len(),
        total_coercions,
        total_retries,
        inputs.len(),
    );

    MetricResult {
        metric: AgenticMetric::SchemaCompliance,
        score: avg_score,
        confidence: confidence.min(1.0),
        rationale,
        is_llm_judged: false,
        model_used: None,
    }
}

/// Extract `SchemaComplianceInput` from a `PipelineAgentTrace`'s schema fields.
///
/// Returns `None` if the trace has no schema validation data (i.e., `schema_valid_first_attempt` is None).
pub fn compliance_input_from_trace(
    trace: &crate::autoresearch::agentic_verification::PipelineAgentTrace,
) -> Option<SchemaComplianceInput> {
    let valid_first = trace.schema_valid_first_attempt?;
    let retries = trace.validation_retries.unwrap_or(0).max(0) as u32;
    let coercions_count: u32 = trace
        .coercions_applied
        .as_ref()
        .and_then(|j| serde_json::from_str::<Vec<serde_json::Value>>(j).ok())
        .map(|v| v.len() as u32)
        .unwrap_or(0);
    let eventually_valid = valid_first || (retries > 0 && trace.validation_error_summary.is_none());

    Some(SchemaComplianceInput {
        valid_first_attempt: valid_first,
        validation_retries: retries,
        coercions_count,
        eventually_valid,
    })
}

/// Extract `SchemaComplianceInput` from all traces that have schema validation data.
pub fn compliance_inputs_from_traces(
    traces: &[crate::autoresearch::agentic_verification::PipelineAgentTrace],
) -> Vec<SchemaComplianceInput> {
    traces
        .iter()
        .filter_map(compliance_input_from_trace)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_perfect_first_attempt_no_coercions() {
        let input = SchemaComplianceInput {
            valid_first_attempt: true,
            validation_retries: 0,
            coercions_count: 0,
            eventually_valid: true,
        };
        assert_eq!(compute_schema_compliance_score(&input), 1.0);
    }

    #[test]
    fn test_first_attempt_with_coercions() {
        let input = SchemaComplianceInput {
            valid_first_attempt: true,
            validation_retries: 0,
            coercions_count: 3,
            eventually_valid: true,
        };
        assert_eq!(compute_schema_compliance_score(&input), 0.8);
    }

    #[test]
    fn test_valid_after_one_retry() {
        let input = SchemaComplianceInput {
            valid_first_attempt: false,
            validation_retries: 1,
            coercions_count: 0,
            eventually_valid: true,
        };
        assert_eq!(compute_schema_compliance_score(&input), 0.5);
    }

    #[test]
    fn test_valid_after_two_retries() {
        let input = SchemaComplianceInput {
            valid_first_attempt: false,
            validation_retries: 2,
            coercions_count: 0,
            eventually_valid: true,
        };
        assert_eq!(compute_schema_compliance_score(&input), 0.2);
    }

    #[test]
    fn test_valid_after_many_retries() {
        let input = SchemaComplianceInput {
            valid_first_attempt: false,
            validation_retries: 5,
            coercions_count: 0,
            eventually_valid: true,
        };
        assert_eq!(compute_schema_compliance_score(&input), 0.1);
    }

    #[test]
    fn test_never_valid() {
        let input = SchemaComplianceInput {
            valid_first_attempt: false,
            validation_retries: 3,
            coercions_count: 0,
            eventually_valid: false,
        };
        assert_eq!(compute_schema_compliance_score(&input), 0.0);
    }

    #[test]
    fn test_score_empty_inputs() {
        let result = score(&[]);
        assert_eq!(result.metric, AgenticMetric::SchemaCompliance);
        assert_eq!(result.score, 0.5);
        assert!(result.confidence < 0.5);
    }

    #[test]
    fn test_score_multiple_inputs() {
        let inputs = vec![
            SchemaComplianceInput {
                valid_first_attempt: true,
                validation_retries: 0,
                coercions_count: 0,
                eventually_valid: true,
            },
            SchemaComplianceInput {
                valid_first_attempt: false,
                validation_retries: 1,
                coercions_count: 2,
                eventually_valid: true,
            },
        ];
        let result = score(&inputs);
        assert_eq!(result.metric, AgenticMetric::SchemaCompliance);
        // (1.0 + 0.5) / 2 = 0.75
        assert!((result.score - 0.75).abs() < 0.01);
    }

    #[test]
    fn test_score_all_failures() {
        let inputs = vec![
            SchemaComplianceInput {
                valid_first_attempt: false,
                validation_retries: 3,
                coercions_count: 0,
                eventually_valid: false,
            },
            SchemaComplianceInput {
                valid_first_attempt: false,
                validation_retries: 2,
                coercions_count: 0,
                eventually_valid: false,
            },
        ];
        let result = score(&inputs);
        assert_eq!(result.score, 0.0);
    }
}
