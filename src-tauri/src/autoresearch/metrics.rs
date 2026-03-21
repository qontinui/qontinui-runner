//! Metric computation, comparison, and statistical significance for autoresearch experiments.

use super::types::{
    AcceptanceCriteria, AggregateMetrics, MultiWorkflowAggregation, PrimaryMetric, TrialResult,
};
use std::collections::HashMap;

/// Compute aggregate metrics from a set of trial results.
pub fn compute_aggregate(trials: &[TrialResult]) -> AggregateMetrics {
    if trials.is_empty() {
        return AggregateMetrics {
            pass_rate: 0.0,
            mean_iterations: 0.0,
            mean_duration_ms: 0.0,
            trial_count: 0,
            mean_spec_compliance: None,
        };
    }

    let n = trials.len() as f64;
    let passed = trials.iter().filter(|t| t.passed).count() as f64;
    let total_iter: f64 = trials.iter().map(|t| t.iterations_used as f64).sum();
    let total_dur: f64 = trials.iter().map(|t| t.duration_ms as f64).sum();

    // Compute mean spec compliance from trials that have the field
    let compliance_scores: Vec<f64> = trials
        .iter()
        .filter_map(|t| t.spec_compliance_score)
        .collect();
    let mean_spec_compliance = if compliance_scores.is_empty() {
        None
    } else {
        Some(compliance_scores.iter().sum::<f64>() / compliance_scores.len() as f64)
    };

    AggregateMetrics {
        pass_rate: passed / n,
        mean_iterations: total_iter / n,
        mean_duration_ms: total_dur / n,
        trial_count: trials.len() as u32,
        mean_spec_compliance,
    }
}

/// Compute aggregate metrics with multi-workflow aggregation strategy.
///
/// When trials come from multiple workflows (tagged via `workflow_id`),
/// the aggregation strategy determines how per-workflow results combine:
/// - **Average**: Average pass rates across all workflows (default, same as compute_aggregate)
/// - **AllMustPass**: Pass rate = minimum per-workflow pass rate (strictest)
/// - **AnyPass**: Pass rate = maximum per-workflow pass rate (most lenient)
///
/// Iterations and duration are always averaged across all trials.
pub fn compute_aggregate_multi_workflow(
    trials: &[TrialResult],
    strategy: &MultiWorkflowAggregation,
) -> AggregateMetrics {
    if trials.is_empty() {
        return compute_aggregate(trials);
    }

    // Group trials by workflow_id
    let mut by_workflow: HashMap<String, Vec<&TrialResult>> = HashMap::new();
    for t in trials {
        let key = t.workflow_id.clone().unwrap_or_default();
        by_workflow.entry(key).or_default().push(t);
    }

    // If only one workflow (or no workflow_ids), just use simple aggregate
    if by_workflow.len() <= 1 {
        return compute_aggregate(trials);
    }

    // Compute per-workflow pass rates
    let per_wf_pass_rates: Vec<f64> = by_workflow
        .values()
        .map(|wf_trials| {
            let n = wf_trials.len() as f64;
            let passed = wf_trials.iter().filter(|t| t.passed).count() as f64;
            passed / n
        })
        .collect();

    // Apply aggregation strategy to pass rate
    let combined_pass_rate = match strategy {
        MultiWorkflowAggregation::Average => {
            per_wf_pass_rates.iter().sum::<f64>() / per_wf_pass_rates.len() as f64
        }
        MultiWorkflowAggregation::AllMustPass => per_wf_pass_rates
            .iter()
            .cloned()
            .fold(f64::INFINITY, f64::min),
        MultiWorkflowAggregation::AnyPass => per_wf_pass_rates
            .iter()
            .cloned()
            .fold(f64::NEG_INFINITY, f64::max),
    };

    // Iterations and duration always averaged across all trials
    let n = trials.len() as f64;
    let total_iter: f64 = trials.iter().map(|t| t.iterations_used as f64).sum();
    let total_dur: f64 = trials.iter().map(|t| t.duration_ms as f64).sum();

    let compliance_scores: Vec<f64> = trials
        .iter()
        .filter_map(|t| t.spec_compliance_score)
        .collect();
    let mean_spec_compliance = if compliance_scores.is_empty() {
        None
    } else {
        Some(compliance_scores.iter().sum::<f64>() / compliance_scores.len() as f64)
    };

    AggregateMetrics {
        pass_rate: combined_pass_rate,
        mean_iterations: total_iter / n,
        mean_duration_ms: total_dur / n,
        trial_count: trials.len() as u32,
        mean_spec_compliance,
    }
}

