//! Shared statistical tests for evaluation, canary verdicts, and comparisons.
//!
//! Pure Rust implementations — no external stats crate needed.
//! Statistical utilities so meta_optimizer,
//! canary, comparison, and eval systems can all reuse them.

// =============================================================================
// Proportion tests (binary pass/fail data)
// =============================================================================

/// Normal-approximation two-proportion z-test (one-sided).
///
/// Tests H₁: p_experiment > p_control.
/// Returns a one-sided p-value.
///
/// # Arguments
/// * `exp_passed` / `exp_total` — experiment successes and total trials
/// * `ctrl_passed` / `ctrl_total` — control successes and total trials
pub(crate) fn proportion_z_test_onesided(
    exp_passed: u64,
    exp_total: u64,
    ctrl_passed: u64,
    ctrl_total: u64,
) -> f64 {
    // Guard against division by zero when either group has no observations
    if exp_total == 0 || ctrl_total == 0 {
        return 1.0;
    }

    let n_exp = exp_total as f64;
    let n_ctrl = ctrl_total as f64;
    let x_exp = exp_passed as f64;
    let x_ctrl = ctrl_passed as f64;

    let p_exp = x_exp / n_exp;
    let p_ctrl = x_ctrl / n_ctrl;

    // Pooled proportion under H₀
    let p_pool = (x_exp + x_ctrl) / (n_exp + n_ctrl);
    if p_pool == 0.0 || p_pool == 1.0 {
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

// =============================================================================
// Continuous tests
// =============================================================================

/// Welch's t-test for unequal variances (two-sided).
///
/// Returns a two-sided p-value testing whether the means of `a` and `b` differ.
/// Requires at least 2 samples per group.
pub fn welch_t_test(a: &[f64], b: &[f64]) -> f64 {
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

/// Paired t-test on per-observation deltas (**one-sided**).
///
/// Tests H₁: `mean(deltas) > 0`, i.e. "the experimental arm beats the control
/// arm on the *same* observations". Returns a **one-sided** p-value, matching
/// the contract [`StatisticalAnalysis::p_value`] documents and that
/// [`compute_verdict`] relies on when it derives `p_regression = 1 - p`.
///
/// Deliberately *not* [`welch_t_test`]: that test is **unpaired and two-sided**,
/// so feeding its p-value into [`compute_verdict`] silently converts a
/// significant regression into `Verdict::Neutral`.
///
/// `deltas[i]` must be `experiment[i] - control[i]` for the same observation
/// `i`. Positions where either arm is missing must be **excluded by the caller**
/// before the call — never imputed as `0.0`, which fabricates a "no change"
/// observation.
///
/// Degenerate inputs:
/// * `n < 2` — no variance is estimable, so the test cannot run: returns `1.0`
///   ("no evidence of improvement"), never `0.0`.
/// * zero variance — every delta is identical, so the direction is certain:
///   returns `0.0` for a strictly positive mean and `1.0` otherwise.
pub fn paired_t_test(deltas: &[f64]) -> f64 {
    let n = deltas.len();
    if n < 2 {
        return 1.0;
    }

    let n_f = n as f64;
    let mean = deltas.iter().sum::<f64>() / n_f;
    let var = deltas.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / (n_f - 1.0);

    if var <= 0.0 {
        return if mean > 0.0 { 0.0 } else { 1.0 };
    }

    let se = (var / n_f).sqrt();
    let t = mean / se;
    let df = n_f - 1.0;

    // One-sided p-value: P(T_df > t)
    (1.0 - t_cdf_signed(t, df)).clamp(0.0, 1.0)
}

// =============================================================================
// Effect size measures
// =============================================================================

/// Cohen's h — effect size for comparing two proportions.
///
/// h = 2 * (arcsin(√p₁) - arcsin(√p₂))
///
/// Interpretation: |h| < 0.2 small, 0.2–0.8 medium, > 0.8 large.
pub(crate) fn cohens_h(p1: f64, p2: f64) -> f64 {
    2.0 * (p1.sqrt().asin() - p2.sqrt().asin())
}

// =============================================================================
// Confidence intervals
// =============================================================================

/// Wilson score confidence interval for a binomial proportion.
///
/// More accurate than the normal (Wald) interval, especially for small n or extreme p.
///
/// Returns (lower, upper) bounds at the given confidence level (e.g., 0.95).
pub fn wilson_confidence_interval(successes: u64, total: u64, confidence: f64) -> (f64, f64) {
    if total == 0 {
        return (0.0, 1.0);
    }

    let n = total as f64;
    let p_hat = successes as f64 / n;

    // z for the given confidence level (two-tailed)
    let alpha = 1.0 - confidence;
    let z = normal_quantile(1.0 - alpha / 2.0);
    let z2 = z * z;

    let denom = 1.0 + z2 / n;
    let center = p_hat + z2 / (2.0 * n);
    let spread = z * (p_hat * (1.0 - p_hat) / n + z2 / (4.0 * n * n)).sqrt();

    let lower = ((center - spread) / denom).clamp(0.0, 1.0);
    let upper = ((center + spread) / denom).clamp(0.0, 1.0);

    (lower, upper)
}

/// Confidence interval for the difference between two proportions (Newcombe-Wilson).
///
/// Returns (lower, upper) for (p₁ - p₂) at the given confidence level.
pub(crate) fn proportion_diff_ci(
    successes_1: u64,
    total_1: u64,
    successes_2: u64,
    total_2: u64,
    confidence: f64,
) -> (f64, f64) {
    let p1 = if total_1 == 0 {
        0.0
    } else {
        successes_1 as f64 / total_1 as f64
    };
    let p2 = if total_2 == 0 {
        0.0
    } else {
        successes_2 as f64 / total_2 as f64
    };
    let diff = p1 - p2;

    let (l1, u1) = wilson_confidence_interval(successes_1, total_1, confidence);
    let (l2, u2) = wilson_confidence_interval(successes_2, total_2, confidence);

    // Newcombe method
    let lower = diff - ((p1 - l1).powi(2) + (u2 - p2).powi(2)).sqrt();
    let upper = diff + ((u1 - p1).powi(2) + (p2 - l2).powi(2)).sqrt();

    (lower.clamp(-1.0, 1.0), upper.clamp(-1.0, 1.0))
}

// =============================================================================
// Statistical verdict (shared across canary, snapshots, comparisons)
// =============================================================================

/// Result of a statistical comparison between two groups' success rates.
#[derive(Debug, Clone)]
pub struct StatisticalAnalysis {
    /// One-sided p-value: is the experiment better than control?
    pub p_value: Option<f64>,
    /// 95% confidence interval for the difference (in percentage points).
    pub confidence_interval: Option<(f64, f64)>,
    /// Cohen's h effect size.
    pub effect_size: Option<f64>,
}

/// Compute p-value, confidence interval, and effect size for a proportion comparison.
///
/// `exp` = (successes, total) for the experimental group.
/// `ctrl` = (successes, total) for the control group.
///
/// Returns `None` fields when either group has fewer than `min_samples` observations.
pub fn proportion_analysis(
    exp: (u64, u64),
    ctrl: (u64, u64),
    min_samples: u64,
) -> StatisticalAnalysis {
    let (exp_succ, exp_total) = exp;
    let (ctrl_succ, ctrl_total) = ctrl;

    if exp_total < min_samples || ctrl_total < min_samples {
        return StatisticalAnalysis {
            p_value: None,
            confidence_interval: None,
            effect_size: None,
        };
    }

    let p = proportion_z_test_onesided(exp_succ, exp_total, ctrl_succ, ctrl_total);

    let ci = proportion_diff_ci(exp_succ, exp_total, ctrl_succ, ctrl_total, 0.95);
    let ci_pp = (ci.0 * 100.0, ci.1 * 100.0);

    let exp_prop = exp_succ as f64 / exp_total as f64;
    let ctrl_prop = ctrl_succ as f64 / ctrl_total as f64;
    let h = cohens_h(exp_prop, ctrl_prop);

    StatisticalAnalysis {
        p_value: Some(p),
        confidence_interval: Some(ci_pp),
        effect_size: Some(h),
    }
}

/// Compute p-value, confidence interval, and effect size for a **paired**
/// continuous comparison.
///
/// The continuous counterpart of [`proportion_analysis`], shaped so its output
/// can be handed straight to [`compute_verdict`]:
///
/// * `p_value` — **one-sided** ([`paired_t_test`]), as
///   [`StatisticalAnalysis::p_value`] documents and [`compute_verdict`] relies on.
/// * `confidence_interval` — the 95% CI of the **mean delta**.
/// * `effect_size` — Cohen's dz for paired samples (`mean(delta) / sd(delta)`),
///   *not* Cohen's h, which is a proportion-only measure.
///
/// # UNITS — read this before calling
///
/// `StatisticalAnalysis` is documented in **percentage points**, and
/// [`compute_verdict`]'s `sr_delta_pp` argument plus every field of
/// [`VerdictThresholds`] are in pp. This function does **no** scaling: the
/// returned `confidence_interval` is in exactly the units of `deltas`.
///
/// So a caller comparing a continuous score on a `0.0..=1.0` scale must scale
/// each delta by 100 **before** calling — a raw `0..1` delta is 100× smaller
/// than a pp delta, and passing it unscaled silently changes the meaning of
/// every threshold. (`p_value` and `effect_size` are scale-invariant; only the
/// CI and the `sr_delta_pp` you pass alongside it are not.)
///
/// `deltas[i]` must be `experiment[i] - control[i]` for the same observation.
/// Unpaired positions are the caller's job to exclude — see [`paired_t_test`].
///
/// Returns all-`None` when fewer than `min_samples` (or fewer than 2) paired
/// observations are supplied, which drives [`compute_verdict`] into its
/// fallback arm; pair that with a non-zero `VerdictThresholds::min_runs` to get
/// `Verdict::InsufficientData` instead.
pub fn paired_analysis(deltas: &[f64], min_samples: usize) -> StatisticalAnalysis {
    if deltas.len() < min_samples.max(2) {
        return StatisticalAnalysis {
            p_value: None,
            confidence_interval: None,
            effect_size: None,
        };
    }

    let n = deltas.len() as f64;
    let mean = deltas.iter().sum::<f64>() / n;
    let var = deltas.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let sd = var.sqrt();

    let p = paired_t_test(deltas);

    // Zero variance: the mean delta is known exactly, so the CI collapses to a
    // point and Cohen's dz (mean/sd) is undefined rather than infinite.
    let (ci, effect_size) = if sd > 0.0 {
        let se = sd / n.sqrt();
        let t_crit = t_quantile(0.975, n - 1.0);
        ((mean - t_crit * se, mean + t_crit * se), Some(mean / sd))
    } else {
        ((mean, mean), None)
    };

    StatisticalAnalysis {
        p_value: Some(p),
        confidence_interval: Some(ci),
        effect_size,
    }
}

/// Configurable thresholds for statistical verdict decisions.
#[derive(Debug, Clone)]
pub struct VerdictThresholds {
    /// p-value threshold for significance (default: 0.05).
    pub alpha: f64,
    /// Minimum success-rate delta (pp) to declare regression with stats.
    pub regression_delta_pp: f64,
    /// Minimum success-rate delta (pp) to declare improvement with stats.
    pub improvement_delta_pp: f64,
    /// Fallback regression threshold when stats unavailable.
    pub fallback_regression_pp: f64,
    /// Fallback improvement threshold when stats unavailable.
    pub fallback_improvement_pp: f64,
    /// Minimum total runs before any verdict (returns `insufficient_data` below this).
    pub min_runs: u64,
}

impl VerdictThresholds {
    /// Thresholds for canary evaluation (more cautious: promotes unless clearly worse).
    pub fn canary() -> Self {
        Self {
            alpha: 0.05,
            regression_delta_pp: -3.0,
            improvement_delta_pp: -2.0, // canary promotes if "not meaningfully worse"
            fallback_regression_pp: -5.0,
            fallback_improvement_pp: -2.0,
            min_runs: 0, // canary uses its own min_runs_met check
        }
    }

    /// Thresholds for recommendation/snapshot evaluation.
    pub fn recommendation() -> Self {
        Self {
            alpha: 0.05,
            regression_delta_pp: -3.0,
            improvement_delta_pp: 2.0,
            fallback_regression_pp: -5.0,
            fallback_improvement_pp: 3.0,
            min_runs: 5,
        }
    }

    /// Thresholds for a **held-out validation gate** on prompt optimization
    /// (see `workflow_generation::gepa_optimizer`).
    ///
    /// Units are the same percentage points every other constructor here uses;
    /// a caller scoring on a `0.0..=1.0` scale scales its deltas by 100 first
    /// (see [`paired_analysis`]).
    ///
    /// Stricter on the accept side than [`Self::recommendation`] because there
    /// is no canary stage behind this gate — the decision adopts the prompt
    /// outright, so it demands a significant gain of at least 5 pp rather than
    /// 2 pp. `min_runs` is the **held-out floor**: below 10 *paired* examples
    /// the gate reports `Verdict::InsufficientData` rather than deciding, which
    /// is also what keeps an all-excluded (zero paired) run from reading as a
    /// 0.0 delta, i.e. "no change".
    pub fn held_out_gate() -> Self {
        Self {
            alpha: 0.05,
            regression_delta_pp: -2.0,
            improvement_delta_pp: 5.0,
            fallback_regression_pp: -5.0,
            fallback_improvement_pp: 10.0,
            min_runs: 10,
        }
    }
}

/// Possible verdict outcomes from a statistical comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Experiment is better (or at least not meaningfully worse for canary).
    Positive,
    /// Experiment is significantly worse.
    Negative,
    /// Not enough signal to decide.
    Neutral,
    /// Not enough data to run statistical tests.
    InsufficientData,
}

impl Verdict {
    /// Convert to canary-style string.
    pub fn as_canary_str(&self) -> &'static str {
        match self {
            Verdict::Positive => "promote",
            Verdict::Negative => "rollback",
            Verdict::Neutral | Verdict::InsufficientData => "continue",
        }
    }

    /// Convert to recommendation-style string.
    pub fn as_recommendation_str(&self) -> &'static str {
        match self {
            Verdict::Positive => "improved",
            Verdict::Negative => "regressed",
            Verdict::Neutral => "neutral",
            Verdict::InsufficientData => "insufficient_data",
        }
    }
}

