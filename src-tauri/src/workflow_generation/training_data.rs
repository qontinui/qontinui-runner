//! Per-agent scoring for workflow generation training data.
//!
//! Combines data from pipeline artifacts, reflection fixes, user feedback,
//! and supervisor eval benchmarks to compute a quality score per generation agent.
//! This enables prompt optimization by identifying which agents produce the
//! most issues and what patterns correlate with high/low quality.

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

// ============================================================================
// Types
// ============================================================================

/// Quality score for a single generation agent on a single pipeline run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentScore {
    pub agent: String,
    pub artifact_id: String,
    pub score: f64,
    pub factors: ScoreFactors,
}

/// Breakdown of factors that contribute to an agent's score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreFactors {
    // From pipeline artifacts
    pub fixer_iterations: u32,
    pub benchmark_score: Option<f64>,

    // From reflection
    pub attributed_fix_count: u32,
    pub effective_fix_count: u32,

    // From feedback
    pub user_edit_count: u32,
    pub user_deleted: bool,
    pub user_rating: Option<f64>,

    // From supervisor eval (if available)
    pub eval_overall_score: Option<f64>,
    pub eval_dimension_scores: Option<serde_json::Value>,
}

/// Training example: prompt + completion + score for one agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingExample {
    pub agent: String,
    pub prompt: String,
    pub completion: String,
    pub score: f64,
    pub artifact_id: String,
    pub workflow_id: Option<String>,
    pub created_at: String,
}

// ============================================================================
// Per-Agent Scoring Functions
// ============================================================================

/// Score the specification agent's output quality.
pub fn score_specification(factors: &ScoreFactors) -> f64 {
    let mut score = 1.0;
    // Penalize if reflection found spec-attributed issues
    score -= 0.15 * factors.attributed_fix_count as f64;
    // Reward if benchmark scored well (criteria led to good verification)
    if let Some(bench) = factors.benchmark_score {
        score *= 0.5 + 0.5 * bench;
    }
    // Penalize if user heavily edited verification steps
    if factors.user_edit_count > 3 {
        score -= 0.2;
    }
    // User deleted the whole workflow
    if factors.user_deleted {
        score -= 0.3;
    }
    score.clamp(0.0, 1.0)
}

/// Score the builder agent's output quality.
pub fn score_builder(factors: &ScoreFactors) -> f64 {
    let mut score = 1.0;
    // Penalize for fixer iterations (0 = builder got it right first time)
    score -= 0.1 * factors.fixer_iterations as f64;
    // Penalize for reflection-attributed issues
    score -= 0.15 * factors.attributed_fix_count as f64;
    // User feedback
    if let Some(rating) = factors.user_rating {
        score = score * 0.6 + (rating / 5.0) * 0.4;
    }
    if factors.user_deleted {
        score -= 0.4;
    }
    // Benchmark score (structural + content)
    if let Some(bench) = factors.benchmark_score {
        score = score * 0.7 + bench * 0.3;
    }
    score.clamp(0.0, 1.0)
}

/// Score the verification agent's output quality.
pub fn score_verification(factors: &ScoreFactors) -> f64 {
    let mut score = 1.0;
    // Penalize for reflection-attributed issues (verification missed real problems)
    score -= 0.2 * factors.attributed_fix_count as f64;
    // Reward effective fixes (verification found real issues)
    if factors.effective_fix_count > 0 {
        score += 0.05 * factors.effective_fix_count as f64;
    }
    // Eval dimensions if available
    if let Some(eval_score) = factors.eval_overall_score {
        score = score * 0.6 + eval_score * 0.4;
    }
    score.clamp(0.0, 1.0)
}

/// Score the hardener agent's output quality.
pub fn score_hardener(factors: &ScoreFactors) -> f64 {
    let mut score = 1.0;
    // Penalize for reflection-attributed issues (hardener broke something)
    score -= 0.2 * factors.attributed_fix_count as f64;
    // User edits after hardening suggest bad conversions
    if factors.user_edit_count > 2 {
        score -= 0.15;
    }
    if factors.user_deleted {
        score -= 0.3;
    }
    score.clamp(0.0, 1.0)
}

/// Score any agent by name.
pub fn score_agent(agent: &str, factors: &ScoreFactors) -> f64 {
    match agent {
        "specification" => score_specification(factors),
        "builder" => score_builder(factors),
        "verification" => score_verification(factors),
        "hardener" => score_hardener(factors),
        _ => {
            warn!("Unknown agent for scoring: {}", agent);
            0.5
        }
    }
}

// ============================================================================
// Data Collection
// ============================================================================

/// Collect score factors for a specific agent and artifact.
pub fn collect_score_factors(
    artifact_id: &str,
    workflow_id: Option<&str>,
    agent: &str,
) -> Result<ScoreFactors, String> {
    Err("SQLite removed".to_string())
}

/// Count reflection fixes attributed to a specific agent for a workflow.
fn count_attributed_fixes(workflow_id: &str, agent: &str) -> Result<(u32, u32), String> {
    Err("SQLite removed".to_string())
}

/// Get user feedback for a workflow.
fn get_user_feedback(workflow_id: &str) -> Result<(u32, bool, Option<f64>), String> {
    Err("SQLite removed".to_string())
}

// ============================================================================
// Training Data Export
// ============================================================================

/// Export scored training examples for a specific agent.
pub fn export_training_data(
    agent: &str,
    min_score: f64,
    limit: u32,
) -> Result<Vec<TrainingExample>, String> {
    Err("SQLite removed".to_string())
}

