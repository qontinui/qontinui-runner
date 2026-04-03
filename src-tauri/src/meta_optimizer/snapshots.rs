//! CRUD operations for meta_optimizer_snapshots table.
//!
//! Captures periodic performance snapshots from learning_outcomes + phase_token_usage
//! to track progress over time and measure the impact of applied recommendations.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

use super::types::WorkflowCategory;
use crate::database::pg::PgDb;

// ── Types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaOptimizerSnapshot {
    pub id: String,
    pub snapshot_type: String,
    pub period_start: String,
    pub period_end: String,
    pub metrics_json: String,
    pub breakdown_json: Option<String>,
    pub recommendation_id: Option<String>,
    pub runs_included: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMetrics {
    pub success_rate: f64,
    pub avg_duration_secs: f64,
    pub avg_iterations: f64,
    pub avg_cost_cents: f64,
    pub total_runs: i64,
    pub successful_runs: i64,
    pub failed_runs: i64,
    pub partial_runs: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_spec_compliance: Option<f64>,
    /// Average composite agentic metric score across runs in this snapshot period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_composite_agentic_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressSummary {
    pub baseline: Option<SnapshotMetrics>,
    pub current: Option<SnapshotMetrics>,
    pub delta: Option<MetricsDelta>,
    pub snapshots: Vec<MetaOptimizerSnapshot>,
    pub applied_recommendations_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsDelta {
    pub success_rate_delta: f64,
    pub duration_delta: f64,
    pub iterations_delta: f64,
    pub cost_delta: f64,
}

// ── Recommendation outcome evaluation ──────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecommendationOutcome {
    pub verdict: String,
    pub metrics_before: Option<crate::database::pipeline_traces::AgentTraceAggregate>,
    pub metrics_after: Option<crate::database::pipeline_traces::AgentTraceAggregate>,
    pub success_rate_delta: Option<f64>,
    pub duration_delta_ms: Option<f64>,
    pub cost_delta_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_compliance_delta: Option<f64>,
    /// One-sided p-value for success rate improvement. None if insufficient data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_value: Option<f64>,
    /// 95% confidence interval for success rate delta (in percentage points).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence_interval: Option<(f64, f64)>,
    /// Cohen's h effect size for success rate change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_size: Option<f64>,
}

