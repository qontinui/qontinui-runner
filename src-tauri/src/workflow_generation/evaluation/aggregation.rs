//! PURE Min-Form Aggregation
//!
//! Implements the key insight from CJReinforce/PURE: the quality of a
//! verification plan equals its weakest step. The overall score is dominated
//! by the minimum step-level score, weighted by criterion priority.

use crate::workflow_generation::specification::CriterionPriority;

use super::StepEvaluation;

/// Aggregate step evaluations into an overall workflow quality score.
///
/// Uses PURE min-form: trajectory reward = minimum step-level reward,
/// weighted by criterion priority.
pub fn aggregate_evaluation(step_evaluations: &[StepEvaluation]) -> f64 {
    if step_evaluations.is_empty() {
        return 0.0;
    }

    // Compute overall_min first — used as fallback for empty priority buckets
    let overall_min = step_evaluations
        .iter()
        .map(|e| e.min_score)
        .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(0.0);

    // For empty priority buckets, fall back to overall_min (not 1.0) to avoid
    // inflating scores when a workflow has no critical/important steps.
    let critical_min = step_evaluations
        .iter()
        .filter(|e| e.criterion_priority == Some(CriterionPriority::Critical))
        .map(|e| e.min_score)
        .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(overall_min);

    let important_min = step_evaluations
        .iter()
        .filter(|e| e.criterion_priority == Some(CriterionPriority::Important))
        .map(|e| e.min_score)
        .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(overall_min);

    // Critical floor dominates: if any critical step is below 0.5, that's the score
    if critical_min < 0.5 {
        return critical_min;
    }

    // Weighted combination: 60% critical min, 30% important min, 10% overall min
    (critical_min * 0.6 + important_min * 0.3 + overall_min * 0.1).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_generation::evaluation::{
        DimensionScore, EvaluationDimension, ScoringTier,
    };

    fn make_eval(min_score: f64, priority: Option<CriterionPriority>) -> StepEvaluation {
        StepEvaluation {
            step_id: "test".into(),
            step_name: "test step".into(),
            scores: vec![DimensionScore {
                dimension: EvaluationDimension::Entailment,
                score: min_score,
                confidence: 0.8,
                tier: ScoringTier::Deterministic,
                explanation: None,
                evidence: vec![],
            }],
            composite_score: min_score,
            min_score,
            flags: vec![],
            criterion_id: None,
            criterion_priority: priority,
        }
    }

    #[test]
    fn test_empty_evaluations() {
        assert_eq!(aggregate_evaluation(&[]), 0.0);
    }

    #[test]
    fn test_critical_floor() {
        let evals = vec![
            make_eval(0.3, Some(CriterionPriority::Critical)),
            make_eval(0.9, Some(CriterionPriority::Important)),
        ];
        assert_eq!(aggregate_evaluation(&evals), 0.3);
    }

    #[test]
    fn test_weighted_combination() {
        let evals = vec![
            make_eval(0.8, Some(CriterionPriority::Critical)),
            make_eval(0.6, Some(CriterionPriority::Important)),
            make_eval(0.4, None),
        ];
        let score = aggregate_evaluation(&evals);
        // 0.8 * 0.6 + 0.6 * 0.3 + 0.4 * 0.1 = 0.48 + 0.18 + 0.04 = 0.70
        assert!((score - 0.70).abs() < 0.01);
    }

    #[test]
    fn test_only_optional_steps_no_inflation() {
        // With only unmapped/optional steps, score should NOT inflate to 0.9+
        let evals = vec![
            make_eval(0.2, None),
            make_eval(0.3, None),
        ];
        let score = aggregate_evaluation(&evals);
        // overall_min = 0.2, critical_min = 0.2 (fallback), important_min = 0.2 (fallback)
        // 0.2 * 0.6 + 0.2 * 0.3 + 0.2 * 0.1 = 0.12 + 0.06 + 0.02 = 0.20
        assert!((score - 0.20).abs() < 0.01);
    }
}
