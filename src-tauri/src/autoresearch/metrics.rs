//! Metric computation, comparison, and statistical significance for autoresearch experiments.

use super::types::{AcceptanceCriteria, AggregateMetrics, MultiWorkflowAggregation, PrimaryMetric, TrialResult};
use std::collections::HashMap;

/// Compute aggregate metrics from a set of trial results.
pub fn compute_aggregate(trials: &[TrialResult]) -> AggregateMetrics {
    if trials.is_empty() {
        return AggregateMetrics {
            pass_rate: 0.0,
            mean_iterations: 0.0,
            mean_duration_ms: 0.0,
            trial_count: 0,
        };
    }

    let n = trials.len() as f64;
    let passed = trials.iter().filter(|t| t.passed).count() as f64;
    let total_iter: f64 = trials.iter().map(|t| t.iterations_used as f64).sum();
    let total_dur: f64 = trials.iter().map(|t| t.duration_ms as f64).sum();

    AggregateMetrics {
        pass_rate: passed / n,
        mean_iterations: total_iter / n,
        mean_duration_ms: total_dur / n,
        trial_count: trials.len() as u32,
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
        MultiWorkflowAggregation::AllMustPass => {
            per_wf_pass_rates.iter().cloned().fold(f64::INFINITY, f64::min)
        }
        MultiWorkflowAggregation::AnyPass => {
            per_wf_pass_rates.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
        }
    };

    // Iterations and duration always averaged across all trials
    let n = trials.len() as f64;
    let total_iter: f64 = trials.iter().map(|t| t.iterations_used as f64).sum();
    let total_dur: f64 = trials.iter().map(|t| t.duration_ms as f64).sum();

    AggregateMetrics {
        pass_rate: combined_pass_rate,
        mean_iterations: total_iter / n,
        mean_duration_ms: total_dur / n,
        trial_count: trials.len() as u32,
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
        PrimaryMetric::PassRate => {
            compare_pass_rate(experiment, control, experiment_trials, control_trials, criteria)
        }
        PrimaryMetric::MeanIterations => {
            compare_lower_is_better(
                experiment.mean_iterations,
                control.mean_iterations,
                "mean_iterations",
                experiment_trials.iter().map(|t| t.iterations_used as f64).collect::<Vec<_>>(),
                control_trials.iter().map(|t| t.iterations_used as f64).collect::<Vec<_>>(),
                criteria,
            )
        }
        PrimaryMetric::MeanDuration => {
            compare_lower_is_better(
                experiment.mean_duration_ms,
                control.mean_duration_ms,
                "mean_duration_ms",
                experiment_trials.iter().map(|t| t.duration_ms as f64).collect::<Vec<_>>(),
                control_trials.iter().map(|t| t.duration_ms as f64).collect::<Vec<_>>(),
                criteria,
            )
        }
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
            format!("pass_rate: experiment={:.3}, control=0 (infinite improvement)", exp_val),
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
        reason.push_str(&format!(", p={:.4} (threshold={:.2})", p, criteria.significance_threshold));
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
        return (true, format!("{}: experiment=0 (perfect)", metric_name), None);
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
        reason.push_str(&format!(", p={:.4} (threshold={:.2})", p, criteria.significance_threshold));
        if ratio_ok && !sig_ok {
            reason.push_str(" [ratio OK but not significant]");
        }
    }

    (accepted, reason, p_value)
}

// =============================================================================
// Statistical tests (pure Rust, no external stats crate needed)
// =============================================================================

/// Fisher's exact test approximation for 2x2 contingency table.
/// Computes a one-sided p-value for whether experiment pass rate > control pass rate.
///
/// Uses the normal approximation to the binomial (valid for n >= 5).
fn fisher_exact_p(experiment_trials: &[TrialResult], control_trials: &[TrialResult]) -> f64 {
    let n_exp = experiment_trials.len() as f64;
    let n_ctrl = control_trials.len() as f64;
    let x_exp = experiment_trials.iter().filter(|t| t.passed).count() as f64;
    let x_ctrl = control_trials.iter().filter(|t| t.passed).count() as f64;

    let p_exp = x_exp / n_exp;
    let p_ctrl = x_ctrl / n_ctrl;

    // Pooled proportion under H0
    let p_pool = (x_exp + x_ctrl) / (n_exp + n_ctrl);
    if p_pool == 0.0 || p_pool == 1.0 {
        // All passed or all failed — no variance
        return if p_exp > p_ctrl { 0.0 } else { 1.0 };
    }

    let se = (p_pool * (1.0 - p_pool) * (1.0 / n_exp + 1.0 / n_ctrl)).sqrt();
    if se == 0.0 {
        return 1.0;
    }

    let z = (p_exp - p_ctrl) / se;
    // One-sided p-value: P(Z > z)
    1.0 - normal_cdf(z)
}

