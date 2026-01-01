//! Type definitions for the Discovery Push mechanism.
//!
//! Discoveries are patterns detected from automation runs that can be pushed
//! to qontinui-web for review and potential state structure updates.

use serde::{Deserialize, Serialize};

/// Types of discoveries that can be detected from automation runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryType {
    /// A new UI element detected consistently across runs
    NewElement,
    /// A new transition path between states
    NewTransition,
    /// Updated timing data for existing transitions
    TimingUpdate,
    /// Detection of flaky behavior (inconsistent success rate)
    FlakyDetection,
    /// An unexpected element appeared that's not in the state structure
    UnexpectedElement,
}

impl DiscoveryType {
    pub fn as_str(&self) -> &'static str {
        match self {
            DiscoveryType::NewElement => "new_element",
            DiscoveryType::NewTransition => "new_transition",
            DiscoveryType::TimingUpdate => "timing_update",
            DiscoveryType::FlakyDetection => "flaky_detection",
            DiscoveryType::UnexpectedElement => "unexpected_element",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "new_element" => Some(DiscoveryType::NewElement),
            "new_transition" => Some(DiscoveryType::NewTransition),
            "timing_update" => Some(DiscoveryType::TimingUpdate),
            "flaky_detection" => Some(DiscoveryType::FlakyDetection),
            "unexpected_element" => Some(DiscoveryType::UnexpectedElement),
            _ => None,
        }
    }
}

/// Evidence supporting a discovery, built from observation data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryEvidence {
    /// Number of runs where this pattern was observed
    pub runs_observed: u32,
    /// First time this pattern was seen (ISO 8601 timestamp)
    pub first_seen: String,
    /// Last time this pattern was seen (ISO 8601 timestamp)
    pub last_seen: String,
    /// Average confidence score of pattern matches (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence_avg: Option<f64>,
    /// Base64-encoded sample screenshots (limited to save bandwidth)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_screenshots: Option<Vec<String>>,
}

impl DiscoveryEvidence {
    /// Create new evidence with initial observation.
    pub fn new(first_seen: String) -> Self {
        Self {
            runs_observed: 1,
            first_seen: first_seen.clone(),
            last_seen: first_seen,
            confidence_avg: None,
            sample_screenshots: None,
        }
    }

    /// Update evidence with a new observation.
    pub fn add_observation(&mut self, timestamp: String, confidence: Option<f64>) {
        self.runs_observed += 1;
        self.last_seen = timestamp;

        if let Some(new_conf) = confidence {
            if let Some(avg) = self.confidence_avg {
                // Running average
                self.confidence_avg = Some(
                    (avg * (self.runs_observed - 1) as f64 + new_conf) / self.runs_observed as f64,
                );
            } else {
                self.confidence_avg = Some(new_conf);
            }
        }
    }
}

/// Payload for pushing a discovery to qontinui-web.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryPayload {
    /// Runner's device ID (from auth)
    pub runner_id: String,
    /// Human-readable runner name (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_name: Option<String>,
    /// Project ID this discovery belongs to
    pub project_id: String,
    /// Config ID (state structure) where this was discovered
    pub config_id: String,
    /// Human-readable config name (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_name: Option<String>,
    /// Type of discovery
    pub discovery_type: DiscoveryType,
    /// Short title for the discovery
    pub title: String,
    /// Detailed description of what was discovered
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Discovery-specific data (varies by type)
    pub discovery_data: serde_json::Value,
    /// Evidence supporting this discovery
    pub evidence: DiscoveryEvidence,
    /// Confidence score (0.0 - 1.0) that this is a valid discovery
    pub confidence: f64,
    /// Number of runs where this pattern was observed
    pub runs_observed: u32,
}

impl DiscoveryPayload {
    /// Create a new discovery payload.
    pub fn new(
        runner_id: String,
        project_id: String,
        config_id: String,
        discovery_type: DiscoveryType,
        title: String,
        evidence: DiscoveryEvidence,
        confidence: f64,
    ) -> Self {
        let runs_observed = evidence.runs_observed;
        Self {
            runner_id,
            runner_name: None,
            project_id,
            config_id,
            config_name: None,
            discovery_type,
            title,
            description: None,
            discovery_data: serde_json::json!({}),
            evidence,
            confidence,
            runs_observed,
        }
    }
}

/// Response from qontinui-web when a discovery is submitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryResponse {
    /// ID assigned to the discovery by the backend
    pub id: String,
    /// Status of the submitted discovery
    pub status: String,
}

/// A discovery queued locally for later sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingDiscovery {
    /// Local ID for tracking
    pub id: String,
    /// The discovery payload to send
    pub payload: DiscoveryPayload,
    /// When this was queued (ISO 8601 timestamp)
    pub created_at: String,
    /// Last sync attempt (ISO 8601 timestamp)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_attempt: Option<String>,
    /// Number of sync attempts
    pub attempt_count: u32,
    /// Last error message if sync failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl PendingDiscovery {
    /// Create a new pending discovery.
    pub fn new(payload: DiscoveryPayload) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            payload,
            created_at: now,
            last_attempt: None,
            attempt_count: 0,
            error: None,
        }
    }

    /// Record a failed sync attempt.
    pub fn record_failure(&mut self, error: String) {
        self.last_attempt = Some(chrono::Utc::now().to_rfc3339());
        self.attempt_count += 1;
        self.error = Some(error);
    }

    /// Record a successful sync.
    pub fn record_success(&mut self) {
        self.last_attempt = Some(chrono::Utc::now().to_rfc3339());
        self.error = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discovery_type_serialization() {
        let dt = DiscoveryType::NewElement;
        let json = serde_json::to_string(&dt).unwrap();
        assert_eq!(json, "\"new_element\"");

        let parsed: DiscoveryType = serde_json::from_str("\"flaky_detection\"").unwrap();
        assert_eq!(parsed, DiscoveryType::FlakyDetection);
    }

    #[test]
    fn test_evidence_running_average() {
        let now = chrono::Utc::now().to_rfc3339();
        let mut evidence = DiscoveryEvidence::new(now.clone());
        evidence.confidence_avg = Some(0.9);

        evidence.add_observation(now.clone(), Some(0.8));
        // (0.9 * 1 + 0.8) / 2 = 0.85
        assert!((evidence.confidence_avg.unwrap() - 0.85).abs() < 0.001);

        evidence.add_observation(now, Some(0.7));
        // (0.85 * 2 + 0.7) / 3 = 0.8
        assert!((evidence.confidence_avg.unwrap() - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_pending_discovery_creation() {
        let evidence = DiscoveryEvidence::new(chrono::Utc::now().to_rfc3339());
        let payload = DiscoveryPayload::new(
            "runner-1".to_string(),
            "project-1".to_string(),
            "config-1".to_string(),
            DiscoveryType::NewElement,
            "Found new button".to_string(),
            evidence,
            0.95,
        );

        let pending = PendingDiscovery::new(payload);
        assert_eq!(pending.attempt_count, 0);
        assert!(pending.error.is_none());
        assert!(pending.last_attempt.is_none());
    }
}
