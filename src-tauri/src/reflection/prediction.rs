//! Prediction engine types for the cognitive system model.
//!
//! The implementations now live on `PgDb` in `database/pg/reflection.rs`.
//! This module retains only the shared result types used across the API
//! surface (mcp/reflection_api.rs, pg/reflection.rs).

use serde::Serialize;

// =============================================================================
// Types
// =============================================================================

/// A predicted fix for an error, based on historical fix application data.
#[derive(Debug, Clone, Serialize)]
pub struct PredictedFix {
    pub fix_id: String,
    pub fix_description: String,
    pub confidence: f64,
    pub reuse_count: u32,
    pub effective_rate: f64,
}

/// Convergence metrics for a workflow or project scope.
#[derive(Debug, Clone, Serialize)]
pub struct ConvergenceMetrics {
    pub score: f64,
    pub consecutive_clean_runs: u32,
    pub novelty_score: f64,
    pub effective_fix_rate: f64,
    pub change_velocity: f64,
    pub total_fixes: u32,
    pub effective_fixes: u32,
}

/// A knowledge entry scored by relevance for token-budget prioritization.
#[derive(Debug, Clone, Serialize)]
pub struct ScoredKnowledge {
    pub knowledge_id: String,
    pub content: String,
    pub category: String,
    pub relevance_score: f64,
    pub effectiveness_weight: f64,
    pub recency_weight: f64,
    pub velocity_factor: f64,
}
