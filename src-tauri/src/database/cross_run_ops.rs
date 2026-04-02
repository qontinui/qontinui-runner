//! CRUD and detection query operations for the `cross_run_patterns` table.
//!
//! Tracks patterns that recur across multiple workflow runs, such as
//! recurring findings and fix oscillations, enabling cross-run analysis.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossRunPattern {
    pub id: String,
    pub pattern_type: String,
    pub signature_hash: String,
    pub workflow_name: Option<String>,
    pub occurrence_count: i32,
    pub first_seen_task_run_id: Option<String>,
    pub last_seen_task_run_id: Option<String>,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub affected_components: Option<String>,
    pub pattern_data: Option<String>,
    pub status: String,
    pub resolved_by_fix_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Fix effectiveness score for resolved patterns.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FixEffectivenessScore {
    pub fix_id: String,
    pub pattern_id: String,
    pub pattern_type: String,
    pub resolved_at: Option<String>,
    pub re_appeared: bool,
    pub occurrence_count_at_resolution: i64,
}
