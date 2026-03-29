//! Eval runner — orchestrates running eval specs against recommendations.
//!
//! Runs baseline trials (current config) and candidate trials (with recommendation
//! applied), then compares statistically using the shared `stats` module.

use std::sync::Arc;
use tracing::info;

use super::eval_spec::{
    AssertionResult, EvalAggregateMetrics, EvalAssertion, EvalComparison, EvalResult, EvalSpec,
    EvalTestCase, TestCaseResult,
};
use super::llm_judge_metrics::{
    evaluate_answer_relevance, evaluate_content_safety, evaluate_factuality,
    evaluate_hallucination, JudgeInput, JudgeResult,
};
use crate::database::CheckpointDb;
use crate::database::pg::PgDb;

// =============================================================================
// Core evaluation logic
// =============================================================================

/// Run an eval spec and produce a result.
///
/// This is the offline evaluation path — it uses historical data from pipeline traces
/// to evaluate assertions rather than launching live trials (which would require
/// the full orchestration loop). For live A/B testing, use the canary system.
///
/// # Arguments
/// * `spec` — the eval spec to evaluate
/// * `recommendation_id` — optional recommendation being validated
/// * `db` — database handle
pub fn evaluate_spec(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    spec: &EvalSpec,
    recommendation_id: Option<&str>,
) -> Result<EvalResult, String> {
    let result_id = format!("er-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();

    // Get baseline metrics for the target agent (last 30 days)
    let baseline_metrics = if let Some(ref agent) = spec.target_agent {
        let period_start = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        let period_end = chrono::Utc::now().to_rfc3339();

        crate::database::pipeline_traces::get_agent_aggregates_for_period(
            db,
            agent,
            &period_start,
            &period_end,
        )?
    } else {
        None
    };

    // Build aggregate from baseline
    let baseline_agg = baseline_metrics.as_ref().map(|agg| EvalAggregateMetrics {
        success_rate: if agg.run_count > 0 {
            agg.success_count as f64 / agg.run_count as f64
        } else {
            0.0
        },
        mean_duration_ms: agg.avg_duration_ms,
        mean_iterations: 0.0, // Not tracked in pipeline traces
        mean_cost_cents: agg.avg_cost_usd * 100.0,
        trial_count: agg.run_count as u32,
    });

    // If recommendation is applied, get post-apply metrics
    let candidate_agg = if let Some(rec_id) = recommendation_id {
        get_post_apply_metrics(db, pg_db, rec_id, spec.target_agent.as_deref())?
    } else {
        None
    };

    // Evaluate test cases
    let test_case_results: Vec<TestCaseResult> = spec
        .test_cases
        .iter()
        .map(|tc| evaluate_test_case(tc, &baseline_agg, &candidate_agg))
        .collect();

    let passed_count = test_case_results.iter().filter(|tc| tc.passed).count();
    let total_count = test_case_results.len();
    let pass_rate = if total_count > 0 {
        passed_count as f64 / total_count as f64
    } else {
        1.0
    };

    // Statistical comparison (if both groups have data)
    let comparison = match (&baseline_agg, &candidate_agg) {
        (Some(b), Some(c)) if b.trial_count >= 2 && c.trial_count >= 2 => {
            let b_succ = (b.success_rate * b.trial_count as f64).round() as u64;
            let c_succ = (c.success_rate * c.trial_count as f64).round() as u64;

            let analysis = crate::stats::proportion_analysis(
                (c_succ, c.trial_count as u64),
                (b_succ, b.trial_count as u64),
                2,
            );

            let sr_delta_pp = (c.success_rate - b.success_rate) * 100.0;
            let verdict = crate::stats::compute_verdict(
                sr_delta_pp,
                &analysis,
                c.trial_count as u64,
                &crate::stats::VerdictThresholds::recommendation(),
            );

            Some(EvalComparison {
                success_rate_delta_pp: sr_delta_pp,
                p_value: analysis.p_value,
                confidence_interval: analysis.confidence_interval,
                effect_size: analysis.effect_size,
                verdict: verdict.as_recommendation_str().to_string(),
            })
        }
        _ => None,
    };

    // Overall status
    let status = if total_count == 0 {
        "inconclusive"
    } else if pass_rate >= spec.thresholds.required_pass_rate {
        if comparison
            .as_ref()
            .is_some_and(|c| c.verdict == "regressed")
        {
            "failed"
        } else {
            "passed"
        }
    } else {
        "failed"
    };

    let trials_run = baseline_agg.as_ref().map(|a| a.trial_count).unwrap_or(0)
        + candidate_agg.as_ref().map(|a| a.trial_count).unwrap_or(0);

    let result = EvalResult {
        id: result_id.clone(),
        spec_id: spec.id.clone(),
        recommendation_id: recommendation_id.map(|s| s.to_string()),
        status: status.to_string(),
        test_case_results,
        baseline_metrics: baseline_agg,
        candidate_metrics: candidate_agg,
        comparison,
        trials_run,
        created_at: now,
    };

    // Persist
    super::eval_spec::save_eval_result(db, pg_db, &result)?;

    // If tied to a recommendation, attach the result
    if let Some(rec_id) = recommendation_id {
        super::eval_spec::attach_eval_result(db, pg_db, rec_id, &result_id, status)?;
    }

    info!(
        "Eval {} completed: status={}, {}/{} test cases passed",
        result_id, status, passed_count, total_count
    );

    Ok(result)
}

/// Validate a specific recommendation by running its associated eval spec
/// (or generating a default one).
pub fn validate_recommendation(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
) -> Result<EvalResult, String> {
    let rec_id = recommendation_id.to_string();

    // Get recommendation details from PG
    let rec = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pg_db.get_recommendation(&rec_id))
    })?
    .ok_or_else(|| format!("Recommendation not found: {}", rec_id))?;
    let target_agent = rec.target_agent;
    let rec_type = rec.recommendation_type;

    // Find or generate an eval spec
    let spec = if let Some(ref agent) = target_agent {
        // Look for an existing spec for this agent
        let specs = super::eval_spec::list_eval_specs(db, pg_db, Some(agent))?;
        if let Some(spec) = specs.into_iter().next() {
            spec
        } else {
            // Auto-generate a default spec
            let spec = super::eval_spec::generate_default_spec(db, agent)?;
            super::eval_spec::save_eval_spec(db, pg_db, &spec)?;
            spec
        }
    } else {
        return Err("Cannot validate recommendation without target_agent".to_string());
    };

    let result = evaluate_spec(db, pg_db, &spec, Some(recommendation_id))?;

    // For prompt_rewrite recommendations, also run robustness tests
    if rec_type == "prompt_rewrite" {
        if let Some(ref agent) = target_agent {
            match super::robustness::run_robustness_test(
                db,
                pg_db,
                agent,
                None, // no specific prompt variant
                Some(recommendation_id),
            ) {
                Ok(report) => {
                    tracing::info!(
                        "Robustness test for recommendation {}: {}/{} passed",
                        recommendation_id,
                        report.passed,
                        report.total_tests
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "Robustness test failed for recommendation {}: {}",
                        recommendation_id,
                        e
                    );
                }
            }
        }
    }

    Ok(result)
}