/// Evaluate whether an applied recommendation improved or regressed performance.
///
/// Compares 7-day windows before and after `applied_at` for the target agent.
/// Uses statistical significance testing (z-test, CI, Cohen's h) via `stats::compute_verdict`
/// with `VerdictThresholds::recommendation()`. Falls back to simple delta thresholds when
/// sample sizes are too small for statistical analysis.
pub fn evaluate_recommendation_outcome(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
) -> Result<RecommendationOutcome, String> {
    let rec_id = recommendation_id.to_string();

    // Fetch the recommendation
    let (target_agent, applied_at) = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(async { pg_db.get_recommendation_outcome_info(&rec_id).await })
    })?;

    let applied_at = match applied_at {
        Some(a) => a,
        None => {
            return Ok(RecommendationOutcome {
                verdict: "insufficient_data".to_string(),
                metrics_before: None,
                metrics_after: None,
                success_rate_delta: None,
                duration_delta_ms: None,
                cost_delta_usd: None,
                spec_compliance_delta: None,
                p_value: None,
                confidence_interval: None,
                effect_size: None,
            });
        }
    };

    let applied = chrono::DateTime::parse_from_rfc3339(&applied_at)
        .map_err(|e| format!("Invalid applied_at date: {}", e))?;

    let outcome = if let Some(agent) = target_agent {
        let before_start = (applied - chrono::Duration::days(7)).to_rfc3339();
        let after_end = (applied + chrono::Duration::days(7)).to_rfc3339();

        let before = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                pg_db
                    .get_agent_aggregates_for_period(&agent, &before_start, &applied_at)
                    .await
            })
        })?;
        let after = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                pg_db
                    .get_agent_aggregates_for_period(&agent, &applied_at, &after_end)
                    .await
            })
        })?;

        compute_verdict(before, after)
    } else {
        // No target agent: compare post_apply snapshot against baseline
        let baseline = get_latest_baseline(pg_db, WorkflowCategory::Main)?;
        let post_snap = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { pg_db.get_post_apply_snapshot(&rec_id).await })
        })?;

        let baseline_metrics = baseline
            .as_ref()
            .and_then(|s| serde_json::from_str::<SnapshotMetrics>(&s.metrics_json).ok());
        let post_metrics = post_snap
            .as_ref()
            .and_then(|s| serde_json::from_str::<SnapshotMetrics>(&s.metrics_json).ok());

        match (baseline_metrics, post_metrics) {
            (Some(b), Some(p)) => {
                let sr_delta = (p.success_rate - b.success_rate) * 100.0; // convert to pp

                // Statistical analysis from snapshot-level aggregate counts
                let analysis = crate::stats::proportion_analysis(
                    (p.successful_runs as u64, p.total_runs as u64),
                    (b.successful_runs as u64, b.total_runs as u64),
                    2,
                );
                let p_value = analysis.p_value;
                let confidence_interval = analysis.confidence_interval;
                let effect_size = analysis.effect_size;

                let verdict_enum = crate::stats::compute_verdict(
                    sr_delta,
                    &analysis,
                    p.total_runs as u64,
                    &crate::stats::VerdictThresholds::recommendation(),
                );
                let verdict = verdict_enum.as_recommendation_str();

                let sc_delta = match (b.avg_spec_compliance, p.avg_spec_compliance) {
                    (Some(b_sc), Some(p_sc)) => Some(p_sc - b_sc),
                    _ => None,
                };
                RecommendationOutcome {
                    verdict: verdict.to_string(),
                    metrics_before: None,
                    metrics_after: None,
                    success_rate_delta: Some(sr_delta),
                    duration_delta_ms: Some((p.avg_duration_secs - b.avg_duration_secs) * 1000.0),
                    cost_delta_usd: Some((p.avg_cost_cents - b.avg_cost_cents) / 100.0),
                    spec_compliance_delta: sc_delta,
                    p_value,
                    confidence_interval,
                    effect_size,
                }
            }
            _ => RecommendationOutcome {
                verdict: "insufficient_data".to_string(),
                metrics_before: None,
                metrics_after: None,
                success_rate_delta: None,
                duration_delta_ms: None,
                cost_delta_usd: None,
                spec_compliance_delta: None,
                p_value: None,
                confidence_interval: None,
                effect_size: None,
            },
        }
    };

    // Persist the outcome to the recommendation row
    let outcome_json = serde_json::to_string(&outcome)
        .map_err(|e| format!("Failed to serialize outcome: {}", e))?;
    update_outcome(pg_db, &rec_id, &outcome_json)?;

    Ok(outcome)
}

fn compute_verdict(
    before: Option<crate::database::pipeline_traces::AgentTraceAggregate>,
    after: Option<crate::database::pipeline_traces::AgentTraceAggregate>,
) -> RecommendationOutcome {
    match (&before, &after) {
        (Some(b), Some(a)) => {
            let b_sr = if b.run_count > 0 {
                b.success_count as f64 / b.run_count as f64 * 100.0
            } else {
                0.0
            };
            let a_sr = if a.run_count > 0 {
                a.success_count as f64 / a.run_count as f64 * 100.0
            } else {
                0.0
            };
            let sr_delta = a_sr - b_sr;
            let dur_delta = a.avg_duration_ms - b.avg_duration_ms;
            let cost_delta = a.avg_cost_usd - b.avg_cost_usd;

            // Statistical analysis when we have enough data
            let analysis = crate::stats::proportion_analysis(
                (a.success_count as u64, a.run_count as u64),
                (b.success_count as u64, b.run_count as u64),
                2,
            );
            let p_value = analysis.p_value;
            let confidence_interval = analysis.confidence_interval;
            let effect_size = analysis.effect_size;

            let verdict_enum = crate::stats::compute_verdict(
                sr_delta,
                &analysis,
                a.run_count as u64,
                &crate::stats::VerdictThresholds::recommendation(),
            );
            let verdict = verdict_enum.as_recommendation_str();

            RecommendationOutcome {
                verdict: verdict.to_string(),
                metrics_before: before,
                metrics_after: after,
                success_rate_delta: Some(sr_delta),
                duration_delta_ms: Some(dur_delta),
                cost_delta_usd: Some(cost_delta),
                spec_compliance_delta: None,
                p_value,
                confidence_interval,
                effect_size,
            }
        }
        _ => RecommendationOutcome {
            verdict: "insufficient_data".to_string(),
            metrics_before: before,
            metrics_after: after,
            success_rate_delta: None,
            duration_delta_ms: None,
            cost_delta_usd: None,
            spec_compliance_delta: None,
            p_value: None,
            confidence_interval: None,
            effect_size: None,
        },
    }
}

