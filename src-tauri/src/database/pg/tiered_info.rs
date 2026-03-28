//! PostgreSQL tiered information queries.
//!
//! Provides PG-backed implementations for tiered info panel commands
//! including config statistics, recent runs, failed runs, flakiness data,
//! and AI session history.

use super::PgDb;
use serde_json::json;

impl PgDb {
    /// Get recent runs, optionally filtered by config_id.
    pub async fn get_recent_runs(
        &self,
        config_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let limit_i64 = limit as i64;

        let rows = if let Some(cid) = config_id {
            conn.query(
                r#"
                SELECT tra.id, tr.config_id, tra.workflow_name, tra.started_at::TEXT,
                       tra.ended_at::TEXT, tra.duration_ms, tra.automation_status,
                       tra.success, tra.error_type, tra.error_message,
                       tra.actions_summary, tra.states_visited, tra.transitions_executed,
                       tra.template_matches, tra.anomalies
                FROM task_run_automation tra
                INNER JOIN task_runs tr ON tra.task_run_id = tr.id
                WHERE tr.config_id = $1
                ORDER BY tra.started_at DESC
                LIMIT $2
                "#,
                &[&cid, &limit_i64],
            )
            .await
            .map_err(|e| format!("PG get_recent_runs: {}", e))?
        } else {
            conn.query(
                r#"
                SELECT tra.id, tr.config_id, tra.workflow_name, tra.started_at::TEXT,
                       tra.ended_at::TEXT, tra.duration_ms, tra.automation_status,
                       tra.success, tra.error_type, tra.error_message,
                       tra.actions_summary, tra.states_visited, tra.transitions_executed,
                       tra.template_matches, tra.anomalies
                FROM task_run_automation tra
                INNER JOIN task_runs tr ON tra.task_run_id = tr.id
                ORDER BY tra.started_at DESC
                LIMIT $1
                "#,
                &[&limit_i64],
            )
            .await
            .map_err(|e| format!("PG get_recent_runs: {}", e))?
        };

        Ok(rows
            .iter()
            .map(|row| {
                json!({
                    "id": row.get::<_, String>(0),
                    "config_id": row.get::<_, Option<String>>(1),
                    "workflow_name": row.get::<_, Option<String>>(2),
                    "started_at": row.get::<_, Option<String>>(3),
                    "ended_at": row.get::<_, Option<String>>(4),
                    "duration_ms": row.get::<_, Option<i64>>(5),
                    "automation_status": row.get::<_, String>(6),
                    "success": row.get::<_, Option<bool>>(7),
                    "error_type": row.get::<_, Option<String>>(8),
                    "error_message": row.get::<_, Option<String>>(9),
                })
            })
            .collect())
    }

    /// Get AI session history (task runs), optionally filtered by config_id.
    pub async fn get_ai_session_history(
        &self,
        config_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let limit_i64 = limit as i64;

        let rows = if let Some(cid) = config_id {
            conn.query(
                r#"
                SELECT id, task_name, created_at::TEXT, completed_at::TEXT, status,
                       sessions_count, task_type
                FROM task_runs
                WHERE config_id = $1
                ORDER BY created_at DESC
                LIMIT $2
                "#,
                &[&cid, &limit_i64],
            )
            .await
            .map_err(|e| format!("PG get_ai_session_history: {}", e))?
        } else {
            conn.query(
                r#"
                SELECT id, task_name, created_at::TEXT, completed_at::TEXT, status,
                       sessions_count, task_type
                FROM task_runs
                ORDER BY created_at DESC
                LIMIT $1
                "#,
                &[&limit_i64],
            )
            .await
            .map_err(|e| format!("PG get_ai_session_history: {}", e))?
        };

        Ok(rows
            .iter()
            .map(|row| {
                json!({
                    "id": row.get::<_, String>(0),
                    "task_name": row.get::<_, String>(1),
                    "created_at": row.get::<_, String>(2),
                    "completed_at": row.get::<_, Option<String>>(3),
                    "status": row.get::<_, String>(4),
                    "sessions_count": row.get::<_, i32>(5),
                    "task_type": row.get::<_, Option<String>>(6),
                })
            })
            .collect())
    }

    /// Cleanup old automation records for a config, keeping the most recent N.
    pub async fn cleanup_old_runs(
        &self,
        config_id: &str,
        keep_count: u32,
    ) -> Result<u32, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let keep_i64 = keep_count as i64;

