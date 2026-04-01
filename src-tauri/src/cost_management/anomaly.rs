//! Cost Anomaly Detection
//!
//! Detects unusual cost patterns using rolling statistics.
//! Uses Welford's online algorithm for numerically stable incremental
//! mean and variance computation, avoiding the need to store all samples.

use serde::Serialize;

/// Describes a detected cost anomaly.
#[derive(Debug, Clone, Serialize)]
pub struct CostAnomaly {
    /// The cost value that triggered the anomaly.
    pub cost_usd: f64,
    /// Rolling mean at the time of detection.
    pub mean: f64,
    /// Rolling standard deviation at the time of detection.
    pub std_dev: f64,
    /// Z-score: how many standard deviations above the mean.
    pub z_score: f64,
}

/// Incremental cost anomaly detector using Welford's online algorithm.
///
/// Maintains a rolling mean and standard deviation. After accumulating
/// at least 10 samples, any cost exceeding `mean + 2*std` is flagged
/// as anomalous.
pub struct CostAnomalyDetector {
    /// Running mean of cost samples.
    rolling_mean: f64,
    /// Running variance numerator (M2 in Welford's algorithm).
    m2: f64,
    /// Number of samples seen so far.
    sample_count: u32,
}

impl CostAnomalyDetector {
    /// Create a new detector with no history.
    pub fn new() -> Self {
        Self {
            rolling_mean: 0.0,
            m2: 0.0,
            sample_count: 0,
        }
    }

    /// Current rolling mean.
    pub fn mean(&self) -> f64 {
        self.rolling_mean
    }

    /// Current rolling standard deviation.
    pub fn std_dev(&self) -> f64 {
        if self.sample_count < 2 {
            return 0.0;
        }
        (self.m2 / (self.sample_count - 1) as f64).sqrt()
    }

    /// Number of samples recorded.
    pub fn sample_count(&self) -> u32 {
        self.sample_count
    }

    /// Update the rolling statistics with a new cost observation.
    ///
    /// Uses Welford's online algorithm for numerically stable incremental
    /// mean/variance computation.
    pub fn update(&mut self, cost_usd: f64) {
        self.sample_count += 1;
        let delta = cost_usd - self.rolling_mean;
        self.rolling_mean += delta / self.sample_count as f64;
        let delta2 = cost_usd - self.rolling_mean;
        self.m2 += delta * delta2;
    }

    /// Check whether a cost value is anomalous.
    ///
    /// Returns `Some(CostAnomaly)` if the cost exceeds `mean + 2*std_dev`
    /// and at least 10 samples have been recorded. The cost is **not**
    /// incorporated into the rolling stats — call `update()` separately
    /// if you want to include it.
    pub fn check(&self, cost_usd: f64) -> Option<CostAnomaly> {
        if self.sample_count < 10 {
            return None;
        }

        let std = self.std_dev();
        if std <= 0.0 {
            return None;
        }

        let z_score = (cost_usd - self.rolling_mean) / std;
        if z_score > 2.0 {
            Some(CostAnomaly {
                cost_usd,
                mean: self.rolling_mean,
                std_dev: std,
                z_score,
            })
        } else {
            None
        }
    }
}

impl Default for CostAnomalyDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_detector_empty() {
        let d = CostAnomalyDetector::new();
        assert_eq!(d.sample_count(), 0);
        assert_eq!(d.mean(), 0.0);
        assert_eq!(d.std_dev(), 0.0);
    }

    #[test]
    fn test_mean_after_updates() {
        let mut d = CostAnomalyDetector::new();
        for cost in [1.0, 2.0, 3.0, 4.0, 5.0] {
            d.update(cost);
        }
        assert!((d.mean() - 3.0).abs() < 1e-10);
        assert_eq!(d.sample_count(), 5);
    }

    #[test]
    fn test_std_dev_calculation() {
        let mut d = CostAnomalyDetector::new();
        // Uniform values -> std dev = 0
        for _ in 0..10 {
            d.update(5.0);
        }
        assert!(d.std_dev() < 1e-10);
    }

    #[test]
    fn test_no_anomaly_below_threshold_samples() {
        let mut d = CostAnomalyDetector::new();
        for i in 0..9 {
            d.update(i as f64);
        }
        // Only 9 samples, need 10
        assert!(d.check(100.0).is_none());
    }

    #[test]
    fn test_anomaly_detected() {
        let mut d = CostAnomalyDetector::new();
        // 10 samples around 1.0
        for _ in 0..10 {
            d.update(1.0);
        }
        // Add a tiny bit of variance so std_dev > 0
        d.update(1.1);
        d.update(0.9);

        // Check a large outlier
        let anomaly = d.check(10.0);
        assert!(anomaly.is_some());
        let a = anomaly.unwrap();
        assert!(a.z_score > 2.0);
        assert!((a.cost_usd - 10.0).abs() < 1e-10);
    }

    #[test]
    fn test_no_anomaly_for_normal_value() {
        let mut d = CostAnomalyDetector::new();
        for i in 0..20 {
            d.update(1.0 + (i as f64 * 0.01));
        }
        // A value within normal range should not be flagged
        assert!(d.check(1.1).is_none());
    }

    #[test]
    fn test_check_does_not_mutate() {
        let mut d = CostAnomalyDetector::new();
        for _ in 0..12 {
            d.update(1.0);
        }
        d.update(1.1);
        let count_before = d.sample_count();
        let mean_before = d.mean();
        d.check(100.0);
        assert_eq!(d.sample_count(), count_before);
        assert!((d.mean() - mean_before).abs() < 1e-10);
    }

    #[test]
    fn test_zero_std_dev_no_anomaly() {
        let mut d = CostAnomalyDetector::new();
        // All identical values -> std_dev ~= 0
        for _ in 0..15 {
            d.update(2.0);
        }
        // Even a different value should return None because std_dev ≈ 0
        // (division by zero guard)
        assert!(d.check(5.0).is_none());
    }
}
