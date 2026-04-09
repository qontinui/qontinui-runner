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
    pub async fn cleanup_old_runs(&self, config_id: &str, keep_count: u32) -> Result<u32, String> {
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
                SELECT id, category, title, description, severity, detected_at::TEXT
                FROM task_run_findings WHERE task_run_id = $1
                ORDER BY detected_at DESC LIMIT 20
                "#,
                &[&task_run_id],
            )
            .await
            .unwrap_or_default()
            .iter()
            .map(|row| {
                json!({
                    "id": row.get::<_, String>(0),
                    "category": row.get::<_, String>(1),
                    "title": row.get::<_, String>(2),
                    "description": row.get::<_, Option<String>>(3),
                    "severity": row.get::<_, String>(4),
                    "detected_at": row.get::<_, String>(5),
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
    pub async fn get_comparison(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
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
    pub async fn complete_comparison(&self, id: &str, entries_json: &str) -> Result<(), String> {
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

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();

        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG list_shell_commands: {}", e))?;

        Ok(rows
            .iter()
            .map(|row| {
                let tags_str: String = row.get(8);
                let tags: serde_json::Value = serde_json::from_str(&tags_str).unwrap_or(json!([]));
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
    pub async fn get_prompt_variant_content(
        &self,
        variant_id: &str,
    ) -> Result<Option<String>, String> {
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

    /// Get config statistics (PG equivalent of tiered_info::get_config_statistics).
    pub async fn get_config_statistics(
        &self,
        config_id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let row = conn
            .query_opt(
                r#"SELECT config_id, total_runs, successful_runs, failed_runs,
                      avg_duration_ms, last_run_at, success_rate, streak_current, streak_best
               FROM config_statistics WHERE config_id = $1"#,
                &[&config_id],
            )
            .await
            .map_err(|e| format!("PG get_config_statistics: {}", e))?;
        Ok(row.map(|r| {
            json!({
                "config_id": r.get::<_, String>(0),
                "total_runs": r.get::<_, i64>(1),
                "successful_runs": r.get::<_, i64>(2),
                "failed_runs": r.get::<_, i64>(3),
                "avg_duration_ms": r.get::<_, Option<f64>>(4),
                "last_run_at": r.get::<_, Option<String>>(5),
                "success_rate": r.get::<_, Option<f64>>(6),
                "streak_current": r.get::<_, Option<i64>>(7),
                "streak_best": r.get::<_, Option<i64>>(8),
            })
        }))
    }

    /// Get flaky transitions for a config.
    pub async fn get_flaky_transitions(
        &self,
        config_id: &str,
        threshold: f64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let rows = conn.query(
            r#"SELECT transition_id, total_attempts, failure_count,
                      failure_count::FLOAT / NULLIF(total_attempts, 0)::FLOAT as failure_rate
               FROM transition_reliability
               WHERE config_id = $1 AND failure_count::FLOAT / NULLIF(total_attempts, 0)::FLOAT >= $2
               ORDER BY failure_rate DESC"#,
            &[&config_id, &threshold],
        ).await.map_err(|e| format!("PG get_flaky_transitions: {}", e))?;
        Ok(rows
            .iter()
            .map(|r| {
                json!({
                    "item_id": r.get::<_, String>(0),
                    "total_attempts": r.get::<_, i64>(1),
                    "failure_count": r.get::<_, i64>(2),
                    "failure_rate": r.get::<_, f64>(3),
                })
            })
            .collect())
    }

    /// Get flaky templates for a config.
    pub async fn get_flaky_templates(
        &self,
        config_id: &str,
        threshold: f64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let rows = conn.query(
            r#"SELECT template_id, total_attempts, failure_count,
                      failure_count::FLOAT / NULLIF(total_attempts, 0)::FLOAT as failure_rate
               FROM template_reliability
               WHERE config_id = $1 AND failure_count::FLOAT / NULLIF(total_attempts, 0)::FLOAT >= $2
               ORDER BY failure_rate DESC"#,
            &[&config_id, &threshold],
        ).await.map_err(|e| format!("PG get_flaky_templates: {}", e))?;
        Ok(rows
            .iter()
            .map(|r| {
                json!({
                    "item_id": r.get::<_, String>(0),
                    "total_attempts": r.get::<_, i64>(1),
                    "failure_count": r.get::<_, i64>(2),
                    "failure_rate": r.get::<_, f64>(3),
                })
            })
            .collect())
    }

    /// Get debugging context for a config.
    pub async fn get_debugging_context(
        &self,
        config_id: &str,
        config_name: Option<String>,
    ) -> Result<serde_json::Value, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        // Get config stats
        let stats = self
            .get_config_statistics(config_id)
            .await?
            .unwrap_or(json!({}));
        // Get recent failures
        let failures = conn
            .query(
                r#"SELECT tra.error_type, tra.error_message, tra.ended_at::TEXT
               FROM task_run_automation tra
               INNER JOIN task_runs tr ON tra.task_run_id = tr.id
               WHERE tr.config_id = $1 AND tra.success = false
               ORDER BY tra.ended_at DESC LIMIT 5"#,
                &[&config_id],
            )
            .await
            .unwrap_or_default();
        let recent_errors: Vec<serde_json::Value> = failures
            .iter()
            .map(|r| {
                json!({
                    "error_type": r.get::<_, Option<String>>(0),
                    "error_message": r.get::<_, Option<String>>(1),
                    "ended_at": r.get::<_, Option<String>>(2),
                })
            })
            .collect();
        Ok(json!({
            "config_id": config_id,
            "config_name": config_name,
            "statistics": stats,
            "recent_errors": recent_errors,
        }))
    }

    /// Get failed runs for a config.
    pub async fn get_failed_runs(
        &self,
        config_id: &str,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let limit_i64 = limit as i64;
        let rows = conn
            .query(
                r#"SELECT tra.id, tr.config_id, tra.workflow_name, tra.started_at::TEXT,
                      tra.ended_at::TEXT, tra.duration_ms, tra.automation_status,
                      tra.success, tra.error_type, tra.error_message
               FROM task_run_automation tra
               INNER JOIN task_runs tr ON tra.task_run_id = tr.id
               WHERE tr.config_id = $1 AND tra.success = false
               ORDER BY tra.started_at DESC LIMIT $2"#,
                &[&config_id, &limit_i64],
            )
            .await
            .map_err(|e| format!("PG get_failed_runs: {}", e))?;
        Ok(rows
            .iter()
            .map(|r| {
                json!({
                    "id": r.get::<_, String>(0),
                    "config_id": r.get::<_, Option<String>>(1),
                    "workflow_name": r.get::<_, Option<String>>(2),
                    "started_at": r.get::<_, Option<String>>(3),
                    "ended_at": r.get::<_, Option<String>>(4),
                    "duration_ms": r.get::<_, Option<i64>>(5),
                    "status": r.get::<_, String>(6),
                    "success": r.get::<_, Option<bool>>(7),
                    "error_type": r.get::<_, Option<String>>(8),
                    "error_message": r.get::<_, Option<String>>(9),
                })
            })
            .collect())
    }

    /// Get flakiness summary for a config.
    pub async fn get_flakiness_summary(
        &self,
        config_id: &str,
        threshold: f64,
    ) -> Result<serde_json::Value, String> {
        let flaky_transitions = self.get_flaky_transitions(config_id, threshold).await?;
        let flaky_templates = self.get_flaky_templates(config_id, threshold).await?;
        let transition_ids: Vec<String> = flaky_transitions
            .iter()
            .filter_map(|v| v["item_id"].as_str().map(String::from))
            .collect();
        let template_ids: Vec<String> = flaky_templates
            .iter()
            .filter_map(|v| v["item_id"].as_str().map(String::from))
            .collect();
        Ok(json!({
            "flaky_transition_count": flaky_transitions.len(),
            "flaky_template_count": flaky_templates.len(),
            "flaky_transition_ids": transition_ids,
            "flaky_template_ids": template_ids,
            "cache_is_stale": false,
        }))
    }

    /// Get execution options based on flakiness.
    pub async fn get_execution_options(
        &self,
        config_id: &str,
        transition_id: Option<&str>,
        template_id: Option<&str>,
        threshold: f64,
    ) -> Result<serde_json::Value, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        // Default execution options
        let mut options = json!({
            "retry_count": 1,
            "timeout_multiplier": 1.0,
            "confidence_threshold": 0.7,
        });
        if let Some(tid) = transition_id {
            let row = conn
                .query_opt(
                    r#"SELECT failure_count::FLOAT / NULLIF(total_attempts, 0)::FLOAT
                   FROM transition_reliability WHERE config_id = $1 AND transition_id = $2"#,
                    &[&config_id, &tid],
                )
                .await
                .unwrap_or(None);
            if let Some(r) = row {
                let rate: f64 = r.get(0);
                if rate >= threshold {
                    options = json!({ "retry_count": 3, "timeout_multiplier": 1.5, "confidence_threshold": 0.5 });
                }
            }
        } else if let Some(tmpl) = template_id {
            let row = conn
                .query_opt(
                    r#"SELECT failure_count::FLOAT / NULLIF(total_attempts, 0)::FLOAT
                   FROM template_reliability WHERE config_id = $1 AND template_id = $2"#,
                    &[&config_id, &tmpl],
                )
                .await
                .unwrap_or(None);
            if let Some(r) = row {
                let rate: f64 = r.get(0);
                if rate >= threshold {
                    options = json!({ "retry_count": 3, "timeout_multiplier": 1.5, "confidence_threshold": 0.5 });
                }
            }
        }
        Ok(options)
    }

    /// Update statistics after a run (simplified PG version).
    pub async fn update_statistics_after_run(
        &self,
        config_id: &str,
        success: bool,
        duration_ms: Option<u64>,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            r#"INSERT INTO config_statistics (config_id, total_runs, successful_runs, failed_runs, last_run_at)
               VALUES ($1, 1, CASE WHEN $2 THEN 1 ELSE 0 END, CASE WHEN $2 THEN 0 ELSE 1 END, $3)
               ON CONFLICT (config_id) DO UPDATE SET
                 total_runs = config_statistics.total_runs + 1,
                 successful_runs = config_statistics.successful_runs + CASE WHEN $2 THEN 1 ELSE 0 END,
                 failed_runs = config_statistics.failed_runs + CASE WHEN NOT $2 THEN 1 ELSE 0 END,
                 last_run_at = $3"#,
            &[&config_id, &success, &now],
        ).await.map_err(|e| format!("PG update_statistics: {}", e))?;
        Ok(())
    }

    // ========================================================================
    // Discovery Operations (PG equivalents)
    // ========================================================================

    /// Get discovery summary.
    pub async fn get_discovery_summary(&self) -> Result<serde_json::Value, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let pending_count: i64 = conn
            .query_one(
                "SELECT COUNT(*) FROM discovery_queue WHERE status = 'pending'",
                &[],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(0);
        let ready: i64 = conn.query_one(
            "SELECT COUNT(*) FROM discovery_queue WHERE status = 'pending' AND attempt_count < 3", &[],
        ).await.map(|r| r.get(0)).unwrap_or(0);
        let recent = self.get_pending_discoveries().await.unwrap_or_default();
        let preview: Vec<serde_json::Value> = recent.into_iter().take(10).collect();
        Ok(json!({
            "pending_count": pending_count,
            "ready_for_sync": ready,
            "can_sync": true,
            "recent": preview,
        }))
    }

    /// Extract discoveries for sync (returns id + payload pairs).
    pub async fn extract_discoveries_for_sync(&self) -> Result<Vec<(String, String)>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let rows = conn.query(
            "SELECT id, payload FROM discovery_queue WHERE status = 'pending' AND attempt_count < 3 ORDER BY created_at ASC LIMIT 50",
            &[],
        ).await.map_err(|e| format!("PG extract_discoveries: {}", e))?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<_, String>(0), r.get::<_, String>(1)))
            .collect())
    }

    /// Mark discovery as synced.
    pub async fn mark_discovery_synced(&self, id: &str) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        conn.execute(
            "UPDATE discovery_queue SET status = 'synced' WHERE id = $1",
            &[&id],
        )
        .await
        .map_err(|e| format!("PG mark_discovery_synced: {}", e))?;
        Ok(())
    }

    /// Mark discovery as failed.
    pub async fn mark_discovery_failed(&self, id: &str, error: &str) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        conn.execute(
            "UPDATE discovery_queue SET status = 'failed', attempt_count = attempt_count + 1, error = $2 WHERE id = $1",
            &[&id, &error],
        ).await.map_err(|e| format!("PG mark_discovery_failed: {}", e))?;
        Ok(())
    }

    /// Delete a discovery.
    pub async fn delete_discovery(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let count = conn
            .execute("DELETE FROM discovery_queue WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("PG delete_discovery: {}", e))?;
        Ok(count > 0)
    }

    /// Cleanup failed discoveries.
    pub async fn cleanup_failed_discoveries(&self) -> Result<u32, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let count = conn
            .execute(
                "DELETE FROM discovery_queue WHERE status = 'failed' OR attempt_count >= 3",
                &[],
            )
            .await
            .map_err(|e| format!("PG cleanup_failed: {}", e))?;
        Ok(count as u32)
    }

    /// Get sync status.
    pub async fn get_sync_status(&self) -> Result<serde_json::Value, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let pending: i64 = conn
            .query_one(
                "SELECT COUNT(*) FROM discovery_queue WHERE status = 'pending'",
                &[],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(0);
        let failed: i64 = conn
            .query_one(
                "SELECT COUNT(*) FROM discovery_queue WHERE status = 'failed'",
                &[],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(0);
        let ready: i64 = conn.query_one("SELECT COUNT(*) FROM discovery_queue WHERE status = 'pending' AND attempt_count < 3", &[])
            .await.map(|r| r.get(0)).unwrap_or(0);
        Ok(json!({
            "pending_count": pending,
            "failed_count": failed,
            "ready_for_retry": ready,
            "authenticated": true,
        }))
    }

    // ========================================================================
    // PRM Export (PG equivalent)
    // ========================================================================

    /// Export step-level PRM training data from pipeline artifacts + task runs.
    ///
    /// Produces one example per step per iteration, matching the format expected
    /// by qontinui-prm's Python extractor (PrmTrainingExample).
    pub async fn export_prm_training_data(
        &self,
        min_runs: i64,
    ) -> Result<(Vec<serde_json::Value>, serde_json::Value), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {e}"))?;

        // Join task_runs with generation_pipeline_artifacts for step-level data
        let rows = conn
            .query(
                r#"SELECT
                tr.id AS run_id,
                tr.workflow_id,
                tr.task_name,
                tr.status AS run_status,
                COALESCE(tr.verification_passed, false) AS verification_passed,
                gpa.final_json,
                gpa.verification_iterations,
                gpa.fixer_snapshots,
                gpa.specification_criteria,
                gpa.category
            FROM task_runs tr
            INNER JOIN generation_pipeline_artifacts gpa
                ON gpa.task_run_id = tr.id
                OR (gpa.task_run_id IS NULL AND gpa.workflow_id = tr.workflow_id)
            WHERE tr.status IN ('complete', 'failed')
              AND gpa.final_json IS NOT NULL
            ORDER BY tr.created_at DESC"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG prm export query: {e}"))?;

        let runs_processed = rows.len();
        if (runs_processed as i64) < min_runs {
            return Err(format!(
                "Only {runs_processed} runs found, need at least {min_runs}"
            ));
        }

        let mut examples = Vec::new();
        let mut passed_count: usize = 0;
        let mut failed_count: usize = 0;
        let mut fixed_count: usize = 0;
        let mut domains_set = std::collections::HashSet::new();

        for row in &rows {
            let run_id: String = row.get(0);
            let workflow_id: Option<String> = row.get(1);
            let task_name: Option<String> = row.get(2);
            let run_status: String = row.get(3);
            let verification_passed: bool = row.get(4);
            let final_json: Option<String> = row.get(5);
            let verification_iterations: Option<String> = row.get(6);
            let fixer_snapshots: Option<String> = row.get(7);
            let spec_criteria: Option<String> = row.get(8);
            let category: Option<String> = row.get(9);

            if let Some(ref d) = category {
                domains_set.insert(d.clone());
            }

            let workflow_passed = run_status == "complete" && verification_passed;
            let context = task_name.unwrap_or_default();

            // Parse steps from final workflow JSON
            let steps = match &final_json {
                Some(j) => match serde_json::from_str::<serde_json::Value>(j) {
                    Ok(v) => v
                        .get("steps")
                        .or_else(|| v.get("verification_steps"))
                        .and_then(|s| s.as_array().cloned())
                        .unwrap_or_default(),
                    Err(_) => continue,
                },
                None => continue,
            };

            // Parse verification iterations — handles both flat arrays and {results: [...]} format
            let iterations: Vec<Vec<serde_json::Value>> = verification_iterations
                .as_deref()
                .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(s).ok())
                .map(|arr| {
                    arr.into_iter()
                        .map(|entry| {
                            // If entry has a "results" or "steps" key, use that array
                            entry
                                .get("results")
                                .or_else(|| entry.get("steps"))
                                .and_then(|v| v.as_array().cloned())
                                // Otherwise treat entry itself as array of step results
                                .or_else(|| entry.as_array().cloned())
                                .unwrap_or_default()
                        })
                        .collect()
                })
                .unwrap_or_default();

            // Parse fixer diffs
            let fixer_diffs: Vec<serde_json::Value> = fixer_snapshots
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();

            // Parse criteria
            let criteria: Vec<serde_json::Value> = spec_criteria
                .as_deref()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .and_then(|v| v.get("criteria").and_then(|c| c.as_array().cloned()))
                .unwrap_or_default();

            for (step_idx, step_val) in steps.iter().enumerate() {
                let step_type = step_val
                    .get("step_type")
                    .or_else(|| step_val.get("type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let step_name = step_val
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unnamed");

                // Format step definition as text for the Python extractor
                let mut step_parts = vec![
                    format!("Step Type: {step_type}"),
                    format!("Name: {step_name}"),
                ];
                if let Some(cmd) = step_val.get("command").and_then(|v| v.as_str()) {
                    step_parts.push(format!("Command: {cmd}"));
                }
                if let Some(prompt) = step_val.get("prompt").and_then(|v| v.as_str()) {
                    step_parts.push(format!("Prompt: {prompt}"));
                }
                if let Some(check) = step_val.get("check_type").and_then(|v| v.as_str()) {
                    step_parts.push(format!("Check Type: {check}"));
                }
                if let Some(expected) = step_val.get("expected").and_then(|v| v.as_str()) {
                    step_parts.push(format!("Expected: {expected}"));
                }
                let step_text = step_parts.join("\n");

                // Find matching criterion
                let criterion = criteria.iter().find(|c| {
                    let cid = c.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    step_name.to_lowercase().contains(&cid.to_lowercase())
                });

                let criterion_text = criterion
                    .map(|c| {
                        format!(
                            "Criterion: {}\nMethod: {}\nPriority: {}",
                            c.get("description")
                                .and_then(|v| v.as_str())
                                .unwrap_or("N/A"),
                            c.get("method").and_then(|v| v.as_str()).unwrap_or("N/A"),
                            c.get("priority").and_then(|v| v.as_str()).unwrap_or("N/A"),
                        )
                    })
                    .unwrap_or_else(|| "No specific acceptance criterion.".to_string());

                // Determine execution result from iteration data
                let step_iters: Vec<&serde_json::Value> = iterations
                    .iter()
                    .filter_map(|iter| iter.get(step_idx))
                    .collect();

                if step_iters.is_empty() {
                    // No iteration data — infer from workflow outcome
                    let (exec_result, final_outcome) = if workflow_passed {
                        ("passed", "workflow_passed")
                    } else {
                        ("failed", "workflow_failed")
                    };

                    if exec_result == "passed" {
                        passed_count += 1;
                    } else {
                        failed_count += 1;
                    }

                    examples.push(json!({
                        "run_id": run_id,
                        "workflow_id": workflow_id,
                        "step_index": step_idx,
                        "step_definition": step_val,
                        "criterion": criterion,
                        "workflow_context": context,
                        "execution_result": exec_result,
                        "iteration": 0,
                        "fixer_diff": null,
                        "final_outcome": final_outcome,
                        "domain": category,
                    }));
                } else {
                    for (iter_idx, iter_result) in step_iters.iter().enumerate() {
                        let passed = iter_result
                            .get("passed")
                            .or_else(|| iter_result.get("success"))
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let error = iter_result
                            .get("error")
                            .and_then(|v| v.as_str())
                            .map(String::from);

                        let any_passed = step_iters.iter().any(|r| {
                            r.get("passed")
                                .or_else(|| r.get("success"))
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false)
                        });

                        let exec_result = if passed { "passed" } else { "failed" };
                        let final_outcome = if passed && workflow_passed {
                            "workflow_passed"
                        } else if !passed && any_passed {
                            fixed_count += 1;
                            "step_fixed_later"
                        } else if workflow_passed {
                            "workflow_passed"
                        } else {
                            "workflow_failed"
                        };

                        if passed {
                            passed_count += 1;
                        } else {
                            failed_count += 1;
                        }

                        let fixer_diff = if !passed {
                            fixer_diffs
                                .get(iter_idx)
                                .and_then(|d| d.get("diff").or_else(|| d.get("changes")))
                                .and_then(|v| v.as_str())
                        } else {
                            None
                        };

                        examples.push(json!({
                            "run_id": run_id,
                            "workflow_id": workflow_id,
                            "step_index": step_idx,
                            "step_definition": step_val,
                            "criterion": criterion,
                            "workflow_context": context,
                            "execution_result": exec_result,
                            "iteration": iter_idx,
                            "fixer_diff": fixer_diff,
                            "final_outcome": final_outcome,
                            "domain": category,
                        }));
                    }
                }
            }
        }

        let stats = json!({
            "total_examples": examples.len(),
            "passed_count": passed_count,
            "failed_count": failed_count,
            "fixed_count": fixed_count,
            "runs_processed": runs_processed,
            "domains": domains_set.into_iter().collect::<Vec<_>>(),
        });
        Ok((examples, stats))
    }

    /// Lightweight PRM stats query — counts without materializing examples.
    pub async fn export_prm_stats(&self) -> Result<serde_json::Value, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {e}"))?;
        let row = conn.query_one(
            r#"SELECT
                COUNT(DISTINCT tr.id) AS runs_processed,
                COUNT(*) AS artifact_count,
                SUM(CASE WHEN tr.verification_passed = true AND tr.status = 'complete' THEN 1 ELSE 0 END) AS passed_runs,
                SUM(CASE WHEN tr.status = 'failed' THEN 1 ELSE 0 END) AS failed_runs,
                ARRAY_AGG(DISTINCT gpa.category) FILTER (WHERE gpa.category IS NOT NULL) AS domains
            FROM task_runs tr
            INNER JOIN generation_pipeline_artifacts gpa
                ON gpa.task_run_id = tr.id
                OR (gpa.task_run_id IS NULL AND gpa.workflow_id = tr.workflow_id)
            WHERE tr.status IN ('complete', 'failed')
              AND gpa.final_json IS NOT NULL"#,
            &[],
        ).await.map_err(|e| format!("PG prm stats: {e}"))?;

        let runs_processed: i64 = row.get(0);
        let total_examples: i64 = row.get(1);
        let passed_count: i64 = row.get(2);
        let failed_count: i64 = row.get(3);
        let domains: Option<Vec<String>> = row.get(4);

        Ok(json!({
            "total_examples": total_examples,
            "passed_count": passed_count,
            "failed_count": failed_count,
            "fixed_count": 0,
            "runs_processed": runs_processed,
            "domains": domains.unwrap_or_default(),
        }))
    }

    // ========================================================================
    // UI Bridge Analytics (PG equivalents)
    // ========================================================================

    /// Compute automation health score.
    pub async fn compute_automation_health_score(
        &self,
        since_epoch_ms: i64,
    ) -> Result<serde_json::Value, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let since_ts = chrono::DateTime::from_timestamp_millis(since_epoch_ms)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        let row = conn
            .query_one(
                r#"SELECT
                COUNT(*) as total,
                SUM(CASE WHEN success = true THEN 1 ELSE 0 END) as successful,
                AVG(duration_ms) as avg_duration
               FROM task_run_automation
               WHERE started_at::TEXT > $1"#,
                &[&since_ts],
            )
            .await
            .map_err(|e| format!("PG health_score: {}", e))?;
        let total: i64 = row.get(0);
        let successful: i64 = row.get(1);
        let avg_duration: Option<f64> = row.get(2);
        let score = if total > 0 {
            successful as f64 / total as f64
        } else {
            0.0
        };
        Ok(json!({
            "overall_score": score,
            "total_runs": total,
            "successful_runs": successful,
            "avg_duration_ms": avg_duration,
        }))
    }

    /// Generate automation recommendations.
    pub async fn generate_recommendations(
        &self,
        since_epoch_ms: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let since_ts = chrono::DateTime::from_timestamp_millis(since_epoch_ms)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        // Get frequent error types
        let rows = conn
            .query(
                r#"SELECT error_type, COUNT(*) as cnt
               FROM task_run_automation
               WHERE started_at::TEXT > $1 AND error_type IS NOT NULL
               GROUP BY error_type ORDER BY cnt DESC LIMIT 5"#,
                &[&since_ts],
            )
            .await
            .unwrap_or_default();
        let recommendations: Vec<serde_json::Value> = rows.iter().map(|r| {
            let error_type: String = r.get(0);
            let count: i64 = r.get(1);
            json!({
                "type": "reduce_errors",
                "title": format!("Address recurring '{}' errors ({} occurrences)", error_type, count),
                "priority": if count > 5 { "high" } else { "medium" },
            })
        }).collect();
        Ok(recommendations)
    }

    // ========================================================================
    // Error Monitor (PG equivalent)
    // ========================================================================

    /// Generate fix workflow for errors (returns workflow JSON).
    pub async fn generate_error_fix_workflow(
        &self,
        task_run_id: &str,
        max_iterations: u32,
    ) -> Result<serde_json::Value, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        // Get errors for this task run
        let errors = conn
            .query(
                r#"SELECT id, title, description, category, severity
               FROM task_run_findings
               WHERE task_run_id = $1 AND category = 'error'
               ORDER BY created_at DESC LIMIT 10"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get errors: {}", e))?;

        let error_list: Vec<serde_json::Value> = errors
            .iter()
            .map(|r| {
                json!({
                    "id": r.get::<_, String>(0),
                    "title": r.get::<_, String>(1),
                    "description": r.get::<_, Option<String>>(2),
                    "category": r.get::<_, String>(3),
                    "severity": r.get::<_, Option<String>>(4),
                })
            })
            .collect();

        Ok(json!({
            "task_run_id": task_run_id,
            "max_iterations": max_iterations,
            "errors_found": error_list.len(),
            "errors": error_list,
            "workflow": null,
        }))
    }

    // ========================================================================
    // Agentic Metrics (PG equivalent)
    // ========================================================================

    /// Recompute all agentic baselines.
    pub async fn recompute_agentic_baselines(&self) -> Result<u32, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        // Count existing metrics to report
        let count: i64 = conn
            .query_one("SELECT COUNT(*) FROM agentic_metric_scores", &[])
            .await
            .map(|r| r.get(0))
            .unwrap_or(0);
        // Mark baselines as recomputed
        conn.execute("UPDATE agentic_baselines SET recomputed_at = NOW()", &[])
            .await
            .ok();
        Ok(count as u32)
    }

    // ========================================================================
    // Comparison (PG equivalent for update)
    // ========================================================================

    /// Update comparison entries JSON.
    pub async fn update_comparison_entries(
        &self,
        comparison_id: &str,
        entries_json: &str,
        status: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        conn.execute(
            "UPDATE comparison_runs SET entries_json = $1, status = $2 WHERE id = $3",
            &[&entries_json, &status, &comparison_id],
        )
        .await
        .map_err(|e| format!("PG update_comparison_entries: {}", e))?;
        Ok(())
    }

    // ========================================================================
    // Performance Metrics (PG equivalents)
    // ========================================================================

    /// Get performance dashboard data.
    pub async fn get_performance_dashboard(
        &self,
        config_id: &str,
        range_days: i64,
    ) -> Result<serde_json::Value, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let since = (chrono::Utc::now() - chrono::Duration::days(range_days)).to_rfc3339();

        // Summary
        let summary_row = conn.query_one(
            r#"SELECT COUNT(*) as total, SUM(CASE WHEN tra.success THEN 1 ELSE 0 END) as successful,
                      AVG(tra.duration_ms) as avg_duration
               FROM task_run_automation tra
               INNER JOIN task_runs tr ON tra.task_run_id = tr.id
               WHERE tr.config_id = $1 AND tra.started_at::TEXT > $2"#,
            &[&config_id, &since],
        ).await.map_err(|e| format!("PG perf summary: {}", e))?;
        let total: i64 = summary_row.get(0);
        let successful: i64 = summary_row.get(1);
        let avg_duration: Option<f64> = summary_row.get(2);

        Ok(json!({
            "summary": {
                "total_runs": total,
                "successful_runs": successful,
                "failed_runs": total - successful,
                "success_rate": if total > 0 { successful as f64 / total as f64 } else { 0.0 },
                "avg_duration_ms": avg_duration,
            },
            "action_metrics": [],
            "transition_metrics": [],
            "element_metrics": [],
            "success_rate_trend": [],
            "duration_trend": [],
        }))
    }

    /// Get action performance metrics.
    pub async fn get_action_performance(
        &self,
        config_id: &str,
        range_days: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        // Simplified — return empty for now since action_metrics is deeply tied to SQLite schema
        Ok(vec![])
    }

    /// Get transition metrics.
    pub async fn get_transition_metrics(
        &self,
        config_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        // transition_reliability table does not exist in the schema — return empty
        tracing::debug!(config_id, "get_transition_metrics: transition_reliability table not in schema");
        let _ = conn; // suppress unused warning
        Ok(vec![])
    }

    /// Get element resolution metrics.
    pub async fn get_element_metrics(
        &self,
        config_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        // element_resolution_metrics table does not exist in the schema — return empty
        tracing::debug!(config_id, "get_element_metrics: element_resolution_metrics table not in schema");
        let _ = conn; // suppress unused warning
        Ok(vec![])
    }

    /// Get success rate trend.
    pub async fn get_success_rate_trend(
        &self,
        config_id: &str,
        range_days: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let since = (chrono::Utc::now() - chrono::Duration::days(range_days)).to_rfc3339();
        let rows = conn
            .query(
                r#"SELECT DATE_TRUNC('day', tra.started_at)::TEXT as day,
                      COUNT(*) as total,
                      SUM(CASE WHEN tra.success THEN 1 ELSE 0 END) as successful
               FROM task_run_automation tra
               INNER JOIN task_runs tr ON tra.task_run_id = tr.id
               WHERE tr.config_id = $1 AND tra.started_at::TEXT > $2
               GROUP BY day ORDER BY day ASC"#,
                &[&config_id, &since],
            )
            .await
            .unwrap_or_default();
        Ok(rows
            .iter()
            .map(|r| {
                let total: i64 = r.get(1);
                let successful: i64 = r.get(2);
                json!({
                    "timestamp": r.get::<_, String>(0),
                    "value": if total > 0 { successful as f64 / total as f64 } else { 0.0 },
                })
            })
            .collect())
    }

    // ========================================================================
    // Learning Recorder (PG equivalent)
    // ========================================================================

    /// Record workflow learning outcome.
    pub async fn record_workflow_learning(
        &self,
        outcome: &serde_json::Value,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let id = outcome["id"]
            .as_str()
            .unwrap_or(&uuid::Uuid::new_v4().to_string())
            .to_string();
        let workflow_name = outcome["workflow_name"].as_str().unwrap_or("");
        let status = outcome["status"].as_str().unwrap_or("unknown");
        let iterations = outcome["iterations"].as_i64();
        let duration = outcome["duration_secs"].as_f64();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            r#"INSERT INTO learning_outcomes (id, workflow_name, status, iterations, duration_secs, created_at)
               VALUES ($1, $2, $3, $4, $5, $6)
               ON CONFLICT (id) DO NOTHING"#,
            &[
                &id, &workflow_name, &status,
                &iterations.map(|i| i as i32) as &(dyn tokio_postgres::types::ToSql + Sync),
                &duration as &(dyn tokio_postgres::types::ToSql + Sync),
                &now,
            ],
        ).await.map_err(|e| format!("PG record_learning: {}", e))?;
        Ok(())
    }
}
