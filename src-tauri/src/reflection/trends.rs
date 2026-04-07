//! Temporal Trend Analysis types.
//!
//! The query implementations live on `PgDb` in `database/pg/reflection.rs`.
//! This module keeps only the shared result types used across the API.

use serde::Serialize;

// =============================================================================
// Types
// =============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct TrendPoint {
    pub timestamp: String,
    pub value: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowTrends {
    pub workflow_name: String,
    pub convergence: Vec<TrendPoint>,
    pub fix_rate: Vec<TrendPoint>,
    pub velocity: Vec<TrendPoint>,
    pub total_fixes: Vec<TrendPoint>,
    pub snapshot_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentTrend {
    pub component_path: String,
    pub health_scores: Vec<TrendPoint>,
    pub fix_counts: Vec<TrendPoint>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EffectivenessBucket {
    pub bucket: String,
    pub total: u32,
    pub effective: u32,
    pub ineffective: u32,
    pub regression: u32,
    pub effectiveness_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EffectivenessOverTime {
    pub workflow_name: String,
    pub bucket_type: String,
    pub buckets: Vec<EffectivenessBucket>,
}
