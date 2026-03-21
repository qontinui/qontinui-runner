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

    /// Save a batch of metric scores for a task run.
    ///
    /// Uses INSERT OR REPLACE to handle re-scoring (e.g., when LLM-judged
    /// scores arrive after deterministic scores).
    pub fn save_agentic_metric_scores(
        &self,
        scores: &[crate::meta_optimizer::agentic_metrics::MetricResult],
        task_run_id: &str,
    ) -> Result<Vec<String>, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();
        let mut ids = Vec::with_capacity(scores.len());

        for score in scores {
            let id = format!("ams-{}", uuid::Uuid::new_v4());
            conn.execute(
                r#"
                INSERT OR REPLACE INTO agentic_metric_scores (
                    id, task_run_id, metric_type, score, confidence,
                    rationale, is_llm_judged, model_used, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                "#,
                params![
                    id,
                    task_run_id,
                    score.metric.as_str(),
                    score.score,
                    score.confidence,
                    score.rationale,
                    score.is_llm_judged as i32,
                    score.model_used,
                    now,
                ],
            )
            .map_err(|e| format!("Failed to save agentic metric score: {}", e))?;
            ids.push(id);
        }

        Ok(ids)
    }

    /// Update the cached composite_agentic_score on learning_outcomes.
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
}
