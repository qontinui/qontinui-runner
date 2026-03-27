//! Ebbinghaus-inspired forgetting curve with importance-weighted differential decay.
//!
//! Retention = importance × e^(-decay_rate × days_since_access)
//!
//! Decay rates:
//! - Mental models: 0.02/day (slow — consolidated knowledge)
//! - High-importance (≥0.8): 0.05/day (intermediate)
//! - Regular observations: 0.1/day (default)
//!
//! Observations with retention below 0.05 are soft-deleted.

/// The retention threshold below which observations are archived (soft-deleted).
pub const ARCHIVE_THRESHOLD: f64 = 0.05;

/// Compute the current retention level of an observation.
///
/// Returns a value in [0.0, importance] representing how "alive" this memory is.
/// Lower values mean the memory is fading and may be archived.
pub fn compute_retention(importance: f64, decay_rate: f64, days_since_access: f64) -> f64 {
    importance * (-decay_rate * days_since_access).exp()
}

/// Check whether an observation should be archived based on its current retention.
pub fn should_archive(importance: f64, decay_rate: f64, days_since_access: f64) -> bool {
    compute_retention(importance, decay_rate, days_since_access) < ARCHIVE_THRESHOLD
}

/// Compute how many days until an observation's retention drops below the archive threshold.
///
/// Returns `None` if the observation would never decay below threshold (importance ≤ threshold
/// or decay_rate ≤ 0).
pub fn days_until_archive(importance: f64, decay_rate: f64) -> Option<f64> {
    if importance <= ARCHIVE_THRESHOLD || decay_rate <= 0.0 {
        return None;
    }
    // Solve: importance * e^(-decay_rate * t) = ARCHIVE_THRESHOLD
    // t = -ln(ARCHIVE_THRESHOLD / importance) / decay_rate
    Some(-(ARCHIVE_THRESHOLD / importance).ln() / decay_rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retention_at_zero_days() {
        // No time elapsed → full importance retained
        let r = compute_retention(0.8, 0.1, 0.0);
        assert!((r - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn test_retention_decays_over_time() {
        let r0 = compute_retention(0.8, 0.1, 0.0);
        let r5 = compute_retention(0.8, 0.1, 5.0);
        let r10 = compute_retention(0.8, 0.1, 10.0);
        assert!(r0 > r5);
        assert!(r5 > r10);
    }

    #[test]
    fn test_retention_mental_model_slow_decay() {
        // Mental model (decay_rate=0.02) after 30 days
        let r = compute_retention(0.8, 0.02, 30.0);
        // 0.8 * e^(-0.02 * 30) = 0.8 * e^(-0.6) ≈ 0.8 * 0.5488 ≈ 0.439
        assert!(r > 0.4);
        assert!(r < 0.5);
    }

    #[test]
    fn test_retention_regular_observation_fast_decay() {
        // Regular observation (decay_rate=0.1) after 30 days
        let r = compute_retention(0.5, 0.1, 30.0);
        // 0.5 * e^(-0.1 * 30) = 0.5 * e^(-3) ≈ 0.5 * 0.0498 ≈ 0.0249
        assert!(r < ARCHIVE_THRESHOLD);
    }

    #[test]
    fn test_should_archive() {
        // Regular obs, 30 days old → should archive
        assert!(should_archive(0.5, 0.1, 30.0));
        // Mental model, 30 days old → should NOT archive
        assert!(!should_archive(0.8, 0.02, 30.0));
        // Fresh observation → should NOT archive
        assert!(!should_archive(0.5, 0.1, 0.0));
    }

    #[test]
    fn test_days_until_archive() {
        // Regular obs with importance 0.5, decay 0.1
        // t = -ln(0.05/0.5)/0.1 = -ln(0.1)/0.1 = 2.3026/0.1 = 23.026 days
        let days = days_until_archive(0.5, 0.1).unwrap();
        assert!((days - 23.026).abs() < 0.01);
    }

    #[test]
    fn test_days_until_archive_mental_model() {
        // Mental model with importance 0.8, decay 0.02
        // t = -ln(0.05/0.8)/0.02 = -ln(0.0625)/0.02 = 2.7726/0.02 = 138.63 days
        let days = days_until_archive(0.8, 0.02).unwrap();
        assert!((days - 138.63).abs() < 0.1);
    }

    #[test]
    fn test_days_until_archive_already_below_threshold() {
        // Importance already at or below threshold → never archives (already dead)
        assert!(days_until_archive(0.05, 0.1).is_none());
        assert!(days_until_archive(0.01, 0.1).is_none());
    }

    #[test]
    fn test_days_until_archive_zero_decay() {
        // Zero decay → never archives
        assert!(days_until_archive(0.8, 0.0).is_none());
    }

    #[test]
    fn test_high_importance_decays_slower_effectively() {
        // High importance (0.95) with intermediate decay (0.05) vs
        // low importance (0.4) with fast decay (0.1) — after 20 days
        let r_high = compute_retention(0.95, 0.05, 20.0);
        let r_low = compute_retention(0.4, 0.1, 20.0);
        // r_high ≈ 0.95 * e^(-1) ≈ 0.349
        // r_low ≈ 0.4 * e^(-2) ≈ 0.054
        assert!(r_high > r_low);
        assert!(r_high > ARCHIVE_THRESHOLD);
    }
}
