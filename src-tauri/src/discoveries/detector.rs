//! Discovery detector for analyzing automation runs.
//!
//! This module analyzes run details and config statistics to detect
//! patterns that should be reported to qontinui-web as discoveries.

use crate::tiered_info::{ConfigStatistics, RunDetails};
use tracing::{debug, info};

use super::types::{DiscoveryEvidence, DiscoveryPayload, DiscoveryType};

/// Detection thresholds for various discovery types.
pub struct DetectionThresholds {
    /// Minimum runs to confirm a new element (default: 3)
    pub new_element_min_runs: u32,
    /// Minimum confidence for new element detection (default: 0.9)
    pub new_element_min_confidence: f64,
    /// Minimum runs for timing update (default: 10)
    pub timing_update_min_runs: u32,
    /// Maximum variance coefficient for stable timing (default: 0.3)
    pub timing_max_variance: f64,
    /// Minimum runs for flaky detection (default: 10)
    pub flaky_min_runs: u32,
    /// Flaky success rate range (default: 0.2 - 0.8)
    pub flaky_rate_min: f64,
    pub flaky_rate_max: f64,
    /// Minimum confidence for discoveries (default: 0.7)
    pub min_discovery_confidence: f64,
}

impl Default for DetectionThresholds {
    fn default() -> Self {
        Self {
            new_element_min_runs: 3,
            new_element_min_confidence: 0.9,
            timing_update_min_runs: 10,
            timing_max_variance: 0.3,
            flaky_min_runs: 10,
            flaky_rate_min: 0.2,
            flaky_rate_max: 0.8,
            min_discovery_confidence: 0.7,
        }
    }
}

/// Context needed for discovery detection.
pub struct DetectionContext {
    pub runner_id: String,
    pub runner_name: Option<String>,
    pub project_id: String,
    pub config_id: String,
    pub config_name: Option<String>,
}

/// Analyze a run and existing statistics to detect discoveries.
///
/// This function examines:
/// 1. Template matches - new elements with high confidence
/// 2. Transition patterns - new transitions or timing changes
/// 3. Flaky behavior - inconsistent success rates
///
/// Returns a list of discoveries that should be queued for sync.
pub fn analyze_run_for_discoveries(
    run: &RunDetails,
    stats: &ConfigStatistics,
    context: &DetectionContext,
    thresholds: &DetectionThresholds,
) -> Vec<DiscoveryPayload> {
    let mut discoveries = Vec::new();

    // Only analyze completed runs
    if !run.status.is_terminal() {
        return discoveries;
    }

    // Detect flaky transitions
    discoveries.extend(detect_flaky_transitions(run, stats, context, thresholds));

    // Detect flaky templates
    discoveries.extend(detect_flaky_templates(run, stats, context, thresholds));

    // Detect timing updates for stable transitions
    discoveries.extend(detect_timing_updates(run, stats, context, thresholds));

    // Detect new transitions (not in original config but consistently observed)
    discoveries.extend(detect_new_transitions(run, stats, context, thresholds));

    if !discoveries.is_empty() {
        info!(
            "Detected {} discoveries from run {}",
            discoveries.len(),
            run.id
        );
    }

    discoveries
}