// =============================================================================
// Internal helpers
// =============================================================================

/// Get post-apply metrics for a recommendation (7-day window after applied_at).
fn get_post_apply_metrics(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
    target_agent: Option<&str>,
) -> Result<Option<EvalAggregateMetrics>, String> {
    let rec_id = recommendation_id.to_string();

    let applied_at: Option<String> = {
        let rec = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(pg_db.get_recommendation(recommendation_id))
        })?
        .ok_or_else(|| format!("Recommendation not found: {}", recommendation_id))?;
        rec.applied_at
    };

    let applied_at = match applied_at {
        Some(a) => a,
        None => return Ok(None),
    };

    if let Some(agent) = target_agent {
        let after_end = chrono::DateTime::parse_from_rfc3339(&applied_at)
            .map(|dt| (dt + chrono::Duration::days(7)).to_rfc3339())
            .map_err(|e| format!("Invalid applied_at: {}", e))?;

        let agg = crate::database::pipeline_traces::get_agent_aggregates_for_period(
            db,
            agent,
            &applied_at,
            &after_end,
        )?;

        Ok(agg.map(|a| EvalAggregateMetrics {
            success_rate: if a.run_count > 0 {
                a.success_count as f64 / a.run_count as f64
            } else {
                0.0
            },
            mean_duration_ms: a.avg_duration_ms,
            mean_iterations: 0.0,
            mean_cost_cents: a.avg_cost_usd * 100.0,
            trial_count: a.run_count as u32,
        }))
    } else {
        Ok(None)
    }
}