/// Capture a baseline snapshot (backward compat, delegates to primary).
#[allow(dead_code)]
pub fn capture_baseline_with_pg(
    pg_db: &Arc<PgDb>,
    category: WorkflowCategory,
) -> Result<MetaOptimizerSnapshot, String> {
    capture_baseline(pg_db, category)
}

/// Capture a periodic snapshot (backward compat, delegates to primary).
#[allow(dead_code)]
pub fn capture_periodic_with_pg(
    pg_db: &Arc<PgDb>,
    category: WorkflowCategory,
) -> Result<MetaOptimizerSnapshot, String> {
    capture_periodic(pg_db, category)
}

/// Capture a post-apply snapshot (backward compat, delegates to primary).
#[allow(dead_code)]
pub fn capture_post_apply_with_pg(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
    category: WorkflowCategory,
) -> Result<MetaOptimizerSnapshot, String> {
    capture_post_apply(pg_db, recommendation_id, category)
}

/// Update the outcome_after_apply column on a recommendation.
pub fn update_outcome(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
    outcome_json: &str,
) -> Result<(), String> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            pg_db
                .update_recommendation_outcome(recommendation_id, outcome_json)
                .await
        })
    })
}

/// Update outcome with PG (same as update_outcome now, kept for backward compat).
#[allow(dead_code)]
pub fn update_outcome_with_pg(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
    outcome_json: &str,
) -> Result<(), String> {
    update_outcome(pg_db, recommendation_id, outcome_json)
}

// ── Core snapshot capture ──────────────────────────────────────────────

/// Capture a performance snapshot from learning_outcomes + phase_token_usage.
///
/// - `snapshot_type`: "baseline", "periodic", or "post_apply"
/// - `recommendation_id`: set for post_apply snapshots
/// - `lookback_days`: how many days of data to include
pub fn capture_snapshot(
    pg_db: &Arc<PgDb>,
    snapshot_type: &str,
    recommendation_id: Option<&str>,
    lookback_days: i64,
    category: WorkflowCategory,
) -> Result<MetaOptimizerSnapshot, String> {
    let category_filter = category.sql_filter("tr");
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            pg_db
                .pg_capture_snapshot(
                    snapshot_type,
                    recommendation_id,
                    lookback_days,
                    &category_filter,
                )
                .await
        })
    })
}

/// Capture a snapshot (same as capture_snapshot now, kept for backward compat).
#[allow(dead_code)]
pub fn capture_snapshot_with_pg(
    pg_db: &Arc<PgDb>,
    snapshot_type: &str,
    recommendation_id: Option<&str>,
    lookback_days: i64,
    category: WorkflowCategory,
) -> Result<MetaOptimizerSnapshot, String> {
    capture_snapshot(
        pg_db,
        snapshot_type,
        recommendation_id,
        lookback_days,
        category,
    )
}

// ── Convenience wrappers ───────────────────────────────────────────────

/// Capture a baseline snapshot (last 30 days).
/// Snapshot type is suffixed by category (e.g., "baseline", "baseline_reflection").
pub fn capture_baseline(
    pg_db: &Arc<PgDb>,
    category: WorkflowCategory,
) -> Result<MetaOptimizerSnapshot, String> {
    let snap_type = format!("baseline{}", category.snapshot_suffix());
    capture_snapshot(pg_db, &snap_type, None, 30, category)
}

/// Capture a periodic snapshot (last 7 days).
/// Snapshot type is suffixed by category (e.g., "periodic", "periodic_reflection").
pub fn capture_periodic(
    pg_db: &Arc<PgDb>,
    category: WorkflowCategory,
) -> Result<MetaOptimizerSnapshot, String> {
    let snap_type = format!("periodic{}", category.snapshot_suffix());
    capture_snapshot(pg_db, &snap_type, None, 7, category)
}

