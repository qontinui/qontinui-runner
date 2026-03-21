//! Step Efficiency metric — deterministic scorer.
//!
//! Evaluates execution efficiency by comparing actual iterations/steps against
//! a baseline expectation. Penalizes unnecessary actions while recognizing that
//! harder tasks legitimately need more iterations.
//!
//! Key design decision: zero-iteration runs (startup crashes) score **0.0**,
//! cleanly separating infrastructure failures from task-level performance.
//! This directly addresses the "96% of failures are zero-iteration" issue.
//!
//! Formula: `max(0.0, 1.0 - (actual - baseline) / baseline)`
//!
//! - At or under baseline → 1.0 (maximally efficient)
//! - 2× baseline → 0.0 (twice as many steps as expected)
//! - Zero iterations → 0.0 (startup crash, no work done)

use super::{AgenticMetric, DeterministicInput, MetricResult};

/// Default baseline iterations when no learned baseline is available.
///
/// Based on observed data: max_iter=2 cohort achieves 100% success at
/// avg 573s — this is the cost/quality sweet spot.
const DEFAULT_BASELINE_ITERATIONS: f64 = 2.0;

/// Default baseline step count when no learned baseline is available.
const DEFAULT_BASELINE_STEPS: f64 = 8.0;

/// A learned baseline for a specific workflow or global default.
#[derive(Debug, Clone)]
pub struct StepBaseline {
    /// Expected iterations for successful completion (P25 of historical successes).
    pub expected_iterations: f64,
    /// Expected total step count (P25 of historical successes).
    pub expected_steps: f64,
}

impl Default for StepBaseline {
    fn default() -> Self {
        Self {
            expected_iterations: DEFAULT_BASELINE_ITERATIONS,
            expected_steps: DEFAULT_BASELINE_STEPS,
        }
    }
}

/// Compute the step efficiency score for a workflow run.
///
/// `baseline`: optional learned baseline; uses defaults if None.
pub fn score(input: &DeterministicInput, baseline: Option<&StepBaseline>) -> MetricResult {
    let default_baseline = StepBaseline::default();
    let baseline = baseline.unwrap_or(&default_baseline);

    let (score_val, rationale) = compute(input, baseline);

    MetricResult {
        metric: AgenticMetric::StepEfficiency,
        score: score_val,
        confidence: confidence(input, baseline),
        rationale,
        is_llm_judged: false,
        model_used: None,
    }
}

fn compute(input: &DeterministicInput, baseline: &StepBaseline) -> (f64, String) {
    // Zero-iteration crash — no work attempted
    if input.iterations == 0 {
        return (
            0.0,
            "Zero iterations — startup crash, no steps executed".into(),
        );
    }

    // Compute iteration efficiency
    let actual_iterations = input.iterations as f64;
    let iter_efficiency = efficiency_ratio(actual_iterations, baseline.expected_iterations);

    // If we have step count data, blend iteration and step efficiency
    if let Some(step_count) = input.step_count {
        let actual_steps = step_count as f64;
        let step_efficiency = efficiency_ratio(actual_steps, baseline.expected_steps);

        // Weight: 60% iteration efficiency, 40% step efficiency
        // Iterations are the stronger signal (they represent full agentic cycles)
        let blended = iter_efficiency * 0.6 + step_efficiency * 0.4;

        return (
            blended,
            format!(
                "Iteration efficiency: {:.2} ({} actual vs {:.0} baseline), \
                 step efficiency: {:.2} ({} actual vs {:.0} baseline), \
                 blended: {:.2}",
                iter_efficiency,
                input.iterations,
                baseline.expected_iterations,
                step_efficiency,
                step_count,
                baseline.expected_steps,
                blended,
            ),
        );
    }

    // Iteration-only scoring
    (
        iter_efficiency,
        format!(
            "Iteration efficiency: {:.2} ({} actual vs {:.0} baseline)",
            iter_efficiency, input.iterations, baseline.expected_iterations,
        ),
    )
}

