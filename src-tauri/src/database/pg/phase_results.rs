//! PostgreSQL operations for structured PhaseResult persistence.
//!
//! Backs the Phase Timeline tab in the run recap
//! (plan: restate-port-part-a-compensation-and-phase-results.md §2.3).
//! Table lives under the `runner` schema (search_path = runner, public).

use crate::unified_workflow_executor::types::PhaseResult;

use super::PgDb;

impl PgDb {
    /// Persist a single `PhaseResult` row.
    pub async fn save_phase_result(
        &self,
        execution_id: &str,
        result: &PhaseResult,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let step_results_json = serde_json::to_value(&result.step_results)
            .map_err(|e| format!("serialize step_results: {}", e))?;
        let variables_set_json = match &result.variables_set {
            Some(v) => Some(
                serde_json::to_value(v).map_err(|e| format!("serialize variables_set: {}", e))?,
            ),
            None => None,
        };
        let iteration_i32: Option<i32> = result.iteration.map(|i| i as i32);
        let stage_index_i32: Option<i32> = result.stage_index.map(|i| i as i32);
        let duration_ms_i64: i64 = result.duration_ms as i64;

        conn.execute(
            r#"
            INSERT INTO phase_results
                (execution_id, phase, iteration, stage_index, success, all_passed,
                 duration_ms, failure_context, commit_hash, step_results, variables_set)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::jsonb, $11::jsonb)
            "#,
            &[
                &execution_id,
                &result.phase,
                &iteration_i32,
                &stage_index_i32,
                &result.success,
                &result.all_passed,
                &duration_ms_i64,
                &result.failure_context,
                &result.commit_hash,
                &step_results_json,
                &variables_set_json,
            ],
        )
        .await
        .map_err(|e| format!("Failed to save phase result: {}", e))?;

        Ok(())
    }

    /// Load all phase results for an execution, ordered by creation time.
    pub async fn get_phase_results(&self, execution_id: &str) -> Result<Vec<PhaseResult>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT phase, iteration, stage_index, success, all_passed,
                       duration_ms, failure_context, commit_hash,
                       step_results, variables_set
                FROM phase_results
                WHERE execution_id = $1
                ORDER BY created_at ASC
                "#,
                &[&execution_id],
            )
            .await
            .map_err(|e| format!("Failed to load phase results: {}", e))?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let phase: String = r.get(0);
            let iteration: Option<i32> = r.get(1);
            let stage_index: Option<i32> = r.get(2);
            let success: bool = r.get(3);
            let all_passed: bool = r.get(4);
            let duration_ms: i64 = r.get(5);
            let failure_context: Option<String> = r.get(6);
            let commit_hash: Option<String> = r.get(7);
            let step_results_json: serde_json::Value = r.get(8);
            let variables_set_json: Option<serde_json::Value> = r.get(9);

            let step_results = serde_json::from_value(step_results_json)
                .map_err(|e| format!("deserialize step_results: {}", e))?;
            let variables_set = match variables_set_json {
                Some(v) => Some(
                    serde_json::from_value(v)
                        .map_err(|e| format!("deserialize variables_set: {}", e))?,
                ),
                None => None,
            };

            out.push(PhaseResult {
                phase,
                iteration: iteration.map(|i| i as u32),
                stage_index: stage_index.map(|i| i as u32),
                success,
                all_passed,
                step_results,
                failure_context,
                duration_ms: duration_ms as u64,
                variables_set,
                commit_hash,
            });
        }

        Ok(out)
    }
}