/// Compute a statistical verdict given success rates and analysis results.
///
/// # Arguments
/// * `sr_delta_pp` — success rate difference in percentage points (exp - ctrl)
/// * `analysis` — pre-computed statistical analysis (from `proportion_analysis`)
/// * `exp_total` — total runs in the experimental group
/// * `thresholds` — configurable decision thresholds
pub fn compute_verdict(
    sr_delta_pp: f64,
    analysis: &StatisticalAnalysis,
    exp_total: u64,
    thresholds: &VerdictThresholds,
) -> Verdict {
    if thresholds.min_runs > 0 && exp_total < thresholds.min_runs {
        return Verdict::InsufficientData;
    }

    if let Some(p_improvement) = analysis.p_value {
        // The analysis already computed p for "exp better than ctrl".
        // We need p for the reverse: "ctrl better than exp" (i.e., regression).
        // For a z-test, p_reverse ≈ 1.0 - p_improvement (by symmetry).
        let p_regression = 1.0 - p_improvement;

        if p_regression < thresholds.alpha && sr_delta_pp < thresholds.regression_delta_pp {
            return Verdict::Negative;
        }

        if p_improvement < thresholds.alpha && sr_delta_pp > thresholds.improvement_delta_pp {
            return Verdict::Positive;
        }

        // Canary special case: promote if not meaningfully worse
        // (CI lower bound above fallback_regression threshold)
        if thresholds.improvement_delta_pp < 0.0 {
            // Only applies when improvement_delta is negative (canary mode)
            if sr_delta_pp > thresholds.improvement_delta_pp
                && analysis
                    .confidence_interval
                    .is_some_and(|ci| ci.0 > thresholds.fallback_regression_pp)
            {
                return Verdict::Positive;
            }
        }

        Verdict::Neutral
    } else {
        // Fallback to simple threshold comparison when stats unavailable
        if sr_delta_pp < thresholds.fallback_regression_pp {
            Verdict::Negative
        } else if sr_delta_pp > thresholds.fallback_improvement_pp {
            Verdict::Positive
        } else {
            Verdict::Neutral
        }
    }
}