/// Compare an experiment's aggregate metrics against the control.
/// Returns (accepted, reason, p_value).
pub fn compare_to_control(
    experiment: &AggregateMetrics,
    control: &AggregateMetrics,
    experiment_trials: &[TrialResult],
    control_trials: &[TrialResult],
    criteria: &AcceptanceCriteria,
) -> (bool, String, Option<f64>) {
    match criteria.primary_metric {
        PrimaryMetric::PassRate => compare_pass_rate(
            experiment,
            control,
            experiment_trials,
            control_trials,
            criteria,
        ),
        PrimaryMetric::MeanIterations => compare_lower_is_better(
            experiment.mean_iterations,
            control.mean_iterations,
            "mean_iterations",
            experiment_trials
                .iter()
                .map(|t| t.iterations_used as f64)
                .collect::<Vec<_>>(),
            control_trials
                .iter()
                .map(|t| t.iterations_used as f64)
                .collect::<Vec<_>>(),
            criteria,
        ),
        PrimaryMetric::MeanDuration => compare_lower_is_better(
            experiment.mean_duration_ms,
            control.mean_duration_ms,
            "mean_duration_ms",
            experiment_trials
                .iter()
                .map(|t| t.duration_ms as f64)
                .collect::<Vec<_>>(),
            control_trials
                .iter()
                .map(|t| t.duration_ms as f64)
                .collect::<Vec<_>>(),
            criteria,
        ),
        PrimaryMetric::SpecCompliance => compare_spec_compliance(
            experiment,
            control,
            experiment_trials,
            control_trials,
            criteria,
        ),
    }
}

/// Compare pass rates with optional significance testing.
fn compare_pass_rate(
    experiment: &AggregateMetrics,
    control: &AggregateMetrics,
    experiment_trials: &[TrialResult],
    control_trials: &[TrialResult],
    criteria: &AcceptanceCriteria,
) -> (bool, String, Option<f64>) {
    let exp_val = experiment.pass_rate;
    let ctrl_val = control.pass_rate;

    if ctrl_val == 0.0 && exp_val > 0.0 {
        return (
            true,
            format!(
                "pass_rate: experiment={:.3}, control=0 (infinite improvement)",
                exp_val
            ),
            None,
        );
    }
    if ctrl_val == 0.0 && exp_val == 0.0 {
        return (
            false,
            "pass_rate: both experiment and control are 0 — no improvement".to_string(),
            None,
        );
    }

    let ratio = exp_val / ctrl_val;
    let ratio_ok = ratio >= criteria.min_improvement_ratio;

    // Statistical significance via Fisher's exact test approximation
    let p_value = if experiment_trials.len() >= 2 && control_trials.len() >= 2 {
        let p = fisher_exact_p(experiment_trials, control_trials);
        Some(p)
    } else {
        None
    };

    let sig_ok = p_value.is_none_or(|p| p <= criteria.significance_threshold);

    let accepted = ratio_ok && sig_ok;
    let mut reason = format!(
        "pass_rate: experiment={:.3}, control={:.3}, ratio={:.3} (threshold={:.3})",
        exp_val, ctrl_val, ratio, criteria.min_improvement_ratio,
    );
    if let Some(p) = p_value {
        reason.push_str(&format!(
            ", p={:.4} (threshold={:.2})",
            p, criteria.significance_threshold
        ));
        if ratio_ok && !sig_ok {
            reason.push_str(" [ratio OK but not significant]");
        }
    }

    (accepted, reason, p_value)
}

