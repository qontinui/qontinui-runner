//! PostgreSQL wrapper for agentic metric score operations.
//!
//! TODO: Switch to Clorinde-generated queries once `clorinde` is run against the
//! agentic_metric_scores table. For now uses raw tokio-postgres queries.

use super::PgDb;
use crate::database::agentic_metrics_ops::{AgenticMetricAggregate, AgenticMetricScoreRow};

impl PgDb {
    /// Get all metric scores for a specific task run.
    pub async fn get_agentic_scores_for_run(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<AgenticMetricScoreRow>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, task_run_id, metric_type, score, confidence,
                          rationale, is_llm_judged, model_used, created_at::text
                   FROM agentic_metric_scores
                   WHERE task_run_id = $1
                   ORDER BY metric_type ASC"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("Failed to query agentic scores: {}", e))?;

        let results = rows
            .iter()
            .map(|row| AgenticMetricScoreRow {
                id: row.get(0),
                task_run_id: row.get(1),
                metric_type: row.get(2),
                score: row.get(3),
                confidence: row.get(4),
                rationale: row.get(5),
                is_llm_judged: row.get(6),
                model_used: row.get(7),
                created_at: row.get(8),
            })
            .collect();

        Ok(results)
    }

    /// Get aggregate metric stats over a time range.
    ///
    /// `time_filter` is a PostgreSQL interval string like `'7 days'`.
    pub async fn get_agentic_metric_aggregates(
        &self,
        time_filter: &str,
    ) -> Result<Vec<AgenticMetricAggregate>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT metric_type,
                          AVG(score) as mean_score,
                          MIN(score) as min_score,
                          MAX(score) as max_score,
                          COUNT(*) as runs_scored
                   FROM agentic_metric_scores
                   WHERE created_at >= NOW() - $1::interval
                   GROUP BY metric_type
                   ORDER BY metric_type ASC"#,
                &[&time_filter],
            )
            .await
            .map_err(|e| format!("Failed to query agentic metric aggregates: {}", e))?;

        let results = rows
            .iter()
            .map(|row| AgenticMetricAggregate {
                metric_type: row.get(0),
                mean_score: row.get(1),
                min_score: row.get(2),
                max_score: row.get(3),
                runs_scored: row.get(4),
            })
            .collect();

        Ok(results)
    }

    /// Get the latest task_run_id that has agentic scores.
    pub async fn get_latest_scored_task_run_id(&self) -> Result<Option<String>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"SELECT task_run_id
                   FROM agentic_metric_scores
                   ORDER BY created_at DESC
                   LIMIT 1"#,
                &[],
            )
            .await
            .map_err(|e| format!("Failed to query latest scored task run: {}", e))?;

        Ok(row.map(|r| r.get(0)))
    }

    /// Check whether a task run already has agentic scores recorded.
    pub async fn has_agentic_scores(&self, task_run_id: &str) -> Result<bool, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_one(
                "SELECT COUNT(*) FROM agentic_metric_scores WHERE task_run_id = $1",
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("Failed to check agentic scores: {}", e))?;

        let count: i64 = row.get(0);
        Ok(count > 0)
    }

    /// Persist a RAG retrieval event as a task_run_event.
    pub async fn persist_retrieval_event(
        &self,
        task_run_id: &str,
        event: &crate::meta_optimizer::agentic_metrics::rag_judge::RetrievalEvent,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = format!("tre-{}", uuid::Uuid::new_v4());
        let data_json = serde_json::to_string(event)
            .map_err(|e| format!("Failed to serialize retrieval event: {}", e))?;

        conn.execute(
            r#"INSERT INTO task_run_events (id, task_run_id, event_type, event_subtype, data)
               VALUES ($1, $2, 'retrieval', $3, $4)"#,
            &[&id.as_str(), &task_run_id, &event.source_table.as_str(), &data_json.as_str()],
        )
        .await
        .map_err(|e| format!("PG persist_retrieval_event: {}", e))?;

        Ok(())
    }

    /// Load retrieval events for a task run.
    pub async fn load_retrieval_events(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<crate::meta_optimizer::agentic_metrics::rag_judge::RetrievalEvent>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT data FROM task_run_events
                   WHERE task_run_id = $1 AND event_type = 'retrieval'
                   ORDER BY timestamp ASC"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG load_retrieval_events: {}", e))?;

        let mut events = Vec::new();
        for row in &rows {
            let data_json: String = row.get(0);
            if let Ok(event) = serde_json::from_str(&data_json) {
                events.push(event);
            }
        }

        Ok(events)
    }

    /// Persist agentic metric judge scores.
    pub async fn persist_judge_scores(
        &self,
        task_run_id: &str,
        results: &[crate::meta_optimizer::agentic_metrics::MetricResult],
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let now = chrono::Utc::now().to_rfc3339();
        for score in results {
            let id = format!("ams-{}", uuid::Uuid::new_v4());
            let is_llm_judged = score.is_llm_judged;
            let score_val = score.score;
            let confidence = score.confidence;
            conn.execute(
                r#"INSERT INTO agentic_metric_scores
                   (id, task_run_id, metric_type, score, confidence,
                    rationale, is_llm_judged, model_used, created_at)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                   ON CONFLICT (id) DO NOTHING"#,
                &[
                    &id.as_str(),
                    &task_run_id,
                    &score.metric.as_str(),
                    &score_val,
                    &confidence,
                    &score.rationale as &(dyn tokio_postgres::types::ToSql + Sync),
                    &is_llm_judged,
                    &score.model_used as &(dyn tokio_postgres::types::ToSql + Sync),
                    &now.as_str(),
                ],
            )
            .await
            .map_err(|e| format!("PG persist_judge_scores: {}", e))?;
        }

        Ok(())
    }
}
