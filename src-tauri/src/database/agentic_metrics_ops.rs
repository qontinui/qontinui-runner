//! Agentic metric score operations: CRUD for metric scores and baselines.
//!
//! Contains all CheckpointDb methods related to agentic metric scores.

use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::CheckpointDb;

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

impl CheckpointDb {
    // ========================================================================
    // Agentic Metric Score Operations
    // ========================================================================

    // Note: save_agentic_metric_scores and update_composite_agentic_score were
    // removed — score writes happen inline in learning_recorder.rs using raw
    // conn.execute() for consistency with the synchronous recording path.

    /// Update the cached composite_agentic_score on learning_outcomes.
    #[allow(dead_code)]
    pub fn update_composite_agentic_score(
        &self,
        task_run_id: &str,
        composite_score: f64,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"
            UPDATE learning_outcomes
            SET composite_agentic_score = ?1
            WHERE task_id = ?2
            "#,
            params![composite_score, task_run_id],
        )
        .map_err(|e| format!("Failed to update composite agentic score: {}", e))?;
        Ok(())
    }

    /// Get all metric scores for a specific task run.
    pub fn get_agentic_scores_for_run(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<AgenticMetricScoreRow>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_run_id, metric_type, score, confidence,
                       rationale, is_llm_judged, model_used, created_at
                FROM agentic_metric_scores
                WHERE task_run_id = ?1
                ORDER BY metric_type ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let rows = stmt
            .query_map(params![task_run_id], |row| {
                Ok(AgenticMetricScoreRow {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    metric_type: row.get(2)?,
                    score: row.get(3)?,
                    confidence: row.get(4)?,
                    rationale: row.get(5)?,
                    is_llm_judged: row.get::<_, i32>(6)? != 0,
                    model_used: row.get(7)?,
                    created_at: row.get(8)?,
                })
            })
            .map_err(|e| format!("Failed to query agentic scores: {}", e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect agentic scores: {}", e))?;

        Ok(rows)
    }

    /// Get aggregate metric stats over a date range.
    ///
    /// Returns per-metric-type averages across all runs in the period.
    pub fn get_agentic_metric_aggregates(
        &self,
        days: i64,
    ) -> Result<Vec<AgenticMetricAggregate>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT metric_type,
                       AVG(score) as mean_score,
                       MIN(score) as min_score,
                       MAX(score) as max_score,
                       COUNT(*) as runs_scored
                FROM agentic_metric_scores
                WHERE created_at >= datetime('now', ?1)
                GROUP BY metric_type
                ORDER BY metric_type ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare aggregates query: {}", e))?;

        let days_param = format!("-{} days", days);
        let rows = stmt
            .query_map(params![days_param], |row| {
                Ok(AgenticMetricAggregate {
                    metric_type: row.get(0)?,
                    mean_score: row.get(1)?,
                    min_score: row.get(2)?,
                    max_score: row.get(3)?,
                    runs_scored: row.get(4)?,
                })
            })
            .map_err(|e| format!("Failed to query aggregates: {}", e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect aggregates: {}", e))?;

        Ok(rows)
    }

    /// Check whether a task run already has agentic scores recorded.
    #[allow(dead_code)]
    pub fn has_agentic_scores(&self, task_run_id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agentic_metric_scores WHERE task_run_id = ?1",
                params![task_run_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to check agentic scores: {}", e))?;
        Ok(count > 0)
    }

    /// Backfill deterministic agentic scores for historical learning_outcomes
    /// that don't have scores yet. Returns the number of outcomes backfilled.
    ///
    /// Safe to call on every startup — idempotent due to the NOT IN subquery.
    pub fn backfill_deterministic_scores(&self) -> Result<usize, String> {
        use crate::meta_optimizer::agentic_metrics::{self, DeterministicInput, LearnedBaselines};

        let conn = self.get_conn()?;

        // Find learning_outcomes without agentic scores
        let mut stmt = conn
            .prepare(
                r#"SELECT lo.task_id, lo.status, lo.iterations, lo.step_count,
                          lo.verification_step_count, lo.agentic_step_count,
                          lo.duration_secs, lo.error_type, lo.tools_used
                   FROM learning_outcomes lo
                   WHERE lo.task_id NOT IN (
                       SELECT DISTINCT task_run_id FROM agentic_metric_scores
                   )
                   ORDER BY lo.created_at DESC
                   LIMIT 500"#,
            )
            .map_err(|e| format!("Failed to prepare backfill query: {}", e))?;

        let rows: Vec<(
            String,
            String,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<f64>,
            Option<String>,
            Option<String>,
        )> = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            })
            .map_err(|e| format!("Failed to query unscored outcomes: {}", e))?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);

        if rows.is_empty() {
            return Ok(0);
        }

        let baselines = LearnedBaselines::default();
        let now = Utc::now().to_rfc3339();
        let mut backfilled = 0usize;

        for (
            task_id,
            status,
            iterations,
            step_count,
            ver_steps,
            ag_steps,
            duration,
            error_type,
            tools_json,
        ) in &rows
        {
            let tools_used: Vec<String> = tools_json
                .as_ref()
                .and_then(|j| serde_json::from_str(j).ok())
                .unwrap_or_default();

            let verification_passed = status == "success";
            let iterations_u32 = iterations.unwrap_or(0) as u32;

            // Load pipeline traces to extract schema compliance data (if available).
            let schema_compliance_inputs = {
                use crate::meta_optimizer::agentic_metrics::schema_compliance::compliance_inputs_from_traces;
                match crate::database::pipeline_traces::get_traces_for_task_run(task_id) {
                    Ok(traces) => compliance_inputs_from_traces(&traces),
                    Err(_) => vec![],
                }
            };

            let input = DeterministicInput {
                task_run_id: task_id.clone(),
                status: status.clone(),
                verification_passed,
                was_stopped: status == "partial",
                iterations: iterations_u32,
                step_count: *step_count,
                verification_step_count: *ver_steps,
                agentic_step_count: *ag_steps,
                max_iterations_reached: error_type.as_deref() == Some("max_iterations_reached"),
                duration_secs: duration.unwrap_or(0.0),
                error_type: error_type.clone(),
                tools_used,
                agent_traces: vec![],
                schema_compliance_inputs,
            };

            let scores = agentic_metrics::compute_deterministic(&input, &baselines);
            let composite = agentic_metrics::composite_score(&scores);

            for score in &scores {
                let id = format!("ams-{}", uuid::Uuid::new_v4());
                conn.execute(
                    r#"INSERT OR IGNORE INTO agentic_metric_scores
                        (id, task_run_id, metric_type, score, confidence,
                         rationale, is_llm_judged, model_used, created_at)
                       VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
                    params![
                        id,
                        task_id,
                        score.metric.as_str(),
                        score.score,
                        score.confidence,
                        score.rationale,
                        score.is_llm_judged as i32,
                        score.model_used,
                        now,
                    ],
                )
                .map_err(|e| format!("Failed to backfill score: {}", e))?;
            }

            conn.execute(
                "UPDATE learning_outcomes SET composite_agentic_score = ?1 WHERE task_id = ?2",
                params![composite, task_id],
            )
            .map_err(|e| format!("Failed to update composite score: {}", e))?;

            backfilled += 1;
        }

        tracing::info!(
            "Backfilled deterministic agentic scores for {} historical runs",
            backfilled
        );
        Ok(backfilled)
    }

    /// Get composite score trend over time, grouped by date.
    pub fn get_composite_score_trend(
        &self,
        days: i64,
    ) -> Result<Vec<CompositeScoreTrendPoint>, String> {
        let conn = self.get_conn()?;
        let days_param = format!("-{} days", days);
        let mut stmt = conn
            .prepare(
                r#"SELECT DATE(lo.created_at) as day,
                          AVG(lo.composite_agentic_score) as avg_score,
                          COUNT(*) as run_count,
                          SUM(CASE WHEN lo.status = 'success' THEN 1 ELSE 0 END) as success_count
                   FROM learning_outcomes lo
                   WHERE lo.created_at >= datetime('now', ?1)
                     AND lo.composite_agentic_score IS NOT NULL
                   GROUP BY DATE(lo.created_at)
                   ORDER BY day ASC"#,
            )
            .map_err(|e| format!("Failed to prepare trend query: {}", e))?;

        let rows = stmt
            .query_map(params![days_param], |row| {
                Ok(CompositeScoreTrendPoint {
                    date: row.get(0)?,
                    avg_composite_score: row.get(1)?,
                    run_count: row.get(2)?,
                    success_count: row.get(3)?,
                })
            })
            .map_err(|e| format!("Failed to query trend: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(rows)
    }
}

/// A single point in the composite score trend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositeScoreTrendPoint {
    pub date: String,
    pub avg_composite_score: f64,
    pub run_count: i64,
    pub success_count: i64,
}