/// Compare metrics where lower is better (iterations, duration).
fn compare_lower_is_better(
    exp_val: f64,
    ctrl_val: f64,
    metric_name: &str,
    exp_samples: Vec<f64>,
    ctrl_samples: Vec<f64>,
    criteria: &AcceptanceCriteria,
) -> (bool, String, Option<f64>) {
    if exp_val == 0.0 {
        return (
            true,
            format!("{}: experiment=0 (perfect)", metric_name),
            None,
        );
    }

    let ratio = ctrl_val / exp_val;
    let ratio_ok = ratio >= criteria.min_improvement_ratio;

    // Welch's t-test for continuous metrics
    let p_value = if exp_samples.len() >= 2 && ctrl_samples.len() >= 2 {
        let p = welch_t_test(&exp_samples, &ctrl_samples);
        Some(p)
    } else {
        None
    };

    let sig_ok = p_value.is_none_or(|p| p <= criteria.significance_threshold);
    let accepted = ratio_ok && sig_ok;

    let mut reason = format!(
        "{}: experiment={:.1}, control={:.1}, ratio={:.3} (threshold={:.3})",
        metric_name, exp_val, ctrl_val, ratio, criteria.min_improvement_ratio,
    );
    if let Some(p) = p_value {
        reason.push_str(&format!(
            ", p={:.4} (threshold={:.2})",
            p, criteria.significance_threshold
        ));
        if ratio_ok && !sig_ok {
            reason.push_str(" [ratio OK but not significant]");
        }
    }

    (accepted, reason, p_value)
}

/// Compare spec compliance scores using Fisher's test on assertion pass/fail counts.
fn compare_spec_compliance(
    experiment: &AggregateMetrics,
    control: &AggregateMetrics,
    experiment_trials: &[TrialResult],
    control_trials: &[TrialResult],
    criteria: &AcceptanceCriteria,
) -> (bool, String, Option<f64>) {
    let exp_compliance = experiment.mean_spec_compliance.unwrap_or(0.0);
    let ctrl_compliance = control.mean_spec_compliance.unwrap_or(0.0);

    if ctrl_compliance == 0.0 && exp_compliance > 0.0 {
        return (
            true,
            format!(
                "spec_compliance: experiment={:.3}, control=0 (infinite improvement)",
                exp_compliance
            ),
            None,
        );
    }
    if ctrl_compliance == 0.0 && exp_compliance == 0.0 {
        return (
            false,
            "spec_compliance: both experiment and control are 0 — no improvement".to_string(),
            None,
        );
    }

    let ratio = exp_compliance / ctrl_compliance;
    let ratio_ok = ratio >= criteria.min_improvement_ratio;

    // Use Fisher's test on total assertion pass/fail across trials
    let exp_passed: u32 = experiment_trials
        .iter()
        .filter_map(|t| t.spec_assertions_passed)
        .sum();
    let exp_total: u32 = experiment_trials
        .iter()
        .filter_map(|t| t.spec_assertions_total)
        .sum();
    let ctrl_passed: u32 = control_trials
        .iter()
        .filter_map(|t| t.spec_assertions_passed)
        .sum();
    let ctrl_total: u32 = control_trials
        .iter()
        .filter_map(|t| t.spec_assertions_total)
        .sum();

    let p_value = if exp_total >= 2 && ctrl_total >= 2 {
        let p_exp = exp_passed as f64 / exp_total as f64;
        let p_ctrl = ctrl_passed as f64 / ctrl_total as f64;
        let total = (exp_total + ctrl_total) as f64;
        let p_pool = (exp_passed + ctrl_passed) as f64 / total;

        if p_pool == 0.0 || p_pool == 1.0 {
            if p_exp > p_ctrl {
                Some(0.0)
            } else {
                Some(1.0)
            }
        } else {
            let se = (p_pool * (1.0 - p_pool) * (1.0 / exp_total as f64 + 1.0 / ctrl_total as f64))
                .sqrt();
            if se > 0.0 {
                let z = (p_exp - p_ctrl) / se;
                Some(1.0 - normal_cdf(z))
            } else {
                None
            }
        }
    } else {
        None
    };

    let sig_ok = p_value.is_none_or(|p| p <= criteria.significance_threshold);
    let accepted = ratio_ok && sig_ok;

    let mut reason = format!(
        "spec_compliance: experiment={:.3}, control={:.3}, ratio={:.3} (threshold={:.3})",
        exp_compliance, ctrl_compliance, ratio, criteria.min_improvement_ratio,
    );
    if let Some(p) = p_value {
        reason.push_str(&format!(
            ", p={:.4} (threshold={:.2})",
            p, criteria.significance_threshold
        ));
        if ratio_ok && !sig_ok {
            reason.push_str(" [ratio OK but not significant]");
        }
    }

    (accepted, reason, p_value)
}

