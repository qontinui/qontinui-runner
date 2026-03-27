//! PostgreSQL pipeline agent traces operations.
//!
//! Provides save/query for the `pipeline_agent_traces` table using raw SQL.

use super::PgDb;
use crate::autoresearch::agentic_verification::{
    GuardrailResult, HandoffContext, PipelineAgentTrace,
};
use tracing::debug;

impl PgDb {
    // ========================================================================
    // Pipeline Agent Traces
    // ========================================================================

    /// Save a single pipeline agent trace.
    pub async fn save_pipeline_trace(
        &self,
        task_run_id: &str,
        trace: &PipelineAgentTrace,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let id = format!("pat-{}", uuid::Uuid::new_v4());
        let now = chrono::Utc::now().to_rfc3339();
        let config_json = serde_json::to_string(&trace.config).unwrap_or_default();
        let input_json = trace.input_snapshot.to_string();
        let output_json = trace.output_snapshot.to_string();
        let duration_ms = trace.duration_ms as i64;
        let tokens_in = trace.tokens_in as i64;
        let tokens_out = trace.tokens_out as i64;
        let downstream_success = trace.downstream_success;
        let guardrail_json = if trace.guardrail_results.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&trace.guardrail_results).unwrap_or_default())
        };
        let handoff_json = trace
            .handoff_received
            .as_ref()
            .and_then(|h| serde_json::to_string(h).ok());

        conn.execute(
            r#"INSERT INTO pipeline_agent_traces
               (id, task_run_id, agent_type, agent_id, run_id,
                input_snapshot, output_snapshot, config_json,
                duration_ms, tokens_in, tokens_out, cost_usd,
                downstream_success, output_quality_score, created_at,
                parent_span_id, span_type, guardrail_results_json, handoff_context_json)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)"#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &task_run_id,
                &trace.agent_type,
                &trace.agent_id,
                &trace.run_id,
                &input_json,
                &output_json,
                &config_json,
                &duration_ms,
                &tokens_in,
                &tokens_out,
                &trace.cost_usd,
                &downstream_success as &(dyn tokio_postgres::types::ToSql + Sync),
                &trace.output_quality_score as &(dyn tokio_postgres::types::ToSql + Sync),
                &now,
                &trace.parent_span_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &trace.span_type,
                &guardrail_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &handoff_json as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG save_pipeline_trace: {}", e))?;

        debug!("Persisted PG trace for {} ({}) in run {}", trace.agent_type, trace.agent_id, task_run_id);
        Ok(())
    }

    /// Save a batch of pipeline agent traces.
    pub async fn save_pipeline_traces(
        &self,
        task_run_id: &str,
        traces: &[PipelineAgentTrace],
    ) -> Result<usize, String> {
        let mut count = 0;
        for trace in traces {
            self.save_pipeline_trace(task_run_id, trace).await?;
            count += 1;
        }
        Ok(count)
    }

    /// Get all pipeline agent traces for a task run.
    pub async fn get_pipeline_traces_for_task(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<PipelineAgentTrace>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT agent_type, agent_id, run_id,
                          input_snapshot, output_snapshot, config_json,
                          duration_ms, tokens_in, tokens_out, cost_usd,
                          downstream_success, output_quality_score,
                          parent_span_id, span_type,
                          guardrail_results_json, handoff_context_json
                   FROM pipeline_agent_traces
                   WHERE task_run_id = $1
                   ORDER BY created_at ASC"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_pipeline_traces_for_task: {}", e))?;

        Ok(rows
            .iter()
            .map(|row| {
                let config_json: String = row.get(5);
                let input_json: String = row.get(3);
                let output_json: String = row.get(4);
                let downstream_success: Option<bool> = row.get(10);
                let guardrail_json: Option<String> = row.get(14);
                let handoff_json: Option<String> = row.get(15);

                PipelineAgentTrace {
                    agent_type: row.get(0),
                    agent_id: row.get(1),
                    run_id: row.get(2),
                    input_snapshot: serde_json::from_str(&input_json).unwrap_or_default(),
                    output_snapshot: serde_json::from_str(&output_json).unwrap_or_default(),
                    config: serde_json::from_str(&config_json).unwrap_or_default(),
                    duration_ms: row.get::<_, i64>(6) as u64,
                    tokens_in: row.get::<_, i64>(7) as u32,
                    tokens_out: row.get::<_, i64>(8) as u32,
                    cost_usd: row.get(9),
                    downstream_success,
                    output_quality_score: row.get(11),
                    parent_span_id: row.get(12),
                    span_type: row.get::<_, Option<String>>(13).unwrap_or_else(|| "agent".to_string()),
                    guardrail_results: guardrail_json
                        .and_then(|j| serde_json::from_str::<Vec<GuardrailResult>>(&j).ok())
                        .unwrap_or_default(),
                    handoff_received: handoff_json
                        .and_then(|j| serde_json::from_str::<HandoffContext>(&j).ok()),
                }
            })
            .collect())
    }

    /// Backfill downstream_success on all traces for a completed execution.
    pub async fn backfill_pipeline_downstream_success(
        &self,
        task_run_id: &str,
        success: bool,
    ) -> Result<u64, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let updated = conn
            .execute(
                "UPDATE pipeline_agent_traces SET downstream_success = $1 WHERE task_run_id = $2 AND downstream_success IS NULL",
                &[&success, &task_run_id],
            )
            .await
            .map_err(|e| format!("PG backfill_pipeline_downstream_success: {}", e))?;

        if updated > 0 {
            debug!(
                "PG backfilled downstream_success={} on {} traces for {}",
                success, updated, task_run_id
            );
        }
        Ok(updated)
    }
}
