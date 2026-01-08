//! Type definitions for the Tiered Information Model.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Run status enumeration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Timeout,
    Cancelled,
}

impl RunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::Timeout => "timeout",
            RunStatus::Cancelled => "cancelled",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "running" => Some(RunStatus::Running),
            "completed" => Some(RunStatus::Completed),
            "failed" => Some(RunStatus::Failed),
            "timeout" => Some(RunStatus::Timeout),
            "cancelled" => Some(RunStatus::Cancelled),
            _ => None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        !matches!(self, RunStatus::Running)
    }
}

/// Summary of actions executed in a run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionsSummary {
    pub total: u32,
    pub success: u32,
    pub failed: u32,
    pub skipped: u32,
}

/// Record of a transition execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionRecord {
    pub from_state: String,
    pub to_state: String,
    pub action: String,
    pub success: bool,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Record of template matching results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateMatchRecord {
    pub template: String,
    pub count: u32,
    pub avg_confidence: f32,
    pub failures: u32,
}

/// Record of a screenshot capture action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotRecord {
    /// Type of screenshot: "standalone" or "post_action"
    pub screenshot_type: String,
    /// Path to the captured screenshot file
    pub file_path: String,
    /// Monitor identifier (if specified)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
    /// Delay in seconds before capture (for post-action screenshots)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay_seconds: Option<u32>,
    /// Whether the capture was successful
    pub success: bool,
    /// Associated step/action name (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub associated_action: Option<String>,
    /// Timestamp of capture
    pub captured_at: String,
    /// Error message if capture failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Type of anomaly detected during a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnomalyType {
    /// Transition took 2x+ expected duration
    TimingAnomaly,
    /// Template match confidence < 0.7
    ConfidenceAnomaly,
    /// Expected element not found
    MissingElement,
    /// Element that shouldn't be present
    UnexpectedElement,
    /// Ended in unexpected state
    StateMismatch,
    /// Action failed unexpectedly
    UnexpectedFailure,
    /// Slow transition but not 2x threshold
    SlowTransition,
    /// Low confidence but not below 0.7 threshold
    LowConfidence,
}

impl AnomalyType {
    pub fn as_str(&self) -> &'static str {
        match self {
            AnomalyType::TimingAnomaly => "timing_anomaly",
            AnomalyType::ConfidenceAnomaly => "confidence_anomaly",
            AnomalyType::MissingElement => "missing_element",
            AnomalyType::UnexpectedElement => "unexpected_element",
            AnomalyType::StateMismatch => "state_mismatch",
            AnomalyType::UnexpectedFailure => "unexpected_failure",
            AnomalyType::SlowTransition => "slow_transition",
            AnomalyType::LowConfidence => "low_confidence",
        }
    }
}

/// Severity level for anomalies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum AnomalySeverity {
    Low,
    #[default]
    Medium,
    High,
}

impl AnomalySeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            AnomalySeverity::Low => "low",
            AnomalySeverity::Medium => "medium",
            AnomalySeverity::High => "high",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "low" | "info" => Some(AnomalySeverity::Low),
            "medium" | "warning" => Some(AnomalySeverity::Medium),
            "high" | "error" => Some(AnomalySeverity::High),
            _ => None,
        }
    }
}

/// Anomaly detected during a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anomaly {
    pub anomaly_type: AnomalyType,
    pub description: String,
    pub severity: AnomalySeverity,
    /// Expected value (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    /// Actual value observed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    /// When the anomaly was detected (ISO timestamp)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_at: Option<String>,
    /// Additional context for debugging
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
}

impl Anomaly {
    /// Create a new anomaly with required fields.
    pub fn new(anomaly_type: AnomalyType, description: String, severity: AnomalySeverity) -> Self {
        Self {
            anomaly_type,
            description,
            severity,
            expected: None,
            actual: None,
            detected_at: Some(chrono::Utc::now().to_rfc3339()),
            context: None,
        }
    }

