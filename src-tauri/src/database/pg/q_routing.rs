//! PostgreSQL Q-routing table operations.
//!
//! CRUD for the `q_routing_table` which stores Q-learning state-action values
//! for architecture routing decisions.

use super::PgDb;

impl PgDb {
    /// Load all Q-table entries from the database.
    ///
    /// Returns (state_key, action, q_value, visit_count) tuples.
    pub async fn get_all_q_entries(&self) -> Result<Vec<(String, String, f64, u32)>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                "SELECT state_key, action, q_value, visit_count FROM q_routing_table ORDER BY state_key, action",
                &[],
            )
            .await
            .map_err(|e| format!("PG get_all_q_entries: {}", e))?;

        Ok(rows
            .iter()
            .map(|row| {
                let state_key: String = row.get(0);
                let action: String = row.get(1);
                let q_value: f64 = row.get(2);
                let visit_count: i32 = row.get(3);
                (state_key, action, q_value, visit_count as u32)
            })
            .collect())
    }

    /// Upsert a Q-table entry after a Q-value update.
    pub async fn upsert_q_entry(
        &self,
        state_key: &str,
        action: &str,
        q_value: f64,
        visit_count: u32,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            r#"INSERT INTO q_routing_table (state_key, action, q_value, visit_count, last_updated)
               VALUES ($1, $2, $3, $4, NOW())
               ON CONFLICT(state_key, action) DO UPDATE SET
                   q_value = EXCLUDED.q_value,
                   visit_count = EXCLUDED.visit_count,
                   last_updated = NOW()"#,
            &[&state_key, &action, &q_value, &(visit_count as i32)],
        )
        .await
        .map_err(|e| format!("PG upsert_q_entry: {}", e))?;

        Ok(())
    }

    /// Get Q-table entries for a specific state.
    pub async fn get_q_entries_for_state(
        &self,
        state_key: &str,
    ) -> Result<Vec<(String, f64, u32)>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                "SELECT action, q_value, visit_count FROM q_routing_table WHERE state_key = $1 ORDER BY action",
                &[&state_key],
            )
            .await
            .map_err(|e| format!("PG get_q_entries_for_state: {}", e))?;

        Ok(rows
            .iter()
            .map(|row| {
                let action: String = row.get(0);
                let q_value: f64 = row.get(1);
                let visit_count: i32 = row.get(2);
                (action, q_value, visit_count as u32)
            })
            .collect())
    }

    // ── Override CRUD ──────────────────────────────────────────────────────

    /// Get all Q-routing overrides.
    pub async fn get_all_q_overrides(&self) -> Result<Vec<(String, String)>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                "SELECT state_key, forced_action FROM q_routing_overrides ORDER BY state_key",
                &[],
            )
            .await
            .map_err(|e| format!("PG get_all_q_overrides: {}", e))?;

        Ok(rows
            .iter()
            .map(|row| {
                let state_key: String = row.get(0);
                let forced_action: String = row.get(1);
                (state_key, forced_action)
            })
            .collect())
    }

    /// Set a Q-routing override (upsert).
    pub async fn upsert_q_override(
        &self,
        state_key: &str,
        forced_action: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            r#"INSERT INTO q_routing_overrides (state_key, forced_action, created_at)
               VALUES ($1, $2, NOW())
               ON CONFLICT(state_key) DO UPDATE SET
                   forced_action = EXCLUDED.forced_action,
                   created_at = NOW()"#,
            &[&state_key, &forced_action],
        )
        .await
        .map_err(|e| format!("PG upsert_q_override: {}", e))?;

        Ok(())
    }

    /// Remove a Q-routing override.
    pub async fn delete_q_override(&self, state_key: &str) -> Result<bool, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let deleted = conn
            .execute(
                "DELETE FROM q_routing_overrides WHERE state_key = $1",
                &[&state_key],
            )
            .await
            .map_err(|e| format!("PG delete_q_override: {}", e))?;

        Ok(deleted > 0)
    }

    /// Clear the entire Q-table (for resets).
    pub async fn clear_q_table(&self) -> Result<u64, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let deleted = conn
            .execute("DELETE FROM q_routing_table", &[])
            .await
            .map_err(|e| format!("PG clear_q_table: {}", e))?;

        Ok(deleted)
    }
}