/// Welch's t-test for unequal variances.
/// Returns a two-sided p-value testing whether the means differ.
fn welch_t_test(a: &[f64], b: &[f64]) -> f64 {
    let n_a = a.len() as f64;
    let n_b = b.len() as f64;

    let mean_a = a.iter().sum::<f64>() / n_a;
    let mean_b = b.iter().sum::<f64>() / n_b;

    let var_a = a.iter().map(|x| (x - mean_a).powi(2)).sum::<f64>() / (n_a - 1.0);
    let var_b = b.iter().map(|x| (x - mean_b).powi(2)).sum::<f64>() / (n_b - 1.0);

    let se_sq = var_a / n_a + var_b / n_b;
    if se_sq == 0.0 {
        return if (mean_a - mean_b).abs() < f64::EPSILON {
            1.0
        } else {
            0.0
        };
    }

    let t = (mean_a - mean_b) / se_sq.sqrt();

    // Welch-Satterthwaite degrees of freedom
    let num = se_sq.powi(2);
    let denom = (var_a / n_a).powi(2) / (n_a - 1.0) + (var_b / n_b).powi(2) / (n_b - 1.0);
    let df = if denom > 0.0 { num / denom } else { 1.0 };

    // Two-sided p-value using t-distribution approximation
    // For df > 30, normal approximation is adequate
    let p = if df > 30.0 {
        2.0 * (1.0 - normal_cdf(t.abs()))
    } else {
        2.0 * (1.0 - t_cdf(t.abs(), df))
    };

    p.clamp(0.0, 1.0)
}

/// Standard normal CDF approximation (Abramowitz & Stegun, eq. 26.2.17).
/// Maximum error: 7.5e-8.
fn normal_cdf(x: f64) -> f64 {
    if x < -8.0 {
        return 0.0;
    }
    if x > 8.0 {
        return 1.0;
    }

    let t = 1.0 / (1.0 + 0.2316419 * x.abs());
    let d = 0.3989422804014327; // 1/sqrt(2*pi)
    let p = d * (-x * x / 2.0).exp();
    let poly = t * (0.319381530 + t * (-0.356563782 + t * (1.781477937 + t * (-1.821255978 + t * 1.330274429))));

    if x >= 0.0 {
        1.0 - p * poly
    } else {
        p * poly
    }
}

/// Student's t-distribution CDF approximation.
/// Uses the regularized incomplete beta function approximation for small df.
fn t_cdf(t: f64, df: f64) -> f64 {
    // Use the relationship: F_t(t|df) = 1 - 0.5 * I_x(df/2, 0.5)
    // where x = df / (df + t^2)
    let x = df / (df + t * t);
    1.0 - 0.5 * regularized_incomplete_beta(x, df / 2.0, 0.5)
}

/// Regularized incomplete beta function I_x(a, b) via continued fraction (Lentz's method).
/// Sufficient precision for p-value computation.
fn regularized_incomplete_beta(x: f64, a: f64, b: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }

    // Use the symmetry relation if x > (a+1)/(a+b+2)
    if x > (a + 1.0) / (a + b + 2.0) {
        return 1.0 - regularized_incomplete_beta(1.0 - x, b, a);
    }

    let ln_beta = ln_gamma(a) + ln_gamma(b) - ln_gamma(a + b);
    let front = (a * x.ln() + b * (1.0 - x).ln() - ln_beta).exp() / a;

    // Continued fraction (Lentz's method)
    let mut c = 1.0;
    let mut d = 1.0 - (a + b) * x / (a + 1.0);
    if d.abs() < 1e-30 {
        d = 1e-30;
    }
    d = 1.0 / d;
    let mut f = d;

    for m in 1..200u32 {
        let m_f = m as f64;

        // Even step
        let num_even = m_f * (b - m_f) * x / ((a + 2.0 * m_f - 1.0) * (a + 2.0 * m_f));
        d = 1.0 + num_even * d;
        if d.abs() < 1e-30 {
            d = 1e-30;
        }
        c = 1.0 + num_even / c;
        if c.abs() < 1e-30 {
            c = 1e-30;
        }
        d = 1.0 / d;
        f *= d * c;

        // Odd step
        let num_odd = -((a + m_f) * (a + b + m_f) * x)
            / ((a + 2.0 * m_f) * (a + 2.0 * m_f + 1.0));
        d = 1.0 + num_odd * d;
        if d.abs() < 1e-30 {
            d = 1e-30;
        }
        c = 1.0 + num_odd / c;
        if c.abs() < 1e-30 {
            c = 1e-30;
        }
        d = 1.0 / d;
        let delta = d * c;
        f *= delta;

        if (delta - 1.0).abs() < 1e-10 {
            break;
        }
    }

    (front * f).clamp(0.0, 1.0)
}