    /// Create a timing anomaly when a transition took too long.
    pub fn timing_anomaly(expected_ms: u64, actual_ms: u64, transition: &str) -> Self {
        let severity = if actual_ms > expected_ms * 3 {
            AnomalySeverity::High
        } else {
            AnomalySeverity::Medium
        };

        Self {
            anomaly_type: AnomalyType::TimingAnomaly,
            description: format!(
                "Transition '{}' took {}ms, expected ~{}ms ({}x slower)",
                transition,
                actual_ms,
                expected_ms,
                actual_ms / expected_ms.max(1)
            ),
            severity,
            expected: Some(format!("{}ms", expected_ms)),
            actual: Some(format!("{}ms", actual_ms)),
            detected_at: Some(chrono::Utc::now().to_rfc3339()),
            context: Some(serde_json::json!({
                "transition": transition,
                "expected_ms": expected_ms,
                "actual_ms": actual_ms,
            })),
        }
    }

    /// Create a confidence anomaly when template match confidence is low.
    pub fn confidence_anomaly(template: &str, confidence: f32, threshold: f32) -> Self {
        let severity = if confidence < threshold * 0.5 {
            AnomalySeverity::High
        } else {
            AnomalySeverity::Medium
        };

        Self {
            anomaly_type: AnomalyType::ConfidenceAnomaly,
            description: format!(
                "Template '{}' matched with low confidence {:.2} (threshold: {:.2})",
                template, confidence, threshold
            ),
            severity,
            expected: Some(format!(">= {:.2}", threshold)),
            actual: Some(format!("{:.2}", confidence)),
            detected_at: Some(chrono::Utc::now().to_rfc3339()),
            context: Some(serde_json::json!({
                "template": template,
                "confidence": confidence,
                "threshold": threshold,
            })),
        }
    }

    /// Create a missing element anomaly.
    pub fn missing_element(element: &str, context: &str) -> Self {
        Self {
            anomaly_type: AnomalyType::MissingElement,
            description: format!("Expected element '{}' not found in {}", element, context),
            severity: AnomalySeverity::High,
            expected: Some(element.to_string()),
            actual: Some("not found".to_string()),
            detected_at: Some(chrono::Utc::now().to_rfc3339()),
            context: Some(serde_json::json!({
                "element": element,
                "search_context": context,
            })),
        }
    }

    /// Create a state mismatch anomaly.
    pub fn state_mismatch(expected_state: &str, actual_state: &str) -> Self {
        Self {
            anomaly_type: AnomalyType::StateMismatch,
            description: format!(
                "Expected to be in state '{}' but found '{}'",
                expected_state, actual_state
            ),
            severity: AnomalySeverity::High,
            expected: Some(expected_state.to_string()),
            actual: Some(actual_state.to_string()),
            detected_at: Some(chrono::Utc::now().to_rfc3339()),
            context: None,
        }
    }
}

/// Tier 1: Detailed run information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunDetails {
    pub id: String,
    pub config_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    pub status: RunStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions_summary: Option<ActionsSummary>,
    #[serde(default)]
    pub states_visited: Vec<String>,
    #[serde(default)]
    pub transitions_executed: Vec<TransitionRecord>,
    #[serde(default)]
    pub template_matches: Vec<TemplateMatchRecord>,
    #[serde(default)]
    pub anomalies: Vec<Anomaly>,
    /// Screenshots captured during the run (standalone and post-action)
    #[serde(default)]
    pub screenshots: Vec<ScreenshotRecord>,
}

impl RunDetails {
    /// Create a new RunDetails for a starting run.
    pub fn new(id: String, config_id: String, workflow_name: Option<String>) -> Self {
        Self {
            id,
            config_id,
            workflow_name,
            started_at: chrono::Utc::now().to_rfc3339(),
            ended_at: None,
            duration_ms: None,
            status: RunStatus::Running,
            success: None,
            error_type: None,
            error_message: None,
            actions_summary: None,
            states_visited: Vec::new(),
            transitions_executed: Vec::new(),
            template_matches: Vec::new(),
            anomalies: Vec::new(),
            screenshots: Vec::new(),
        }
    }
}

/// Statistics for a specific transition.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransitionStats {
    pub total: u32,
    pub success: u32,
    pub failure: u32,
    pub avg_duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<String>, // ISO timestamp
    /// Standard deviation of duration (for flakiness detection)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_stddev: Option<f64>,
}

impl TransitionStats {
    pub fn success_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.success as f64 / self.total as f64
        }
    }

    /// Check if this transition is flaky (high variance or intermediate success rate)
    pub fn is_flaky(&self, threshold: f64) -> bool {
        let rate = self.success_rate();
        // Flaky if success rate is between threshold and 1-threshold
        // e.g., threshold=0.2 means 20%-80% success rate is considered flaky
        rate > threshold && rate < (1.0 - threshold) && self.total >= 5
    }
}

