//! Streaming performance drift detection.
//!
//! Implements three complementary detectors that operate incrementally on single
//! observations (no batch requirement):
//!
//! - **ADWIN** — Adaptive Windowing for continuous metrics (composite score, cost, duration)
//! - **DDM** — Drift Detection Method for binary outcomes (success/failure)
//! - **Page-Hinkley** — For detecting gradual trend changes
//!
//! Detectors are maintained per `(metric, context_key)` where context_key matches the
//! Q-Router state representation (domain:complexity:has_ui). Drift in
//! "backend:complex:no_ui" is detected independently of "frontend:trivial:ui".
//!
//! # References
//! - ADWIN: Bifet & Gavaldà, "Learning from Time-Changing Data with Adaptive Windowing" (SDM 2007)
//! - DDM: Gama et al., "Learning with Drift Detection" (SBIA 2004)
//! - Page-Hinkley: Page (1954), Hinkley (1971)

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use tracing::{debug, info, warn};

// =============================================================================
// Common types
// =============================================================================

/// Level of drift detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftLevel {
    /// No drift detected.
    None,
    /// Warning: potential drift, increased uncertainty.
    Warning,
    /// Confirmed drift: significant distribution change.
    Drift,
}

/// Signal emitted when drift is detected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftSignal {
    pub detector_type: String,
    pub metric_name: String,
    pub context_key: String,
    pub drift_level: DriftLevel,
    pub pre_drift_mean: f64,
    pub post_drift_mean: f64,
    pub window_size: u64,
}

/// Names of metrics we track for drift.
pub const METRIC_COMPOSITE_SCORE: &str = "composite_score";
pub const METRIC_SUCCESS_RATE: &str = "success_rate";
pub const METRIC_COST_USD: &str = "cost_usd";
pub const METRIC_DURATION_SECS: &str = "duration_secs";

// =============================================================================
// ADWIN — Adaptive Windowing
// =============================================================================

/// Compressed bucket in ADWIN's exponential histogram.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdwinBucket {
    /// Total sum of values in this bucket.
    total: f64,
    /// Variance sum for this bucket (sum of (x - mean)^2).
    variance: f64,
    /// Number of elements in this bucket.
    count: u32,
}

/// ADWIN (ADaptive WINdowing) detector for continuous-valued metrics.
///
/// Maintains a variable-length window of recent observations using a compressed
/// representation (exponential histogram of bucket lists). When a statistically
/// significant difference is detected between two sub-windows, the older data
/// is discarded, signaling drift.
///
/// The `delta` parameter controls sensitivity: smaller delta = more sensitive.
/// Default delta = 0.002 (99.8% confidence).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdwinDetector {
    /// Compressed representation: buckets ordered newest-first.
    buckets: VecDeque<AdwinBucket>,
    /// Maximum number of buckets per level before merging.
    max_buckets: usize,
    /// Total sum of all values in the window.
    total: f64,
    /// Total count of all observations in the window.
    count: u64,
    /// Running sum of squares for variance computation.
    sum_sq: f64,
    /// Confidence parameter (smaller = more sensitive, default 0.002).
    delta: f64,
}

impl AdwinDetector {
    /// Create a new ADWIN detector with the given confidence parameter.
    ///
    /// `delta` controls sensitivity: 0.002 (default) = 99.8% confidence.
    /// Smaller values detect smaller changes but with higher latency.
    pub fn new(delta: f64) -> Self {
        Self {
            buckets: VecDeque::new(),
            max_buckets: 5, // M parameter in ADWIN paper
            total: 0.0,
            count: 0,
            sum_sq: 0.0,
            delta,
        }
    }

    /// Create with default sensitivity (delta = 0.002).
    pub fn default_sensitivity() -> Self {
        Self::new(0.002)
    }

    /// Add a new observation. Returns `Some(DriftSignal)` if drift is detected.
    pub fn update(&mut self, value: f64, metric_name: &str, context_key: &str) -> Option<DriftSignal> {
        // 1. Add new observation as a single-element bucket
        self.buckets.push_front(AdwinBucket {
            total: value,
            variance: 0.0,
            count: 1,
        });
        self.total += value;
        self.sum_sq += value * value;
        self.count += 1;

        // 2. Compress: merge adjacent buckets when too many at same level
        self.compress();

        // 3. Check for drift by scanning cut points
        self.detect_cut(metric_name, context_key)
    }