// =============================================================================
// Distribution functions (pure Rust)
// =============================================================================

/// Standard normal CDF approximation (Abramowitz & Stegun, eq. 26.2.17).
/// Maximum error: 7.5e-8.
pub fn normal_cdf(x: f64) -> f64 {
    if x < -8.0 {
        return 0.0;
    }
    if x > 8.0 {
        return 1.0;
    }

    let t = 1.0 / (1.0 + 0.2316419 * x.abs());
    let d = 0.3989422804014327; // 1/sqrt(2*pi)
    let p = d * (-x * x / 2.0).exp();
    let poly = t
        * (0.319381530
            + t * (-0.356563782 + t * (1.781477937 + t * (-1.821255978 + t * 1.330274429))));

    if x >= 0.0 {
        1.0 - p * poly
    } else {
        p * poly
    }
}

/// Inverse normal CDF (quantile function).
///
/// Uses a rational initial approximation refined with one Newton-Raphson step
/// for accuracy across the full range.
pub(crate) fn normal_quantile(p: f64) -> f64 {
    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }
    if (p - 0.5).abs() < f64::EPSILON {
        return 0.0;
    }

    // Rational approximation for central region (|q| <= 0.425)
    let q = p - 0.5;
    let mut z = if q.abs() <= 0.425 {
        let r = 0.180625 - q * q;
        q * (((((((2.509_080_928_730_122_7e3 * r + 3.343_057_558_358_813e4) * r
            + 6.726_577_092_700_87e4)
            * r
            + 4.592_195_393_154_987e4)
            * r
            + 1.373_169_376_550_946e4)
            * r
            + 1.971_590_950_306_551_3e3)
            * r
            + 1.331_416_676_407_822_6e2)
            * r
            + 3.387_132_872_796_366_5)
            / (((((((5.226_495_278_852_854e3 * r + 2.872_908_573_572_194_3e4) * r
                + 3.930_789_580_009_271e4)
                * r
                + 2.121_379_430_158_659_7e4)
                * r
                + 5.394_196_021_424_751e3)
                * r
                + 6.871_870_074_920_579e2)
                * r
                + 4.231_333_070_160_091e1)
                * r
                + 1.0)
    } else {
        // Tail: use simple Hastings approximation as starting point
        let r = if q < 0.0 { p } else { 1.0 - p };
        let t = (-2.0 * r.ln()).sqrt();
        let sign = if q < 0.0 { -1.0 } else { 1.0 };
        // Hastings approximation
        let c0 = 2.515517;
        let c1 = 0.802853;
        let c2 = 0.010328;
        let d1 = 1.432788;
        let d2 = 0.189269;
        let d3 = 0.001308;
        sign * (t - (c0 + c1 * t + c2 * t * t) / (1.0 + d1 * t + d2 * t * t + d3 * t * t * t))
    };

    // Newton-Raphson refinement (2 iterations for high accuracy)
    let inv_sqrt_2pi = 0.3989422804014327;
    for _ in 0..2 {
        let err = normal_cdf(z) - p;
        let pdf = inv_sqrt_2pi * (-0.5 * z * z).exp();
        if pdf.abs() > 1e-300 {
            z -= err / pdf;
        }
    }

    z
}

