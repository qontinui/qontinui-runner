//! PostgreSQL operations for the compensation action stack.
//!
//! Backs `unified_workflow_executor::compensation::CompensationManager`
//! (see plan: `restate-port-part-a-compensation-and-phase-results.md` §1.3–1.4).
//!
//! Table lives under the `runner` schema (search_path = runner, public) —
//! see `schema.pg.sql` for the DDL.

use super::PgDb;

impl PgDb {
    /// Insert a new compensation action row and return its SERIAL id.
    pub async fn insert_compensation_action(
        &self,
        execution_id: &str,
        action_index: i32,
        action_json: &serde_json::Value,
    ) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_one(
                r#"
                INSERT INTO compensation_actions
                    (execution_id, action_index, action_json)
                VALUES ($1, $2, $3::jsonb)
                RETURNING id
                "#,
                &[&execution_id, &action_index, action_json],
            )
            .await
            .map_err(|e| format!("Failed to insert compensation action: {}", e))?;

        let id: i64 = row.get(0);
        Ok(id)
    }

    /// Atomically claim a compensation action for execution.
    ///
    /// Returns `Ok(true)` if this call acquired the claim (row transitioned
    /// from `executed=false` → `executed=true`), `Ok(false)` if another run
    /// already claimed/executed it (no-op, caller should skip), or `Err` on
    /// DB failure.
    pub async fn claim_compensation_action(&self, row_id: i64) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                UPDATE compensation_actions
                SET executed = TRUE, executed_at = NOW()
                WHERE id = $1 AND NOT executed
                RETURNING id
                "#,
                &[&row_id],
            )
            .await
            .map_err(|e| format!("Failed to claim compensation action: {}", e))?;

        Ok(!rows.is_empty())
    }

    /// Store the JSON-serialized `CompensationResult` for a previously claimed
    /// action row.
    pub async fn save_compensation_result(
        &self,
        row_id: i64,
        result_json: &serde_json::Value,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            r#"
            UPDATE compensation_actions
            SET result_json = $2::jsonb
            WHERE id = $1
            "#,
            &[&row_id, result_json],
        )
        .await
        .map_err(|e| format!("Failed to save compensation result: {}", e))?;

        Ok(())
    }

    /// Load all unexecuted compensation actions for an execution, ordered by
    /// `action_index` (insertion order). Intended for startup-resume to
    /// rehydrate an in-memory stack.
    pub async fn load_pending_compensation_actions(
        &self,
        execution_id: &str,
    ) -> Result<Vec<(i64, serde_json::Value)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, action_json
                FROM compensation_actions
                WHERE execution_id = $1 AND NOT executed
                ORDER BY action_index
                "#,
                &[&execution_id],
            )
            .await
            .map_err(|e| format!("Failed to load pending compensation actions: {}", e))?;

        let out = rows
            .into_iter()
            .map(|r| {
                let id: i64 = r.get(0);
                let action_json: serde_json::Value = r.get(1);
                (id, action_json)
            })
            .collect();

        Ok(out)
    }
}
