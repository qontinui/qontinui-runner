//! Task Completion metric — deterministic scorer.
//!
//! Scores how completely the task was accomplished based on the outcome status
//! and verification results. Unlike the binary pass/fail in `learning_outcomes`,
//! this produces a nuanced 0.0–1.0 score:
//!
//! - **1.0** — Verification passed, task completed successfully
//! - **0.7** — Stopped by user (partial) with some iterations completed
//! - **0.5** — Stopped by user before any real work
//! - **0.3** — Failed after attempting work (max iterations reached)
//! - **0.1** — Failed with a runtime/generation error
//! - **0.0** — Zero-iteration startup crash (no work attempted)

use super::{AgenticMetric, DeterministicInput, MetricResult};

/// Compute the task completion score for a workflow run.
pub fn score(input: &DeterministicInput) -> MetricResult {
    let (score, rationale) = compute(input);

    MetricResult {
        metric: AgenticMetric::TaskCompletion,
        score,
        confidence: confidence(input),
        rationale,
        is_llm_judged: false,
        model_used: None,
    }
}

fn compute(input: &DeterministicInput) -> (f64, String) {
    // Success — verification passed
    if input.verification_passed {
        return (
            1.0,
            "Verification passed, task completed successfully".into(),
        );
    }

    // Zero-iteration crash — no work was attempted
    if input.iterations == 0 {
        return (
            0.0,
            "Zero-iteration startup crash — no work attempted".into(),
        );
    }

    // Stopped by user (partial completion)
    if input.was_stopped {
        // Give more credit if more iterations were completed
        let partial_score = if input.iterations >= 2 { 0.7 } else { 0.5 };
        return (
            partial_score,
            format!(
                "Stopped by user after {} iteration(s) — partial completion",
                input.iterations
            ),
        );
    }

    // Failed after attempting work
    if input.max_iterations_reached {
        return (
            0.3,
            format!(
                "Max iterations reached ({}) without passing verification",
                input.iterations
            ),
        );
    }

    // Failed with error
    match input.error_type.as_deref() {
        Some("generation_error") => (
            0.1,
            "Failed during generation phase — workflow could not be constructed".into(),
        ),
        Some("timeout") => (
            0.2,
            format!(
                "Timed out after {} iteration(s) and {:.0}s",
                input.iterations, input.duration_secs
            ),
        ),
        Some(error_type) => (
            0.1,
            format!(
                "Failed with error type '{}' after {} iteration(s)",
                error_type, input.iterations
            ),
        ),
        None => (
            0.15,
            format!(
                "Failed after {} iteration(s) without specific error classification",
                input.iterations
            ),
        ),
    }
}

/// Confidence in the task completion score.
///
/// Deterministic scores from clear signals (pass/fail) get high confidence.
/// Ambiguous outcomes (partial, error without clear type) get lower confidence.
fn confidence(input: &DeterministicInput) -> f64 {
    if input.verification_passed {
        1.0 // Clear success signal
    } else if input.iterations == 0 {
        0.95 // Very confident this is a startup crash
    } else if input.was_stopped {
        0.6 // User intent is ambiguous — could be "good enough" or "giving up"
    } else if input.max_iterations_reached {
        0.85 // Clear that it tried and failed
    } else if input.error_type.is_some() {
        0.9 // Error type gives clear signal
    } else {
        0.7 // Unclear failure mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_input(
        verification_passed: bool,
        iterations: u32,
        was_stopped: bool,
        max_iterations_reached: bool,
        error_type: Option<&str>,
    ) -> DeterministicInput {
        DeterministicInput {
            task_run_id: "test-1".to_string(),
            status: if verification_passed {
                "success"
            } else {
                "failure"
            }
            .to_string(),
            verification_passed,
            was_stopped,
            iterations,
            step_count: Some(5),
            verification_step_count: Some(2),
            agentic_step_count: Some(3),
            max_iterations_reached,
            duration_secs: 100.0,
            error_type: error_type.map(String::from),
            tools_used: Vec::new(),
            agent_traces: Vec::new(),
            schema_compliance_inputs: Vec::new(),
        }
    }

    #[test]
    fn test_success() {
        let result = score(&make_input(true, 2, false, false, None));
        assert_eq!(result.score, 1.0);
        assert_eq!(result.confidence, 1.0);
    }

    #[test]
    fn test_zero_iteration_crash() {
        let result = score(&make_input(false, 0, false, false, Some("runtime_error")));
        assert_eq!(result.score, 0.0);
        assert!(result.rationale.contains("Zero-iteration"));
    }

    #[test]
    fn test_stopped_early() {
        let result = score(&make_input(false, 1, true, false, None));
        assert_eq!(result.score, 0.5);
    }

    #[test]
    fn test_stopped_with_progress() {
        let result = score(&make_input(false, 3, true, false, None));
        assert_eq!(result.score, 0.7);
    }

    #[test]
    fn test_max_iterations() {
        let result = score(&make_input(false, 3, false, true, None));
        assert_eq!(result.score, 0.3);
    }

    #[test]
    fn test_generation_error() {
        let result = score(&make_input(
            false,
            1,
            false,
            false,
            Some("generation_error"),
        ));
        assert_eq!(result.score, 0.1);
    }

    #[test]
    fn test_timeout() {
        let result = score(&make_input(false, 2, false, false, Some("timeout")));
        assert_eq!(result.score, 0.2);
    }
}