/// Student's t-distribution CDF approximation.
/// Uses the regularized incomplete beta function for small df.
pub fn t_cdf(t: f64, df: f64) -> f64 {
    let x = df / (df + t * t);
    1.0 - 0.5 * regularized_incomplete_beta(x, df / 2.0, 0.5)
}

/// Student's t CDF valid over the **whole** real line.
///
/// [`t_cdf`] is only correct for `t >= 0`: its `x = df / (df + t²)` is even in
/// `t`, so `t_cdf(-2.0, 4.0) == t_cdf(2.0, 4.0)` — always `>= 0.5`. Callers
/// that can see a negative statistic (any one-sided test) must go through here.
pub fn t_cdf_signed(t: f64, df: f64) -> f64 {
    if t >= 0.0 {
        t_cdf(t, df)
    } else {
        1.0 - t_cdf(-t, df)
    }
}

/// Inverse Student's t CDF (quantile function), by bisection on [`t_cdf_signed`].
///
/// There is no closed form; 100 bisection steps over `[-1e3, 1e3]` converge far
/// below the precision of the CDF approximation itself.
pub fn t_quantile(p: f64, df: f64) -> f64 {
    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }

    let mut lo = -1e3;
    let mut hi = 1e3;
    for _ in 0..100 {
        let mid = 0.5 * (lo + hi);
        if t_cdf_signed(mid, df) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// Regularized incomplete beta function I_x(a, b) via continued fraction (Lentz's method).
pub fn regularized_incomplete_beta(x: f64, a: f64, b: f64) -> f64 {
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
        let num_odd = -((a + m_f) * (a + b + m_f) * x) / ((a + 2.0 * m_f) * (a + 2.0 * m_f + 1.0));
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

/// Lanczos approximation of ln(Gamma(x)) for x > 0 (g=7, n=9).
pub fn ln_gamma(x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
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

// =============================================================================
// Copeland score (dueling bandits ranking)
// =============================================================================

/// Compute Copeland score for a candidate given pairwise win/loss counts.
///
/// Score = (wins - losses) / max(1, num_opponents), normalized to [-1.0, 1.0].
/// Used by the duel engine to rank prompt candidates after pairwise duels.
pub fn copeland_score(wins: u32, losses: u32, num_opponents: u32) -> f64 {
    let denom = num_opponents.max(1) as f64;
    (wins as f64 - losses as f64) / denom
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normal_cdf_basic() {
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-6);
        assert!((normal_cdf(1.96) - 0.975).abs() < 1e-3);
        assert!(normal_cdf(-8.0) < 1e-10);
        assert!(normal_cdf(8.0) > 1.0 - 1e-10);
    }

    #[test]
    fn test_normal_quantile_roundtrip() {
        for &p in &[0.025, 0.05, 0.1, 0.5, 0.9, 0.95, 0.975] {
            let z = normal_quantile(p);
            let p_back = normal_cdf(z);
            assert!(
                (p - p_back).abs() < 1e-5,
                "roundtrip failed for p={}: z={}, p_back={}",
                p,
                z,
                p_back
            );
        }
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

    #[test]
    fn test_proportion_z_test_clear_winner() {
        // 9/10 vs 3/10
        let p = proportion_z_test_onesided(9, 10, 3, 10);
        assert!(
            p < 0.01,
            "Clear improvement should have p < 0.01, got {}",
            p
        );
    }

    #[test]
    fn test_proportion_z_test_no_difference() {
        // 5/10 vs 5/10
        let p = proportion_z_test_onesided(5, 10, 5, 10);
        assert!(p > 0.4, "No difference should have p > 0.4, got {}", p);
    }

    #[test]
    fn test_proportion_z_test_worse() {
        // 2/10 vs 8/10 — experiment is worse
        let p = proportion_z_test_onesided(2, 10, 8, 10);
        assert!(p > 0.95, "Experiment worse should have p > 0.95, got {}", p);
    }

    #[test]
    fn test_cohens_h() {
        // Same proportions → h = 0
        assert!((cohens_h(0.5, 0.5)).abs() < 1e-10);
        // Large difference
        let h = cohens_h(0.9, 0.1);
        assert!(h > 1.0, "Large difference should have |h| > 1.0, got {}", h);
        // Direction: p1 > p2 → positive h
        assert!(cohens_h(0.8, 0.3) > 0.0);
    }

    #[test]
    fn test_wilson_ci_basic() {
        // Wilson CI for 7/10 at 95%: approximately (0.35, 0.93)
        let (lower, upper) = wilson_confidence_interval(7, 10, 0.95);
        assert!(lower > 0.3 && lower < 0.7, "lower={}", lower);
        assert!(upper > 0.7 && upper <= 1.0, "upper={}", upper);
        assert!(lower < upper);
    }

    #[test]
    fn test_wilson_ci_zero() {
        // Wilson CI for 0/10: lower ≈ 0, upper bounded but can be up to ~0.31
        let (lower, upper) = wilson_confidence_interval(0, 10, 0.95);
        assert!(lower < 0.01, "lower={}", lower);
        assert!(upper > 0.0 && upper < 1.0, "upper={}", upper);
    }

    #[test]
    fn test_wilson_ci_all_pass() {
        // Wilson CI for 10/10: lower bounded, upper ≈ 1
        let (lower, upper) = wilson_confidence_interval(10, 10, 0.95);
        assert!(lower > 0.5, "lower={}", lower);
        assert!(upper > 0.9 && upper <= 1.0, "upper={}", upper);
    }

    #[test]
    fn test_wilson_ci_empty() {
        let (lower, upper) = wilson_confidence_interval(0, 0, 0.95);
        assert_eq!(lower, 0.0);
        assert_eq!(upper, 1.0);
    }

    #[test]
    fn test_proportion_diff_ci() {
        // 8/10 vs 3/10 — difference should be positive
        let (lower, upper) = proportion_diff_ci(8, 10, 3, 10, 0.95);
        assert!(lower > 0.0, "lower={} should be > 0 for clear diff", lower);
        assert!(upper > lower);
    }

    #[test]
    fn test_proportion_diff_ci_no_difference() {
        // 5/10 vs 5/10 — CI should contain 0
        let (lower, upper) = proportion_diff_ci(5, 10, 5, 10, 0.95);
        assert!(lower < 0.0, "lower={} should be < 0", lower);
        assert!(upper > 0.0, "upper={} should be > 0", upper);
    }

    #[test]
    fn test_t_cdf_basic() {
        // t=0 should give CDF=0.5 for any df
        let cdf = t_cdf(0.0, 10.0);
        assert!(
            (cdf - 0.5).abs() < 1e-6,
            "t_cdf(0, 10) should be ~0.5, got {}",
            cdf
        );
    }

    // ── Statistical verdict tests ──────────────────────────────────────

    #[test]
    fn test_proportion_analysis_sufficient_data() {
        let a = proportion_analysis((8, 10), (3, 10), 2);
        assert!(a.p_value.is_some());
        assert!(a.confidence_interval.is_some());
        assert!(a.effect_size.is_some());
        assert!(a.p_value.unwrap() < 0.05);
    }

    #[test]
    fn test_proportion_analysis_insufficient_data() {
        let a = proportion_analysis((1, 1), (0, 1), 2);
        assert!(a.p_value.is_none());
        assert!(a.confidence_interval.is_none());
    }

    #[test]
    fn test_verdict_clear_improvement() {
        let analysis = proportion_analysis((9, 10), (3, 10), 2);
        let v = compute_verdict(60.0, &analysis, 10, &VerdictThresholds::recommendation());
        assert_eq!(v, Verdict::Positive);
        assert_eq!(v.as_recommendation_str(), "improved");
    }

    #[test]
    fn test_verdict_clear_regression() {
        let analysis = proportion_analysis((2, 10), (8, 10), 2);
        let v = compute_verdict(-60.0, &analysis, 10, &VerdictThresholds::recommendation());
        assert_eq!(v, Verdict::Negative);
        assert_eq!(v.as_recommendation_str(), "regressed");
    }

    #[test]
    fn test_verdict_neutral_no_difference() {
        let analysis = proportion_analysis((5, 10), (5, 10), 2);
        let v = compute_verdict(0.0, &analysis, 10, &VerdictThresholds::recommendation());
        assert_eq!(v, Verdict::Neutral);
    }

    #[test]
    fn test_verdict_insufficient_data() {
        let analysis = proportion_analysis((1, 3), (1, 3), 2);
        let v = compute_verdict(0.0, &analysis, 3, &VerdictThresholds::recommendation());
        assert_eq!(v, Verdict::InsufficientData);
    }

    #[test]
    fn test_verdict_canary_promote_not_worse() {
        // Canary is slightly worse (-1pp) but CI doesn't indicate catastrophic regression.
        // Need enough samples so CI is tight enough (lower bound > -5pp).
        let analysis = proportion_analysis((890, 1000), (900, 1000), 2);
        let v = compute_verdict(-1.0, &analysis, 1000, &VerdictThresholds::canary());
        // -1.0 > -2.0 (improvement_delta) and CI lower bound > -5.0, so canary should promote
        assert_eq!(v, Verdict::Positive);
        assert_eq!(v.as_canary_str(), "promote");
    }

    #[test]
    fn test_verdict_canary_rollback() {
        let analysis = proportion_analysis((1, 10), (9, 10), 2);
        let v = compute_verdict(-80.0, &analysis, 10, &VerdictThresholds::canary());
        assert_eq!(v, Verdict::Negative);
        assert_eq!(v.as_canary_str(), "rollback");
    }

    #[test]
    fn test_verdict_canary_ci_too_wide_stays_neutral() {
        // Canary is slightly worse (-1pp) but with tiny samples the CI is very wide
        // (lower bound well below -5pp), so we can't confidently say "not catastrophic".
        let analysis = proportion_analysis((8, 10), (9, 10), 2);
        let v = compute_verdict(-1.0, &analysis, 10, &VerdictThresholds::canary());
        // CI lower bound is ~-41pp which is < -5pp, so canary should NOT promote
        assert_eq!(v, Verdict::Neutral);
        assert_eq!(v.as_canary_str(), "continue");
    }

    #[test]
    fn test_copeland_score_basic() {
        // 3 wins, 1 loss out of 4 opponents
        let score = copeland_score(3, 1, 4);
        assert!((score - 0.5).abs() < 1e-10);

        // All wins
        assert!((copeland_score(4, 0, 4) - 1.0).abs() < 1e-10);

        // All losses
        assert!((copeland_score(0, 4, 4) - -1.0).abs() < 1e-10);

        // No opponents
        assert!((copeland_score(0, 0, 0) - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_verdict_fallback_no_stats() {
        let analysis = StatisticalAnalysis {
            p_value: None,
            confidence_interval: None,
            effect_size: None,
        };
        // Large regression triggers fallback
        let v = compute_verdict(-10.0, &analysis, 10, &VerdictThresholds::recommendation());
        assert_eq!(v, Verdict::Negative);
        // Large improvement triggers fallback
        let v = compute_verdict(10.0, &analysis, 10, &VerdictThresholds::recommendation());
        assert_eq!(v, Verdict::Positive);
        // Small delta is neutral
        let v = compute_verdict(1.0, &analysis, 10, &VerdictThresholds::recommendation());
        assert_eq!(v, Verdict::Neutral);
    }

    // =========================================================================
    // Paired continuous tests
    //
    // Expected p-values below are LITERALS derived by hand from the closed form
    // of the t distribution at df = 4, not by re-running the code under test.
    //
    // For df = 4 the density is f(t) = (3/8)(1 + t²/4)^(-5/2), so with
    // θ = arctan(t/2),   P(T > t) = 1/2 - (3/4)·(sinθ - sin³θ/3).
    //
    // deltas = [0.1, 0.2, 0.3, 0.4, 0.5]:
    //   mean = 0.3, s² = 0.025, se = sqrt(0.025/5) = sqrt(0.005),
    //   t = 0.3/sqrt(0.005) = 3√2 = 4.242640687…, df = 4.
    //   tanθ = t/2 = 3/√2 ⇒ sin²θ = 9/11 ⇒ sinθ = 3/√11, so
    //   P(T > t) = 1/2 - (3/4)·(24/(11√11)) = 1/2 - 18/(11√11) = 0.00661793…
    // =========================================================================

    /// One-sided p for a fixed vector, against a hand-derived literal.
    #[test]
    fn test_paired_t_test_literal_one_sided_p() {
        let deltas = [0.1, 0.2, 0.3, 0.4, 0.5];
        let p = paired_t_test(&deltas);
        assert!(
            (p - 0.006_617_933_3).abs() < 1e-6,
            "expected one-sided p ≈ 0.0066179333 for t = 3√2 at df = 4, got {p}"
        );
    }

    /// One-sidedness, proven directly: the SAME magnitude of effect in the two
    /// directions must land on OPPOSITE sides of 0.5 and sum to 1.
    ///
    /// A two-sided test (e.g. `welch_t_test`) would return the same small p for
    /// both, which is exactly the misuse `compute_verdict` cannot survive.
    #[test]
    fn test_paired_t_test_is_one_sided_not_two_sided() {
        let up = [0.1, 0.2, 0.3, 0.4, 0.5];
        let down = [-0.1, -0.2, -0.3, -0.4, -0.5];

        let p_up = paired_t_test(&up);
        let p_down = paired_t_test(&down);

        assert!((p_up - 0.006_617_933_3).abs() < 1e-6, "p_up = {p_up}");
        assert!((p_down - 0.993_382_066_7).abs() < 1e-6, "p_down = {p_down}");

        assert!(p_up < 0.5, "improvement must give p < 0.5, got {p_up}");
        assert!(p_down > 0.5, "regression must give p > 0.5, got {p_down}");
        assert!(
            (p_up + p_down - 1.0).abs() < 1e-9,
            "one-sided p-values must be complementary: {p_up} + {p_down}"
        );
    }

    #[test]
    fn test_paired_t_test_degenerate_inputs() {
        // n < 2: no variance estimable — "no evidence", never 0.0.
        assert_eq!(paired_t_test(&[]), 1.0);
        assert_eq!(paired_t_test(&[5.0]), 1.0);

        // Zero variance: the direction is certain.
        assert_eq!(paired_t_test(&[0.25, 0.25, 0.25]), 0.0);
        assert_eq!(paired_t_test(&[-0.25, -0.25, -0.25]), 1.0);
        assert_eq!(paired_t_test(&[0.0, 0.0, 0.0]), 1.0);
    }

    /// `t_cdf` alone is even in `t`; `t_cdf_signed` must not be.
    #[test]
    fn test_t_cdf_signed_handles_negative_t() {
        // The bug this wrapper exists to avoid:
        assert!((t_cdf(-2.0, 4.0) - t_cdf(2.0, 4.0)).abs() < 1e-12);
        // The corrected behaviour:
        assert!((t_cdf_signed(-2.0, 4.0) + t_cdf_signed(2.0, 4.0) - 1.0).abs() < 1e-9);
        assert!(t_cdf_signed(-2.0, 4.0) < 0.5);
        assert!(t_cdf_signed(2.0, 4.0) > 0.5);
        assert!((t_cdf_signed(0.0, 4.0) - 0.5).abs() < 1e-9);
    }

    /// Literal table values for the Student-t quantile.
    #[test]
    fn test_t_quantile_literal_table_values() {
        // t(0.975, df=4) = 2.776445 ; t(0.95, df=9) = 1.833113
        assert!(
            (t_quantile(0.975, 4.0) - 2.776_445).abs() < 1e-4,
            "got {}",
            t_quantile(0.975, 4.0)
        );
        assert!(
            (t_quantile(0.95, 9.0) - 1.833_113).abs() < 1e-4,
            "got {}",
            t_quantile(0.95, 9.0)
        );
    }

    /// CI of the MEAN DELTA and Cohen's dz, against hand-computed literals.
    ///
    /// mean = 0.3, sd = sqrt(0.025) = 0.15811388…, se = 0.07071068…,
    /// t(0.975, 4) = 2.776445 ⇒ half-width = 0.19632448…
    /// ⇒ CI = (0.10367552, 0.49632448); dz = 0.3 / 0.15811388 = 1.8973666…
    #[test]
    fn test_paired_analysis_literal_ci_and_effect_size() {
        let deltas = [0.1, 0.2, 0.3, 0.4, 0.5];
        let a = paired_analysis(&deltas, 2);

        let p = a.p_value.expect("p_value");
        assert!((p - 0.006_617_933_3).abs() < 1e-6, "p = {p}");

        let (lo, hi) = a.confidence_interval.expect("ci");
        assert!((lo - 0.103_675_5).abs() < 1e-5, "ci lower = {lo}");
        assert!((hi - 0.496_324_5).abs() < 1e-5, "ci upper = {hi}");

        let d = a.effect_size.expect("effect_size");
        assert!((d - 1.897_366_6).abs() < 1e-6, "cohen's dz = {d}");
    }

    #[test]
    fn test_paired_analysis_below_min_samples_is_all_none() {
        let a = paired_analysis(&[0.1, 0.2, 0.3], 10);
        assert!(a.p_value.is_none());
        assert!(a.confidence_interval.is_none());
        assert!(a.effect_size.is_none());

        // n < 2 is all-None regardless of min_samples.
        let a = paired_analysis(&[0.1], 1);
        assert!(a.p_value.is_none());
        let a = paired_analysis(&[], 0);
        assert!(a.p_value.is_none());
    }

    /// Zero variance: the CI collapses to a point and dz is undefined (`None`),
    /// not infinite.
    #[test]
    fn test_paired_analysis_zero_variance() {
        let a = paired_analysis(&[2.0, 2.0, 2.0, 2.0], 2);
        assert_eq!(a.p_value, Some(0.0));
        assert_eq!(a.confidence_interval, Some((2.0, 2.0)));
        assert!(a.effect_size.is_none());
    }

    /// End-to-end: a clear regression must reach `Verdict::Negative` through
    /// `compute_verdict` — the arm the plan says a two-sided p silently loses.
    #[test]
    fn test_paired_regression_reaches_negative_verdict_end_to_end() {
        // Ten paired deltas in percentage points, mean = -9.5 pp.
        let deltas_pp: Vec<f64> = (5..15).map(|i| -(i as f64)).collect();
        let mean_pp = deltas_pp.iter().sum::<f64>() / deltas_pp.len() as f64;
        assert!((mean_pp - -9.5).abs() < 1e-12);

        let analysis = paired_analysis(&deltas_pp, 10);
        let p = analysis.p_value.expect("p_value");
        assert!(
            p > 0.99,
            "a regression must give a LARGE one-sided p, got {p}"
        );

        let v = compute_verdict(
            mean_pp,
            &analysis,
            deltas_pp.len() as u64,
            &VerdictThresholds::held_out_gate(),
        );
        assert_eq!(v, Verdict::Negative);
        assert_eq!(v.as_recommendation_str(), "regressed");
    }

    /// The mirror image reaches `Positive`.
    #[test]
    fn test_paired_improvement_reaches_positive_verdict_end_to_end() {
        let deltas_pp: Vec<f64> = (5..15).map(|i| i as f64).collect();
        let mean_pp = deltas_pp.iter().sum::<f64>() / deltas_pp.len() as f64;
        assert!((mean_pp - 9.5).abs() < 1e-12);

        let analysis = paired_analysis(&deltas_pp, 10);
        let p = analysis.p_value.expect("p_value");
        assert!(
            p < 0.01,
            "an improvement must give a SMALL one-sided p, got {p}"
        );

        let v = compute_verdict(
            mean_pp,
            &analysis,
            deltas_pp.len() as u64,
            &VerdictThresholds::held_out_gate(),
        );
        assert_eq!(v, Verdict::Positive);
    }

    /// Pins the misuse the plan warns about: substituting `welch_t_test`'s
    /// TWO-SIDED p for the one-sided one turns the very same clear regression
    /// into `Neutral`. This is why `paired_t_test` exists.
    #[test]
    fn test_two_sided_p_would_downgrade_a_regression_to_neutral() {
        let ctrl: Vec<f64> = (0..10).map(|i| 50.0 + i as f64).collect();
        let exp: Vec<f64> = ctrl.iter().map(|c| c - 9.5).collect();

        let two_sided = welch_t_test(&exp, &ctrl);
        let wrong = StatisticalAnalysis {
            p_value: Some(two_sided),
            confidence_interval: Some((-14.0, -5.0)),
            effect_size: None,
        };
        let v = compute_verdict(-9.5, &wrong, 10, &VerdictThresholds::held_out_gate());
        assert_eq!(
            v,
            Verdict::Neutral,
            "a two-sided p must NOT be fed to compute_verdict — it hides regressions"
        );

        // The one-sided paired test on the same data does not lose it.
        let deltas: Vec<f64> = exp.iter().zip(ctrl.iter()).map(|(e, c)| e - c).collect();
        let right = paired_analysis(&deltas, 10);
        let v = compute_verdict(-9.5, &right, 10, &VerdictThresholds::held_out_gate());
        assert_eq!(v, Verdict::Negative);
    }

    /// `held_out_gate`'s `min_runs` is what stops a zero-paired run (every
    /// example excluded — today's actual state of the world) from reading as a
    /// 0.0 delta, i.e. "no change".
    #[test]
    fn test_held_out_gate_min_runs_yields_insufficient_data() {
        let empty = paired_analysis(&[], 10);
        let v = compute_verdict(0.0, &empty, 0, &VerdictThresholds::held_out_gate());
        assert_eq!(v, Verdict::InsufficientData);
        assert_eq!(v.as_recommendation_str(), "insufficient_data");

        // Still insufficient just below the floor.
        let nine: Vec<f64> = vec![1.0; 9];
        let a = paired_analysis(&nine, 10);
        let v = compute_verdict(1.0, &a, 9, &VerdictThresholds::held_out_gate());
        assert_eq!(v, Verdict::InsufficientData);
    }
}
