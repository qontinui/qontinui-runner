//! PostgreSQL pipeline agent traces and span events operations.
//!
//! Provides save/query for `pipeline_agent_traces` and `span_events` tables.

use super::PgDb;
use crate::autoresearch::agentic_verification::{
    GuardrailResult, HandoffContext, PipelineAgentTrace,
};
use crate::meta_optimizer::span_instrumentation::SpanEventRow;
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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

        let validation_retries_i32 = trace.validation_retries;

        conn.execute(
            r#"INSERT INTO pipeline_agent_traces
               (id, task_run_id, agent_type, agent_id, run_id,
                input_snapshot, output_snapshot, config_json,
                duration_ms, tokens_in, tokens_out, cost_usd,
                downstream_success, output_quality_score, created_at,
                parent_span_id, span_type, guardrail_results_json, handoff_context_json,
                schema_valid_first_attempt, validation_retries, coercions_applied, validation_error_summary)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23)"#,
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
                &trace.schema_valid_first_attempt as &(dyn tokio_postgres::types::ToSql + Sync),
                &validation_retries_i32 as &(dyn tokio_postgres::types::ToSql + Sync),
                &trace.coercions_applied as &(dyn tokio_postgres::types::ToSql + Sync),
                &trace.validation_error_summary as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG save_pipeline_trace: {}", e))?;

        debug!(
            "Persisted PG trace for {} ({}) in run {}",
            trace.agent_type, trace.agent_id, task_run_id
        );
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT agent_type, agent_id, run_id,
                          input_snapshot, output_snapshot, config_json,
                          duration_ms, tokens_in, tokens_out, cost_usd,
                          downstream_success, output_quality_score,
                          parent_span_id, span_type,
                          guardrail_results_json, handoff_context_json,
                          schema_valid_first_attempt, validation_retries,
                          coercions_applied, validation_error_summary
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
                    span_type: row
                        .get::<_, Option<String>>(13)
                        .unwrap_or_else(|| "agent".to_string()),
                    guardrail_results: guardrail_json
                        .and_then(|j| serde_json::from_str::<Vec<GuardrailResult>>(&j).ok())
                        .unwrap_or_default(),
                    handoff_received: handoff_json
                        .and_then(|j| serde_json::from_str::<HandoffContext>(&j).ok()),
                    schema_valid_first_attempt: row.get(16),
                    validation_retries: row.get(17),
                    coercions_applied: row.get(18),
                    validation_error_summary: row.get(19),
                }
            })
            .collect())
    }

    // ========================================================================
    // Span Events
    // ========================================================================

    /// Save a batch of span events.
    pub async fn save_span_events(&self, events: &[SpanEventRow]) -> Result<usize, String> {
        if events.is_empty() {
            return Ok(0);
        }
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let mut count = 0;
        for event in events {
            let now = chrono::Utc::now().to_rfc3339();
            conn.execute(
                r#"INSERT INTO span_events
                   (id, execution_id, trace_id, agent_type, event_type, step_index,
                    metric_name, reward_value, data_key, data_json, role, content, created_at)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"#,
                &[
                    &event.id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &event.execution_id,
                    &event.trace_id,
                    &event.agent_type,
                    &event.event_type,
                    &event.step_index,
                    &event.metric_name as &(dyn tokio_postgres::types::ToSql + Sync),
                    &event.reward_value as &(dyn tokio_postgres::types::ToSql + Sync),
                    &event.data_key as &(dyn tokio_postgres::types::ToSql + Sync),
                    &event.data_json as &(dyn tokio_postgres::types::ToSql + Sync),
                    &event.role as &(dyn tokio_postgres::types::ToSql + Sync),
                    &event.content as &(dyn tokio_postgres::types::ToSql + Sync),
                    &now,
                ],
            )
            .await
            .map_err(|e| format!("PG save_span_event: {}", e))?;
            count += 1;
        }

        debug!("Persisted {} PG span events", count);
        Ok(count)
    }

    /// Get all span events for an execution.
    pub async fn get_span_events_for_execution(
        &self,
        execution_id: &str,
    ) -> Result<Vec<SpanEventRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, execution_id, trace_id, agent_type, event_type, step_index,
                          metric_name, reward_value, data_key, data_json, role, content
                   FROM span_events
                   WHERE execution_id = $1
                   ORDER BY step_index ASC, created_at ASC"#,
                &[&execution_id],
            )
            .await
            .map_err(|e| format!("PG get_span_events_for_execution: {}", e))?;

        Ok(rows
            .iter()
            .map(|row| SpanEventRow {
                id: row.get(0),
                execution_id: row.get(1),
                trace_id: row.get(2),
                agent_type: row.get(3),
                event_type: row.get(4),
                step_index: row.get(5),
                metric_name: row.get(6),
                reward_value: row.get(7),
                data_key: row.get(8),
                data_json: row.get(9),
                role: row.get(10),
                content: row.get(11),
            })
            .collect())
    }

    /// Get reward events for a specific agent type, ordered by most recent.
    pub async fn get_reward_events_for_agent(
        &self,
        agent_type: &str,
        limit: i64,
    ) -> Result<Vec<(String, f64, String)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT execution_id, reward_value, metric_name
                   FROM span_events
                   WHERE agent_type = $1 AND event_type = 'reward' AND reward_value IS NOT NULL
                   ORDER BY created_at DESC
                   LIMIT $2"#,
                &[&agent_type, &limit],
            )
            .await
            .map_err(|e| format!("PG get_reward_events_for_agent: {}", e))?;

        Ok(rows
            .iter()
            .map(|row| {
                let exec_id: String = row.get(0);
                let reward: f64 = row.get(1);
                let metric: String = row.get::<_, Option<String>>(2).unwrap_or_default();
                (exec_id, reward, metric)
            })
            .collect())
    }

    // ========================================================================
    // Pipeline Agent Traces (continued)
    // ========================================================================

    /// Backfill downstream_success on all traces for a completed execution.
    pub async fn backfill_pipeline_downstream_success(
        &self,
        task_run_id: &str,
        success: bool,
    ) -> Result<u64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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