/// Statistics for a specific template.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TemplateStats {
    pub total: u32,
    pub matches: u32,
    pub failures: u32,
    pub avg_confidence: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_confidence: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_confidence: Option<f32>,
}

impl TemplateStats {
    pub fn match_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.matches as f64 / self.total as f64
        }
    }

    /// Check if this template is flaky (unreliable matching)
    pub fn is_flaky(&self, threshold: f64) -> bool {
        let rate = self.match_rate();
        rate > threshold && rate < (1.0 - threshold) && self.total >= 5
    }
}

/// Statistics for a specific state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StateStats {
    pub visits: u32,
    pub avg_time_in_state_ms: u64,
    pub entry_failures: u32,
}

/// Error pattern tracking.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ErrorPattern {
    pub count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<String>, // ISO timestamp
    pub contexts: Vec<String>, // Recent error contexts (limited)
}

/// Tier 4: Aggregated config statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigStatistics {
    pub id: String,
    pub config_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_hash: Option<String>,
    pub total_runs: u32,
    pub successful_runs: u32,
    pub failed_runs: u32,
    pub timeout_runs: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent_success_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent_avg_duration_ms: Option<u64>,
    #[serde(default)]
    pub transition_stats: HashMap<String, TransitionStats>,
    #[serde(default)]
    pub template_stats: HashMap<String, TemplateStats>,
    #[serde(default)]
    pub state_stats: HashMap<String, StateStats>,
    #[serde(default)]
    pub error_patterns: HashMap<String, ErrorPattern>,
    #[serde(default)]
    pub flaky_transitions: Vec<String>,
    #[serde(default)]
    pub flaky_templates: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_run_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated_at: Option<String>,
}

impl ConfigStatistics {
    /// Create empty statistics for a config.
    pub fn new(config_id: String) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            id,
            config_id,
            config_hash: None,
            total_runs: 0,
            successful_runs: 0,
            failed_runs: 0,
            timeout_runs: 0,
            avg_duration_ms: None,
            recent_success_rate: None,
            recent_avg_duration_ms: None,
            transition_stats: HashMap::new(),
            template_stats: HashMap::new(),
            state_stats: HashMap::new(),
            error_patterns: HashMap::new(),
            flaky_transitions: Vec::new(),
            flaky_templates: Vec::new(),
            first_run_at: None,
            last_run_at: None,
            last_updated_at: None,
        }
    }

    /// Overall success rate.
    pub fn success_rate(&self) -> f64 {
        if self.total_runs == 0 {
            0.0
        } else {
            self.successful_runs as f64 / self.total_runs as f64
        }
    }

    /// Recalculate flaky transitions based on current stats.
    pub fn recalculate_flaky_transitions(&mut self, threshold: f64) {
        self.flaky_transitions = self
            .transition_stats
            .iter()
            .filter(|(_, stats)| stats.is_flaky(threshold))
            .map(|(key, _)| key.clone())
            .collect();
    }

    /// Recalculate flaky templates based on current stats.
    pub fn recalculate_flaky_templates(&mut self, threshold: f64) {
        self.flaky_templates = self
            .template_stats
            .iter()
            .filter(|(_, stats)| stats.is_flaky(threshold))
            .map(|(key, _)| key.clone())
            .collect();
    }
}

/// Flaky item (transition or template) with detailed info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlakyItem {
    pub name: String,
    pub item_type: String, // "transition" or "template"
    pub success_rate: f64,
    pub total_attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Debugging context for AI prompts.
/// This is the Tier 4 summary formatted for AI consumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebuggingContext {
    pub config_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_name: Option<String>,

    // Overall health
    pub total_runs: u32,
    pub success_rate: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent_success_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_duration_ms: Option<u64>,

    // Problem areas
    #[serde(default)]
    pub flaky_transitions: Vec<FlakyItem>,
    #[serde(default)]
    pub flaky_templates: Vec<FlakyItem>,
    #[serde(default)]
    pub common_errors: Vec<(String, u32)>, // (error_type, count)

    // Recent failures (for immediate context)
    #[serde(default)]
    pub recent_failures: Vec<RunFailureSummary>,
}

/// Brief summary of a failed run for debugging context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunFailureSummary {
    pub run_id: String,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_transition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_template: Option<String>,
}
