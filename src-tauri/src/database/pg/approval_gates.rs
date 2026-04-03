//! PostgreSQL approval gate operations via Clorinde-generated queries.

use super::PgDb;

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

impl PgDb {
    /// Record a new approval gate request.
    pub async fn insert_approval_gate(
        &self,
        id: &str,
        task_run_id: &str,
        iteration: i32,
        prompt: &str,
        context_json: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::approval_gates::insert_approval_gate()
            .bind(&conn, &id, &task_run_id, &iteration, &prompt, &context_json)
            .one()
            .await
            .map_err(|e| format!("PG insert_approval_gate: {}", e))?;
        Ok(())
    }

    /// Resolve an approval gate (record human response).
    pub async fn resolve_approval_gate(
        &self,
        id: &str,
        action: &str,
        comment: Option<&str>,
        status: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::approval_gates::resolve_approval_gate()
            .bind(&conn, &action, &comment, &status, &id)
            .opt()
            .await
            .map_err(|e| format!("PG resolve_approval_gate: {}", e))?;
        Ok(())
    }

    /// Get approval gate history for a task run.
    pub async fn get_approval_gates_for_task_run(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::approval_gates::get_approval_gates_for_task_run()
            .bind(&conn, &task_run_id)
            .all()
            .await
            .map_err(|e| format!("PG get_approval_gates_for_task_run: {}", e))?;

        Ok(rows.into_iter().map(|r| {
            serde_json::json!({
                "id": r.id,
                "task_run_id": r.task_run_id,
                "iteration": r.iteration,
                "prompt": r.prompt,
                "context_json": r.context_json,
                "action": non_empty(r.action),
                "comment": non_empty(r.comment),
                "status": r.status,
                "created_at": r.created_at.to_rfc3339(),
                "resolved_at": if r.resolved_at.timestamp() == 0 { None } else { Some(r.resolved_at.to_rfc3339()) },
            })
        }).collect())
    }
}