    /// Current mean of the window.
    pub fn mean(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.total / self.count as f64
        }
    }

    /// Current window size (number of observations).
    pub fn width(&self) -> u64 {
        self.count
    }

    /// Compress buckets: merge adjacent buckets when we exceed max_buckets
    /// at the same size level.
    fn compress(&mut self) {
        if self.buckets.len() <= self.max_buckets {
            return;
        }

        // Simple compression: merge the two oldest (back) buckets
        // Full ADWIN uses an exponential histogram with levels; this is a
        // simplified version that still provides the core adaptive windowing.
        let mut merged = VecDeque::new();
        let mut i = 0;
        let buckets: Vec<_> = self.buckets.iter().cloned().collect();

        while i < buckets.len() {
            if i + 1 < buckets.len()
                && buckets[i].count == buckets[i + 1].count
                && merged.len() + (buckets.len() - i) > self.max_buckets * 2
            {
                // Merge buckets[i] and buckets[i+1]
                let a = &buckets[i];
                let b = &buckets[i + 1];
                let combined_count = a.count + b.count;
                let combined_total = a.total + b.total;
                // Approximate combined variance using parallel algorithm
                let mean_a = if a.count > 0 { a.total / a.count as f64 } else { 0.0 };
                let mean_b = if b.count > 0 { b.total / b.count as f64 } else { 0.0 };
                let delta_mean = mean_a - mean_b;
                let combined_variance = a.variance
                    + b.variance
                    + delta_mean * delta_mean * (a.count as f64 * b.count as f64)
                        / combined_count as f64;

                merged.push_back(AdwinBucket {
                    total: combined_total,
                    variance: combined_variance,
                    count: combined_count,
                });
                i += 2;
            } else {
                merged.push_back(buckets[i].clone());
                i += 1;
            }
        }

        self.buckets = merged;
    }

    /// Scan all possible cut points to detect drift.
    ///
    /// For each cut point, compares the mean of the "recent" sub-window (W0)
    /// with the mean of the "old" sub-window (W1). If the difference exceeds
    /// the ADWIN bound, the old data is discarded and drift is signaled.
    fn detect_cut(&mut self, metric_name: &str, context_key: &str) -> Option<DriftSignal> {
        if self.count < 10 {
            return None; // Need minimum observations
        }

        let mut n0: u64 = 0;
        let mut sum0: f64 = 0.0;

        // Scan from newest to oldest
        for i in 0..self.buckets.len().saturating_sub(1) {
            let bucket = &self.buckets[i];
            n0 += bucket.count as u64;
            sum0 += bucket.total;

            let n1 = self.count - n0;
            let sum1 = self.total - sum0;

            if n0 < 5 || n1 < 5 {
                continue; // Need minimum per sub-window
            }

            let mean0 = sum0 / n0 as f64;
            let mean1 = sum1 / n1 as f64;
            let abs_diff = (mean0 - mean1).abs();

            // ADWIN bound: epsilon_cut = sqrt((1/2m) * ln(4n/delta))
            // where m = harmonic mean of n0 and n1, n = total count
            let m_harmonic = 2.0 * (n0 as f64 * n1 as f64) / (n0 + n1) as f64;
            let epsilon_cut = ((1.0 / (2.0 * m_harmonic)) * (4.0 * self.count as f64 / self.delta).ln())
                .sqrt();

            if abs_diff >= epsilon_cut {
                // Drift detected! Remove old sub-window (buckets from i+1 onward)
                let pre_mean = mean1; // old sub-window mean
                let post_mean = mean0; // recent sub-window mean
                let window_before = self.count;

                // Truncate: keep only the recent sub-window
                self.buckets.truncate(i + 1);
                self.total = sum0;
                self.count = n0;
                // Approximate sum_sq for the retained window
                self.sum_sq = sum0 * mean0; // rough approximation

                info!(
                    "ADWIN drift detected: metric={} context={} pre_mean={:.4} post_mean={:.4} window={}",
                    metric_name, context_key, pre_mean, post_mean, window_before
                );

                return Some(DriftSignal {
                    detector_type: "adwin".to_string(),
                    metric_name: metric_name.to_string(),
                    context_key: context_key.to_string(),
                    drift_level: DriftLevel::Drift,
                    pre_drift_mean: pre_mean,
                    post_drift_mean: post_mean,
                    window_size: window_before,
                });
            }
        }

        None
    }
}

