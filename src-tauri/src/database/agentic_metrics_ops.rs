//! Agentic metric score types (SQLite impl removed).

use serde::{Deserialize, Serialize};

/// A stored agentic metric score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticMetricScoreRow {
    pub id: String,
    pub task_run_id: String,
    pub metric_type: String,
    pub score: f64,
    pub confidence: f64,
    pub rationale: Option<String>,
    pub is_llm_judged: bool,
    pub model_used: Option<String>,
    pub created_at: String,
}

/// Aggregate metric stats across multiple runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticMetricAggregate {
    pub metric_type: String,
    pub mean_score: f64,
    pub min_score: f64,
    pub max_score: f64,
    pub runs_scored: i64,
}

/// A single point in the composite score trend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositeScoreTrendPoint {
    pub date: String,
    pub avg_composite_score: f64,
    pub run_count: i64,
    pub success_count: i64,
}
