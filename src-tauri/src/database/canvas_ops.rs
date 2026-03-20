//! Canvas panel (A2UI) operations.
//!
//! Contains all CheckpointDb methods related to canvas panels.

use rusqlite::params;

use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Canvas Panels (A2UI)
    // ========================================================================

    /// Insert or update a canvas panel.
    pub fn insert_or_update_canvas_panel(
        &self,
        panel: &crate::mcp::canvas::StoredPanel,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let data_json = panel.data.to_string();

        conn.execute(
            r#"
            INSERT INTO canvas_panels (id, task_run_id, component, title, data_json, priority, size, group_name, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(id) DO UPDATE SET
                component = ?3,
                title = ?4,
                data_json = ?5,
                priority = ?6,
                size = ?7,
                group_name = ?8,
                updated_at = ?10
            "#,
            params![
                panel.panel_id,
                panel.task_run_id,
                panel.component,
                panel.title,
                data_json,
                panel.priority,
                panel.size,
                panel.group,
                panel.created_at,
                panel.updated_at,
            ],
        )
        .map_err(|e| format!("Failed to upsert canvas panel: {}", e))?;

        Ok(())
    }

    /// Get all canvas panels for a task run.
    pub fn get_canvas_panels_for_task_run(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<crate::mcp::canvas::StoredPanel>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_run_id, component, title, data_json, priority, size, group_name, created_at, updated_at
                FROM canvas_panels
                WHERE task_run_id = ?1
                ORDER BY priority ASC, created_at ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare canvas panels query: {}", e))?;

        let panels = stmt
            .query_map(params![task_run_id], |row| {
                let data_json: String = row.get(4)?;
                let data: serde_json::Value =
                    serde_json::from_str(&data_json).unwrap_or(serde_json::json!({}));
                Ok(crate::mcp::canvas::StoredPanel {
                    panel_id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    component: row.get(2)?,
                    title: row.get(3)?,
                    data,
                    priority: row.get(5)?,
                    size: row.get(6)?,
                    group: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                })
            })
            .map_err(|e| format!("Failed to query canvas panels: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(panels)
    }

    /// Delete a single canvas panel.
    pub fn delete_canvas_panel(&self, panel_id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;
        let rows = conn
            .execute("DELETE FROM canvas_panels WHERE id = ?1", params![panel_id])
            .map_err(|e| format!("Failed to delete canvas panel: {}", e))?;
        Ok(rows > 0)
    }

    /// Clear all canvas panels for a task run.
    pub fn clear_canvas_panels_for_task_run(&self, task_run_id: &str) -> Result<usize, String> {
        let conn = self.get_conn()?;
        let rows = conn
            .execute(
                "DELETE FROM canvas_panels WHERE task_run_id = ?1",
                params![task_run_id],
            )
            .map_err(|e| format!("Failed to clear canvas panels: {}", e))?;
        Ok(rows)
    }
}