/// Evaluate a single test case against baseline and/or candidate metrics.
fn evaluate_test_case(
    tc: &super::eval_spec::EvalTestCase,
    baseline: &Option<EvalAggregateMetrics>,
    candidate: &Option<EvalAggregateMetrics>,
) -> TestCaseResult {
    // Use candidate metrics if available, otherwise baseline
    let metrics = candidate.as_ref().or(baseline.as_ref());

    let assertion_results: Vec<AssertionResult> = tc
        .assertions
        .iter()
        .map(|assertion| evaluate_assertion(assertion, metrics, baseline))
        .collect();

    let passed = assertion_results.iter().all(|a| a.passed);

    TestCaseResult {
        test_case_id: tc.id.clone(),
        passed,
        assertion_results,
    }
}

/// Evaluate a single assertion.
fn evaluate_assertion(
    assertion: &EvalAssertion,
    metrics: Option<&EvalAggregateMetrics>,
    baseline: &Option<EvalAggregateMetrics>,
) -> AssertionResult {
    match assertion {
        EvalAssertion::SuccessRate { min } => {
            let actual = metrics.map(|m| m.success_rate).unwrap_or(0.0);
            let passed = actual >= *min;
            AssertionResult {
                assertion_type: "success_rate".to_string(),
                passed,
                actual_value: actual,
                threshold_value: *min,
                message: format!(
                    "Success rate {:.1}% {} {:.1}%",
                    actual * 100.0,
                    if passed { ">=" } else { "<" },
                    min * 100.0
                ),
            }
        }
        EvalAssertion::MaxDuration { ms } => {
            let actual = metrics.map(|m| m.mean_duration_ms).unwrap_or(0.0);
            let threshold = *ms as f64;
            let passed = actual <= threshold;
            AssertionResult {
                assertion_type: "max_duration".to_string(),
                passed,
                actual_value: actual,
                threshold_value: threshold,
                message: format!(
                    "Duration {:.0}ms {} {:.0}ms",
                    actual,
                    if passed { "<=" } else { ">" },
                    threshold
                ),
            }
        }
        EvalAssertion::MaxCost { cents } => {
            let actual = metrics.map(|m| m.mean_cost_cents).unwrap_or(0.0);
            let passed = actual <= *cents;
            AssertionResult {
                assertion_type: "max_cost".to_string(),
                passed,
                actual_value: actual,
                threshold_value: *cents,
                message: format!(
                    "Cost {:.1}¢ {} {:.1}¢",
                    actual,
                    if passed { "<=" } else { ">" },
                    cents
                ),
            }
        }
        EvalAssertion::MaxIterations { count } => {
            let actual = metrics.map(|m| m.mean_iterations).unwrap_or(0.0);
            let threshold = *count as f64;
            let passed = actual <= threshold;
            AssertionResult {
                assertion_type: "max_iterations".to_string(),
                passed,
                actual_value: actual,
                threshold_value: threshold,
                message: format!(
                    "Iterations {:.1} {} {}",
                    actual,
                    if passed { "<=" } else { ">" },
                    count
                ),
            }
        }
        EvalAssertion::NoRegression {
            metric,
            tolerance_pp,
        } => {
            let (actual_delta, passed) = match (metrics, baseline.as_ref()) {
                (Some(m), Some(b)) if metric == "success_rate" => {
                    let delta = (m.success_rate - b.success_rate) * 100.0;
                    (delta, delta >= -tolerance_pp)
                }
                _ => (0.0, true), // No baseline → pass by default
            };
            AssertionResult {
                assertion_type: "no_regression".to_string(),
                passed,
                actual_value: actual_delta,
                threshold_value: -*tolerance_pp,
                message: format!(
                    "{} delta {:.1}pp {} -{:.1}pp tolerance",
                    metric,
                    actual_delta,
                    if passed { ">=" } else { "<" },
                    tolerance_pp
                ),
            }
        }
        // LLM judge assertions require I/O context; skip in aggregate evaluation.
        EvalAssertion::HallucinationCheck { .. }
        | EvalAssertion::AnswerRelevance { .. }
        | EvalAssertion::Factuality { .. }
        | EvalAssertion::ContentSafety { .. } => AssertionResult {
            assertion_type: "llm_judge".to_string(),
            passed: true,
            actual_value: 0.0,
            threshold_value: 0.0,
            message: "Skipped in offline evaluation (no per-run I/O available)".to_string(),
        },
    }
}