/// Detect flaky transitions based on success rate.
fn detect_flaky_transitions(
    run: &RunDetails,
    stats: &ConfigStatistics,
    context: &DetectionContext,
    thresholds: &DetectionThresholds,
) -> Vec<DiscoveryPayload> {
    let mut discoveries = Vec::new();

    for (transition_key, transition_stats) in &stats.transition_stats {
        // Check if meets minimum runs threshold
        if transition_stats.total < thresholds.flaky_min_runs {
            continue;
        }

        let success_rate = transition_stats.success_rate();

        // Check if in flaky range
        if success_rate > thresholds.flaky_rate_min && success_rate < thresholds.flaky_rate_max {
            // This transition is flaky - create discovery
            let evidence = DiscoveryEvidence {
                runs_observed: transition_stats.total,
                first_seen: stats.first_run_at.clone().unwrap_or_default(),
                last_seen: run.started_at.clone(),
                confidence_avg: Some(success_rate),
                sample_screenshots: None,
            };

            // Calculate confidence based on how much data we have
            let confidence = calculate_flaky_confidence(transition_stats.total, success_rate);

            if confidence >= thresholds.min_discovery_confidence {
                let mut payload = DiscoveryPayload::new(
                    context.runner_id.clone(),
                    context.project_id.clone(),
                    context.config_id.clone(),
                    DiscoveryType::FlakyDetection,
                    format!("Flaky transition: {}", transition_key),
                    evidence,
                    confidence,
                );

                payload.runner_name = context.runner_name.clone();
                payload.config_name = context.config_name.clone();
                payload.description = Some(format!(
                    "Transition {} has {:.1}% success rate over {} runs. This indicates unreliable behavior that may need investigation.",
                    transition_key,
                    success_rate * 100.0,
                    transition_stats.total
                ));
                payload.discovery_data = serde_json::json!({
                    "transition_key": transition_key,
                    "success_rate": success_rate,
                    "total_runs": transition_stats.total,
                    "successes": transition_stats.success,
                    "failures": transition_stats.failure,
                    "avg_duration_ms": transition_stats.avg_duration_ms,
                    "last_failure": transition_stats.last_failure,
                });

                discoveries.push(payload);
                debug!(
                    "Detected flaky transition: {} ({:.1}% success)",
                    transition_key,
                    success_rate * 100.0
                );
            }
        }
    }

    discoveries
}

/// Detect flaky templates based on match rate.
fn detect_flaky_templates(
    run: &RunDetails,
    stats: &ConfigStatistics,
    context: &DetectionContext,
    thresholds: &DetectionThresholds,
) -> Vec<DiscoveryPayload> {
    let mut discoveries = Vec::new();

    for (template_name, template_stats) in &stats.template_stats {
        // Check if meets minimum runs threshold
        if template_stats.total < thresholds.flaky_min_runs {
            continue;
        }

        let match_rate = template_stats.match_rate();

        // Check if in flaky range
        if match_rate > thresholds.flaky_rate_min && match_rate < thresholds.flaky_rate_max {
            let evidence = DiscoveryEvidence {
                runs_observed: template_stats.total,
                first_seen: stats.first_run_at.clone().unwrap_or_default(),
                last_seen: run.started_at.clone(),
                confidence_avg: Some(template_stats.avg_confidence as f64),
                sample_screenshots: None,
            };

            let confidence = calculate_flaky_confidence(template_stats.total, match_rate);

            if confidence >= thresholds.min_discovery_confidence {
                let mut payload = DiscoveryPayload::new(
                    context.runner_id.clone(),
                    context.project_id.clone(),
                    context.config_id.clone(),
                    DiscoveryType::FlakyDetection,
                    format!("Flaky template: {}", template_name),
                    evidence,
                    confidence,
                );

                payload.runner_name = context.runner_name.clone();
                payload.config_name = context.config_name.clone();
                payload.description = Some(format!(
                    "Template {} has {:.1}% match rate over {} attempts. Average confidence when matched: {:.1}%.",
                    template_name,
                    match_rate * 100.0,
                    template_stats.total,
                    template_stats.avg_confidence * 100.0
                ));
                payload.discovery_data = serde_json::json!({
                    "template_name": template_name,
                    "match_rate": match_rate,
                    "total_attempts": template_stats.total,
                    "matches": template_stats.matches,
                    "failures": template_stats.failures,
                    "avg_confidence": template_stats.avg_confidence,
                });

                discoveries.push(payload);
                debug!(
                    "Detected flaky template: {} ({:.1}% match rate)",
                    template_name,
                    match_rate * 100.0
                );
            }
        }
    }

    discoveries
}

