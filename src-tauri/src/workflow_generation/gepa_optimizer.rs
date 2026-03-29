//! GEPA Prompt Evolution Optimizer
//!
//! Integrates with a Python GEPA sidecar service to evolve verification
//! prompts using Greedy Evolutionary Prompt Adaptation. The Rust side calls
//! the Python service via HTTP, supplying training examples extracted from
//! historical learning outcomes and task runs.

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

// ============================================================================
// Domain Classification
// ============================================================================

/// Verification domain for domain-specific prompt optimization.
///
/// Each domain groups verification steps by the system layer they test,
/// enabling targeted prompt evolution per domain rather than one-size-fits-all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationDomain {
    Compilation,
    Api,
    Ui,
    Database,
    Security,
    General,
}

impl VerificationDomain {
    /// All known verification domains.
    pub fn all() -> &'static [VerificationDomain] {
        &[
            VerificationDomain::Compilation,
            VerificationDomain::Api,
            VerificationDomain::Ui,
            VerificationDomain::Database,
            VerificationDomain::Security,
            VerificationDomain::General,
        ]
    }

    /// String representation used in API calls and logging.
    pub fn as_str(&self) -> &'static str {
        match self {
            VerificationDomain::Compilation => "compilation",
            VerificationDomain::Api => "api",
            VerificationDomain::Ui => "ui",
            VerificationDomain::Database => "database",
            VerificationDomain::Security => "security",
            VerificationDomain::General => "general",
        }
    }
}

impl std::fmt::Display for VerificationDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the GEPA optimization process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GEPAConfig {
    /// Maximum optimization rounds per invocation.
    pub max_rounds: usize,
    /// Number of candidate prompts to evaluate per round.
    pub num_candidates: usize,
    /// Minimum score improvement to accept the new prompt.
    pub min_improvement_threshold: f64,
}

impl Default for GEPAConfig {
    fn default() -> Self {
        Self {
            max_rounds: 5,
            num_candidates: 3,
            min_improvement_threshold: 0.05,
        }
    }
}

// ============================================================================
// Result Types
// ============================================================================

/// Result of a GEPA optimization run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GEPAResult {
    /// Original instructions before optimization.
    pub old_instructions: String,
    /// New optimized instructions.
    pub new_instructions: String,
    /// Score of the original instructions.
    pub old_score: f64,
    /// Score of the optimized instructions.
    pub new_score: f64,
    /// Improvement delta (new_score - old_score).
    pub improvement: f64,
    /// Few-shot examples selected during optimization.
    pub few_shot_examples: Vec<serde_json::Value>,
}

// ============================================================================
// HTTP API Types
// ============================================================================

/// Request body for the domain-specific optimization endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainOptimizeRequest {
    pub domain: String,
    pub training_examples: Vec<serde_json::Value>,
    pub current_instructions: String,
    pub evaluation_api_url: String,
    #[serde(default)]
    pub config: Option<GEPAConfig>,
}

/// Response from the GEPA optimization endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizeResponse {
    pub success: bool,
    #[serde(default)]
    pub result: Option<GEPAResult>,
    #[serde(default)]
    pub error: Option<String>,
}

// ============================================================================
// Integration
// ============================================================================

/// Client for the GEPA Python sidecar service.
///
/// Manages connection state, cooldown tracking, and HTTP communication
/// with the GEPA optimization service.
pub struct GepaIntegration {
    /// Base URL of the GEPA service (e.g. "http://localhost:8200").
    gepa_url: String,
    /// Whether GEPA optimization is enabled.
    enabled: bool,
    /// Minimum duration between optimization runs.
    optimization_cooldown: std::time::Duration,
    /// Minimum number of new runs before re-optimizing.
    min_runs_between_optimizations: usize,
    /// Timestamp of the last optimization run.
    last_optimization: Option<std::time::Instant>,
    /// HTTP client for communicating with the GEPA sidecar.
    client: reqwest::Client,
}

impl GepaIntegration {
    /// Create a new GEPA integration pointed at the given service URL.
    pub fn new(gepa_url: &str) -> Self {
        Self {
            gepa_url: gepa_url.trim_end_matches('/').to_string(),
            enabled: true,
            optimization_cooldown: std::time::Duration::from_secs(30 * 60), // 30 minutes
            min_runs_between_optimizations: 10,
            last_optimization: None,
            client: reqwest::Client::new(),
        }
    }