/// Stirling's approximation of ln(Gamma(x)) for x > 0.
fn ln_gamma(x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    // Lanczos approximation (g=7, n=9)
    #[allow(clippy::excessive_precision)]
    let coeffs: [f64; 9] = [
        0.99999999999980993,
        676.5203681218851,
        -1259.1392167224028,
        771.32342877765313,
        -176.61502916214059,
        12.507343278686905,
        -0.13857109526572012,
        9.9843695780195716e-6,
        1.5056327351493116e-7,
    ];

    if x < 0.5 {
        // Reflection formula
        let pi = std::f64::consts::PI;
        return (pi / (pi * x).sin()).ln() - ln_gamma(1.0 - x);
    }

    let x = x - 1.0;
    let mut sum = coeffs[0];
    for (i, &c) in coeffs.iter().enumerate().skip(1) {
        sum += c / (x + i as f64);
    }

    let t = x + 7.5;
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + sum.ln()
}

/// Format experiment results as a TSV table.
pub fn format_results_tsv(
    results: &[(u32, super::types::ExperimentResult)],
) -> String {
    let mut out = String::new();
    out.push_str("experiment\tconfig\tpass_rate\tmean_iterations\tmean_duration_ms\taccepted\tp_value\treason\n");
    for (num, result) in results {
        let p_str = result.p_value.map_or("-".to_string(), |p| format!("{:.4}", p));
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
            },
            TrialResult {
                task_run_id: "t2".into(),
                passed: false,
                iterations_used: 10,
                duration_ms: 5000,
                workflow_id: None,
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
            TrialResult { task_run_id: "e1".into(), passed: true, iterations_used: 3, duration_ms: 1000, workflow_id: None },
            TrialResult { task_run_id: "e2".into(), passed: true, iterations_used: 4, duration_ms: 1200, workflow_id: None },
            TrialResult { task_run_id: "e3".into(), passed: true, iterations_used: 3, duration_ms: 1100, workflow_id: None },
        ];
        let ctrl_trials = vec![
            TrialResult { task_run_id: "c1".into(), passed: true, iterations_used: 5, duration_ms: 2000, workflow_id: None },
            TrialResult { task_run_id: "c2".into(), passed: false, iterations_used: 10, duration_ms: 5000, workflow_id: None },
            TrialResult { task_run_id: "c3".into(), passed: false, iterations_used: 10, duration_ms: 5000, workflow_id: None },
        ];
        let exp_agg = compute_aggregate(&exp_trials);
        let ctrl_agg = compute_aggregate(&ctrl_trials);
        let criteria = AcceptanceCriteria::default();
        let (accepted, _reason, p_value) = compare_to_control(&exp_agg, &ctrl_agg, &exp_trials, &ctrl_trials, &criteria);
        assert!(accepted);
        assert!(p_value.is_some());
    }

    #[test]
    fn test_compare_pass_rate_no_improvement() {
        let exp_trials = vec![
            TrialResult { task_run_id: "e1".into(), passed: false, iterations_used: 10, duration_ms: 5000, workflow_id: None },
            TrialResult { task_run_id: "e2".into(), passed: true, iterations_used: 5, duration_ms: 2000, workflow_id: None },
        ];
        let ctrl_trials = vec![
            TrialResult { task_run_id: "c1".into(), passed: true, iterations_used: 3, duration_ms: 1000, workflow_id: None },
            TrialResult { task_run_id: "c2".into(), passed: true, iterations_used: 4, duration_ms: 1500, workflow_id: None },
        ];
        let exp_agg = compute_aggregate(&exp_trials);
        let ctrl_agg = compute_aggregate(&ctrl_trials);
        let criteria = AcceptanceCriteria::default();
        let (accepted, _reason, _p) = compare_to_control(&exp_agg, &ctrl_agg, &exp_trials, &ctrl_trials, &criteria);
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
        assert!(p < 0.01, "Very different samples should have p < 0.01, got {}", p);
    }
}
