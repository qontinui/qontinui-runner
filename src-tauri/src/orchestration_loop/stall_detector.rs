//! Stall detection for the orchestration loop.
//!
//! Monitors action history to detect repeated actions, oscillation patterns,
//! step limits, timeouts, and lack of progress.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use super::types::StallDetectorConfig;

/// Detected stall pattern type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StallPattern {
    RepeatedIdenticalAction {
        action_signature: String,
        count: u32,
    },
    Oscillation {
        states: Vec<String>,
        cycle_count: u32,
    },
    StepLimitExceeded {
        step_count: u32,
        limit: u32,
    },
    StallTimeout {
        elapsed_secs: u64,
    },
    NoProgressDetected {
        iterations_without_change: u32,
    },
    /// UI Bridge reports the page is stuck (loading spinner, no DOM changes, etc.)
    UIStuck {
        verdict: String,
        loading_indicators: u32,
        dom_changed: bool,
        observation_window_ms: u64,
    },
}

impl std::fmt::Display for StallPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RepeatedIdenticalAction {
                action_signature,
                count,
            } => write!(f, "Action '{}' repeated {} times", action_signature, count),
            Self::Oscillation {
                states,
                cycle_count,
            } => write!(
                f,
                "Oscillating between {} states ({} cycles)",
                states.len(),
                cycle_count
            ),
            Self::StepLimitExceeded { step_count, limit } => {
                write!(f, "Step limit exceeded: {} / {}", step_count, limit)
            }
            Self::StallTimeout { elapsed_secs } => {
                write!(f, "No progress for {}s", elapsed_secs)
            }
            Self::NoProgressDetected {
                iterations_without_change,
            } => write!(
                f,
                "No progress for {} iterations",
                iterations_without_change
            ),
            Self::UIStuck {
                verdict,
                loading_indicators,
                ..
            } => write!(
                f,
                "UI stuck: {} ({} loading indicators)",
                verdict, loading_indicators
            ),
        }
    }
}

/// UI Bridge health signals from idle detection and stuck screen diagnosis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiHealthSignals {
    /// Composite idle score (0.0 = busy, 1.0 = fully idle)
    pub idle_score: f64,
    /// Whether the network is idle
    pub network_idle: bool,
    /// Whether the DOM is idle (no mutations)
    pub dom_idle: bool,
    /// Whether loading indicators are present
    pub loading: bool,
    /// Stuck screen verdict from diagnosis
    pub stuck_verdict: String,
    /// Number of detected loading indicators (spinners, progress bars, etc.)
    pub loading_indicator_count: u32,
    /// Whether the DOM has changed since last check
    pub dom_changed: bool,
    /// How long the observation window was (ms)
    pub observation_window_ms: u64,
}

/// A record of a single action performed in the loop.
#[derive(Debug, Clone)]
pub struct ActionRecord {
    pub signature: String,
    pub step_type: String,
    pub timestamp: DateTime<Utc>,
    pub iteration: u32,
    pub output_hash: Option<String>,
}

/// Monitors orchestration loop actions for stall patterns.
pub struct StallDetector {
    config: StallDetectorConfig,
    action_history: Vec<ActionRecord>,
    last_meaningful_change_at: DateTime<Utc>,
    last_output_hash: Option<String>,
    iterations_without_change: u32,
}

impl StallDetector {
    pub fn new(config: StallDetectorConfig) -> Self {
        Self {
            config,
            action_history: Vec::new(),
            last_meaningful_change_at: Utc::now(),
            last_output_hash: None,
            iterations_without_change: 0,
        }
    }

    /// Record an action that occurred during the loop.
    pub fn record_action(
        &mut self,
        signature: String,
        step_type: String,
        iteration: u32,
        output_hash: Option<String>,
    ) {
        let record = ActionRecord {
            signature,
            step_type,
            timestamp: Utc::now(),
            iteration,
            output_hash: output_hash.clone(),
        };

        // Check if output changed
        if output_hash != self.last_output_hash {
            self.last_meaningful_change_at = Utc::now();
            self.last_output_hash = output_hash;
            self.iterations_without_change = 0;
        } else {
            self.iterations_without_change += 1;
        }

        self.action_history.push(record);
        debug!(
            "Stall detector: recorded action #{}, total: {}",
            self.action_history.len(),
            self.action_history.len()
        );
    }

    /// Check all stall patterns and return the first detected one.
    pub fn check(&self) -> Option<StallPattern> {
        // Check in order of severity
        if let Some(pattern) = self.check_step_limit() {
            return Some(pattern);
        }
        if let Some(pattern) = self.check_repeated_actions() {
            return Some(pattern);
        }
        if let Some(pattern) = self.check_oscillation() {
            return Some(pattern);
        }
        if let Some(pattern) = self.check_timeout() {
            return Some(pattern);
        }
        if let Some(pattern) = self.check_no_progress() {
            return Some(pattern);
        }
        None
    }

