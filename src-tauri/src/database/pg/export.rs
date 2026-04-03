//! PostgreSQL export operations for backup/restore.
//!
//! Each method returns `Vec<serde_json::Value>` by querying entire tables,
//! matching the SQLite export_all_* interface.

use crate::database::types::{DatabaseStats, TableRowCount};

use super::PgDb;

impl PgDb {
    /// Export all settings.
    pub async fn export_all_settings(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT key, value, updated_at FROM settings ORDER BY key",
                &[],
            )
            .await
            .map_err(|e| format!("PG export_all_settings: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let value_str: String = r.get(1);
                let value: serde_json::Value =
                    serde_json::from_str(&value_str).unwrap_or(serde_json::Value::Null);
                let updated: chrono::DateTime<chrono::Utc> = r.get(2);
                serde_json::json!({
                    "key": r.get::<_, String>(0),
                    "value": value,
                    "updated_at": updated.to_rfc3339(),
                })
            })
            .collect())
    }

    /// Export all unified workflows.
    pub async fn export_all_unified_workflows(&self) -> Result<Vec<serde_json::Value>, String> {
        let workflows = self.list_unified_workflows().await?;
        Ok(workflows
            .into_iter()
            .map(|w| serde_json::to_value(w).unwrap_or_default())
            .collect())
    }

    /// Export all saved API requests.
    pub async fn export_all_saved_api_requests(&self) -> Result<Vec<serde_json::Value>, String> {
        self.list_saved_api_requests().await
    }

    /// Export all verification tests.
    pub async fn export_all_verification_tests(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT id, name, description, workflow_id, test_type, command,
                          expected_exit_code, expected_output, timeout_seconds, enabled,
                          tags, created_at, updated_at
                   FROM verification_tests ORDER BY updated_at DESC"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG export_all_verification_tests: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let tags_str: Option<String> = r.get(10);
                let tags: serde_json::Value = tags_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!([]));
                let created: chrono::DateTime<chrono::Utc> = r.get(11);
                let updated: chrono::DateTime<chrono::Utc> = r.get(12);
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "name": r.get::<_, String>(1),
                    "description": r.get::<_, Option<String>>(2),
                    "workflow_id": r.get::<_, Option<String>>(3),
                    "test_type": r.get::<_, String>(4),
                    "command": r.get::<_, Option<String>>(5),
                    "expected_exit_code": r.get::<_, Option<i32>>(6),
                    "expected_output": r.get::<_, Option<String>>(7),
                    "timeout_seconds": r.get::<_, Option<i32>>(8),
                    "enabled": r.get::<_, bool>(9),
                    "tags": tags,
                    "created_at": created.to_rfc3339(),
                    "updated_at": updated.to_rfc3339(),
                })
            })
            .collect())
    }

    /// Export all task hooks.
    pub async fn export_all_task_hooks(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT id, name, hook_type, trigger_event, command, working_directory,
                          timeout_seconds, enabled, workflow_filter, created_at, updated_at
                   FROM task_hooks ORDER BY updated_at DESC"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG export_all_task_hooks: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let created: chrono::DateTime<chrono::Utc> = r.get(9);
                let updated: chrono::DateTime<chrono::Utc> = r.get(10);
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "name": r.get::<_, String>(1),
                    "hook_type": r.get::<_, String>(2),
                    "trigger_event": r.get::<_, String>(3),
                    "command": r.get::<_, String>(4),
                    "working_directory": r.get::<_, Option<String>>(5),
                    "timeout_seconds": r.get::<_, Option<i32>>(6),
                    "enabled": r.get::<_, bool>(7),
                    "workflow_filter": r.get::<_, Option<String>>(8),
                    "created_at": created.to_rfc3339(),
                    "updated_at": updated.to_rfc3339(),
                })
            })
            .collect())
    }

    /// Export all prompts.
    pub async fn export_all_prompts(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT id, name, category, content, variables, created_at, updated_at
                   FROM prompts ORDER BY updated_at DESC"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG export_all_prompts: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let vars_str: String = r.get(4);
                let variables: serde_json::Value =
                    serde_json::from_str(&vars_str).unwrap_or(serde_json::json!([]));
                let created: chrono::DateTime<chrono::Utc> = r.get(5);
                let updated: chrono::DateTime<chrono::Utc> = r.get(6);
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "name": r.get::<_, String>(1),
                    "category": r.get::<_, String>(2),
                    "content": r.get::<_, String>(3),
                    "variables": variables,
                    "created_at": created.to_rfc3339(),
                    "updated_at": updated.to_rfc3339(),
                })
            })
            .collect())
    }

    /// Export all configs.
    pub async fn export_all_configs(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT id, name, config_json, source_type, source_path, created_at, updated_at
                   FROM configs ORDER BY updated_at DESC"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG export_all_configs: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let config_str: String = r.get(2);
                let config: serde_json::Value =
                    serde_json::from_str(&config_str).unwrap_or(serde_json::json!({}));
                let created: chrono::DateTime<chrono::Utc> = r.get(5);
                let updated: chrono::DateTime<chrono::Utc> = r.get(6);
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "name": r.get::<_, String>(1),
                    "config": config,
                    "source_type": r.get::<_, String>(3),
                    "source_path": r.get::<_, Option<String>>(4),
                    "created_at": created.to_rfc3339(),
                    "updated_at": updated.to_rfc3339(),
                })
            })
            .collect())
    }

    /// Export all scheduled tasks.
    pub async fn export_all_scheduled_tasks(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT id, name, schedule, workflow_id, workflow_name, enabled,
                          last_run_at, next_run_at, created_at, updated_at
                   FROM scheduled_tasks ORDER BY updated_at DESC"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG export_all_scheduled_tasks: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let last_run: Option<chrono::DateTime<chrono::Utc>> = r.get(6);
                let next_run: Option<chrono::DateTime<chrono::Utc>> = r.get(7);
                let created: chrono::DateTime<chrono::Utc> = r.get(8);
                let updated: chrono::DateTime<chrono::Utc> = r.get(9);
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "name": r.get::<_, String>(1),
                    "schedule": r.get::<_, String>(2),
                    "workflow_id": r.get::<_, Option<String>>(3),
                    "workflow_name": r.get::<_, Option<String>>(4),
                    "enabled": r.get::<_, bool>(5),
                    "last_run_at": last_run.map(|dt| dt.to_rfc3339()),
                    "next_run_at": next_run.map(|dt| dt.to_rfc3339()),
                    "created_at": created.to_rfc3339(),
                    "updated_at": updated.to_rfc3339(),
                })
            })
            .collect())
    }

    /// Export all orchestrator checkpoints.
    pub async fn export_all_orchestrator_checkpoints(
        &self,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT id, task_id, iteration, trigger, state, name, created_at
                   FROM orchestrator_checkpoints ORDER BY created_at DESC"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG export_all_orchestrator_checkpoints: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let state_str: String = r.get(4);
                let state: serde_json::Value =
                    serde_json::from_str(&state_str).unwrap_or(serde_json::json!({}));
                let created: chrono::DateTime<chrono::Utc> = r.get(6);
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "task_id": r.get::<_, String>(1),
                    "iteration": r.get::<_, i32>(2),
                    "trigger": r.get::<_, String>(3),
                    "state": state,
                    "name": r.get::<_, Option<String>>(5),
                    "created_at": created.to_rfc3339(),
                })
            })
            .collect())
    }

    /// Export all flows (orchestrator_flows).
    pub async fn export_all_flows(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT id, name, description, definition_json, tags, version, created_at, updated_at
                   FROM orchestrator_flows ORDER BY updated_at DESC"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG export_all_flows: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let tags_str: String = r.get(4);
                let tags: serde_json::Value =
                    serde_json::from_str(&tags_str).unwrap_or(serde_json::json!([]));
                let def_str: String = r.get(3);
                let definition: serde_json::Value =
                    serde_json::from_str(&def_str).unwrap_or(serde_json::json!({}));
                let created: chrono::DateTime<chrono::Utc> = r.get(6);
                let updated: chrono::DateTime<chrono::Utc> = r.get(7);
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "name": r.get::<_, String>(1),
                    "description": r.get::<_, Option<String>>(2),
                    "definition": definition,
                    "tags": tags,
                    "version": r.get::<_, String>(5),
                    "created_at": created.to_rfc3339(),
                    "updated_at": updated.to_rfc3339(),
                })
            })
            .collect())
    }

    /// Get a summary of all exportable data counts.
    pub async fn get_export_summary(&self) -> Result<serde_json::Value, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_one(
                r#"SELECT
                    (SELECT COUNT(*) FROM settings) as settings_count,
                    (SELECT COUNT(*) FROM unified_workflows) as unified_workflows_count,
                    (SELECT COUNT(*) FROM verification_tests) as verification_tests_count,
                    (SELECT COUNT(*) FROM learning_outcomes) as learning_outcomes_count,
                    (SELECT COUNT(*) FROM learning_patterns) as learning_patterns_count,
                    (SELECT COUNT(*) FROM orchestrator_flows) as flows_count,
                    (SELECT COUNT(*) FROM flow_executions) as flow_executions_count,
                    (SELECT COUNT(*) FROM orchestrator_checkpoints) as checkpoints_count,
                    (SELECT COUNT(*) FROM prompts) as prompts_count,
                    (SELECT COUNT(*) FROM ai_workflows) as ai_workflows_count,
                    (SELECT COUNT(*) FROM task_hooks) as task_hooks_count,
                    (SELECT COUNT(*) FROM scheduled_tasks) as scheduled_tasks_count,
                    (SELECT COUNT(*) FROM saved_api_requests) as saved_api_requests_count,
                    (SELECT COUNT(*) FROM configs) as configs_count
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_export_summary: {}", e))?;

        Ok(serde_json::json!({
            "settings": row.get::<_, i64>(0),
            "unified_workflows": row.get::<_, i64>(1),
            "verification_tests": row.get::<_, i64>(2),
            "learning_outcomes": row.get::<_, i64>(3),
            "learning_patterns": row.get::<_, i64>(4),
            "flows": row.get::<_, i64>(5),
            "flow_executions": row.get::<_, i64>(6),
            "checkpoints": row.get::<_, i64>(7),
            "prompts": row.get::<_, i64>(8),
            "ai_workflows": row.get::<_, i64>(9),
            "task_hooks": row.get::<_, i64>(10),
            "scheduled_tasks": row.get::<_, i64>(11),
            "saved_api_requests": row.get::<_, i64>(12),
            "configs": row.get::<_, i64>(13),
        }))
    }

    /// Get database statistics from PostgreSQL.
    pub async fn get_database_stats(&self) -> Result<DatabaseStats, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Get table sizes using pg_stat_user_tables
        let rows = conn
            .query(
                "SELECT relname, n_live_tup FROM pg_stat_user_tables WHERE schemaname = 'runner' ORDER BY n_live_tup DESC",
                &[],
            )
            .await
            .map_err(|e| format!("PG get_database_stats: {}", e))?;

        let table_counts: Vec<TableRowCount> = rows
            .iter()
            .map(|r| TableRowCount {
                table_name: r.get(0),
                row_count: r.get(1),
            })
            .collect();

        // Get total database size
        let size_row = conn
            .query_one("SELECT pg_database_size(current_database())", &[])
            .await
            .map_err(|e| format!("PG database size: {}", e))?;
        let total_size: i64 = size_row.get(0);

        Ok(DatabaseStats {
            total_size_bytes: total_size,
            page_count: 0,     // Not applicable for PG
            page_size: 8192,   // PG default page size
            freelist_count: 0, // Not applicable for PG
            wal_pages: 0,      // Not directly queryable the same way
            wal_frames: 0,
            table_counts,
        })
    }

    /// Run EXPLAIN on a query for debugging.
    /// Returns the query plan as a formatted string.
    /// Only allows SELECT queries to prevent arbitrary SQL execution.
    pub async fn explain_query_plan(&self, query: &str) -> Result<String, String> {
        let trimmed = query.trim();
        if !trimmed.to_uppercase().starts_with("SELECT") {
            return Err("Only SELECT queries are allowed for EXPLAIN".to_string());
        }
        if trimmed.contains(';') {
            return Err("Multiple statements are not allowed".to_string());
        }

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let explain_query = format!("EXPLAIN {}", trimmed);
        let rows = conn
            .query(&explain_query, &[])
            .await
            .map_err(|e| format!("PG EXPLAIN failed: {}", e))?;

        let plan_lines: Vec<String> = rows.iter().map(|r| r.get::<_, String>(0)).collect();

        Ok(plan_lines.join("\n"))
    }

    /// Export all flow executions.
    pub async fn export_all_flow_executions(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT instance_id, flow_id, current_step, context_json, status, error,
                          step_results_json, started_at, completed_at
                   FROM flow_executions ORDER BY started_at DESC"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG export_all_flow_executions: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let context_str: Option<String> = r.get(3);
                let context: serde_json::Value = context_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!({}));
                let step_results_str: Option<String> = r.get(6);
                let step_results: serde_json::Value = step_results_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!([]));
                let started: chrono::DateTime<chrono::Utc> = r.get(7);
                let completed: Option<chrono::DateTime<chrono::Utc>> = r.get(8);
                serde_json::json!({
                    "instance_id": r.get::<_, String>(0),
                    "flow_id": r.get::<_, String>(1),
                    "current_step": r.get::<_, Option<String>>(2),
                    "context": context,
                    "status": r.get::<_, String>(4),
                    "error": r.get::<_, Option<String>>(5),
                    "step_results": step_results,
                    "started_at": started.to_rfc3339(),
                    "completed_at": completed.map(|dt| dt.to_rfc3339()),
                })
            })
            .collect())
    }
}
