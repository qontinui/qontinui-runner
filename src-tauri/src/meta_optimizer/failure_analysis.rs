//! Failure analysis module for the meta-optimizer.
//!
//! Aggregates failure data from multiple existing tables (read-only) to provide
//! a comprehensive view of what's going wrong: abort reasons, verification failures,
//! finding distributions, fix effectiveness, generation quality, and recurring issues.

#[allow(unused_imports)]
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use tokio::runtime::Handle;

use super::types::WorkflowCategory;
use crate::database::pg::PgDb;
use tracing::debug;

// ── Data Structures ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureAnalysis {
    pub period_days: u32,
    pub total_runs: i64,
    pub failed_runs: i64,
    pub failure_rate: f64,
    pub abort_reasons: Vec<AbortReason>,
    pub verification_failures: Vec<VerificationFailurePattern>,
    pub finding_distribution: Vec<CategoryCount>,
    pub severity_distribution: Vec<CategoryCount>,
    pub reflection_fix_effectiveness: Vec<FixEffectivenessRecord>,
    pub generation_quality: GenerationQualityMetrics,
    pub pipeline_agent_failures: Vec<PipelineAgentFailureRecord>,
    pub recurring_issues: Vec<RecurringIssue>,
    /// Aggregate agentic metric scores over the analysis period.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agentic_metric_summary: Vec<crate::database::agentic_metrics_ops::AgenticMetricAggregate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbortReason {
    pub reason: String,
    pub count: i64,
    pub percentage: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationFailurePattern {
    pub iteration: i64,
    pub total_checks: i64,
    pub failed_checks: i64,
    pub failure_rate: f64,
    pub critical_failure_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryCount {
    pub category: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixEffectivenessRecord {
    pub fix_type: String,
    pub total: i64,
    pub effective: i64,
    pub ineffective: i64,
    pub caused_regression: i64,
    pub effectiveness_rate: f64,
    pub source_agent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationQualityMetrics {
    pub total_feedback: i64,
    pub edits: i64,
    pub deletes: i64,
    pub avg_rating: Option<f64>,
    pub most_edited_fields: Vec<CategoryCount>,
    pub delete_reasons: Vec<CategoryCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineAgentFailureRecord {
    pub agent_type: String,
    pub total_runs: i64,
    pub failures: i64,
    pub failure_rate: f64,
    pub avg_duration_ms: f64,
    pub avg_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecurringIssue {
    pub signature_hash: String,
    pub title: String,
    pub category: String,
    pub severity: String,
    pub occurrence_count: i64,
    pub last_seen: String,
}

// ── Public API ───────────────────────────────────────────────────────────

pub fn get_failure_analysis(
    pg_db: &Arc<PgDb>,
    days: u32,
    category: WorkflowCategory,
) -> Result<FailureAnalysis, String> {
    let since = (chrono::Utc::now() - chrono::Duration::days(days as i64)).to_rfc3339();
    debug!(days, %since, "Running failure analysis (PG)");

    let mut analysis = tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.get_failure_analysis(&since, &category))
    })?;

    analysis.period_days = days;
    // agentic_metric_summary: SQLite method removed, use default
    analysis.agentic_metric_summary = Default::default();

    Ok(analysis)
}

// ── PG dual-write wrappers ──────────────────────────────────────────────

/// Get failure analysis with PG (now primary).
pub fn get_failure_analysis_with_pg(
    pg_db: &Arc<PgDb>,
    days: u32,
    category: WorkflowCategory,
) -> Result<FailureAnalysis, String> {
    get_failure_analysis(pg_db, days, category)
}

// ── Private Query Functions ──────────────────────────────────────────────

fn query_run_totals(
    since: &str,
    category: WorkflowCategory,
) -> (i64, i64) {
    // SQLite removed — return zero totals
    (0, 0)
}

fn query_abort_reasons(
    since: &str,
    category: WorkflowCategory,
) -> Vec<AbortReason> {
    Vec::new()
}

fn query_verification_failures(
    since: &str,
    category: WorkflowCategory,
) -> Vec<VerificationFailurePattern> {
    Vec::new()
}

fn query_finding_distribution(
    since: &str,
    category: WorkflowCategory,
) -> Vec<CategoryCount> {
    Vec::new()
}

fn query_severity_distribution(
    since: &str,
    category: WorkflowCategory,
) -> Vec<CategoryCount> {
    Vec::new()
}

fn query_fix_effectiveness(
    since: &str,
) -> Vec<FixEffectivenessRecord> {
    Vec::new()
}

fn query_generation_quality(since: &str) -> GenerationQualityMetrics {
    // SQLite removed — return empty metrics
    GenerationQualityMetrics {
        total_feedback: 0,
        edits: 0,
        deletes: 0,
        avg_rating: None,
        most_edited_fields: Vec::new(),
        delete_reasons: Vec::new(),
    }
}

fn query_pipeline_agent_failures(
    since: &str,
    category: WorkflowCategory,
) -> Vec<PipelineAgentFailureRecord> {
    Vec::new()
}

fn query_recurring_issues(
    since: &str,
    category: WorkflowCategory,
) -> Vec<RecurringIssue> {
    Vec::new()
}
