//! Attribution-Driven Credit Assignment (ADCA) for step-level scoring.
//!
//! Instead of scoring steps independently, this module traces the causal
//! contribution of each intermediate step to the final workflow outcome.
//!
//! Five attribution signals are combined per step:
//! 1. **Temporal proximity** — Steps closer to the outcome get more credit
//! 2. **Output utilization** — Steps whose output was consumed downstream
//! 3. **Confidence delta** — Steps that improved verification confidence
//! 4. **Downstream success** — Steps followed by successful continuations
//! 5. **Cost efficiency** — High cost without proportional impact reduces credit
//!
//! Inspired by AgentEvolver's ADCA approach (modelscope/AgentEvolver).

use serde::{Deserialize, Serialize};

// =============================================================================
// Input types
// =============================================================================

/// Step-level attribution data collected during execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepAttribution {
    /// Index of the step in the workflow execution sequence.
    pub step_index: usize,
    /// Type of step (e.g., "generate_code", "verify", "implement").
    pub step_type: String,
    /// Agent that executed this step.
    pub agent_type: String,
    /// Execution duration in milliseconds.
    pub duration_ms: u64,
    /// Total tokens used by this step.
    pub tokens_used: u64,
    /// Cost in USD for this step.
    pub cost_usd: f64,
    /// Did this step's output pass downstream validation?
    /// From `pipeline_agent_traces.downstream_success`.
    pub downstream_success: Option<bool>,
    /// How much did verification confidence change after this step?
    /// Positive = improved, negative = degraded.
    pub confidence_delta: Option<f64>,
    /// What fraction of this step's output was used by subsequent steps?
    /// Derived from handoff context analysis (0.0 = unused, 1.0 = fully consumed).
    pub output_utilization: f64,
    /// Magnitude of state changes (files modified, lines changed).
    pub state_change_magnitude: f64,
}

// =============================================================================
// Output types
// =============================================================================

/// Credit assignment result for a single step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepCredit {
    /// Step index in the execution sequence.
    pub step_index: usize,
    /// Step type.
    pub step_type: String,
    /// Agent that executed this step.
    pub agent_type: String,
    /// Raw (un-normalized) credit score.
    pub raw_credit: f64,
    /// Normalized credit (sum across all steps = final_outcome_score).
    pub normalized_credit: f64,
    /// Breakdown of individual attribution signals.
    pub signals: AttributionSignals,
}

/// Breakdown of the individual signals that contributed to a step's credit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributionSignals {
    /// Temporal proximity weight (exponential decay from end).
    pub temporal_proximity: f64,
    /// Output utilization weight.
    pub output_utilization: f64,
    /// Confidence delta weight.
    pub confidence_delta: f64,
    /// Downstream success weight.
    pub downstream_success: f64,
    /// Cost efficiency weight.
    pub cost_efficiency: f64,
}

// =============================================================================
// Credit computation
// =============================================================================