// =============================================================================
// Live evaluation with I/O context (for LLM-as-judge assertions)
// =============================================================================

/// Convert an LLM judge result into an assertion result.
fn judge_result_to_assertion(result: &JudgeResult, assertion_type: &str) -> AssertionResult {
    AssertionResult {
        assertion_type: assertion_type.to_string(),
        passed: result.passed,
        actual_value: result.score,
        threshold_value: 0.0,
        message: result.reason.clone(),
    }
}

/// Evaluate a single assertion using live I/O data.
///
/// For LLM-as-judge assertions, this calls the actual judge functions
/// from `llm_judge_metrics`. For all other assertion types, it delegates
/// to the aggregate-based `evaluate_assertion`.
pub fn evaluate_assertion_with_io(
    assertion: &EvalAssertion,
    input: &str,
    output: &str,
    context: Option<&str>,
    metrics: &EvalAggregateMetrics,
) -> AssertionResult {
    let judge_input = JudgeInput {
        input: input.to_string(),
        output: output.to_string(),
        context: context.map(|c| c.to_string()),
    };

    match assertion {
        EvalAssertion::HallucinationCheck {
            max_hallucination_rate,
        } => {
            let result = evaluate_hallucination(&judge_input, *max_hallucination_rate);
            judge_result_to_assertion(&result, "hallucination_check")
        }
        EvalAssertion::AnswerRelevance { min_relevance } => {
            let result = evaluate_answer_relevance(&judge_input, *min_relevance);
            judge_result_to_assertion(&result, "answer_relevance")
        }
        EvalAssertion::Factuality { min_factuality } => {
            let result = evaluate_factuality(&judge_input, *min_factuality);
            judge_result_to_assertion(&result, "factuality")
        }
        EvalAssertion::ContentSafety {
            blocked_categories,
        } => {
            let result = evaluate_content_safety(&judge_input, blocked_categories);
            judge_result_to_assertion(&result, "content_safety")
        }
        // Non-judge assertions delegate to the aggregate-based evaluator
        other => evaluate_assertion(other, Some(metrics), &None),
    }
}

/// Evaluate all assertions in a test case using live I/O data.
pub fn evaluate_test_case_with_io(
    test_case: &EvalTestCase,
    input: &str,
    output: &str,
    context: Option<&str>,
    metrics: &EvalAggregateMetrics,
) -> Vec<AssertionResult> {
    test_case
        .assertions
        .iter()
        .map(|a| evaluate_assertion_with_io(a, input, output, context, metrics))
        .collect()
}

// ── PG dual-write wrappers ─────────────────────────────────────────────