    fn check_step_limit(&self) -> Option<StallPattern> {
        let count = self.action_history.len() as u32;
        if count >= self.config.max_total_steps {
            Some(StallPattern::StepLimitExceeded {
                step_count: count,
                limit: self.config.max_total_steps,
            })
        } else {
            None
        }
    }

    fn check_repeated_actions(&self) -> Option<StallPattern> {
        if self.action_history.len() < self.config.max_repeated_actions as usize {
            return None;
        }

        // Count consecutive identical actions from the end
        let last = self.action_history.last()?;
        let mut count = 1u32;
        for record in self.action_history.iter().rev().skip(1) {
            if record.signature == last.signature {
                count += 1;
            } else {
                break;
            }
        }

        if count >= self.config.max_repeated_actions {
            Some(StallPattern::RepeatedIdenticalAction {
                action_signature: last.signature.clone(),
                count,
            })
        } else {
            None
        }
    }

    fn check_oscillation(&self) -> Option<StallPattern> {
        let window = self.config.oscillation_window as usize;
        if self.action_history.len() < window {
            return None;
        }

        let recent: Vec<&str> = self
            .action_history
            .iter()
            .rev()
            .take(window)
            .map(|r| r.signature.as_str())
            .collect();

        // Detect A-B-A-B pattern
        if recent.len() >= 4 {
            let mut unique: Vec<String> = Vec::new();
            for sig in &recent {
                if !unique.contains(&sig.to_string()) {
                    unique.push(sig.to_string());
                }
            }
            if unique.len() <= 2 && recent.len() >= 6 {
                let cycle_count = (recent.len() / 2) as u32;
                return Some(StallPattern::Oscillation {
                    states: unique,
                    cycle_count,
                });
            }
        }
        None
    }

    fn check_timeout(&self) -> Option<StallPattern> {
        let elapsed = (Utc::now() - self.last_meaningful_change_at).num_seconds() as u64;
        if elapsed >= self.config.stall_timeout_secs {
            Some(StallPattern::StallTimeout {
                elapsed_secs: elapsed,
            })
        } else {
            None
        }
    }

    fn check_no_progress(&self) -> Option<StallPattern> {
        if self.iterations_without_change >= 3 {
            Some(StallPattern::NoProgressDetected {
                iterations_without_change: self.iterations_without_change,
            })
        } else {
            None
        }
    }

    /// Check UI Bridge health signals for stuck page detection.
    ///
    /// Returns `Some(UIStuck)` when the page appears stuck based on
    /// idle detection and stuck screen diagnosis from the UI Bridge SDK.
    pub fn check_ui_health(&self, signals: &UiHealthSignals) -> Option<StallPattern> {
        // Only fire if the verdict is "stuck" or if we have loading indicators
        // combined with no DOM changes (strong signal of a hung page)
        let is_stuck = signals.stuck_verdict == "stuck"
            || (signals.loading_indicator_count > 0
                && !signals.dom_changed
                && signals.idle_score < 0.3);

        if is_stuck {
            info!(
                "UI stuck detected: verdict={}, loading_indicators={}, dom_changed={}, idle_score={}",
                signals.stuck_verdict,
                signals.loading_indicator_count,
                signals.dom_changed,
                signals.idle_score
            );
            Some(StallPattern::UIStuck {
                verdict: signals.stuck_verdict.clone(),
                loading_indicators: signals.loading_indicator_count,
                dom_changed: signals.dom_changed,
                observation_window_ms: signals.observation_window_ms,
            })
        } else {
            None
        }
    }

    /// Get the total number of recorded actions.
    pub fn action_count(&self) -> usize {
        self.action_history.len()
    }

