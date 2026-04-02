//! CRUD operations for workflow generation graph tables.
//!
//! Covers: workflow_versions, step_finding_links, step_provenance,
//! generation_pipeline_events, and rule_influence_log.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkflowVersion {
    pub id: String,
    pub workflow_id: String,
    pub version_number: i32,
    pub parent_version_id: Option<String>,
    pub generation_task_run_id: Option<String>,
    pub workflow_json: String,
    pub diff_summary: Option<String>,
    pub diff_json: Option<String>,
    pub trigger: String,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StepFindingLink {
    pub id: String,
    pub task_run_id: String,
    pub step_name: String,
    pub step_index: i32,
    pub finding_id: String,
    pub link_type: String,
    pub confidence: f64,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StepProvenance {
    pub id: String,
    pub workflow_id: String,
    pub workflow_version_id: Option<String>,
    pub step_name: String,
    pub step_index: i32,
    pub phase: String,
    pub generating_agent: String,
    pub generation_iteration: Option<i32>,
    pub original_step_json: Option<String>,
    pub final_step_json: Option<String>,
    pub ui_bridge_event_ids: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PipelineEvent {
    pub id: String,
    pub task_run_id: String,
    pub workflow_id: Option<String>,
    pub event_type: String,
    pub phase: Option<String>,
    pub iteration: Option<i32>,
    pub payload: Option<String>,
    pub duration_ms: Option<i64>,
    pub token_count: Option<i64>,
    pub validation_errors_before: Option<i32>,
    pub validation_errors_after: Option<i32>,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RuleInfluence {
    pub id: String,
    pub rule_id: String,
    pub task_run_id: String,
    pub workflow_id: Option<String>,
    pub influence_type: String,
    pub evidence: Option<String>,
    pub phase: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IneffectiveRule {
    pub rule_id: String,
    pub no_effect_count: i64,
    pub prevented_error_count: i64,
    pub total_loaded_count: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PhaseStats {
    pub phase: String,
    pub event_count: i64,
    pub avg_duration_ms: Option<f64>,
    pub total_token_count: i64,
    pub avg_errors_before: Option<f64>,
    pub avg_errors_after: Option<f64>,
}
