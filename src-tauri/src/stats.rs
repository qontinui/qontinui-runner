//! Shared statistical tests for evaluation, canary verdicts, and comparisons.
//!
//! Pure Rust implementations — no external stats crate needed.
//! Originally in `autoresearch::metrics`, extracted here so meta_optimizer,
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
pub fn proportion_z_test_onesided(
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

// =============================================================================
// Effect size measures
// =============================================================================

/// Cohen's h — effect size for comparing two proportions.
///
/// h = 2 * (arcsin(√p₁) - arcsin(√p₂))
///
/// Interpretation: |h| < 0.2 small, 0.2–0.8 medium, > 0.8 large.
pub fn cohens_h(p1: f64, p2: f64) -> f64 {
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
pub fn proportion_diff_ci(
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
pub fn normal_quantile(p: f64) -> f64 {
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
        let num_odd =
            -((a + m_f) * (a + b + m_f) * x) / ((a + 2.0 * m_f) * (a + 2.0 * m_f + 1.0));
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
        assert!(
            p > 0.95,
            "Experiment worse should have p > 0.95, got {}",
            p
        );
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
}
