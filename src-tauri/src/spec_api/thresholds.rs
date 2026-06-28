//! Threshold configuration and match-rate classification logic.
//!
//! Per-app Red/Yellow/Green thresholds for spec match rates. The default
//! configuration is Red < 0.5, Yellow 0.5–0.8, Green >= 0.8. Apps can
//! customize thresholds via the App registry (UpdateAppRequest).

use serde::{Deserialize, Serialize};

/// Configuration for classifying spec match rates into status levels.
///
/// `red` and `yellow` define the boundary points:
/// - Match rate < `red` → Red (fail)
/// - Match rate >= `red` and < `yellow` → Yellow (warn)
/// - Match rate >= `yellow` → Green (pass)
///
/// Both must be in [0.0, 1.0] and `red` must be strictly less than `yellow`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct ThresholdConfig {
    pub red: f64,
    pub yellow: f64,
}

impl ThresholdConfig {
    /// Construct a new threshold config with validation.
    ///
    /// Returns `Err(String)` if thresholds are invalid:
    /// - Either threshold is outside [0.0, 1.0]
    /// - `red >= yellow`
    pub fn new(red: f64, yellow: f64) -> Result<Self, String> {
        if !(0.0..=1.0).contains(&red) || !(0.0..=1.0).contains(&yellow) {
            return Err("thresholds must be in [0.0, 1.0]".into());
        }
        if red >= yellow {
            return Err("red_threshold must be < yellow_threshold".into());
        }
        Ok(ThresholdConfig { red, yellow })
    }

    /// Classify a match rate into Red, Yellow, or Green status.
    pub fn classify_match_rate(&self, rate: f64) -> ClassificationStatus {
        if rate < self.red {
            ClassificationStatus::Red
        } else if rate < self.yellow {
            ClassificationStatus::Yellow
        } else {
            ClassificationStatus::Green
        }
    }

    /// Return the canonical default thresholds (0.5 / 0.8).
    pub fn default() -> Self {
        ThresholdConfig {
            red: 0.5,
            yellow: 0.8,
        }
    }
}

/// Classification of a spec match rate into a user-visible status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationStatus {
    Red,
    Yellow,
    Green,
}

impl std::fmt::Display for ClassificationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            ClassificationStatus::Red => write!(f, "red"),
            ClassificationStatus::Yellow => write!(f, "yellow"),
            ClassificationStatus::Green => write!(f, "green"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_threshold_config_construction() {
        let cfg = ThresholdConfig::new(0.5, 0.8).unwrap();
        assert_eq!(cfg.red, 0.5);
        assert_eq!(cfg.yellow, 0.8);
    }

    #[test]
    fn test_classify_below_red() {
        let cfg = ThresholdConfig::new(0.5, 0.8).unwrap();
        assert_eq!(cfg.classify_match_rate(0.0), ClassificationStatus::Red);
        assert_eq!(cfg.classify_match_rate(0.3), ClassificationStatus::Red);
        assert_eq!(cfg.classify_match_rate(0.49999), ClassificationStatus::Red);
    }

    #[test]
    fn test_classify_between_red_yellow() {
        let cfg = ThresholdConfig::new(0.5, 0.8).unwrap();
        assert_eq!(cfg.classify_match_rate(0.5), ClassificationStatus::Yellow);
        assert_eq!(cfg.classify_match_rate(0.6), ClassificationStatus::Yellow);
        assert_eq!(cfg.classify_match_rate(0.79999), ClassificationStatus::Yellow);
    }

    #[test]
    fn test_classify_at_or_above_yellow() {
        let cfg = ThresholdConfig::new(0.5, 0.8).unwrap();
        assert_eq!(cfg.classify_match_rate(0.8), ClassificationStatus::Green);
        assert_eq!(cfg.classify_match_rate(0.9), ClassificationStatus::Green);
        assert_eq!(cfg.classify_match_rate(1.0), ClassificationStatus::Green);
    }

    #[test]
    fn test_invalid_threshold_red_outside_range() {
        assert!(ThresholdConfig::new(-0.1, 0.8).is_err());
        assert!(ThresholdConfig::new(1.1, 0.9).is_err());
    }

    #[test]
    fn test_invalid_threshold_yellow_outside_range() {
        assert!(ThresholdConfig::new(0.5, -0.1).is_err());
        assert!(ThresholdConfig::new(0.5, 1.1).is_err());
    }

    #[test]
    fn test_invalid_threshold_red_gte_yellow() {
        assert!(ThresholdConfig::new(0.5, 0.3).is_err()); // red > yellow
        assert!(ThresholdConfig::new(0.8, 0.8).is_err()); // red == yellow
    }

    #[test]
    fn test_default_thresholds() {
        let cfg = ThresholdConfig::default();
        assert_eq!(cfg.red, 0.5);
        assert_eq!(cfg.yellow, 0.8);
    }

    #[test]
    fn test_custom_thresholds() {
        let cfg = ThresholdConfig::new(0.55, 0.85).unwrap();
        assert_eq!(cfg.classify_match_rate(0.54), ClassificationStatus::Red);
        assert_eq!(cfg.classify_match_rate(0.56), ClassificationStatus::Yellow);
        assert_eq!(cfg.classify_match_rate(0.85), ClassificationStatus::Green);
    }

    #[test]
    fn test_classification_status_display() {
        assert_eq!(ClassificationStatus::Red.to_string(), "red");
        assert_eq!(ClassificationStatus::Yellow.to_string(), "yellow");
        assert_eq!(ClassificationStatus::Green.to_string(), "green");
    }
}