        let count = conn
            .execute(
                r#"
                DELETE FROM task_run_automation
                WHERE id IN (
                    SELECT tra.id FROM task_run_automation tra
                    INNER JOIN task_runs tr ON tra.task_run_id = tr.id
                    WHERE tr.config_id = $1
                    ORDER BY tra.started_at DESC
                    OFFSET $2
                )
                "#,
                &[&config_id, &keep_i64],
            )
            .await
            .map_err(|e| format!("PG cleanup_old_runs: {}", e))?;

        Ok(count as u32)
    }

    /// Get recent task runs (for list_recent_task_runs in testing.rs).
    pub async fn list_recent_task_runs_pg(
        &self,
        limit: i32,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let limit_i64 = limit as i64;

        let rows = conn
            .query(
                r#"
                SELECT
                    tr.id,
                    tr.task_name,
                    tr.workflow_name,
                    tr.status,
                    tr.created_at::TEXT,
                    tr.completed_at::TEXT,
                    tr.goal_achieved,
                    CASE
                        WHEN tr.completed_at IS NOT NULL
                        THEN EXTRACT(EPOCH FROM (tr.completed_at - tr.created_at))::BIGINT * 1000
                        ELSE NULL
                    END as duration_ms
                FROM task_runs tr
                ORDER BY tr.created_at DESC
                LIMIT $1
                "#,
                &[&limit_i64],
            )
            .await
            .map_err(|e| format!("PG list_recent_task_runs: {}", e))?;

        Ok(rows
            .iter()
            .map(|row| {
                json!({
                    "id": row.get::<_, String>(0),
                    "task_name": row.get::<_, String>(1),
                    "workflow_name": row.get::<_, Option<String>>(2),
                    "status": row.get::<_, String>(3),
                    "created_at": row.get::<_, String>(4),
                    "completed_at": row.get::<_, Option<String>>(5),
                    "goal_achieved": row.get::<_, Option<bool>>(6),
                    "duration_ms": row.get::<_, Option<i64>>(7),
                })
            })
            .collect())
    }

    /// Get workflow run context for AI test generation (for get_workflow_run_context in testing.rs).
    pub async fn get_workflow_run_context_pg(
        &self,
        task_run_id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Get task run
        let task_run = conn
            .query_opt(
                r#"
                SELECT id, task_name, prompt, status, workflow_name,
                       summary, goal_achieved, remaining_work,
                       created_at::TEXT, completed_at::TEXT
                FROM task_runs WHERE id = $1
                "#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_workflow_run_context: {}", e))?;

        let task_run = match task_run {
            Some(row) => json!({
                "id": row.get::<_, String>(0),
                "task_name": row.get::<_, String>(1),
                "prompt": row.get::<_, Option<String>>(2),
                "status": row.get::<_, String>(3),
                "workflow_name": row.get::<_, Option<String>>(4),
                "summary": row.get::<_, Option<String>>(5),
                "goal_achieved": row.get::<_, Option<bool>>(6),
                "remaining_work": row.get::<_, Option<String>>(7),
                "created_at": row.get::<_, String>(8),
                "completed_at": row.get::<_, Option<String>>(9),
            }),
            None => return Ok(None),
        };

        // Get automation
        let automation = conn
            .query_opt(
                r#"
                SELECT workflow_name, automation_status, duration_ms,
                       actions_summary, states_visited, transitions_executed
                FROM task_run_automation WHERE task_run_id = $1
                ORDER BY started_at DESC LIMIT 1
                "#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_workflow_run_context automation: {}", e))?
            .map(|row| {
                json!({
                    "workflow_name": row.get::<_, Option<String>>(0),
                    "automation_status": row.get::<_, String>(1),
                    "duration_ms": row.get::<_, Option<i64>>(2),
                    "actions_summary": row.get::<_, Option<String>>(3).and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "states_visited": row.get::<_, Option<String>>(4).and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "transitions_executed": row.get::<_, Option<String>>(5).and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                })
            });

        // Get events
        let events: Vec<serde_json::Value> = conn
            .query(
                r#"
                SELECT id, event_type, event_subtype, data, duration_ms, timestamp::TEXT
                FROM task_run_events WHERE task_run_id = $1
                ORDER BY timestamp DESC LIMIT 50
                "#,
                &[&task_run_id],
            )
            .await
            .unwrap_or_default()
            .iter()
            .map(|row| {
                json!({
                    "id": row.get::<_, String>(0),
                    "event_type": row.get::<_, String>(1),
                    "event_subtype": row.get::<_, Option<String>>(2),
                    "data": row.get::<_, Option<String>>(3).and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "duration_ms": row.get::<_, Option<i64>>(4),
                    "timestamp": row.get::<_, String>(5),
                })
            })
            .collect();

        // Get findings
        let findings: Vec<serde_json::Value> = conn
            .query(
                r#"
                SELECT id, finding_type, title, description, severity, created_at::TEXT
                FROM task_run_findings WHERE task_run_id = $1
                ORDER BY created_at DESC LIMIT 20
                "#,
                &[&task_run_id],
            )
            .await
            .unwrap_or_default()
            .iter()
            .map(|row| {
                json!({
                    "id": row.get::<_, i64>(0),
                    "finding_type": row.get::<_, String>(1),
                    "title": row.get::<_, String>(2),
                    "description": row.get::<_, Option<String>>(3),
                    "severity": row.get::<_, String>(4),
                    "created_at": row.get::<_, String>(5),
                })
            })
            .collect();

        Ok(Some(json!({
            "task_run": task_run,
            "automation": automation,
            "events": events,
            "findings": findings,
        })))
    }

    /// Get comparison runs list.
    pub async fn list_comparisons(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, workflow_id, variation_type, status, entries_json,
                       report, created_at::TEXT, completed_at::TEXT
                FROM comparison_runs
                ORDER BY created_at DESC
                LIMIT 50
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("PG list_comparisons: {}", e))?;

        Ok(rows
            .iter()
            .map(|row| {
                let entries_str: String = row.get(4);
                let entries: serde_json::Value =
                    serde_json::from_str(&entries_str).unwrap_or_default();
                json!({
                    "id": row.get::<_, String>(0),
                    "workflow_id": row.get::<_, String>(1),
                    "variation_type": row.get::<_, String>(2),
                    "status": row.get::<_, String>(3),
                    "entries": entries,
                    "report": row.get::<_, Option<String>>(5),
                    "created_at": row.get::<_, String>(6),
                    "completed_at": row.get::<_, Option<String>>(7),
                })
            })
            .collect())
    }

    /// Insert a comparison run.
    pub async fn insert_comparison(
        &self,
        id: &str,
        workflow_id: &str,
        variation_type: &str,
        entries_json: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO comparison_runs (id, workflow_id, variation_type, status, entries_json, created_at)
            VALUES ($1, $2, $3, 'running', $4, NOW())
            "#,
            &[&id, &workflow_id, &variation_type, &entries_json],
        )
        .await
        .map_err(|e| format!("PG insert_comparison: {}", e))?;

        Ok(())
    }

    /// Update comparison run entries and status.
    pub async fn update_comparison(
        &self,
        id: &str,
        entries_json: &str,
        status: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            "UPDATE comparison_runs SET entries_json = $1, status = $2 WHERE id = $3",
            &[&entries_json, &status, &id],
        )
        .await
        .map_err(|e| format!("PG update_comparison: {}", e))?;

        Ok(())
    }

    /// Get a comparison run by ID.
    pub async fn get_comparison(
        &self,
        id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT id, workflow_id, variation_type, status, entries_json,
                       report, created_at::TEXT, completed_at::TEXT
                FROM comparison_runs WHERE id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_comparison: {}", e))?;

        Ok(row.map(|r| {
            json!({
                "id": r.get::<_, String>(0),
                "workflow_id": r.get::<_, String>(1),
                "variation_type": r.get::<_, String>(2),
                "status": r.get::<_, String>(3),
                "entries_json": r.get::<_, String>(4),
                "report": r.get::<_, Option<String>>(5),
                "created_at": r.get::<_, String>(6),
                "completed_at": r.get::<_, Option<String>>(7),
            })
        }))
    }

    /// Complete a comparison run.
    pub async fn complete_comparison(
        &self,
        id: &str,
        entries_json: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            r#"
            UPDATE comparison_runs SET
                status = 'completed',
                completed_at = NOW(),
                entries_json = $1
            WHERE id = $2
            "#,
            &[&entries_json, &id],
        )
        .await
        .map_err(|e| format!("PG complete_comparison: {}", e))?;

        Ok(())
    }

    /// List shell commands with optional filters.
    pub async fn list_shell_commands_filtered(
        &self,
        enabled_only: bool,
        category: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let mut conditions: Vec<String> = vec!["1=1".to_string()];
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        let mut param_idx = 1u32;

        if enabled_only {
            conditions.push("enabled = true".to_string());
        }

        if let Some(cat) = category {
            conditions.push(format!("category = ${}", param_idx));
            params.push(Box::new(cat.to_string()));
            param_idx += 1;
        }

        let _ = param_idx; // suppress unused warning

        let sql = format!(
            r#"
            SELECT id, name, description, command, working_directory,
                   timeout_seconds, fail_on_error, category, tags,
                   enabled, created_at::TEXT, updated_at::TEXT
            FROM shell_commands
            WHERE {}
            ORDER BY name ASC
            "#,
            conditions.join(" AND ")
        );

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();

        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG list_shell_commands: {}", e))?;

        Ok(rows
            .iter()
            .map(|row| {
                let tags_str: String = row.get(8);
                let tags: serde_json::Value =
                    serde_json::from_str(&tags_str).unwrap_or(json!([]));
                json!({
                    "id": row.get::<_, String>(0),
                    "name": row.get::<_, String>(1),
                    "description": row.get::<_, Option<String>>(2),
                    "command": row.get::<_, String>(3),
                    "working_directory": row.get::<_, Option<String>>(4),
                    "timeout_seconds": row.get::<_, i32>(5),
                    "fail_on_error": row.get::<_, bool>(6),
                    "category": row.get::<_, Option<String>>(7),
                    "tags": tags,
                    "enabled": row.get::<_, bool>(9),
                    "created_at": row.get::<_, String>(10),
                    "updated_at": row.get::<_, String>(11),
                })
            })
            .collect())
    }

    /// Get all distinct shell command categories.
    pub async fn get_shell_command_categories(&self) -> Result<Vec<String>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT DISTINCT category
                FROM shell_commands
                WHERE category IS NOT NULL AND category != ''
                ORDER BY category ASC
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_shell_command_categories: {}", e))?;

        Ok(rows.iter().map(|row| row.get::<_, String>(0)).collect())
    }

    /// Update a shell command.
    pub async fn update_shell_command_full(
        &self,
        id: &str,
        name: &str,
        description: Option<&str>,
        command: &str,
        working_directory: Option<&str>,
        timeout_seconds: i32,
        fail_on_error: bool,
        category: Option<&str>,
        tags: &str,
        enabled: bool,
    ) -> Result<bool, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let count = conn
            .execute(
                r#"
                UPDATE shell_commands SET
                    name = $1,
                    description = $2,
                    command = $3,
                    working_directory = $4,
                    timeout_seconds = $5,
                    fail_on_error = $6,
                    category = $7,
                    tags = $8,
                    enabled = $9,
                    updated_at = NOW()
                WHERE id = $10
                "#,
                &[
                    &name,
                    &description,
                    &command,
                    &working_directory,
                    &timeout_seconds,
                    &fail_on_error,
                    &category,
                    &tags,
                    &enabled,
                    &id,
                ],
            )
            .await
            .map_err(|e| format!("PG update_shell_command: {}", e))?;

        Ok(count > 0)
    }

    /// Set shell command enabled status.
    pub async fn set_shell_command_enabled(&self, id: &str, enabled: bool) -> Result<bool, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let count = conn
            .execute(
                "UPDATE shell_commands SET enabled = $1, updated_at = NOW() WHERE id = $2",
                &[&enabled, &id],
            )
            .await
            .map_err(|e| format!("PG set_shell_command_enabled: {}", e))?;

        Ok(count > 0)
    }

    /// Get prompt variant content by ID.
    pub async fn get_prompt_variant_content(&self, variant_id: &str) -> Result<Option<String>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                "SELECT prompt_content FROM prompt_registry WHERE id = $1",
                &[&variant_id],
            )
            .await
            .map_err(|e| format!("PG get_prompt_variant_content: {}", e))?;

        Ok(row.map(|r| r.get::<_, String>(0)))
    }

    /// Get pending discoveries.
    pub async fn get_pending_discoveries(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, payload, attempt_count, error, created_at::TEXT, updated_at::TEXT
                FROM discovery_queue
                WHERE status = 'pending'
                ORDER BY created_at ASC
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_pending_discoveries: {}", e))?;

        Ok(rows
            .iter()
            .map(|row| {
                json!({
                    "id": row.get::<_, String>(0),
                    "payload": row.get::<_, String>(1),
                    "attempt_count": row.get::<_, i32>(2),
                    "error": row.get::<_, Option<String>>(3),
                    "created_at": row.get::<_, String>(4),
                    "updated_at": row.get::<_, String>(5),
                })
            })
            .collect())
    }
}
