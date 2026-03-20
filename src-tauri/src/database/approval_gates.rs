//! Approval gate operations.
//!
//! Contains all CheckpointDb methods related to approval gates for human-in-the-loop workflows.

use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Approval Gate Operations
    // ========================================================================

    /// Record a new approval gate request (audit trail).
    pub fn insert_approval_gate(
        &self,
        id: &str,
        task_run_id: &str,
        iteration: u32,
        prompt: &str,
        context_json: &str,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "INSERT INTO approval_gates (id, task_run_id, iteration, prompt, context_json, status, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', datetime('now'))",
            rusqlite::params![id, task_run_id, iteration as i64, prompt, context_json],
        )
        .map_err(|e| format!("Failed to insert approval gate: {}", e))?;
        Ok(())
    }

    /// Resolve an approval gate (record human response).
    pub fn resolve_approval_gate(
        &self,
        id: &str,
        action: &str,
        comment: Option<&str>,
    ) -> Result<(), String> {
        let status = match action {
            "approve" => "approved",
            "reject" => "rejected",
            "abort" => "aborted",
            _ => action,
        };
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE approval_gates SET action = ?1, comment = ?2, status = ?3, resolved_at = datetime('now') WHERE id = ?4",
            rusqlite::params![action, comment, status, id],
        )
        .map_err(|e| format!("Failed to resolve approval gate: {}", e))?;
        Ok(())
    }

    /// Get approval gate history for a task run.
    pub fn get_approval_gates_for_task_run(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_run_id, iteration, prompt, context_json, action, comment, status, created_at, resolved_at \
                 FROM approval_gates WHERE task_run_id = ?1 ORDER BY created_at ASC",
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let rows = stmt
            .query_map(rusqlite::params![task_run_id], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_run_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "prompt": row.get::<_, String>(3)?,
                    "context_json": row.get::<_, String>(4).unwrap_or_default(),
                    "action": row.get::<_, Option<String>>(5)?,
                    "comment": row.get::<_, Option<String>>(6)?,
                    "status": row.get::<_, String>(7)?,
                    "created_at": row.get::<_, String>(8)?,
                    "resolved_at": row.get::<_, Option<String>>(9)?,
                }))
            })
            .map_err(|e| format!("Failed to query approval gates: {}", e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect approval gates: {}", e))?;

        Ok(rows)
    }
}