    /// Run domain-specific prompt optimization via the GEPA sidecar.
    ///
    /// Returns `Ok(Some(result))` if the optimization improved the score above
    /// the threshold, `Ok(None)` if improvement was insufficient, or `Err` on
    /// communication failure.
    pub async fn optimize_domain(
        &mut self,
        domain: VerificationDomain,
        training_examples: Vec<serde_json::Value>,
        current_instructions: &str,
        evaluation_api_url: &str,
    ) -> Result<Option<GEPAResult>, String> {
        if !self.enabled {
            debug!("GEPA optimization disabled, skipping");
            return Ok(None);
        }

        if !self.cooldown_elapsed() {
            debug!("GEPA optimization cooldown not yet elapsed, skipping");
            return Ok(None);
        }

        if training_examples.len() < self.min_runs_between_optimizations {
            debug!(
                count = training_examples.len(),
                min = self.min_runs_between_optimizations,
                "Not enough training examples for GEPA optimization"
            );
            return Ok(None);
        }

        let request = DomainOptimizeRequest {
            domain: domain.as_str().to_string(),
            training_examples,
            current_instructions: current_instructions.to_string(),
            evaluation_api_url: evaluation_api_url.to_string(),
            config: Some(GEPAConfig::default()),
        };

        let url = format!("{}/gepa/optimize/domain", self.gepa_url);
        info!(domain = %domain, url = %url, "Sending GEPA domain optimization request");

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| format!("GEPA HTTP request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable>".to_string());
            warn!(status = %status, body = %body, "GEPA optimization request failed");
            return Err(format!("GEPA returned {status}: {body}"));
        }

        let opt_response: OptimizeResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse GEPA response: {e}"))?;

        if !opt_response.success {
            let err = opt_response
                .error
                .unwrap_or_else(|| "unknown error".to_string());
            warn!(error = %err, "GEPA optimization reported failure");
            return Err(err);
        }

        // Update cooldown timestamp
        self.last_optimization = Some(std::time::Instant::now());

        match opt_response.result {
            Some(result) if result.improvement >= GEPAConfig::default().min_improvement_threshold => {
                info!(
                    domain = %domain,
                    old_score = result.old_score,
                    new_score = result.new_score,
                    improvement = result.improvement,
                    "GEPA optimization improved prompt"
                );
                Ok(Some(result))
            }
            Some(result) => {
                debug!(
                    domain = %domain,
                    improvement = result.improvement,
                    threshold = GEPAConfig::default().min_improvement_threshold,
                    "GEPA improvement below threshold, keeping current prompt"
                );
                Ok(None)
            }
            None => {
                debug!("GEPA returned success but no result");
                Ok(None)
            }
        }
    }

    /// Check whether enough time has passed since the last optimization.
    pub fn cooldown_elapsed(&self) -> bool {
        match self.last_optimization {
            None => true,
            Some(last) => last.elapsed() >= self.optimization_cooldown,
        }
    }

    /// SQL query to extract training examples from learning outcomes and task runs.
    ///
    /// Returns rows with: domain, instructions, outcome_json, score, created_at.
    /// These feed into the GEPA training pipeline as (input, output, label) triples.
    pub fn extract_training_examples_query() -> &'static str {
        r#"
        SELECT
            lo.category AS domain,
            tr.prompt AS instructions,
            lo.outcome AS outcome_json,
            COALESCE(lo.confidence, 0.5) AS score,
            lo.created_at
        FROM learning_outcomes lo
        JOIN task_runs tr ON tr.id = lo.task_run_id
        WHERE lo.outcome IS NOT NULL
          AND tr.prompt IS NOT NULL
          AND lo.created_at > datetime('now', '-30 days')
        ORDER BY lo.created_at DESC
        LIMIT 500
        "#
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verification_domain_all() {
        let all = VerificationDomain::all();
        assert_eq!(all.len(), 6);
    }

    #[test]
    fn test_verification_domain_as_str() {
        assert_eq!(VerificationDomain::Compilation.as_str(), "compilation");
        assert_eq!(VerificationDomain::Api.as_str(), "api");
        assert_eq!(VerificationDomain::Ui.as_str(), "ui");
        assert_eq!(VerificationDomain::Database.as_str(), "database");
        assert_eq!(VerificationDomain::Security.as_str(), "security");
        assert_eq!(VerificationDomain::General.as_str(), "general");
    }

    #[test]
    fn test_gepa_config_defaults() {
        let config = GEPAConfig::default();
        assert_eq!(config.max_rounds, 5);
        assert_eq!(config.num_candidates, 3);
        assert!((config.min_improvement_threshold - 0.05).abs() < f64::EPSILON);
    }

    #[test]
    fn test_gepa_integration_new() {
        let integration = GepaIntegration::new("http://localhost:8200/");
        assert_eq!(integration.gepa_url, "http://localhost:8200");
        assert!(integration.enabled);
        assert!(integration.cooldown_elapsed());
    }

    #[test]
    fn test_extract_training_examples_query() {
        let query = GepaIntegration::extract_training_examples_query();
        assert!(query.contains("learning_outcomes"));
        assert!(query.contains("task_runs"));
    }

    #[test]
    fn test_domain_serde_roundtrip() {
        let domain = VerificationDomain::Security;
        let json = serde_json::to_string(&domain).unwrap();
        assert_eq!(json, "\"security\"");
        let back: VerificationDomain = serde_json::from_str(&json).unwrap();
        assert_eq!(back, domain);
    }
}