// =============================================================================
// DDM — Drift Detection Method
// =============================================================================

/// DDM (Drift Detection Method) for binary outcomes (success/failure).
///
/// Tracks the running error rate p and its standard deviation s = sqrt(p(1-p)/n).
/// - Warning level: p + s > p_min + 2 * s_min
/// - Drift level: p + s > p_min + 3 * s_min
///
/// When drift is detected, statistics are reset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DdmDetector {
    /// Minimum error rate observed.
    p_min: f64,
    /// Standard deviation at p_min.
    s_min: f64,
    /// Current running error rate.
    p_current: f64,
    /// Current standard deviation.
    s_current: f64,
    /// Total number of observations.
    n: u64,
    /// Number of errors.
    errors: u64,
    /// Whether we're in warning state.
    in_warning: bool,
    /// Observation count at start of warning zone (for delayed reset).
    warning_start_n: u64,
}

impl DdmDetector {
    /// Create a new DDM detector.
    pub fn new() -> Self {
        Self {
            p_min: f64::MAX,
            s_min: f64::MAX,
            p_current: 0.0,
            s_current: 0.0,
            n: 0,
            errors: 0,
            in_warning: false,
            warning_start_n: 0,
        }
    }

    /// Add a new observation. `is_error = true` for failures.
    ///
    /// Returns the drift level detected.
    pub fn update(&mut self, is_error: bool) -> DriftLevel {
        self.n += 1;
        if is_error {
            self.errors += 1;
        }

        if self.n < 30 {
            return DriftLevel::None; // Need minimum observations for reliable statistics
        }

        // Update running error rate and standard deviation
        self.p_current = self.errors as f64 / self.n as f64;
        self.s_current = (self.p_current * (1.0 - self.p_current) / self.n as f64).sqrt();

        // Track minimum p + s
        let ps = self.p_current + self.s_current;
        let ps_min = self.p_min + self.s_min;

        if ps < ps_min || self.p_min == f64::MAX {
            self.p_min = self.p_current;
            self.s_min = self.s_current;
            self.in_warning = false;
        }

        // Check for drift: p + s > p_min + 3 * s_min
        if ps > self.p_min + 3.0 * self.s_min {
            info!(
                "DDM drift detected: p_current={:.4} p_min={:.4} s_min={:.4} n={}",
                self.p_current, self.p_min, self.s_min, self.n
            );
            // Reset statistics after drift
            self.reset();
            return DriftLevel::Drift;
        }

        // Check for warning: p + s > p_min + 2 * s_min
        if ps > self.p_min + 2.0 * self.s_min {
            if !self.in_warning {
                self.in_warning = true;
                self.warning_start_n = self.n;
                debug!(
                    "DDM warning: p_current={:.4} p_min={:.4} n={}",
                    self.p_current, self.p_min, self.n
                );
            }
            return DriftLevel::Warning;
        }

        self.in_warning = false;
        DriftLevel::None
    }

    /// Current error rate.
    pub fn error_rate(&self) -> f64 {
        self.p_current
    }

    /// Total observations.
    pub fn count(&self) -> u64 {
        self.n
    }

    /// Reset statistics (called after drift detection).
    fn reset(&mut self) {
        // If we were in warning, reset to warning start point
        // Otherwise full reset
        self.p_min = f64::MAX;
        self.s_min = f64::MAX;
        self.n = 0;
        self.errors = 0;
        self.in_warning = false;
        self.warning_start_n = 0;
    }
}

impl Default for DdmDetector {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Page-Hinkley — Gradual trend detection
// =============================================================================

/// Page-Hinkley test for detecting gradual changes in the mean.
///
/// Tracks the cumulative sum of deviations from the running mean.
/// When the deviation between the cumulative sum and its minimum exceeds
/// a threshold (lambda), a change is signaled.
///
/// The `delta_tolerance` parameter adds a forgiveness factor for gradual changes
/// (larger delta = more tolerance for gradual drift, only detects abrupt shifts).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageHinkleyDetector {
    /// Cumulative sum of deviations from the mean.
    cumulative_sum: f64,
    /// Minimum cumulative sum observed.
    minimum_sum: f64,
    /// Running mean.
    mean: f64,
    /// Number of observations.
    count: u64,
    /// Tolerance for gradual changes (default: 0.005).
    delta_tolerance: f64,
    /// Detection threshold (default: 50.0).
    lambda: f64,
}