/// Validate a recommendation with PG dual-write (fire-and-forget).
#[allow(dead_code)]
pub fn validate_recommendation_with_pg(
    db: &CheckpointDb,
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    recommendation_id: &str,
) -> Result<EvalResult, String> {
    validate_recommendation(db, pg_db, recommendation_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta_optimizer::eval_spec::*;

    #[test]
    fn test_evaluate_assertion_success_rate_pass() {
        let assertion = EvalAssertion::SuccessRate { min: 0.7 };
        let metrics = EvalAggregateMetrics {
            success_rate: 0.85,
            mean_duration_ms: 1000.0,
            mean_iterations: 3.0,
            mean_cost_cents: 50.0,
            trial_count: 10,
        };
        let result = evaluate_assertion(&assertion, Some(&metrics), &None);
        assert!(result.passed);
    }

    #[test]
    fn test_evaluate_assertion_success_rate_fail() {
        let assertion = EvalAssertion::SuccessRate { min: 0.9 };
        let metrics = EvalAggregateMetrics {
            success_rate: 0.7,
            mean_duration_ms: 1000.0,
            mean_iterations: 3.0,
            mean_cost_cents: 50.0,
            trial_count: 10,
        };
        let result = evaluate_assertion(&assertion, Some(&metrics), &None);
        assert!(!result.passed);
    }

    #[test]
    fn test_evaluate_assertion_max_duration() {
        let assertion = EvalAssertion::MaxDuration { ms: 5000 };
        let metrics = EvalAggregateMetrics {
            success_rate: 0.85,
            mean_duration_ms: 3000.0,
            mean_iterations: 3.0,
            mean_cost_cents: 50.0,
            trial_count: 10,
        };
        let result = evaluate_assertion(&assertion, Some(&metrics), &None);
        assert!(result.passed);
    }

    #[test]
    fn test_evaluate_assertion_no_regression_pass() {
        let assertion = EvalAssertion::NoRegression {
            metric: "success_rate".to_string(),
            tolerance_pp: 5.0,
        };
        let baseline = EvalAggregateMetrics {
            success_rate: 0.80,
            mean_duration_ms: 1000.0,
            mean_iterations: 3.0,
            mean_cost_cents: 50.0,
            trial_count: 10,
        };
        let candidate = EvalAggregateMetrics {
            success_rate: 0.78, // -2pp, within 5pp tolerance
            mean_duration_ms: 900.0,
            mean_iterations: 2.5,
            mean_cost_cents: 45.0,
            trial_count: 10,
        };
        let result = evaluate_assertion(&assertion, Some(&candidate), &Some(baseline));
        assert!(result.passed);
    }

    #[test]
    fn test_evaluate_assertion_no_regression_fail() {
        let assertion = EvalAssertion::NoRegression {
            metric: "success_rate".to_string(),
            tolerance_pp: 3.0,
        };
        let baseline = EvalAggregateMetrics {
            success_rate: 0.80,
            mean_duration_ms: 1000.0,
            mean_iterations: 3.0,
            mean_cost_cents: 50.0,
            trial_count: 10,
        };
        let candidate = EvalAggregateMetrics {
            success_rate: 0.70, // -10pp, exceeds 3pp tolerance
            mean_duration_ms: 900.0,
            mean_iterations: 2.5,
            mean_cost_cents: 45.0,
            trial_count: 10,
        };
        let result = evaluate_assertion(&assertion, Some(&candidate), &Some(baseline));
        assert!(!result.passed);
    }

    #[test]
    fn test_evaluate_test_case_all_pass() {
        let tc = EvalTestCase {
            id: "tc1".to_string(),
            description: "Test".to_string(),
            input: EvalInput::WorkflowIds { ids: vec![] },
            assertions: vec![
                EvalAssertion::SuccessRate { min: 0.5 },
                EvalAssertion::MaxDuration { ms: 10000 },
            ],
        };
        let metrics = Some(EvalAggregateMetrics {
            success_rate: 0.8,
            mean_duration_ms: 5000.0,
            mean_iterations: 3.0,
            mean_cost_cents: 50.0,
            trial_count: 10,
        });
        let result = evaluate_test_case(&tc, &metrics, &None);
        assert!(result.passed);
        assert_eq!(result.assertion_results.len(), 2);
    }

    #[test]
    fn test_evaluate_test_case_partial_fail() {
        let tc = EvalTestCase {
            id: "tc1".to_string(),
            description: "Test".to_string(),
            input: EvalInput::WorkflowIds { ids: vec![] },
            assertions: vec![
                EvalAssertion::SuccessRate { min: 0.9 },  // will fail
                EvalAssertion::MaxDuration { ms: 10000 }, // will pass
            ],
        };
        let metrics = Some(EvalAggregateMetrics {
            success_rate: 0.7,
            mean_duration_ms: 5000.0,
            mean_iterations: 3.0,
            mean_cost_cents: 50.0,
            trial_count: 10,
        });
        let result = evaluate_test_case(&tc, &metrics, &None);
        assert!(!result.passed);
    }
}
