//! PostgreSQL checkpoint operations (active_workflows) and orchestrator checkpoints.

use super::PgDb;
use crate::database::types::CheckpointData;
use chrono::Utc;

impl PgDb {
    // ========================================================================
    // Checkpoint / active_workflows Operations
    // ========================================================================

    /// Get a checkpoint by workflow name.
    pub async fn get_checkpoint(&self, workflow_name: &str) -> Result<Option<CheckpointData>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT
                    workflow_name,
                    checkpoint_data,
                    completed,
                    run_id,
                    created_at,
                    updated_at
                FROM active_workflows
                WHERE workflow_name = $1
                "#,
                &[&workflow_name],
            )
            .await
            .map_err(|e| format!("Failed to get checkpoint: {}", e))?;

        Ok(row.map(|r| {
            let checkpoint_str: String = r.get(1);
            let checkpoint_json: serde_json::Value =
                serde_json::from_str(&checkpoint_str).unwrap_or(serde_json::Value::Null);

            CheckpointData {
                session_id: None,
                workflow_name: r.get(0),
                current_phase: checkpoint_json["current_phase"].as_u64().unwrap_or(0) as u32,
                total_phases: checkpoint_json["total_phases"].as_u64().map(|v| v as u32),
                completed: r.get::<_, bool>(2),
                restart_permitted: checkpoint_json["restart_permitted"].as_bool().unwrap_or(false),
                status: checkpoint_json["status"].as_str().map(|s| s.to_string()),
                run_id: r.get(3),
                repos_to_process: checkpoint_json["repos_to_process"]
                    .as_array()
                    .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()),
                work_completed: {
                    let v = &checkpoint_json["work_completed"];
                    if v.is_null() { None } else { Some(v.clone()) }
                },
                items_needing_user_input: checkpoint_json["items_needing_user_input"]
                    .as_array()
                    .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()),
                created_at: r.get(4),
                updated_at: r.get(5),
                error_message: checkpoint_json["error_message"].as_str().map(|s| s.to_string()),
                extra: Some(checkpoint_json),
            }
        }))
    }

    /// Save or update a checkpoint.
    pub async fn save_checkpoint(&self, data: &CheckpointData) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let workflow_name = data
            .workflow_name
            .as_ref()
            .ok_or("workflow_name is required")?;

        let now = Utc::now().to_rfc3339();
        let run_id = data
            .run_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let checkpoint_data = serde_json::json!({
            "current_phase": data.current_phase,
            "total_phases": data.total_phases,
            "completed": data.completed,
            "restart_permitted": data.restart_permitted,
            "status": data.status,
            "repos_to_process": data.repos_to_process,
            "work_completed": data.work_completed,
            "items_needing_user_input": data.items_needing_user_input,
            "error_message": data.error_message,
        });
        let checkpoint_str = checkpoint_data.to_string();
        let completed = data.completed;

        conn.execute(
            r#"
            INSERT INTO active_workflows (workflow_name, checkpoint_data, run_id, phase_field, completion_value, created_at, updated_at, completed)
            VALUES ($1, $2, $3, 'current_phase', 12, $4, $4, $5)
            ON CONFLICT(workflow_name) DO UPDATE SET
                checkpoint_data = $2,
                run_id = CASE WHEN $3 != '' THEN $3 ELSE active_workflows.run_id END,
                updated_at = $4,
                completed = $5
            "#,
            &[
                workflow_name,
                &checkpoint_str,
                &run_id,
                &now,
                &completed,
            ],
        )
        .await
        .map_err(|e| format!("Failed to save checkpoint: {}", e))?;

        Ok(())
    }

    /// Delete a checkpoint by workflow name.
    pub async fn delete_checkpoint(&self, workflow_name: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute(
                "DELETE FROM active_workflows WHERE workflow_name = $1",
                &[&workflow_name],
            )
            .await
            .map_err(|e| format!("Failed to delete checkpoint: {}", e))?;

        Ok(affected > 0)
    }

    /// List all active (non-completed) checkpoints.
    pub async fn list_active_checkpoints(&self) -> Result<Vec<CheckpointData>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT
                    workflow_name,
                    checkpoint_data,
                    completed,
                    run_id,
                    created_at,
                    updated_at
                FROM active_workflows
                WHERE completed = false
                ORDER BY updated_at DESC
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("Failed to list active checkpoints: {}", e))?;

        let checkpoints = rows
            .iter()
            .map(|r| {
                let checkpoint_str: String = r.get(1);
                let checkpoint_json: serde_json::Value =
                    serde_json::from_str(&checkpoint_str).unwrap_or(serde_json::Value::Null);

                CheckpointData {
                    session_id: None,
                    workflow_name: r.get(0),
                    current_phase: checkpoint_json["current_phase"].as_u64().unwrap_or(0) as u32,
                    total_phases: None,
                    completed: r.get::<_, bool>(2),
                    restart_permitted: false,
                    status: None,
                    run_id: r.get(3),
                    repos_to_process: None,
                    work_completed: None,
                    items_needing_user_input: None,
                    created_at: r.get(4),
                    updated_at: r.get(5),
                    error_message: None,
                    extra: None,
                }
            })
            .collect();

        Ok(checkpoints)
    }

    /// Get all unique task IDs that have orchestrator checkpoints.
    pub async fn get_checkpoint_task_ids(&self) -> Result<Vec<String>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                "SELECT DISTINCT task_id FROM orchestrator_checkpoints ORDER BY task_id",
                &[],
            )
            .await
            .map_err(|e| format!("Failed to get task IDs: {}", e))?;

        let results = rows.iter().map(|r| r.get::<_, String>(0)).collect();
        Ok(results)
    }

    /// Get checkpoints with optional filtering by task ID, trigger type, and date.
    pub async fn get_checkpoints_filtered(
        &self,
        task_id: Option<&str>,
        trigger: Option<&str>,
        since: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        // Build dynamic query
        let mut conditions = Vec::new();
        let mut param_idx = 1u32;

        if task_id.is_some() {
            conditions.push(format!("task_id = ${}", param_idx));
            param_idx += 1;
        }
        if trigger.is_some() {
            conditions.push(format!("trigger = ${}", param_idx));
            param_idx += 1;
        }
        if since.is_some() {
            conditions.push(format!("created_at >= ${}", param_idx));
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let query = format!(
            r#"
            SELECT id, task_id, iteration, trigger, state, name, created_at
            FROM orchestrator_checkpoints
            {}
            ORDER BY created_at DESC
            LIMIT 100
            "#,
            where_clause
        );

        // Build params dynamically — values live long enough since they come from fn args
        let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
        if let Some(ref tid) = task_id {
            params.push(tid);
        }
        if let Some(ref trig) = trigger {
            params.push(trig);
        }
        if let Some(ref s) = since {
            params.push(s);
        }

        let rows = conn
            .query(&query, &params)
            .await
            .map_err(|e| format!("Failed to get checkpoints: {}", e))?;

        let results = rows
            .iter()
            .map(|r| {
                let state_str: String = r.get(4);
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "task_id": r.get::<_, String>(1),
                    "iteration": r.get::<_, i32>(2) as i64,
                    "trigger": r.get::<_, String>(3),
                    "state": serde_json::from_str::<serde_json::Value>(&state_str).ok(),
                    "name": r.get::<_, Option<String>>(5),
                    "created_at": r.get::<_, String>(6),
                })
            })
            .collect();

        Ok(results)
    }

    // ========================================================================
    // Orchestrator Checkpoint Operations
    // ========================================================================

    /// Save an orchestrator checkpoint.
    pub async fn save_orchestrator_checkpoint(
        &self,
        id: &str,
        task_id: &str,
        iteration: u32,
        trigger: &str,
        state: &serde_json::Value,
        name: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let now = Utc::now().to_rfc3339();
        let state_json = state.to_string();
        let iteration_i32 = iteration as i32;

        conn.execute(
            r#"
            INSERT INTO orchestrator_checkpoints (id, task_id, iteration, trigger, state, name, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
            &[&id, &task_id, &iteration_i32, &trigger, &state_json, &name, &now],
        )
        .await
        .map_err(|e| format!("Failed to save orchestrator checkpoint: {}", e))?;

        Ok(())
    }

    /// Get a single orchestrator checkpoint by ID.
    pub async fn get_orchestrator_checkpoint(
        &self,
        id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT id, task_id, iteration, trigger, state, name, created_at
                FROM orchestrator_checkpoints
                WHERE id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("Failed to get checkpoint: {}", e))?;

        Ok(row.map(|r| {
            let state_str: String = r.get(4);
            serde_json::json!({
                "id": r.get::<_, String>(0),
                "task_id": r.get::<_, String>(1),
                "iteration": r.get::<_, i32>(2) as i64,
                "trigger": r.get::<_, String>(3),
                "state": serde_json::from_str::<serde_json::Value>(&state_str).ok(),
                "name": r.get::<_, Option<String>>(5),
                "created_at": r.get::<_, String>(6),
            })
        }))
    }

    /// Get orchestrator checkpoints for a task (or all if task_id is None).
    pub async fn get_orchestrator_checkpoints(
        &self,
        task_id: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = if let Some(tid) = task_id {
            conn.query(
                r#"
                SELECT id, task_id, iteration, trigger, state, name, created_at
                FROM orchestrator_checkpoints
                WHERE task_id = $1
                ORDER BY iteration ASC
                "#,
                &[&tid],
            )
            .await
        } else {
            conn.query(
                r#"
                SELECT id, task_id, iteration, trigger, state, name, created_at
                FROM orchestrator_checkpoints
                ORDER BY created_at DESC
                LIMIT 100
                "#,
                &[],
            )
            .await
        }
        .map_err(|e| format!("Failed to get orchestrator checkpoints: {}", e))?;

        let results = rows
            .iter()
            .map(|r| {
                let state_str: String = r.get(4);
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "task_id": r.get::<_, String>(1),
                    "iteration": r.get::<_, i32>(2) as i64,
                    "trigger": r.get::<_, String>(3),
                    "state": serde_json::from_str::<serde_json::Value>(&state_str).ok(),
                    "name": r.get::<_, Option<String>>(5),
                    "created_at": r.get::<_, String>(6),
                })
            })
            .collect();

        Ok(results)
    }

    /// Delete an orchestrator checkpoint by ID.
    pub async fn delete_orchestrator_checkpoint(&self, id: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute(
                "DELETE FROM orchestrator_checkpoints WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(|e| format!("Failed to delete orchestrator checkpoint: {}", e))?;

        Ok(affected > 0)
    }
}