/// Detect stable timing patterns that differ from configured values.
fn detect_timing_updates(
    run: &RunDetails,
    stats: &ConfigStatistics,
    context: &DetectionContext,
    thresholds: &DetectionThresholds,
) -> Vec<DiscoveryPayload> {
    let mut discoveries = Vec::new();

    for (transition_key, transition_stats) in &stats.transition_stats {
        // Need enough runs for reliable timing
        if transition_stats.total < thresholds.timing_update_min_runs {
            continue;
        }

        // Only report if we have high success rate (stable transition)
        if transition_stats.success_rate() < 0.9 {
            continue;
        }

        // Check if we have duration variance data
        if let Some(stddev) = transition_stats.duration_stddev {
            let avg = transition_stats.avg_duration_ms as f64;
            if avg > 0.0 {
                let variance_coeff = stddev / avg;

                // Low variance means consistent timing
                if variance_coeff < thresholds.timing_max_variance {
                    let evidence = DiscoveryEvidence {
                        runs_observed: transition_stats.total,
                        first_seen: stats.first_run_at.clone().unwrap_or_default(),
                        last_seen: run.started_at.clone(),
                        confidence_avg: None,
                        sample_screenshots: None,
                    };

                    // High confidence for stable timing
                    let confidence = 0.9 - variance_coeff;

                    if confidence >= thresholds.min_discovery_confidence {
                        let mut payload = DiscoveryPayload::new(
                            context.runner_id.clone(),
                            context.project_id.clone(),
                            context.config_id.clone(),
                            DiscoveryType::TimingUpdate,
                            format!("Stable timing for: {}", transition_key),
                            evidence,
                            confidence,
                        );

                        payload.runner_name = context.runner_name.clone();
                        payload.config_name = context.config_name.clone();
                        payload.description = Some(format!(
                            "Transition {} has stable timing: {}ms avg (stddev: {:.0}ms, {:.1}% variance) over {} runs.",
                            transition_key,
                            transition_stats.avg_duration_ms,
                            stddev,
                            variance_coeff * 100.0,
                            transition_stats.total
                        ));
                        payload.discovery_data = serde_json::json!({
                            "transition_key": transition_key,
                            "avg_duration_ms": transition_stats.avg_duration_ms,
                            "duration_stddev": stddev,
                            "variance_coefficient": variance_coeff,
                            "total_runs": transition_stats.total,
                        });

                        discoveries.push(payload);
                        debug!(
                            "Detected timing update: {} ({}ms avg)",
                            transition_key, transition_stats.avg_duration_ms
                        );
                    }
                }
            }
        }
    }

    discoveries
}

/// Detect new transitions that aren't in the original config.
fn detect_new_transitions(
    run: &RunDetails,
    stats: &ConfigStatistics,
    context: &DetectionContext,
    thresholds: &DetectionThresholds,
) -> Vec<DiscoveryPayload> {
    let mut discoveries = Vec::new();

    // Look for transitions that appeared in recent runs but might be new
    // This is a heuristic - we mark transitions as "new" if:
    // 1. They have enough observations
    // 2. They have high success rate
    // 3. They first appeared recently (based on stats)

    for transition in &run.transitions_executed {
        let key = format!("{}|{}", transition.from_state, transition.to_state);

        if let Some(transition_stats) = stats.transition_stats.get(&key) {
            // Check if this might be a newly discovered transition
            // (appeared in recent runs with high success)
            if transition_stats.total >= thresholds.new_element_min_runs
                && transition_stats.success_rate() >= thresholds.new_element_min_confidence
                && transition_stats.total <= 10
            // Relatively new
            {
                let evidence = DiscoveryEvidence {
                    runs_observed: transition_stats.total,
                    first_seen: stats.first_run_at.clone().unwrap_or_default(),
                    last_seen: run.started_at.clone(),
                    confidence_avg: Some(transition_stats.success_rate()),
                    sample_screenshots: None,
                };

                let confidence = transition_stats.success_rate();

                if confidence >= thresholds.min_discovery_confidence {
                    let mut payload = DiscoveryPayload::new(
                        context.runner_id.clone(),
                        context.project_id.clone(),
                        context.config_id.clone(),
                        DiscoveryType::NewTransition,
                        format!("New transition: {}", key),
                        evidence,
                        confidence,
                    );

                    payload.runner_name = context.runner_name.clone();
                    payload.config_name = context.config_name.clone();
                    payload.description = Some(format!(
                        "Discovered new transition {} with {:.1}% success rate over {} runs.",
                        key,
                        transition_stats.success_rate() * 100.0,
                        transition_stats.total
                    ));
                    payload.discovery_data = serde_json::json!({
                        "from_state": transition.from_state,
                        "to_state": transition.to_state,
                        "action": transition.action,
                        "success_rate": transition_stats.success_rate(),
                        "total_runs": transition_stats.total,
                        "avg_duration_ms": transition_stats.avg_duration_ms,
                    });

                    discoveries.push(payload);
                    debug!("Detected new transition: {}", key);
                }
            }
        }
    }

    discoveries
}