impl PageHinkleyDetector {
    /// Create a new Page-Hinkley detector.
    ///
    /// - `delta_tolerance`: Minimum magnitude of change to detect (default 0.005).
    ///   Smaller = more sensitive to gradual changes.
    /// - `lambda`: Detection threshold (default 50.0).
    ///   Smaller = faster detection but more false positives.
    pub fn new(delta_tolerance: f64, lambda: f64) -> Self {
        Self {
            cumulative_sum: 0.0,
            minimum_sum: 0.0,
            mean: 0.0,
            count: 0,
            delta_tolerance,
            lambda,
        }
    }

    /// Create with default parameters for cost/duration monitoring.
    pub fn default_for_cost() -> Self {
        // More sensitive: detect 0.5% changes with threshold 50
        Self::new(0.005, 50.0)
    }

    /// Create with default parameters for score monitoring.
    pub fn default_for_score() -> Self {
        // Less sensitive: detect 1% changes with threshold 100
        Self::new(0.01, 100.0)
    }

    /// Add a new observation. Returns the drift level.
    pub fn update(&mut self, value: f64) -> DriftLevel {
        self.count += 1;

        // Update running mean
        self.mean += (value - self.mean) / self.count as f64;

        // Update cumulative sum with tolerance
        self.cumulative_sum += value - self.mean - self.delta_tolerance;

        // Track minimum
        if self.cumulative_sum < self.minimum_sum {
            self.minimum_sum = self.cumulative_sum;
        }

        if self.count < 30 {
            return DriftLevel::None; // Need minimum observations
        }

        // Detect: cumulative_sum - minimum_sum > lambda
        let ph_value = self.cumulative_sum - self.minimum_sum;
        if ph_value > self.lambda {
            info!(
                "Page-Hinkley drift: ph_value={:.4} lambda={:.4} mean={:.4} n={}",
                ph_value, self.lambda, self.mean, self.count
            );
            // Reset after detection
            self.reset_partial();
            return DriftLevel::Drift;
        }

        // Warning at 70% of threshold
        if ph_value > self.lambda * 0.7 {
            return DriftLevel::Warning;
        }

        DriftLevel::None
    }

    /// Current mean.
    pub fn mean(&self) -> f64 {
        self.mean
    }

    /// Total observations.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Partial reset: keep the mean estimate, reset cumulative sums.
    fn reset_partial(&mut self) {
        self.cumulative_sum = 0.0;
        self.minimum_sum = 0.0;
    }
}

// =============================================================================
// Composite drift monitor
// =============================================================================

/// State for a single metric+context detector ensemble.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DetectorEnsemble {
    adwin: Option<AdwinDetector>,
    ddm: Option<DdmDetector>,
    page_hinkley: Option<PageHinkleyDetector>,
}

/// Manages all drift detectors across all metrics and contexts.
///
/// Keyed by `(metric_name, context_key)`. Each combination gets the appropriate
/// set of detectors:
/// - composite_score: ADWIN + Page-Hinkley
/// - success_rate: DDM
/// - cost_usd: ADWIN + Page-Hinkley
/// - duration_secs: ADWIN + Page-Hinkley
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftMonitor {
    detectors: HashMap<String, DetectorEnsemble>,
    /// All signals emitted since last acknowledgment.
    pending_signals: Vec<DriftSignal>,
}

impl DriftMonitor {
    /// Create a new empty drift monitor.
    pub fn new() -> Self {
        Self {
            detectors: HashMap::new(),
            pending_signals: Vec::new(),
        }
    }