/// Compute efficiency ratio: `max(0.0, 1.0 - (actual - expected) / expected)`.
///
/// Returns 1.0 when actual <= expected (at or better than baseline).
/// Returns 0.0 when actual >= 2 * expected (twice the baseline).
fn efficiency_ratio(actual: f64, expected: f64) -> f64 {
    if expected <= 0.0 {
        return if actual <= 0.0 { 1.0 } else { 0.0 };
    }
    (1.0 - (actual - expected) / expected).clamp(0.0, 1.0)
}

fn confidence(input: &DeterministicInput, baseline: &StepBaseline) -> f64 {
    if input.iterations == 0 {
        return 0.95; // Very confident about zero-iteration classification
    }

    // Confidence is higher when we have both iteration and step data
    let base_confidence = if input.step_count.is_some() {
        0.85
    } else {
        0.7
    };

    // Confidence is lower when using default baselines vs learned ones
    let baseline_penalty = if baseline.expected_iterations == DEFAULT_BASELINE_ITERATIONS {
        0.0 // Using defaults — less confident
    } else {
        0.1 // Using learned baselines — more confident
    };

    base_confidence + baseline_penalty
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_input(iterations: u32, step_count: Option<i64>) -> DeterministicInput {
        DeterministicInput {
            task_run_id: "test-1".to_string(),
            status: "success".to_string(),
            verification_passed: true,
            was_stopped: false,
            iterations,
            step_count,
            verification_step_count: None,
            agentic_step_count: None,
            max_iterations_reached: false,
            duration_secs: 100.0,
            error_type: None,
        }
    }

    #[test]
    fn test_zero_iterations() {
        let result = score(&make_input(0, None), None);
        assert_eq!(result.score, 0.0);
        assert!(result.rationale.contains("Zero iterations"));
    }

    #[test]
    fn test_at_baseline() {
        // 2 iterations with default baseline of 2.0 → efficiency 1.0
        let result = score(&make_input(2, None), None);
        assert_eq!(result.score, 1.0);
    }

    #[test]
    fn test_under_baseline() {
        // 1 iteration with default baseline of 2.0 → clamped to 1.0
        let result = score(&make_input(1, None), None);
        assert_eq!(result.score, 1.0);
    }

    #[test]
    fn test_over_baseline() {
        // 3 iterations with default baseline of 2.0 → 1.0 - (3-2)/2 = 0.5
        let result = score(&make_input(3, None), None);
        assert!((result.score - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_double_baseline() {
        // 4 iterations with default baseline of 2.0 → 1.0 - (4-2)/2 = 0.0
        let result = score(&make_input(4, None), None);
        assert_eq!(result.score, 0.0);
    }

    #[test]
    fn test_with_step_count() {
        // 2 iterations (at baseline) + 8 steps (at baseline) → blended 1.0
        let result = score(&make_input(2, Some(8)), None);
        assert!((result.score - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_with_step_count_over() {
        // 3 iterations (0.5 efficiency) + 16 steps (0.0 efficiency)
        // blended: 0.5 * 0.6 + 0.0 * 0.4 = 0.3
        let result = score(&make_input(3, Some(16)), None);
        assert!((result.score - 0.3).abs() < 0.01);
    }

    #[test]
    fn test_custom_baseline() {
        let baseline = StepBaseline {
            expected_iterations: 1.0,
            expected_steps: 4.0,
        };
        // 1 iteration at baseline → 1.0
        let result = score(&make_input(1, None), Some(&baseline));
        assert_eq!(result.score, 1.0);
    }

    #[test]
    fn test_efficiency_ratio_edge_cases() {
        assert_eq!(efficiency_ratio(0.0, 0.0), 1.0);
        assert_eq!(efficiency_ratio(5.0, 0.0), 0.0);
        assert_eq!(efficiency_ratio(0.0, 5.0), 1.0); // Better than expected
    }
}
