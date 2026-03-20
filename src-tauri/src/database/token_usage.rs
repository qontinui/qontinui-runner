//! Phase token usage operations.
//!
//! Contains all CheckpointDb methods related to tracking AI token usage per workflow phase.

use rusqlite::params;

use super::types::*;
use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Phase Token Usage Operations
    // ========================================================================

    /// Record token usage for a single AI call within a workflow phase.
    pub fn create_phase_token_usage(
        &self,
        task_run_id: &str,
        phase: &str,
        stage_index: Option<u32>,
        iteration: Option<u32>,
        model_used: Option<&str>,
        provider_used: Option<&str>,
        input_tokens: u64,
        output_tokens: u64,
        cost_cents: u64,
        duration_ms: Option<u64>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"INSERT INTO phase_token_usage
                (task_run_id, phase, stage_index, iteration, model_used, provider_used,
                 input_tokens, output_tokens, cost_cents, duration_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"#,
            params![
                task_run_id,
                phase,
                stage_index.map(|v| v as i64),
                iteration.map(|v| v as i64),
                model_used,
                provider_used,
                input_tokens as i64,
                output_tokens as i64,
                cost_cents as i64,
                duration_ms.map(|v| v as i64),
            ],
        )
        .map_err(|e| format!("Failed to insert phase token usage: {}", e))?;
        Ok(())
    }

    /// Get per-phase token usage breakdown for a task run.
    pub fn get_phase_token_usage(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<PhaseTokenUsageRow>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT phase, stage_index, iteration, model_used, provider_used,
                       input_tokens, output_tokens, cost_cents, duration_ms, created_at
                FROM phase_token_usage
                WHERE task_run_id = ?1
                ORDER BY created_at ASC"#,
            )
            .map_err(|e| format!("Failed to prepare phase token usage query: {}", e))?;

        let rows = stmt
            .query_map(params![task_run_id], |row| {
                Ok(PhaseTokenUsageRow {
                    phase: row.get(0)?,
                    stage_index: row.get::<_, Option<i64>>(1)?.map(|v| v as u32),
                    iteration: row.get::<_, Option<i64>>(2)?.map(|v| v as u32),
                    model_used: row.get(3)?,
                    provider_used: row.get(4)?,
                    input_tokens: row.get::<_, i64>(5)? as u64,
                    output_tokens: row.get::<_, i64>(6)? as u64,
                    cost_cents: row.get::<_, i64>(7)? as u64,
                    duration_ms: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
                    created_at: row.get(9)?,
                })
            })
            .map_err(|e| format!("Failed to query phase token usage: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect phase token usage rows: {}", e))
    }

    /// Update the aggregate token totals on a task run.
    pub fn update_task_run_token_totals(&self, task_run_id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"UPDATE task_runs SET
                total_input_tokens = COALESCE((SELECT SUM(input_tokens) FROM phase_token_usage WHERE task_run_id = ?1), 0),
                total_output_tokens = COALESCE((SELECT SUM(output_tokens) FROM phase_token_usage WHERE task_run_id = ?1), 0),
                total_cost_cents = COALESCE((SELECT SUM(cost_cents) FROM phase_token_usage WHERE task_run_id = ?1), 0)
            WHERE id = ?1"#,
            params![task_run_id],
        )
        .map_err(|e| format!("Failed to update task run token totals: {}", e))?;
        Ok(())
    }
}
