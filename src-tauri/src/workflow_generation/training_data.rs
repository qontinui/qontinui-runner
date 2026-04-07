//! Per-agent scoring for workflow generation training data.
//!
//! Currently only the `TrainingExample` type is retained as a public API
//! surface for the generator eval HTTP handler. Scoring and export logic
//! pending re-implementation against PostgreSQL.

use serde::{Deserialize, Serialize};

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