// =============================================================================
// Statistical tests — delegated to shared crate::stats module
// =============================================================================

/// Fisher's exact test approximation for 2x2 contingency table.
/// Computes a one-sided p-value for whether experiment pass rate > control pass rate.
///
/// Delegates to `crate::stats::proportion_z_test_onesided`.
fn fisher_exact_p(experiment_trials: &[TrialResult], control_trials: &[TrialResult]) -> f64 {
    let exp_passed = experiment_trials.iter().filter(|t| t.passed).count() as u64;
    let exp_total = experiment_trials.len() as u64;
    let ctrl_passed = control_trials.iter().filter(|t| t.passed).count() as u64;
    let ctrl_total = control_trials.len() as u64;

    crate::stats::proportion_z_test_onesided(exp_passed, exp_total, ctrl_passed, ctrl_total)
}

/// Welch's t-test for unequal variances.
/// Delegates to `crate::stats::welch_t_test`.
fn welch_t_test(a: &[f64], b: &[f64]) -> f64 {
    crate::stats::welch_t_test(a, b)
}

/// Standard normal CDF. Delegates to `crate::stats::normal_cdf`.
fn normal_cdf(x: f64) -> f64 {
    crate::stats::normal_cdf(x)
}

/// Format experiment results as a TSV table.
pub fn format_results_tsv(results: &[(u32, super::types::ExperimentResult)]) -> String {
    let mut out = String::new();
    out.push_str("experiment\tconfig\tpass_rate\tmean_iterations\tmean_duration_ms\taccepted\tp_value\treason\n");
    for (num, result) in results {
        let p_str = result
            .p_value
            .map_or("-".to_string(), |p| format!("{:.4}", p));
        out.push_str(&format!(
            "{}\t{}\t{:.3}\t{:.1}\t{:.0}\t{}\t{}\t{}\n",
            num,
            result.config.summary(),
            result.aggregate.pass_rate,
            result.aggregate.mean_iterations,
            result.aggregate.mean_duration_ms,
            result.accepted,
            p_str,
            result.reason,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_aggregate_empty() {
        let agg = compute_aggregate(&[]);
        assert_eq!(agg.trial_count, 0);
        assert_eq!(agg.pass_rate, 0.0);
    }

    #[test]
    fn test_compute_aggregate_basic() {
        let trials = vec![
            TrialResult {
                task_run_id: "t1".into(),
                passed: true,
                iterations_used: 3,
                duration_ms: 1000,
                workflow_id: None,
                spec_compliance_score: None,
                spec_assertions_passed: None,
                spec_assertions_total: None,
            },
            TrialResult {
                task_run_id: "t2".into(),
                passed: false,
                iterations_used: 10,
                duration_ms: 5000,
                workflow_id: None,
                spec_compliance_score: None,
                spec_assertions_passed: None,
                spec_assertions_total: None,
            },
        ];
        let agg = compute_aggregate(&trials);
        assert_eq!(agg.trial_count, 2);
        assert!((agg.pass_rate - 0.5).abs() < f64::EPSILON);
        assert!((agg.mean_iterations - 6.5).abs() < f64::EPSILON);
        assert!((agg.mean_duration_ms - 3000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compare_pass_rate_improvement() {
        let exp_trials = vec![
            TrialResult {
                task_run_id: "e1".into(),
                passed: true,
                iterations_used: 3,
                duration_ms: 1000,
                workflow_id: None,
                spec_compliance_score: None,
                spec_assertions_passed: None,
                spec_assertions_total: None,
            },
            TrialResult {
                task_run_id: "e2".into(),
                passed: true,
                iterations_used: 4,
                duration_ms: 1200,
                workflow_id: None,
                spec_compliance_score: None,
                spec_assertions_passed: None,
                spec_assertions_total: None,
            },
            TrialResult {
                task_run_id: "e3".into(),
                passed: true,
                iterations_used: 3,
                duration_ms: 1100,
                workflow_id: None,
                spec_compliance_score: None,
                spec_assertions_passed: None,
                spec_assertions_total: None,
            },
        ];
        let ctrl_trials = vec![
            TrialResult {
                task_run_id: "c1".into(),
                passed: true,
                iterations_used: 5,
                duration_ms: 2000,
                workflow_id: None,
                spec_compliance_score: None,
                spec_assertions_passed: None,
                spec_assertions_total: None,
            },
            TrialResult {
                task_run_id: "c2".into(),
                passed: false,
                iterations_used: 10,
                duration_ms: 5000,
                workflow_id: None,
                spec_compliance_score: None,
                spec_assertions_passed: None,
                spec_assertions_total: None,
            },
            TrialResult {
                task_run_id: "c3".into(),
                passed: false,
                iterations_used: 10,
                duration_ms: 5000,
                workflow_id: None,
                spec_compliance_score: None,
                spec_assertions_passed: None,
                spec_assertions_total: None,
            },
        ];
        let exp_agg = compute_aggregate(&exp_trials);
        let ctrl_agg = compute_aggregate(&ctrl_trials);
        let criteria = AcceptanceCriteria::default();
        let (accepted, _reason, p_value) =
            compare_to_control(&exp_agg, &ctrl_agg, &exp_trials, &ctrl_trials, &criteria);
        assert!(accepted);
        assert!(p_value.is_some());
    }

    #[test]
    fn test_compare_pass_rate_no_improvement() {
        let exp_trials = vec![
            TrialResult {
                task_run_id: "e1".into(),
                passed: false,
                iterations_used: 10,
                duration_ms: 5000,
                workflow_id: None,
                spec_compliance_score: None,
                spec_assertions_passed: None,
                spec_assertions_total: None,
            },
            TrialResult {
                task_run_id: "e2".into(),
                passed: true,
                iterations_used: 5,
                duration_ms: 2000,
                workflow_id: None,
                spec_compliance_score: None,
                spec_assertions_passed: None,
                spec_assertions_total: None,
            },
        ];
        let ctrl_trials = vec![
            TrialResult {
                task_run_id: "c1".into(),
                passed: true,
                iterations_used: 3,
                duration_ms: 1000,
                workflow_id: None,
                spec_compliance_score: None,
                spec_assertions_passed: None,
                spec_assertions_total: None,
            },
            TrialResult {
                task_run_id: "c2".into(),
                passed: true,
                iterations_used: 4,
                duration_ms: 1500,
                workflow_id: None,
                spec_compliance_score: None,
                spec_assertions_passed: None,
                spec_assertions_total: None,
            },
        ];
        let exp_agg = compute_aggregate(&exp_trials);
        let ctrl_agg = compute_aggregate(&ctrl_trials);
        let criteria = AcceptanceCriteria::default();
        let (accepted, _reason, _p) =
            compare_to_control(&exp_agg, &ctrl_agg, &exp_trials, &ctrl_trials, &criteria);
        assert!(!accepted);
    }

    #[test]
    fn test_normal_cdf_basic() {
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-6);
        assert!((normal_cdf(1.96) - 0.975).abs() < 1e-3);
        assert!(normal_cdf(-8.0) < 1e-10);
        assert!(normal_cdf(8.0) > 1.0 - 1e-10);
    }

    #[test]
    fn test_welch_t_test_identical_samples() {
        let a = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let b = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let p = welch_t_test(&a, &b);
        assert!(p > 0.99, "Identical samples should have p ≈ 1.0, got {}", p);
    }

    #[test]
    fn test_welch_t_test_different_samples() {
        let a = vec![100.0, 101.0, 102.0, 103.0, 104.0];
        let b = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let p = welch_t_test(&a, &b);
        assert!(
            p < 0.01,
            "Very different samples should have p < 0.01, got {}",
            p
        );
    }
}