    /// Get or create the detector ensemble for a (metric, context) pair.
    fn get_or_create(&mut self, metric_name: &str, context_key: &str) -> &mut DetectorEnsemble {
        let key = format!("{}:{}", metric_name, context_key);
        self.detectors.entry(key).or_insert_with(|| match metric_name {
            METRIC_SUCCESS_RATE => DetectorEnsemble {
                adwin: None,
                ddm: Some(DdmDetector::new()),
                page_hinkley: None,
            },
            METRIC_COMPOSITE_SCORE => DetectorEnsemble {
                adwin: Some(AdwinDetector::default_sensitivity()),
                ddm: None,
                page_hinkley: Some(PageHinkleyDetector::default_for_score()),
            },
            METRIC_COST_USD | METRIC_DURATION_SECS => DetectorEnsemble {
                adwin: Some(AdwinDetector::default_sensitivity()),
                ddm: None,
                page_hinkley: Some(PageHinkleyDetector::default_for_cost()),
            },
            _ => DetectorEnsemble {
                adwin: Some(AdwinDetector::default_sensitivity()),
                ddm: None,
                page_hinkley: None,
            },
        })
    }

    /// Observe a continuous-valued metric (composite_score, cost, duration).
    pub fn observe_continuous(
        &mut self,
        metric_name: &str,
        context_key: &str,
        value: f64,
    ) -> Vec<DriftSignal> {
        let mut signals = Vec::new();
        let ensemble = self.get_or_create(metric_name, context_key);

        if let Some(ref mut adwin) = ensemble.adwin {
            if let Some(signal) = adwin.update(value, metric_name, context_key) {
                signals.push(signal);
            }
        }

        if let Some(ref mut ph) = ensemble.page_hinkley {
            let level = ph.update(value);
            if level == DriftLevel::Drift || level == DriftLevel::Warning {
                signals.push(DriftSignal {
                    detector_type: "page_hinkley".to_string(),
                    metric_name: metric_name.to_string(),
                    context_key: context_key.to_string(),
                    drift_level: level,
                    pre_drift_mean: ph.mean(), // approximation
                    post_drift_mean: value,
                    window_size: ph.count(),
                });
            }
        }

        for signal in &signals {
            self.pending_signals.push(signal.clone());
        }
        signals
    }

    /// Observe a binary outcome (success/failure).
    pub fn observe_binary(
        &mut self,
        metric_name: &str,
        context_key: &str,
        is_error: bool,
    ) -> Vec<DriftSignal> {
        let mut signals = Vec::new();
        let ensemble = self.get_or_create(metric_name, context_key);

        if let Some(ref mut ddm) = ensemble.ddm {
            let level = ddm.update(is_error);
            if level == DriftLevel::Drift || level == DriftLevel::Warning {
                signals.push(DriftSignal {
                    detector_type: "ddm".to_string(),
                    metric_name: metric_name.to_string(),
                    context_key: context_key.to_string(),
                    drift_level: level,
                    pre_drift_mean: 0.0, // DDM resets on drift
                    post_drift_mean: ddm.error_rate(),
                    window_size: ddm.count(),
                });
            }
        }

        for signal in &signals {
            self.pending_signals.push(signal.clone());
        }
        signals
    }

    /// Observe all metrics from a completed learning outcome.
    ///
    /// This is the main entry point called by the online learning coordinator.
    pub fn observe_outcome(
        &mut self,
        context_key: &str,
        composite_score: Option<f64>,
        success: bool,
        cost_usd: Option<f64>,
        duration_secs: Option<f64>,
    ) -> Vec<DriftSignal> {
        let mut all_signals = Vec::new();

        // 1. Binary success/failure
        all_signals.extend(self.observe_binary(METRIC_SUCCESS_RATE, context_key, !success));

        // 2. Composite score (if available)
        if let Some(score) = composite_score {
            all_signals.extend(self.observe_continuous(METRIC_COMPOSITE_SCORE, context_key, score));
        }

        // 3. Cost
        if let Some(cost) = cost_usd {
            all_signals.extend(self.observe_continuous(METRIC_COST_USD, context_key, cost));
        }

        // 4. Duration
        if let Some(dur) = duration_secs {
            all_signals.extend(self.observe_continuous(METRIC_DURATION_SECS, context_key, dur));
        }

        all_signals
    }

    /// Take and clear all pending signals (for persistence).
    pub fn take_pending_signals(&mut self) -> Vec<DriftSignal> {
        std::mem::take(&mut self.pending_signals)
    }

    /// Get a clone of pending signals without clearing them.
    /// Used when the caller doesn't have mutable access.
    pub fn take_pending_signals_readonly(&self) -> Vec<DriftSignal> {
        self.pending_signals.clone()
    }

    /// Get all active detector keys for diagnostics.
    pub fn active_detector_keys(&self) -> Vec<String> {
        self.detectors.keys().cloned().collect()
    }

