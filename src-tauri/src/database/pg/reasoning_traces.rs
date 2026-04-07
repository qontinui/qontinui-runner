//! PostgreSQL reasoning trace operations.
//!
//! Stores inductive, deductive, and abductive reasoning traces produced by
//! the dreamer formal reasoning cycle. Also manages dreamer log entries
//! in memory_consolidation_log.

use super::PgDb;
use crate::database::types::*;

impl PgDb {
    /// Save a reasoning trace. Returns the new trace ID.
    pub async fn save_reasoning_trace(
        &self,
        input: &CreateReasoningTraceInput,
    ) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_one(
                "INSERT INTO reasoning_traces (
                    reasoning_type, premise_ids, conclusion, confidence,
                    evidence_json, created_observation_id, dreamer_run_id
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                RETURNING id",
                &[
                    &input.reasoning_type,
                    &input.premise_ids,
                    &input.conclusion,
                    &input.confidence,
                    &input.evidence_json,
                    &input.created_observation_id,
                    &input.dreamer_run_id,
                ],
            )
            .await
            .map_err(|e| format!("PG save_reasoning_trace: {}", e))?;

        Ok(row.get::<_, i64>("id"))
    }

    /// Get all traces for a specific dreamer run.
    pub async fn get_traces_by_run(
        &self,
        dreamer_run_id: i64,
    ) -> Result<Vec<ReasoningTrace>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                "SELECT id, reasoning_type, premise_ids, conclusion, confidence,
                        evidence_json, created_observation_id, dreamer_run_id,
                        is_valid, invalidated_by, created_at
                 FROM reasoning_traces
                 WHERE dreamer_run_id = $1
                 ORDER BY created_at",
                &[&dreamer_run_id],
            )
            .await
            .map_err(|e| format!("PG get_traces_by_run: {}", e))?;

        Ok(rows.iter().map(|r| row_to_trace(r)).collect())
    }

    /// Get recent reasoning traces, optionally filtered by type.
    pub async fn get_recent_traces(
        &self,
        reasoning_type: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ReasoningTrace>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = if let Some(rt) = reasoning_type {
            conn.query(
                "SELECT id, reasoning_type, premise_ids, conclusion, confidence,
                        evidence_json, created_observation_id, dreamer_run_id,
                        is_valid, invalidated_by, created_at
                 FROM reasoning_traces
                 WHERE reasoning_type = $1
                 ORDER BY created_at DESC
                 LIMIT $2",
                &[&rt, &limit],
            )
            .await
            .map_err(|e| format!("PG get_recent_traces (filtered): {}", e))?
        } else {
            conn.query(
                "SELECT id, reasoning_type, premise_ids, conclusion, confidence,
                        evidence_json, created_observation_id, dreamer_run_id,
                        is_valid, invalidated_by, created_at
                 FROM reasoning_traces
                 ORDER BY created_at DESC
                 LIMIT $1",
                &[&limit],
            )
            .await
            .map_err(|e| format!("PG get_recent_traces: {}", e))?
        };

        Ok(rows.iter().map(|r| row_to_trace(r)).collect())
    }

    /// Invalidate a reasoning trace.
    pub async fn invalidate_trace(
        &self,
        trace_id: i64,
        invalidated_by: Option<i64>,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let modified = conn
            .execute(
                "UPDATE reasoning_traces
                 SET is_valid = false, invalidated_by = $2
                 WHERE id = $1 AND is_valid = true",
                &[&trace_id, &invalidated_by],
            )
            .await
            .map_err(|e| format!("PG invalidate_trace: {}", e))?;

        Ok(modified > 0)
    }

    /// Insert a dreamer log entry. Returns the log ID.
    pub async fn insert_dreamer_log(&self) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_one(
                "INSERT INTO memory_consolidation_log (is_dreamer)
                 VALUES (true)
                 RETURNING id",
                &[],
            )
            .await
            .map_err(|e| format!("PG insert_dreamer_log: {}", e))?;

        Ok(row.get::<_, i64>("id"))
    }

    /// Complete a dreamer log entry with stats and optional error.
    pub async fn complete_dreamer_log(
        &self,
        id: i64,
        stats: &DreamerStats,
        error: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            "UPDATE memory_consolidation_log
             SET completed_at = NOW(),
                 inductive_traces = $1,
                 deductive_traces = $2,
                 abductive_traces = $3,
                 error = $4
             WHERE id = $5",
            &[
                &stats.inductive_traces,
                &stats.deductive_traces,
                &stats.abductive_traces,
                &error,
                &id,
            ],
        )
        .await
        .map_err(|e| format!("PG complete_dreamer_log: {}", e))?;

        Ok(())
    }

    /// Get the most recent completed dreamer run timestamp.
    pub async fn get_last_dreamer_time(
        &self,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_one(
                "SELECT MAX(started_at) AS last_run
                 FROM memory_consolidation_log
                 WHERE is_dreamer = true AND completed_at IS NOT NULL",
                &[],
            )
            .await
            .map_err(|e| format!("PG get_last_dreamer_time: {}", e))?;

        let last_run: Option<chrono::DateTime<chrono::Utc>> = row.get("last_run");
        Ok(last_run)
    }

    /// Get recent dreamer log entries.
    pub async fn get_dreamer_log(&self, limit: i64) -> Result<Vec<DreamerLogEntry>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                "SELECT id, started_at, completed_at, is_dreamer,
                        inductive_traces, deductive_traces, abductive_traces, error
                 FROM memory_consolidation_log
                 WHERE is_dreamer = true
                 ORDER BY started_at DESC
                 LIMIT $1",
                &[&limit],
            )
            .await
            .map_err(|e| format!("PG get_dreamer_log: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let started_at: chrono::DateTime<chrono::Utc> = r.get("started_at");
                let completed_at: Option<chrono::DateTime<chrono::Utc>> = r.get("completed_at");
                DreamerLogEntry {
                    id: r.get("id"),
                    started_at: started_at.to_rfc3339(),
                    completed_at: completed_at.map(|t| t.to_rfc3339()),
                    is_dreamer: r.get("is_dreamer"),
                    inductive_traces: r.get("inductive_traces"),
                    deductive_traces: r.get("deductive_traces"),
                    abductive_traces: r.get("abductive_traces"),
                    error: r.get("error"),
                }
            })
            .collect())
    }
}

/// Convert a row to a ReasoningTrace.
fn row_to_trace(r: &tokio_postgres::Row) -> ReasoningTrace {
    let created_at: chrono::DateTime<chrono::Utc> = r.get("created_at");
    ReasoningTrace {
        id: r.get("id"),
        reasoning_type: r.get("reasoning_type"),
        premise_ids: r.get("premise_ids"),
        conclusion: r.get("conclusion"),
        confidence: r.get("confidence"),
        evidence_json: r.get("evidence_json"),
        created_observation_id: r.get("created_observation_id"),
        dreamer_run_id: r.get("dreamer_run_id"),
        is_valid: r.get("is_valid"),
        invalidated_by: r.get("invalidated_by"),
        created_at: created_at.to_rfc3339(),
    }
}
