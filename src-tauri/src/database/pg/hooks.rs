//! PostgreSQL hooks CRUD operations.
//!
//! Provides PG-backed implementations of task_hooks CRUD used by hooks commands.

use super::PgDb;
use serde_json::json;

impl PgDb {
    /// List all global hooks (task_run_id IS NULL), ordered by execution_order.
    pub async fn list_hooks(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, name, description, trigger, action_type, action_config,
                       enabled, execution_order, continue_on_failure, conditions,
                       task_run_id, created_at::TEXT, updated_at::TEXT
                FROM task_hooks
                WHERE task_run_id IS NULL
                ORDER BY execution_order ASC, created_at ASC
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("PG list_hooks: {}", e))?;

        Ok(rows.iter().map(|row| Self::hook_row_to_json(row)).collect())
    }

    /// Get a single hook by ID.
    pub async fn get_hook(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT id, name, description, trigger, action_type, action_config,
                       enabled, execution_order, continue_on_failure, conditions,
                       task_run_id, created_at::TEXT, updated_at::TEXT
                FROM task_hooks
                WHERE id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_hook: {}", e))?;

        Ok(row.map(|r| Self::hook_row_to_json(&r)))
    }

    /// Create a new hook. Returns the created hook.
    pub async fn create_hook(
        &self,
        id: &str,
        name: &str,
        description: Option<&str>,
        trigger: &str,
        action_type: &str,
        action_config: &str,
        enabled: bool,
        execution_order: i32,
        continue_on_failure: bool,
        conditions: &str,
        task_run_id: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO task_hooks (
                id, name, description, trigger, action_type, action_config,
                enabled, execution_order, continue_on_failure, conditions,
                task_run_id, created_at, updated_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, NOW(), NOW())
            "#,
            &[
                &id,
                &name,
                &description,
                &trigger,
                &action_type,
                &action_config,
                &enabled,
                &execution_order,
                &continue_on_failure,
                &conditions,
                &task_run_id,
            ],
        )
        .await
        .map_err(|e| format!("PG create_hook: {}", e))?;

        Ok(())
    }

    /// Update an existing hook.
    pub async fn update_hook(
        &self,
        id: &str,
        name: &str,
        description: Option<&str>,
        trigger: &str,
        action_type: &str,
        action_config: &str,
        enabled: bool,
        execution_order: i32,
        continue_on_failure: bool,
        conditions: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            r#"
            UPDATE task_hooks SET
                name = $1,
                description = $2,
                trigger = $3,
                action_type = $4,
                action_config = $5,
                enabled = $6,
                execution_order = $7,
                continue_on_failure = $8,
                conditions = $9,
                updated_at = NOW()
            WHERE id = $10
            "#,
            &[
                &name,
                &description,
                &trigger,
                &action_type,
                &action_config,
                &enabled,
                &execution_order,
                &continue_on_failure,
                &conditions,
                &id,
            ],
        )
        .await
        .map_err(|e| format!("PG update_hook: {}", e))?;

        Ok(())
    }

    /// Delete a hook by ID. Returns true if a row was deleted.
    pub async fn delete_hook(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let count = conn
            .execute("DELETE FROM task_hooks WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("PG delete_hook: {}", e))?;

        Ok(count > 0)
    }

    /// Set enabled status for a hook. Returns true if a row was updated.
    pub async fn set_hook_enabled(&self, id: &str, enabled: bool) -> Result<bool, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let count = conn
            .execute(
                "UPDATE task_hooks SET enabled = $1, updated_at = NOW() WHERE id = $2",
                &[&enabled, &id],
            )
            .await
            .map_err(|e| format!("PG set_hook_enabled: {}", e))?;

        Ok(count > 0)
    }

    /// Reorder hooks by setting execution_order based on position in the list.
    pub async fn reorder_hooks(&self, hook_ids: &[String]) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        for (index, hook_id) in hook_ids.iter().enumerate() {
            let order = index as i32;
            conn.execute(
                "UPDATE task_hooks SET execution_order = $1, updated_at = NOW() WHERE id = $2",
                &[&order, &hook_id],
            )
            .await
            .map_err(|e| format!("PG reorder_hooks: {}", e))?;
        }

        Ok(())
    }

    /// Convert a tokio_postgres::Row from task_hooks to JSON.
    fn hook_row_to_json(row: &tokio_postgres::Row) -> serde_json::Value {
        let id: String = row.get(0);
        let name: String = row.get(1);
        let description: Option<String> = row.get(2);
        let trigger: String = row.get(3);
        let action_type: String = row.get(4);
        let action_config: String = row.get(5);
        let enabled: bool = row.get(6);
        let execution_order: i32 = row.get(7);
        let continue_on_failure: bool = row.get(8);
        let conditions: String = row.get(9);
        let task_run_id: Option<String> = row.get(10);
        let created_at: String = row.get(11);
        let updated_at: String = row.get(12);

        json!({
            "id": id,
            "name": name,
            "description": description,
            "trigger": trigger,
            "action_type": action_type,
            "action_config": action_config,
            "enabled": enabled,
            "execution_order": execution_order,
            "continue_on_failure": continue_on_failure,
            "conditions": conditions,
            "task_run_id": task_run_id,
            "created_at": created_at,
            "updated_at": updated_at,
        })
    }
}
