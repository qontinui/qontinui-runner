//! Tier 3: PRM Client
//!
//! HTTP client for the PRM scoring service (qontinui-prm).
//! Sends step text + criterion + context and receives a score + critique.
//! Falls back gracefully if the service is unavailable.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::workflow_generation::specification::AcceptanceCriteria;

pub struct PrmClient {
    base_url: String,
    client: reqwest::blocking::Client,
}

/// Request matching qontinui-prm's ScoreRequest schema.
#[derive(Debug, Clone, Serialize)]
struct PrmScoreRequest {
    step_text: String,
    criterion_text: String,
    context_text: String,
}

/// Request for batch scoring.
#[derive(Debug, Clone, Serialize)]
struct PrmBatchScoreRequest {
    items: Vec<PrmScoreRequest>,
}

/// Response matching qontinui-prm's ScoreResponse schema.
#[derive(Debug, Clone, Deserialize)]
pub struct PrmScoreResponse {
    pub score: f64,
    pub critique: String,
    pub confidence: f64,
    #[serde(default)]
    pub dimensions: std::collections::HashMap<String, bool>,
}

/// Response from batch scoring.
#[derive(Debug, Clone, Deserialize)]
pub struct PrmBatchScoreResponse {
    pub results: Vec<PrmScoreResponse>,
    pub total_duration_ms: f64,
}

/// Response from trajectory scoring.
#[derive(Debug, Clone, Deserialize)]
pub struct PrmTrajectoryScoreResponse {
    pub trajectory_score: f64,
    pub step_scores: Vec<PrmScoreResponse>,
    pub weakest_step_index: usize,
    pub weakest_step_critique: String,
}

impl PrmClient {
    pub fn new(base_url: &str) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());

        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
        }
    }

    /// Format a step JSON value into the text format the PRM server expects.
    fn format_step_text(step: &Value) -> String {
        let mut parts = Vec::new();

        let step_type = step
            .get("step_type")
            .or_else(|| step.get("type"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        parts.push(format!("Step Type: {step_type}"));

        let name = step
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed");
        parts.push(format!("Name: {name}"));

        if let Some(cmd) = step.get("command").and_then(|v| v.as_str()) {
            parts.push(format!("Command: {cmd}"));
        }
        if let Some(prompt) = step.get("prompt").and_then(|v| v.as_str()) {
            parts.push(format!("Prompt: {prompt}"));
        }
        if let Some(check) = step.get("check_type").and_then(|v| v.as_str()) {
            parts.push(format!("Check Type: {check}"));
        }
        if let Some(expected) = step.get("expected").and_then(|v| v.as_str()) {
            parts.push(format!("Expected: {expected}"));
        }

        parts.join("\n")
    }

    /// Format acceptance criteria into text for the PRM server.
    fn format_criteria_text(criteria: Option<&AcceptanceCriteria>) -> String {
        match criteria {
            Some(c) => {
                let mut parts = vec![format!("Goal: {}", c.goal_summary)];
                for crit in &c.criteria {
                    parts.push(format!(
                        "- {} ({:?}, {:?}): {}",
                        crit.id, crit.priority, crit.method, crit.description
                    ));
                }
                parts.join("\n")
            }
            None => "No specific acceptance criterion.".to_string(),
        }
    }

    /// Build a score request from a step JSON and optional criteria.
    fn build_request(
        step: &Value,
        criteria: Option<&AcceptanceCriteria>,
        context: Option<&str>,
    ) -> PrmScoreRequest {
        PrmScoreRequest {
            step_text: Self::format_step_text(step),
            criterion_text: Self::format_criteria_text(criteria),
            context_text: context.unwrap_or("").to_string(),
        }
    }

    /// Score a single step via the PRM service.
    pub fn score_step_sync(
        &self,
        step: &Value,
        criteria: Option<&AcceptanceCriteria>,
    ) -> Result<PrmScoreResponse, String> {
        let url = format!("{}/v1/score", self.base_url);
        let request = Self::build_request(step, criteria, None);

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .map_err(|e| format!("PRM service unavailable: {e}"))?;

        if !response.status().is_success() {
            return Err(format!(
                "PRM service returned error status: {}",
                response.status()
            ));
        }

        response
            .json::<PrmScoreResponse>()
            .map_err(|e| format!("Failed to parse PRM response: {e}"))
    }

    /// Batch score multiple steps via the PRM service.
    pub fn score_batch_sync(
        &self,
        steps: &[Value],
        criteria: Option<&AcceptanceCriteria>,
    ) -> Result<PrmBatchScoreResponse, String> {
        let url = format!("{}/v1/score/batch", self.base_url);
        let items: Vec<PrmScoreRequest> = steps
            .iter()
            .map(|s| Self::build_request(s, criteria, None))
            .collect();

        let request = PrmBatchScoreRequest { items };

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .map_err(|e| format!("PRM batch service unavailable: {e}"))?;

        if !response.status().is_success() {
            return Err(format!(
                "PRM batch service returned error status: {}",
                response.status()
            ));
        }

        response
            .json::<PrmBatchScoreResponse>()
            .map_err(|e| format!("Failed to parse PRM batch response: {e}"))
    }

    /// Score an entire trajectory (PURE min-form) via the PRM service.
    pub fn score_trajectory_sync(
        &self,
        steps: &[Value],
        criteria: Option<&AcceptanceCriteria>,
    ) -> Result<PrmTrajectoryScoreResponse, String> {
        let url = format!("{}/v1/score/trajectory", self.base_url);
        let items: Vec<PrmScoreRequest> = steps
            .iter()
            .map(|s| Self::build_request(s, criteria, None))
            .collect();

        let body = serde_json::json!({ "steps": items });

        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .map_err(|e| format!("PRM trajectory service unavailable: {e}"))?;

        if !response.status().is_success() {
            return Err(format!(
                "PRM trajectory service returned error status: {}",
                response.status()
            ));
        }

        response
            .json::<PrmTrajectoryScoreResponse>()
            .map_err(|e| format!("Failed to parse PRM trajectory response: {e}"))
    }

    /// Check if the PRM service is healthy.
    pub fn health(&self) -> bool {
        let url = format!("{}/health", self.base_url);
        match self.client.get(&url).send() {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }
}
