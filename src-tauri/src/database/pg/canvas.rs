//! PostgreSQL canvas_panels operations (raw SQL).
//!
//! Mirrors the SQLite database/canvas_ops.rs operations.

use super::PgDb;
use crate::mcp::canvas::StoredPanel;

impl PgDb {
    // ========================================================================
    // Canvas Panels
    // ========================================================================

    /// Insert or update a canvas panel.
    pub async fn insert_or_update_canvas_panel(&self, panel: &StoredPanel) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let data_json = panel.data.to_string();

        conn.execute(
            r#"
            INSERT INTO canvas_panels (id, task_run_id, component, title, data_json, priority, size, group_name, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::TIMESTAMPTZ, $10::TIMESTAMPTZ)
            ON CONFLICT(id) DO UPDATE SET
                component = $3,
                title = $4,
                data_json = $5,
                priority = $6,
                size = $7,
                group_name = $8,
                updated_at = $10::TIMESTAMPTZ
            "#,
            &[
                &panel.panel_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &panel.task_run_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &panel.component as &(dyn tokio_postgres::types::ToSql + Sync),
                &panel.title as &(dyn tokio_postgres::types::ToSql + Sync),
                &data_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &panel.priority as &(dyn tokio_postgres::types::ToSql + Sync),
                &panel.size as &(dyn tokio_postgres::types::ToSql + Sync),
                &panel.group as &(dyn tokio_postgres::types::ToSql + Sync),
                &panel.created_at as &(dyn tokio_postgres::types::ToSql + Sync),
                &panel.updated_at as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG insert_or_update_canvas_panel: {}", e))?;

        Ok(())
    }

    /// Get all canvas panels for a task run.
    pub async fn get_canvas_panels_for_task_run(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<StoredPanel>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, task_run_id, component, title, data_json, priority, size, group_name,
                       created_at::TEXT, updated_at::TEXT
                FROM canvas_panels
                WHERE task_run_id = $1
                ORDER BY priority ASC, created_at ASC
                "#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_canvas_panels_for_task_run: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let data_json: String = r.get(4);
                StoredPanel {
                    panel_id: r.get(0),
                    task_run_id: r.get(1),
                    component: r.get(2),
                    title: r.get(3),
                    data: serde_json::from_str(&data_json).unwrap_or(serde_json::json!({})),
                    priority: r.get(5),
                    size: r.get(6),
                    group: r.get(7),
                    created_at: r.get(8),
                    updated_at: r.get(9),
                }
            })
            .collect())
    }

    /// Delete a single canvas panel.
    pub async fn delete_canvas_panel(&self, panel_id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute("DELETE FROM canvas_panels WHERE id = $1", &[&panel_id])
            .await
            .map_err(|e| format!("PG delete_canvas_panel: {}", e))?;

        Ok(affected > 0)
    }

    /// Clear all canvas panels for a task run.
    pub async fn clear_canvas_panels_for_task_run(&self, task_run_id: &str) -> Result<u64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute(
                "DELETE FROM canvas_panels WHERE task_run_id = $1",
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG clear_canvas_panels: {}", e))?;

        Ok(affected)
    }
}