    /// Get the N most recent actions (newest first).
    pub fn recent_actions(&self, n: usize) -> Vec<&ActionRecord> {
        self.action_history.iter().rev().take(n).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> StallDetectorConfig {
        StallDetectorConfig {
            max_repeated_actions: 5,
            max_total_steps: 100,
            stall_timeout_secs: 300,
            oscillation_window: 10,
        }
    }

    #[test]
    fn no_stall_on_empty_history() {
        let detector = StallDetector::new(default_config());
        assert!(detector.check().is_none());
    }

    #[test]
    fn no_stall_with_varied_actions_under_threshold() {
        let mut detector = StallDetector::new(default_config());
        for i in 0..4 {
            detector.record_action(
                format!("action_{}", i),
                "step".to_string(),
                i,
                Some(format!("hash_{}", i)),
            );
        }
        assert!(detector.check().is_none());
    }

    #[test]
    fn detect_repeated_identical_actions() {
        let mut detector = StallDetector::new(default_config());
        for i in 0..5 {
            detector.record_action(
                "same_action".to_string(),
                "step".to_string(),
                i,
                Some("same_hash".to_string()),
            );
        }
        let pattern = detector.check();
        assert!(pattern.is_some());
        match pattern.unwrap() {
            StallPattern::RepeatedIdenticalAction {
                action_signature,
                count,
            } => {
                assert_eq!(action_signature, "same_action");
                assert_eq!(count, 5);
            }
            other => panic!("Expected RepeatedIdenticalAction, got {:?}", other),
        }
    }

    #[test]
    fn detect_oscillation_pattern() {
        let config = StallDetectorConfig {
            max_repeated_actions: 10, // high so repeated doesn't trigger first
            oscillation_window: 6,    // must be <= number of actions for detection
            max_total_steps: 100,
            stall_timeout_secs: 300,
        };
        let mut detector = StallDetector::new(config);
        // A-B-A-B-A-B pattern (6 actions, 2 unique, within window of 6)
        for i in 0..6 {
            let sig = if i % 2 == 0 { "action_A" } else { "action_B" };
            detector.record_action(
                sig.to_string(),
                "step".to_string(),
                i,
                Some(format!("hash_{}", i)),
            );
        }
        let pattern = detector.check();
        assert!(pattern.is_some());
        match pattern.unwrap() {
            StallPattern::Oscillation {
                states,
                cycle_count,
            } => {
                assert!(states.len() <= 2);
                assert!(cycle_count >= 3);
            }
            other => panic!("Expected Oscillation, got {:?}", other),
        }
    }

    #[test]
    fn detect_step_limit_exceeded() {
        let config = StallDetectorConfig {
            max_total_steps: 5,
            max_repeated_actions: 100,
            stall_timeout_secs: 300,
            oscillation_window: 10,
        };
        let mut detector = StallDetector::new(config);
        for i in 0..5 {
            detector.record_action(
                format!("action_{}", i),
                "step".to_string(),
                i,
                Some(format!("hash_{}", i)),
            );
        }
        let pattern = detector.check();
        assert!(pattern.is_some());
        match pattern.unwrap() {
            StallPattern::StepLimitExceeded { step_count, limit } => {
                assert_eq!(step_count, 5);
                assert_eq!(limit, 5);
            }
            other => panic!("Expected StepLimitExceeded, got {:?}", other),
        }
    }

    #[test]
    fn detect_no_progress_same_output_hash() {
        let config = StallDetectorConfig {
            max_repeated_actions: 100,
            max_total_steps: 100,
            stall_timeout_secs: 300,
            oscillation_window: 10,
        };
        let mut detector = StallDetector::new(config);
        // First action sets the hash
        detector.record_action(
            "action_a".to_string(),
            "step".to_string(),
            0,
            Some("same_hash".to_string()),
        );
        // 3 more with same hash but different signatures => no_progress triggers at 3
        for i in 1..4 {
            detector.record_action(
                format!("action_{}", i),
                "step".to_string(),
                i,
                Some("same_hash".to_string()),
            );
        }
        let pattern = detector.check();
        assert!(pattern.is_some());
        match pattern.unwrap() {
            StallPattern::NoProgressDetected {
                iterations_without_change,
            } => {
                assert!(iterations_without_change >= 3);
            }
            other => panic!("Expected NoProgressDetected, got {:?}", other),
        }
    }

    #[test]
    fn step_limit_takes_priority_over_repeated() {
        // Step limit is checked first (most severe)
        let config = StallDetectorConfig {
            max_total_steps: 5,
            max_repeated_actions: 5,
            stall_timeout_secs: 300,
            oscillation_window: 10,
        };
        let mut detector = StallDetector::new(config);
        for i in 0..5 {
            detector.record_action(
                "same".to_string(),
                "step".to_string(),
                i,
                Some("hash".to_string()),
            );
        }
        match detector.check().unwrap() {
            StallPattern::StepLimitExceeded { .. } => {}
            other => panic!(
                "Expected StepLimitExceeded (highest priority), got {:?}",
                other
            ),
        }
    }

    #[test]
    fn record_action_resets_change_counter_on_new_output() {
        let config = StallDetectorConfig {
            max_repeated_actions: 100,
            max_total_steps: 100,
            stall_timeout_secs: 300,
            oscillation_window: 10,
        };
        let mut detector = StallDetector::new(config);
        // Build up iterations_without_change
        detector.record_action("a".to_string(), "s".to_string(), 0, Some("h1".to_string()));
        detector.record_action("b".to_string(), "s".to_string(), 1, Some("h1".to_string()));
        detector.record_action("c".to_string(), "s".to_string(), 2, Some("h1".to_string()));
        // Now change the output hash - should reset the counter
        detector.record_action("d".to_string(), "s".to_string(), 3, Some("h2".to_string()));
        detector.record_action("e".to_string(), "s".to_string(), 4, Some("h2".to_string()));
        // Only 1 iteration without change, not 3, so no NoProgress
        assert!(detector.check().is_none());
    }
}