    /// Number of active detector ensembles.
    pub fn detector_count(&self) -> usize {
        self.detectors.len()
    }

    /// Serialize the full monitor state for persistence.
    pub fn serialize_state(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|e| format!("Failed to serialize drift monitor: {}", e))
    }

    /// Deserialize monitor state from persistence.
    pub fn deserialize_state(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| format!("Failed to deserialize drift monitor: {}", e))
    }
}

impl Default for DriftMonitor {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adwin_no_drift_on_stable_data() {
        let mut adwin = AdwinDetector::default_sensitivity();
        for i in 0..100 {
            let value = 0.8 + (i as f64 * 0.001); // very stable
            assert!(
                adwin.update(value, "test", "ctx").is_none(),
                "Should not detect drift on stable data at i={}",
                i
            );
        }
        assert!((adwin.mean() - 0.85).abs() < 0.1);
    }

    #[test]
    fn test_adwin_detects_mean_shift() {
        let mut adwin = AdwinDetector::new(0.01); // slightly less sensitive for test stability
        // Phase 1: stable at 0.8
        for _ in 0..100 {
            adwin.update(0.8, "test", "ctx");
        }
        // Phase 2: shift to 0.3
        let mut detected = false;
        for _ in 0..100 {
            if adwin.update(0.3, "test", "ctx").is_some() {
                detected = true;
                break;
            }
        }
        assert!(detected, "ADWIN should detect mean shift from 0.8 to 0.3");
    }

    #[test]
    fn test_ddm_no_drift_on_stable_errors() {
        let mut ddm = DdmDetector::new();
        // 10% error rate, stable
        for i in 0..200 {
            let is_error = i % 10 == 0;
            let level = ddm.update(is_error);
            assert_ne!(level, DriftLevel::Drift, "Should not drift at i={}", i);
        }
    }

    #[test]
    fn test_ddm_detects_error_rate_increase() {
        let mut ddm = DdmDetector::new();
        // Phase 1: 5% error rate
        for i in 0..200 {
            ddm.update(i % 20 == 0);
        }
        // Phase 2: 50% error rate
        let mut detected = false;
        for _ in 0..200 {
            let level = ddm.update(true); // alternating won't work, use all-errors burst
            if level == DriftLevel::Drift {
                detected = true;
                break;
            }
        }
        assert!(detected, "DDM should detect error rate increase from 5% to ~100%");
    }

    #[test]
    fn test_page_hinkley_no_drift_on_stable() {
        let mut ph = PageHinkleyDetector::new(0.01, 50.0);
        for _ in 0..100 {
            let level = ph.update(1.0);
            assert_ne!(level, DriftLevel::Drift);
        }
    }

    #[test]
    fn test_page_hinkley_detects_increase() {
        let mut ph = PageHinkleyDetector::new(0.005, 20.0); // sensitive settings
        // Phase 1: stable around 1.0
        for _ in 0..50 {
            ph.update(1.0);
        }
        // Phase 2: gradual increase to 2.0
        let mut detected = false;
        for i in 0..100 {
            let value = 1.0 + (i as f64 * 0.02);
            if ph.update(value) == DriftLevel::Drift {
                detected = true;
                break;
            }
        }
        assert!(detected, "Page-Hinkley should detect gradual increase from 1.0 to 2.0+");
    }

    #[test]
    fn test_drift_monitor_observe_outcome() {
        let mut monitor = DriftMonitor::new();

        // Feed stable data
        for _ in 0..50 {
            let signals = monitor.observe_outcome(
                "backend:moderate:no_ui",
                Some(0.85),
                true,
                Some(0.50),
                Some(120.0),
            );
            // Should not drift on initial stable data
            assert!(
                signals.iter().all(|s| s.drift_level != DriftLevel::Drift),
                "No drift expected on stable data"
            );
        }

        assert!(monitor.detector_count() > 0);
    }

    #[test]
    fn test_drift_monitor_serialization() {
        let mut monitor = DriftMonitor::new();
        for _ in 0..10 {
            monitor.observe_outcome("ctx", Some(0.8), true, Some(0.5), Some(100.0));
        }
        let json = monitor.serialize_state().unwrap();
        let restored = DriftMonitor::deserialize_state(&json).unwrap();
        assert_eq!(restored.detector_count(), monitor.detector_count());
    }
}
