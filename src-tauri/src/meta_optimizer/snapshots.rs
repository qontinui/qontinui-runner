//! CRUD operations for meta_optimizer_snapshots table.
//!
//! Captures periodic performance snapshots from learning_outcomes + phase_token_usage
//! to track progress over time and measure the impact of applied recommendations.

use rusqlite::params;
use serde::{Deserialize, Serialize};
use tracing::info;

use super::types::WorkflowCategory;
use crate::database::CheckpointDb;

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
    db: &CheckpointDb,
    recommendation_id: &str,
) -> Result<RecommendationOutcome, String> {
    let rec_id = recommendation_id.to_string();

    // Fetch the recommendation
    let rec = db.with_conn({
        let rec_id = rec_id.clone();
        move |conn| {
            conn.query_row(
                "SELECT target_agent, applied_at FROM meta_optimizer_recommendations WHERE id = ?1",
                rusqlite::params![rec_id],
                |row| {
                    let target_agent: Option<String> = row.get(0)?;
                    let applied_at: Option<String> = row.get(1)?;
                    Ok((target_agent, applied_at))
                },
            )
            .map_err(|e| format!("Recommendation not found: {}", e))
        }
    })?;

    let (target_agent, applied_at) = rec;

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

        let before = crate::database::pipeline_traces::get_agent_aggregates_for_period(
            db,
            &agent,
            &before_start,
            &applied_at,
        )?;
        let after = crate::database::pipeline_traces::get_agent_aggregates_for_period(
            db,
            &agent,
            &applied_at,
            &after_end,
        )?;

        compute_verdict(before, after)
    } else {
        // No target agent: compare post_apply snapshot against baseline
        let baseline = get_latest_baseline(db, WorkflowCategory::Main)?;
        let post_snap: Option<MetaOptimizerSnapshot> = db.with_conn({
            let rec_id = rec_id.clone();
            move |conn| {
                let result = conn.query_row(
                    r#"SELECT id, snapshot_type, period_start, period_end, metrics_json,
                              breakdown_json, recommendation_id, runs_included, created_at
                       FROM meta_optimizer_snapshots
                       WHERE recommendation_id = ?1 AND snapshot_type = 'post_apply'
                       ORDER BY created_at DESC LIMIT 1"#,
                    rusqlite::params![rec_id],
                    |row| {
                        Ok(MetaOptimizerSnapshot {
                            id: row.get(0)?,
                            snapshot_type: row.get(1)?,
                            period_start: row.get(2)?,
                            period_end: row.get(3)?,
                            metrics_json: row.get(4)?,
                            breakdown_json: row.get(5)?,
                            recommendation_id: row.get(6)?,
                            runs_included: row.get(7)?,
                            created_at: row.get(8)?,
                        })
                    },
                );
                match result {
                    Ok(snap) => Ok(Some(snap)),
                    Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                    Err(e) => Err(format!("Failed to query post_apply snapshot: {}", e)),
                }
            }
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
    update_outcome(db, &rec_id, &outcome_json)?;

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

/// Update the outcome_after_apply column on a recommendation.
pub fn update_outcome(
    db: &CheckpointDb,
    recommendation_id: &str,
    outcome_json: &str,
) -> Result<(), String> {
    let id = recommendation_id.to_string();
    let json = outcome_json.to_string();

    db.with_conn(move |conn| {
        conn.execute(
            "UPDATE meta_optimizer_recommendations SET outcome_after_apply = ?1 WHERE id = ?2",
            rusqlite::params![json, id],
        )
        .map_err(|e| format!("Failed to update outcome: {}", e))?;
        Ok(())
    })
}

// ── Core snapshot capture ──────────────────────────────────────────────

/// Capture a performance snapshot from learning_outcomes + phase_token_usage.
///
/// - `snapshot_type`: "baseline", "periodic", or "post_apply"
/// - `recommendation_id`: set for post_apply snapshots
/// - `lookback_days`: how many days of data to include
pub fn capture_snapshot(
    db: &CheckpointDb,
    snapshot_type: &str,
    recommendation_id: Option<&str>,
    lookback_days: i64,
    category: WorkflowCategory,
) -> Result<MetaOptimizerSnapshot, String> {
    let id = format!("mos-{}", uuid::Uuid::new_v4());
    let snap_type = snapshot_type.to_string();
    let rec_id = recommendation_id.map(|s| s.to_string());
    let id_clone = id.clone();

    db.with_conn(move |conn| {
        let now = chrono::Utc::now();
        let period_end = now.to_rfc3339();
        let period_start = (now - chrono::Duration::days(lookback_days)).to_rfc3339();

        let helper_filter = category.sql_filter("tr");

        // ── Aggregate metrics from learning_outcomes ───────────────────
        let (total_runs, successful_runs, failed_runs, partial_runs, avg_duration, avg_iterations): (
            i64, i64, i64, i64, f64, f64,
        ) = conn
            .query_row(
                &format!(
                    r#"SELECT
                           COUNT(*),
                           SUM(CASE WHEN lo.status = 'success' THEN 1 ELSE 0 END),
                           SUM(CASE WHEN lo.status = 'failure' THEN 1 ELSE 0 END),
                           SUM(CASE WHEN lo.status = 'partial' THEN 1 ELSE 0 END),
                           COALESCE(AVG(lo.duration_secs), 0.0),
                           COALESCE(AVG(lo.iterations), 0.0)
                       FROM learning_outcomes lo
                       JOIN task_runs tr ON lo.task_id = tr.id
                       WHERE lo.created_at > ?1
                         AND (lo.iterations IS NULL OR lo.iterations > 0){}"#,
                    helper_filter
                ),
                params![period_start],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .map_err(|e| format!("Failed to query learning_outcomes metrics: {}", e))?;

        let success_rate = if total_runs > 0 {
            successful_runs as f64 / total_runs as f64
        } else {
            0.0
        };

        // ── Average cost per run via LEFT JOIN on phase_token_usage ────
        let avg_cost_cents: f64 = conn
            .query_row(
                &format!(
                    r#"SELECT COALESCE(AVG(run_cost), 0.0) FROM (
                           SELECT lo.task_id, COALESCE(SUM(ptu.cost_cents), 0) as run_cost
                           FROM learning_outcomes lo
                           JOIN task_runs tr ON lo.task_id = tr.id
                           LEFT JOIN phase_token_usage ptu ON ptu.task_run_id = lo.task_id
                           WHERE lo.created_at > ?1{}
                           GROUP BY lo.task_id
                       )"#,
                    helper_filter
                ),
                params![period_start],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to query cost metrics: {}", e))?;

        // ── Spec compliance average (if any spec compliance data exists) ──
        let avg_spec_compliance: Option<f64> = conn
            .query_row(
                "SELECT AVG(overall_score) FROM spec_compliance_results WHERE created_at > ?1",
                params![period_start],
                |row| row.get::<_, Option<f64>>(0),
            )
            .unwrap_or(None);

        // ── Composite agentic score average ──
        let avg_composite_agentic_score: Option<f64> = conn
            .query_row(
                "SELECT AVG(composite_agentic_score) FROM learning_outcomes WHERE created_at > ?1 AND composite_agentic_score IS NOT NULL",
                params![period_start],
                |row| row.get::<_, Option<f64>>(0),
            )
            .unwrap_or(None);

        let metrics = SnapshotMetrics {
            success_rate,
            avg_duration_secs: avg_duration,
            avg_iterations,
            avg_cost_cents,
            total_runs,
            successful_runs,
            failed_runs,
            partial_runs,
            avg_spec_compliance,
            avg_composite_agentic_score,
        };

        let metrics_json = serde_json::to_string(&metrics)
            .map_err(|e| format!("Failed to serialize metrics: {}", e))?;

        // ── Per-architecture breakdown (if data exists) ───────────────
        let breakdown_json: Option<String> = {
            let mut stmt = conn
                .prepare(
                    &format!(
                        r#"SELECT lo.workflow_architecture, COUNT(*) as cnt,
                                  SUM(CASE WHEN lo.status = 'success' THEN 1 ELSE 0 END) * 100.0 / COUNT(*) as sr,
                                  AVG(lo.duration_secs) as avg_dur,
                                  AVG(lo.iterations) as avg_iter
                           FROM learning_outcomes lo
                           JOIN task_runs tr ON lo.task_id = tr.id
                           WHERE lo.created_at > ?1 AND lo.workflow_architecture IS NOT NULL
                             AND (lo.iterations IS NULL OR lo.iterations > 0){}
                           GROUP BY lo.workflow_architecture"#,
                        helper_filter
                    ),
                )
                .map_err(|e| format!("Failed to prepare breakdown query: {}", e))?;

            let rows: Vec<serde_json::Value> = stmt
                .query_map(params![period_start], |row| {
                    let arch: String = row.get(0)?;
                    let count: i64 = row.get(1)?;
                    let sr: f64 = row.get(2)?;
                    let avg_dur: f64 = row.get(3)?;
                    let avg_iter: f64 = row.get(4)?;
                    Ok(serde_json::json!({
                        "architecture": arch,
                        "count": count,
                        "success_rate": sr,
                        "avg_duration_secs": avg_dur,
                        "avg_iterations": avg_iter,
                    }))
                })
                .map_err(|e| format!("Failed to query breakdown: {}", e))?
                .filter_map(|r| r.ok())
                .collect();

            if rows.is_empty() {
                None
            } else {
                Some(
                    serde_json::to_string(&rows)
                        .map_err(|e| format!("Failed to serialize breakdown: {}", e))?,
                )
            }
        };

        // ── Insert snapshot row ───────────────────────────────────────
        conn.execute(
            r#"INSERT INTO meta_optimizer_snapshots
               (id, snapshot_type, period_start, period_end, metrics_json, breakdown_json,
                recommendation_id, runs_included, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
            params![
                id_clone,
                snap_type,
                period_start,
                period_end,
                metrics_json,
                breakdown_json,
                rec_id,
                total_runs,
                period_end,
            ],
        )
        .map_err(|e| format!("Failed to insert snapshot: {}", e))?;

        info!(
            "Captured {} snapshot {} ({} runs, success_rate={:.1}%)",
            snap_type,
            id_clone,
            total_runs,
            success_rate * 100.0
        );

        Ok(MetaOptimizerSnapshot {
            id: id_clone,
            snapshot_type: snap_type,
            period_start,
            period_end: period_end.clone(),
            metrics_json,
            breakdown_json,
            recommendation_id: rec_id,
            runs_included: total_runs,
            created_at: period_end,
        })
    })
}

// ── Convenience wrappers ───────────────────────────────────────────────

/// Capture a baseline snapshot (last 30 days).
/// Snapshot type is suffixed by category (e.g., "baseline", "baseline_reflection").
pub fn capture_baseline(
    db: &CheckpointDb,
    category: WorkflowCategory,
) -> Result<MetaOptimizerSnapshot, String> {
    let snap_type = format!("baseline{}", category.snapshot_suffix());
    capture_snapshot(db, &snap_type, None, 30, category)
}

/// Capture a periodic snapshot (last 7 days).
/// Snapshot type is suffixed by category (e.g., "periodic", "periodic_reflection").
pub fn capture_periodic(
    db: &CheckpointDb,
    category: WorkflowCategory,
) -> Result<MetaOptimizerSnapshot, String> {
    let snap_type = format!("periodic{}", category.snapshot_suffix());
    capture_snapshot(db, &snap_type, None, 7, category)
}

/// Capture a post-apply snapshot to measure recommendation impact (last 7 days).
pub fn capture_post_apply(
    db: &CheckpointDb,
    recommendation_id: &str,
    category: WorkflowCategory,
) -> Result<MetaOptimizerSnapshot, String> {
    capture_snapshot(db, "post_apply", Some(recommendation_id), 7, category)
}

// ── Query helpers ──────────────────────────────────────────────────────

/// Get the latest baseline snapshot for the given category.
pub fn get_latest_baseline(
    db: &CheckpointDb,
    category: WorkflowCategory,
) -> Result<Option<MetaOptimizerSnapshot>, String> {
    let snap_type_owned = format!("baseline{}", category.snapshot_suffix());
    db.with_conn(move |conn| {
        let result = conn.query_row(
            r#"SELECT id, snapshot_type, period_start, period_end, metrics_json,
                      breakdown_json, recommendation_id, runs_included, created_at
               FROM meta_optimizer_snapshots
               WHERE snapshot_type = ?1
               ORDER BY created_at DESC
               LIMIT 1"#,
            params![snap_type_owned],
            |row| {
                Ok(MetaOptimizerSnapshot {
                    id: row.get(0)?,
                    snapshot_type: row.get(1)?,
                    period_start: row.get(2)?,
                    period_end: row.get(3)?,
                    metrics_json: row.get(4)?,
                    breakdown_json: row.get(5)?,
                    recommendation_id: row.get(6)?,
                    runs_included: row.get(7)?,
                    created_at: row.get(8)?,
                })
            },
        );

        match result {
            Ok(snap) => Ok(Some(snap)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to query baseline snapshot: {}", e)),
        }
    })
}

/// List snapshots with optional type filter.
pub fn list_snapshots(
    db: &CheckpointDb,
    snapshot_type: Option<&str>,
) -> Result<Vec<MetaOptimizerSnapshot>, String> {
    let snap_type = snapshot_type.map(|s| s.to_string());

    db.with_conn(move |conn| {
        let (sql, has_filter) = if snap_type.is_some() {
            (
                r#"SELECT id, snapshot_type, period_start, period_end, metrics_json,
                          breakdown_json, recommendation_id, runs_included, created_at
                   FROM meta_optimizer_snapshots
                   WHERE snapshot_type = ?1
                   ORDER BY created_at DESC
                   LIMIT 100"#
                    .to_string(),
                true,
            )
        } else {
            (
                r#"SELECT id, snapshot_type, period_start, period_end, metrics_json,
                          breakdown_json, recommendation_id, runs_included, created_at
                   FROM meta_optimizer_snapshots
                   ORDER BY created_at DESC
                   LIMIT 100"#
                    .to_string(),
                false,
            )
        };

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare snapshot query: {}", e))?;

        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<MetaOptimizerSnapshot> {
            Ok(MetaOptimizerSnapshot {
                id: row.get(0)?,
                snapshot_type: row.get(1)?,
                period_start: row.get(2)?,
                period_end: row.get(3)?,
                metrics_json: row.get(4)?,
                breakdown_json: row.get(5)?,
                recommendation_id: row.get(6)?,
                runs_included: row.get(7)?,
                created_at: row.get(8)?,
            })
        };

        let rows: Vec<MetaOptimizerSnapshot> = if has_filter {
            stmt.query_map(params![snap_type.unwrap()], map_row)
                .map_err(|e| format!("Failed to query snapshots: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            stmt.query_map([], map_row)
                .map_err(|e| format!("Failed to query snapshots: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        };

        Ok(rows)
    })
}

/// Get a full progress summary: baseline vs current, deltas, and all snapshots.
pub fn get_progress_summary(
    db: &CheckpointDb,
    category: WorkflowCategory,
) -> Result<ProgressSummary, String> {
    let baseline_snap = get_latest_baseline(db, category)?;

    // Get latest periodic snapshot for this category
    let periodic_type = format!("periodic{}", category.snapshot_suffix());
    let current_snap: Option<MetaOptimizerSnapshot> = db.with_conn(move |conn| {
        let result = conn.query_row(
            r#"SELECT id, snapshot_type, period_start, period_end, metrics_json,
                      breakdown_json, recommendation_id, runs_included, created_at
               FROM meta_optimizer_snapshots
               WHERE snapshot_type = ?1
               ORDER BY created_at DESC
               LIMIT 1"#,
            params![periodic_type],
            |row| {
                Ok(MetaOptimizerSnapshot {
                    id: row.get(0)?,
                    snapshot_type: row.get(1)?,
                    period_start: row.get(2)?,
                    period_end: row.get(3)?,
                    metrics_json: row.get(4)?,
                    breakdown_json: row.get(5)?,
                    recommendation_id: row.get(6)?,
                    runs_included: row.get(7)?,
                    created_at: row.get(8)?,
                })
            },
        );

        match result {
            Ok(snap) => Ok(Some(snap)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to query periodic snapshot: {}", e)),
        }
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

    let snapshots = list_snapshots(db, None)?;

    let applied_recommendations_count: i64 = db.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM meta_optimizer_recommendations WHERE status = 'applied'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to count applied recommendations: {}", e))
    })?;

    Ok(ProgressSummary {
        baseline: baseline_metrics,
        current: current_metrics,
        delta,
        snapshots,
        applied_recommendations_count,
    })
}