/// Capture a post-apply snapshot to measure recommendation impact (last 7 days).
pub fn capture_post_apply(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
    category: WorkflowCategory,
) -> Result<MetaOptimizerSnapshot, String> {
    capture_snapshot(pg_db, "post_apply", Some(recommendation_id), 7, category)
}

// ── Query helpers ──────────────────────────────────────────────────────

/// Get the latest baseline snapshot for the given category.
pub fn get_latest_baseline(
    pg_db: &Arc<PgDb>,
    category: WorkflowCategory,
) -> Result<Option<MetaOptimizerSnapshot>, String> {
    let snap_type = format!("baseline{}", category.snapshot_suffix());
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(async { pg_db.get_latest_baseline_snapshot(&snap_type).await })
    })
}

/// List snapshots with optional type filter.
pub fn list_snapshots(
    pg_db: &Arc<PgDb>,
    snapshot_type: Option<&str>,
) -> Result<Vec<MetaOptimizerSnapshot>, String> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(async { pg_db.list_snapshots(snapshot_type).await })
    })
}

/// Get a full progress summary: baseline vs current, deltas, and all snapshots.
pub fn get_progress_summary(
    pg_db: &Arc<PgDb>,
    category: WorkflowCategory,
) -> Result<ProgressSummary, String> {
    let baseline_snap = get_latest_baseline(pg_db, category)?;

    // Get latest periodic snapshot for this category
    let periodic_type = format!("periodic{}", category.snapshot_suffix());
    let current_snap: Option<MetaOptimizerSnapshot> = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(async { pg_db.get_latest_baseline_snapshot(&periodic_type).await })
    })?;

    let baseline_metrics = baseline_snap
        .as_ref()
        .and_then(|s| serde_json::from_str::<SnapshotMetrics>(&s.metrics_json).ok());

    let current_metrics = current_snap
        .as_ref()
        .and_then(|s| serde_json::from_str::<SnapshotMetrics>(&s.metrics_json).ok());

    let delta = match (&baseline_metrics, &current_metrics) {
        (Some(b), Some(c)) => Some(MetricsDelta {
            success_rate_delta: c.success_rate - b.success_rate,
            duration_delta: c.avg_duration_secs - b.avg_duration_secs,
            iterations_delta: c.avg_iterations - b.avg_iterations,
            cost_delta: c.avg_cost_cents - b.avg_cost_cents,
        }),
        _ => None,
    };

    let snapshots = list_snapshots(pg_db, None)?;

    let applied_recommendations_count: i64 = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(async { pg_db.count_applied_recommendations().await })
    })?;

    Ok(ProgressSummary {
        baseline: baseline_metrics,
        current: current_metrics,
        delta,
        snapshots,
        applied_recommendations_count,
    })
}

// ── Backward-compat wrappers (same as primary now) ────────────────────

/// Get the latest baseline snapshot (backward compat, delegates to primary).
#[allow(dead_code)]
pub fn get_latest_baseline_with_pg(
    pg_db: &Arc<PgDb>,
    category: WorkflowCategory,
) -> Result<Option<MetaOptimizerSnapshot>, String> {
    get_latest_baseline(pg_db, category)
}

/// List snapshots (backward compat, delegates to primary).
#[allow(dead_code)]
pub fn list_snapshots_with_pg(
    pg_db: &Arc<PgDb>,
    snapshot_type: Option<&str>,
) -> Result<Vec<MetaOptimizerSnapshot>, String> {
    list_snapshots(pg_db, snapshot_type)
}

/// Get a full progress summary (backward compat, delegates to primary).
#[allow(dead_code)]
pub fn get_progress_summary_with_pg(
    pg_db: &Arc<PgDb>,
    category: WorkflowCategory,
) -> Result<ProgressSummary, String> {
    get_progress_summary(pg_db, category)
}

/// Evaluate recommendation outcome (backward compat, delegates to primary).
#[allow(dead_code)]
pub fn evaluate_recommendation_outcome_with_pg(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
) -> Result<RecommendationOutcome, String> {
    evaluate_recommendation_outcome(pg_db, recommendation_id)
}