/// Compute credit assignments for all steps in a completed workflow.
///
/// Credits are computed multiplicatively from five signals and then normalized
/// so that the sum of all normalized_credit values equals `final_outcome_score`.
///
/// # Arguments
/// - `steps`: Step-level attribution data in execution order.
/// - `final_outcome_score`: The composite agentic score of the workflow (0.0-1.0).
/// - `final_success`: Whether the overall workflow succeeded.
///
/// # Returns
/// A `StepCredit` for each input step, or an empty vec if no steps.
pub fn compute_step_credits(
    steps: &[StepAttribution],
    final_outcome_score: f64,
    _final_success: bool,
) -> Vec<StepCredit> {
    if steps.is_empty() {
        return Vec::new();
    }

    let n = steps.len() as f64;
    let total_cost: f64 = steps.iter().map(|s| s.cost_usd).sum();
    let avg_cost = if total_cost > 0.0 { total_cost / n } else { 0.01 };

    let mut credits: Vec<StepCredit> = steps
        .iter()
        .enumerate()
        .map(|(i, step)| {
            // 1. Temporal proximity: exponential decay from the final step
            // Steps closer to the end (and thus the outcome) get more credit
            let distance_from_end = (n - 1.0 - i as f64) / n.max(1.0);
            let temporal = (-0.5 * distance_from_end).exp();

            // 2. Output utilization: steps whose output was consumed downstream
            let utilization = 0.3 + 0.7 * step.output_utilization;

            // 3. Confidence delta: steps that improved verification confidence
            let confidence = step
                .confidence_delta
                .map(|d| {
                    if d > 0.0 {
                        1.0 + d.min(0.5) // Positive contribution, capped
                    } else {
                        (0.5 + d).max(0.1) // Negative contribution, floored
                    }
                })
                .unwrap_or(1.0); // Neutral if no confidence data

            // 4. Downstream success: steps followed by successful continuations
            let downstream = match step.downstream_success {
                Some(true) => 1.2,
                Some(false) => 0.6,
                None => 1.0, // Neutral if unknown
            };

            // 5. Cost efficiency: penalize steps that consumed disproportionate resources
            // without proportional state change or confidence improvement
            let relative_cost = step.cost_usd / avg_cost;
            let relative_impact = step.state_change_magnitude.max(0.1)
                + step.confidence_delta.unwrap_or(0.0).max(0.0);
            let cost_eff = if relative_cost > 2.0 && relative_impact < 0.5 {
                0.7 // Expensive step with low impact
            } else if relative_cost < 0.5 && relative_impact > 0.5 {
                1.3 // Cheap step with high impact
            } else {
                1.0 // Normal
            };

            // Combine signals multiplicatively
            let raw_credit = temporal * utilization * confidence * downstream * cost_eff;

            StepCredit {
                step_index: step.step_index,
                step_type: step.step_type.clone(),
                agent_type: step.agent_type.clone(),
                raw_credit,
                normalized_credit: 0.0, // Set after normalization
                signals: AttributionSignals {
                    temporal_proximity: temporal,
                    output_utilization: utilization,
                    confidence_delta: confidence,
                    downstream_success: downstream,
                    cost_efficiency: cost_eff,
                },
            }
        })
        .collect();

    // Normalize so sum of credits = final_outcome_score
    let total_raw: f64 = credits.iter().map(|c| c.raw_credit).sum();
    if total_raw > 0.0 {
        for credit in &mut credits {
            credit.normalized_credit = (credit.raw_credit / total_raw) * final_outcome_score;
        }
    }

    credits
}

/// Compute a credit-weighted composite score.
///
/// Instead of uniformly weighting all dimension scores, this uses step credit
/// data to weight dimensions based on the causal contribution of their associated
/// steps. Falls back to uniform weighting if credit data is unavailable.
///
/// This replaces the uniform `composite_score()` in agentic_metrics when
/// credit data is available.
pub fn credit_weighted_composite(
    dimension_scores: &[(String, f64, f64)], // (metric_type, score, weight)
    step_credits: Option<&[StepCredit]>,
) -> f64 {
    // If no credit data, fall back to standard weighted average
    let credits = match step_credits {
        Some(c) if !c.is_empty() => c,
        _ => {
            let total_weight: f64 = dimension_scores.iter().map(|(_, _, w)| w).sum();
            if total_weight == 0.0 {
                return 0.0;
            }
            return dimension_scores
                .iter()
                .map(|(_, score, weight)| score * weight)
                .sum::<f64>()
                / total_weight;
        }
    };

    // Compute per-step-type average credit for weighting
    let mut step_type_credit: std::collections::HashMap<String, (f64, u32)> =
        std::collections::HashMap::new();
    for credit in credits {
        let entry = step_type_credit
            .entry(credit.step_type.clone())
            .or_insert((0.0, 0));
        entry.0 += credit.normalized_credit;
        entry.1 += 1;
    }

    // For now, use credits as a modulating factor on the existing weights
    // Steps with higher credit get their associated metrics weighted more
    let total_credit: f64 = credits.iter().map(|c| c.normalized_credit).sum();
    let credit_factor = if total_credit > 0.0 {
        total_credit
    } else {
        1.0
    };

    // Apply a blended approach: 70% original weight + 30% credit-adjusted
    let total_weight: f64 = dimension_scores.iter().map(|(_, _, w)| w).sum();
    if total_weight == 0.0 {
        return 0.0;
    }

    let base_score: f64 = dimension_scores
        .iter()
        .map(|(_, score, weight)| score * weight)
        .sum::<f64>()
        / total_weight;

    // Credit adjustment: if high-credit steps have low scores, the overall
    // score should reflect that more than a uniform weighting would suggest
    let credit_adjusted = base_score * (0.7 + 0.3 * credit_factor.min(1.0));

    credit_adjusted.clamp(0.0, 1.0)
}