/// Calculate confidence score for flaky detection.
///
/// Higher confidence when:
/// - More runs observed
/// - Success rate closer to 50% (more clearly flaky)
fn calculate_flaky_confidence(total_runs: u32, success_rate: f64) -> f64 {
    // Base confidence from sample size (logarithmic growth)
    let sample_confidence = (total_runs as f64).ln() / 10.0;
    let sample_confidence = sample_confidence.min(0.3);

    // Higher confidence when rate is closer to 50% (clearly flaky)
    let rate_distance = (success_rate - 0.5).abs();
    let rate_confidence = 0.7 - rate_distance;

    (sample_confidence + rate_confidence).min(0.95)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiered_info::{RunStatus, TransitionStats};

    fn create_test_context() -> DetectionContext {
        DetectionContext {
            runner_id: "test-runner".to_string(),
            runner_name: Some("Test Runner".to_string()),
            project_id: "test-project".to_string(),
            config_id: "test-config".to_string(),
            config_name: Some("Test Config".to_string()),
        }
    }

    fn create_test_run() -> RunDetails {
        RunDetails::new(
            "run-1".to_string(),
            "test-config".to_string(),
            Some("test-workflow".to_string()),
        )
    }

    fn create_test_stats() -> ConfigStatistics {
        ConfigStatistics::new("test-config".to_string())
    }

    #[test]
    fn test_flaky_detection() {
        let context = create_test_context();
        let mut run = create_test_run();
        run.status = RunStatus::Completed;

        let mut stats = create_test_stats();
        stats.first_run_at = Some(chrono::Utc::now().to_rfc3339());

        // Add a flaky transition (50% success rate)
        let mut trans_stats = TransitionStats::default();
        trans_stats.total = 20;
        trans_stats.success = 10;
        trans_stats.failure = 10;
        stats
            .transition_stats
            .insert("StateA|StateB".to_string(), trans_stats);

        let thresholds = DetectionThresholds::default();
        let discoveries = detect_flaky_transitions(&run, &stats, &context, &thresholds);

        assert_eq!(discoveries.len(), 1);
        assert_eq!(discoveries[0].discovery_type, DiscoveryType::FlakyDetection);
        assert!(discoveries[0].title.contains("StateA|StateB"));
    }

    #[test]
    fn test_no_detection_below_threshold() {
        let context = create_test_context();
        let mut run = create_test_run();
        run.status = RunStatus::Completed;

        let mut stats = create_test_stats();

        // Add transition with too few runs
        let mut trans_stats = TransitionStats::default();
        trans_stats.total = 5; // Below threshold
        trans_stats.success = 2;
        trans_stats.failure = 3;
        stats
            .transition_stats
            .insert("StateA|StateB".to_string(), trans_stats);

        let thresholds = DetectionThresholds::default();
        let discoveries = detect_flaky_transitions(&run, &stats, &context, &thresholds);

        assert!(discoveries.is_empty());
    }

    #[test]
    fn test_running_run_not_analyzed() {
        let context = create_test_context();
        let run = create_test_run(); // Status is Running by default
        let stats = create_test_stats();
        let thresholds = DetectionThresholds::default();

        let discoveries = analyze_run_for_discoveries(&run, &stats, &context, &thresholds);
        assert!(discoveries.is_empty());
    }

    #[test]
    fn test_flaky_confidence_calculation() {
        // 50% success rate should have high confidence
        let conf_50 = calculate_flaky_confidence(20, 0.5);
        assert!(conf_50 > 0.6);

        // 30% success rate should have lower confidence
        let conf_30 = calculate_flaky_confidence(20, 0.3);
        assert!(conf_30 < conf_50);

        // More runs should increase confidence
        let conf_50_more = calculate_flaky_confidence(100, 0.5);
        assert!(conf_50_more > conf_50);
    }
}