// =============================================================================
// Aggregation utilities
// =============================================================================

/// Aggregate step credits by step type for scorecard reporting.
///
/// Returns: (step_type, avg_credit, total_count, success_weighted_credit)
pub fn aggregate_by_step_type(credits: &[StepCredit]) -> Vec<StepTypeScorecard> {
    let mut map: std::collections::HashMap<String, (f64, u32, f64)> =
        std::collections::HashMap::new();

    for credit in credits {
        let entry = map
            .entry(credit.step_type.clone())
            .or_insert((0.0, 0, 0.0));
        entry.0 += credit.normalized_credit;
        entry.1 += 1;
        entry.2 += credit.raw_credit;
    }

    let mut result: Vec<StepTypeScorecard> = map
        .into_iter()
        .map(|(step_type, (total_credit, count, total_raw))| StepTypeScorecard {
            step_type,
            avg_normalized_credit: total_credit / count as f64,
            total_count: count,
            avg_raw_credit: total_raw / count as f64,
        })
        .collect();

    result.sort_by(|a, b| {
        b.avg_normalized_credit
            .partial_cmp(&a.avg_normalized_credit)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    result
}

/// Step type performance scorecard entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepTypeScorecard {
    pub step_type: String,
    pub avg_normalized_credit: f64,
    pub total_count: u32,
    pub avg_raw_credit: f64,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_step(
        index: usize,
        step_type: &str,
        downstream_success: Option<bool>,
        confidence_delta: Option<f64>,
        output_utilization: f64,
        cost: f64,
    ) -> StepAttribution {
        StepAttribution {
            step_index: index,
            step_type: step_type.to_string(),
            agent_type: "test_agent".to_string(),
            duration_ms: 1000,
            tokens_used: 500,
            cost_usd: cost,
            downstream_success,
            confidence_delta,
            output_utilization,
            state_change_magnitude: 1.0,
        }
    }

    #[test]
    fn test_empty_steps() {
        let credits = compute_step_credits(&[], 0.85, true);
        assert!(credits.is_empty());
    }

    #[test]
    fn test_single_step() {
        let steps = vec![make_step(0, "generate", Some(true), Some(0.5), 1.0, 0.10)];
        let credits = compute_step_credits(&steps, 0.85, true);
        assert_eq!(credits.len(), 1);
        // Single step gets all the credit
        assert!((credits[0].normalized_credit - 0.85).abs() < 0.01);
    }

    #[test]
    fn test_later_steps_get_more_temporal_credit() {
        let steps = vec![
            make_step(0, "plan", None, None, 0.5, 0.05),
            make_step(1, "generate", None, None, 0.5, 0.05),
            make_step(2, "verify", None, None, 0.5, 0.05),
        ];
        let credits = compute_step_credits(&steps, 0.90, true);
        assert_eq!(credits.len(), 3);

        // Verify step (last) should have highest temporal credit
        assert!(credits[2].signals.temporal_proximity > credits[0].signals.temporal_proximity);
    }

    #[test]
    fn test_credits_sum_to_outcome_score() {
        let steps = vec![
            make_step(0, "plan", Some(true), Some(0.1), 0.8, 0.05),
            make_step(1, "generate", Some(true), Some(0.3), 0.9, 0.10),
            make_step(2, "verify", Some(true), Some(0.2), 0.5, 0.03),
        ];
        let outcome_score = 0.85;
        let credits = compute_step_credits(&steps, outcome_score, true);

        let total_normalized: f64 = credits.iter().map(|c| c.normalized_credit).sum();
        assert!(
            (total_normalized - outcome_score).abs() < 0.001,
            "Credits should sum to outcome score: {} vs {}",
            total_normalized,
            outcome_score
        );
    }

    #[test]
    fn test_downstream_failure_reduces_credit() {
        let steps = vec![
            make_step(0, "generate", Some(false), None, 0.5, 0.05), // Failed downstream
            make_step(1, "generate", Some(true), None, 0.5, 0.05),  // Succeeded downstream
        ];
        let credits = compute_step_credits(&steps, 0.80, false);

        // Step with downstream failure should have lower credit signal
        assert!(credits[0].signals.downstream_success < credits[1].signals.downstream_success);
    }

    #[test]
    fn test_confidence_improvement_boosts_credit() {
        let steps = vec![
            make_step(0, "generate", None, Some(-0.2), 0.5, 0.05), // Reduced confidence
            make_step(1, "verify", None, Some(0.4), 0.5, 0.05),    // Improved confidence
        ];
        let credits = compute_step_credits(&steps, 0.80, true);

        assert!(credits[1].signals.confidence_delta > credits[0].signals.confidence_delta);
    }

    #[test]
    fn test_aggregate_by_step_type() {
        let credits = vec![
            StepCredit {
                step_index: 0,
                step_type: "generate".to_string(),
                agent_type: "gen".to_string(),
                raw_credit: 0.4,
                normalized_credit: 0.3,
                signals: AttributionSignals {
                    temporal_proximity: 0.8,
                    output_utilization: 0.9,
                    confidence_delta: 1.0,
                    downstream_success: 1.2,
                    cost_efficiency: 1.0,
                },
            },
            StepCredit {
                step_index: 1,
                step_type: "generate".to_string(),
                agent_type: "gen".to_string(),
                raw_credit: 0.5,
                normalized_credit: 0.4,
                signals: AttributionSignals {
                    temporal_proximity: 0.9,
                    output_utilization: 0.9,
                    confidence_delta: 1.1,
                    downstream_success: 1.2,
                    cost_efficiency: 1.0,
                },
            },
            StepCredit {
                step_index: 2,
                step_type: "verify".to_string(),
                agent_type: "ver".to_string(),
                raw_credit: 0.3,
                normalized_credit: 0.15,
                signals: AttributionSignals {
                    temporal_proximity: 1.0,
                    output_utilization: 0.5,
                    confidence_delta: 1.0,
                    downstream_success: 1.0,
                    cost_efficiency: 1.0,
                },
            },
        ];

        let scorecards = aggregate_by_step_type(&credits);
        assert_eq!(scorecards.len(), 2);

        // "generate" should rank higher (avg 0.35 credit vs 0.15)
        assert_eq!(scorecards[0].step_type, "generate");
        assert_eq!(scorecards[0].total_count, 2);
    }

    #[test]
    fn test_credit_weighted_composite_fallback() {
        let dimensions = vec![
            ("task_completion".to_string(), 0.8, 0.25),
            ("step_efficiency".to_string(), 0.6, 0.12),
        ];
        let score = credit_weighted_composite(&dimensions, None);
        // Without credit data, should be uniform weighted average
        let expected = (0.8 * 0.25 + 0.6 * 0.12) / (0.25 + 0.12);
        assert!((score - expected).abs() < 0.01);
    }
}
