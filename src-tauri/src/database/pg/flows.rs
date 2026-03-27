//! PostgreSQL flow designer operations (orchestrator_flows, flow_versions, flow_executions).

use super::PgDb;
use chrono::Utc;

impl PgDb {
    // ========================================================================
    // Flow Designer Operations
    // ========================================================================

    /// Save a flow definition (upsert).
    pub async fn save_flow(&self, flow: &serde_json::Value) -> Result<String, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let now = Utc::now().to_rfc3339();

        let id = flow["id"].as_str().ok_or("Flow must have an id")?;
        let name = flow["name"].as_str().ok_or("Flow must have a name")?;
        let description = flow["description"].as_str();
        let steps = serde_json::to_string(&flow["steps"]).map_err(|e| e.to_string())?;
        let start_step = flow["start_step"].as_str();
        let timeout_secs = flow["timeout_secs"].as_i64().map(|v| v as i32);
        let inputs: Option<String> = serde_json::to_string(&flow["inputs"]).ok();
        let outputs: Option<String> = serde_json::to_string(&flow["outputs"]).ok();
        let tags: Option<String> = serde_json::to_string(&flow["tags"]).ok();
        let version = flow["version"].as_str().unwrap_or("1.0.0");

        conn.execute(
            r#"
            INSERT INTO orchestrator_flows (
                id, name, description, steps, start_step, timeout_secs,
                inputs, outputs, tags, version, created_at, updated_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $11)
            ON CONFLICT(id) DO UPDATE SET
                name = $2,
                description = $3,
                steps = $4,
                start_step = $5,
                timeout_secs = $6,
                inputs = $7,
                outputs = $8,
                tags = $9,
                version = $10,
                updated_at = $11
            "#,
            &[
                &id, &name, &description, &steps, &start_step, &timeout_secs,
                &inputs, &outputs, &tags, &version, &now,
            ],
        )
        .await
        .map_err(|e| format!("Failed to save flow: {}", e))?;

        Ok(id.to_string())
    }

    /// Get a flow by ID.
    pub async fn get_flow(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT id, name, description, steps, start_step, timeout_secs,
                       inputs, outputs, tags, version, created_at, updated_at
                FROM orchestrator_flows
                WHERE id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("Failed to get flow: {}", e))?;

        Ok(row.map(|r| {
            let steps_str: String = r.get(3);
            let inputs_str: Option<String> = r.get(6);
            let outputs_str: Option<String> = r.get(7);
            let tags_str: Option<String> = r.get(8);

            serde_json::json!({
                "id": r.get::<_, String>(0),
                "name": r.get::<_, String>(1),
                "description": r.get::<_, Option<String>>(2),
                "steps": serde_json::from_str::<serde_json::Value>(&steps_str).ok(),
                "start_step": r.get::<_, Option<String>>(4),
                "timeout_secs": r.get::<_, Option<i32>>(5),
                "inputs": inputs_str.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                "outputs": outputs_str.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                "tags": tags_str.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                "version": r.get::<_, String>(9),
                "created_at": r.get::<_, String>(10),
                "updated_at": r.get::<_, String>(11),
            })
        }))
    }

    /// Delete a flow by ID.
    pub async fn delete_flow(&self, id: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute("DELETE FROM orchestrator_flows WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("Failed to delete flow: {}", e))?;

        Ok(affected > 0)
    }

    /// List all flows (summaries).
    pub async fn list_flows(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, name, description,
                       COALESCE(jsonb_array_length(steps::jsonb), 0) as step_count,
                       tags, version
                FROM orchestrator_flows
                ORDER BY updated_at DESC
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("Failed to list flows: {}", e))?;

        let results = rows
            .iter()
            .map(|r| {
                let tags_str: Option<String> = r.get(4);
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "name": r.get::<_, String>(1),
                    "description": r.get::<_, Option<String>>(2),
                    "step_count": r.get::<_, Option<i64>>(3).unwrap_or(0),
                    "tags": tags_str.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "version": r.get::<_, String>(5),
                })
            })
            .collect();

        Ok(results)
    }

    /// Get flows filtered by tag.
    pub async fn get_flows_by_tag(&self, tag: &str) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let search_pattern = format!("%\"{}%", tag);
        let rows = conn
            .query(
                r#"
                SELECT id, name, description,
                       COALESCE(jsonb_array_length(steps::jsonb), 0) as step_count,
                       tags, version
                FROM orchestrator_flows
                WHERE tags LIKE $1
                ORDER BY updated_at DESC
                "#,
                &[&search_pattern],
            )
            .await
            .map_err(|e| format!("Failed to get flows by tag: {}", e))?;

        let results = rows
            .iter()
            .map(|r| {
                let tags_str: Option<String> = r.get(4);
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "name": r.get::<_, String>(1),
                    "description": r.get::<_, Option<String>>(2),
                    "step_count": r.get::<_, Option<i64>>(3).unwrap_or(0),
                    "tags": tags_str.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "version": r.get::<_, String>(5),
                })
            })
            .collect();

        Ok(results)
    }

    // ========================================================================
    // Flow Execution Operations
    // ========================================================================

    /// Save flow execution state (upsert).
    pub async fn save_flow_execution(&self, execution: &serde_json::Value) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let instance_id = execution["instance_id"]
            .as_str()
            .ok_or("Execution must have instance_id")?;
        let flow_id = execution["flow_id"]
            .as_str()
            .ok_or("Execution must have flow_id")?;
        let current_step = execution["current_step"].as_str();
        let status = execution["status"].as_str().unwrap_or("pending");
        let context: Option<String> = serde_json::to_string(&execution["context"]).ok();
        let history: Option<String> = serde_json::to_string(&execution["history"]).ok();
        let error = execution["error"].as_str();
        let default_started_at = Utc::now().to_rfc3339();
        let started_at = execution["started_at"]
            .as_str()
            .unwrap_or(&default_started_at);
        let completed_at = execution["completed_at"].as_str();

        conn.execute(
            r#"
            INSERT INTO flow_executions (
                instance_id, flow_id, current_step, status, context, history, error, started_at, completed_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT(instance_id) DO UPDATE SET
                current_step = $3,
                status = $4,
                context = $5,
                history = $6,
                error = $7,
                completed_at = $9
            "#,
            &[
                &instance_id, &flow_id, &current_step, &status,
                &context, &history, &error, &started_at, &completed_at,
            ],
        )
        .await
        .map_err(|e| format!("Failed to save flow execution: {}", e))?;

        Ok(())
    }

    /// Get flow execution by instance ID.
    pub async fn get_flow_execution(
        &self,
        instance_id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT instance_id, flow_id, current_step, status, context, history, error, started_at, completed_at
                FROM flow_executions
                WHERE instance_id = $1
                "#,
                &[&instance_id],
            )
            .await
            .map_err(|e| format!("Failed to get flow execution: {}", e))?;

        Ok(row.map(|r| {
            let context_str: Option<String> = r.get(4);
            let history_str: Option<String> = r.get(5);
            serde_json::json!({
                "instance_id": r.get::<_, String>(0),
                "flow_id": r.get::<_, String>(1),
                "current_step": r.get::<_, Option<String>>(2),
                "status": r.get::<_, String>(3),
                "context": context_str.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                "history": history_str.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                "error": r.get::<_, Option<String>>(6),
                "started_at": r.get::<_, String>(7),
                "completed_at": r.get::<_, Option<String>>(8),
            })
        }))
    }

    /// List flow executions (most recent 50).
    pub async fn list_flow_executions(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT instance_id, flow_id, current_step, status, started_at, completed_at
                FROM flow_executions
                ORDER BY started_at DESC
                LIMIT 50
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("Failed to list flow executions: {}", e))?;

        let results = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "instance_id": r.get::<_, String>(0),
                    "flow_id": r.get::<_, String>(1),
                    "current_step": r.get::<_, Option<String>>(2),
                    "status": r.get::<_, String>(3),
                    "started_at": r.get::<_, String>(4),
                    "completed_at": r.get::<_, Option<String>>(5),
                })
            })
            .collect();

        Ok(results)
    }

    // ========================================================================
    // Flow Version Operations
    // ========================================================================

    /// Create a new version snapshot of a flow.
    pub async fn create_flow_version(
        &self,
        flow_id: &str,
        definition: &serde_json::Value,
        message: Option<&str>,
        created_by: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let now = Utc::now().to_rfc3339();

        // Get the next version number
        let row = conn
            .query_one(
                "SELECT COALESCE(MAX(version), 0) + 1 FROM flow_versions WHERE flow_id = $1",
                &[&flow_id],
            )
            .await
            .map_err(|e| format!("Failed to get next version number: {}", e))?;
        let next_version: i32 = row.get(0);

        let id = format!("{}_v{}", flow_id, next_version);
        let definition_json = serde_json::to_string(definition)
            .map_err(|e| format!("Failed to serialize flow definition: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO flow_versions (id, flow_id, version, definition, message, created_by, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
            &[&id, &flow_id, &next_version, &definition_json, &message, &created_by, &now],
        )
        .await
        .map_err(|e| format!("Failed to create flow version: {}", e))?;

        Ok(serde_json::json!({
            "id": id,
            "flow_id": flow_id,
            "version": next_version,
            "message": message,
            "created_by": created_by,
            "created_at": now
        }))
    }

    /// Get a specific version of a flow (full definition).
    pub async fn get_flow_version(
        &self,
        flow_id: &str,
        version: i32,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT id, flow_id, version, definition, message, created_by, created_at
                FROM flow_versions
                WHERE flow_id = $1 AND version = $2
                "#,
                &[&flow_id, &version],
            )
            .await
            .map_err(|e| format!("Failed to get flow version: {}", e))?;

        Ok(row.map(|r| {
            let definition_str: String = r.get(3);
            let definition: serde_json::Value =
                serde_json::from_str(&definition_str).unwrap_or(serde_json::Value::Null);
            serde_json::json!({
                "id": r.get::<_, String>(0),
                "flow_id": r.get::<_, String>(1),
                "version": r.get::<_, i32>(2),
                "definition": definition,
                "message": r.get::<_, Option<String>>(4),
                "created_by": r.get::<_, Option<String>>(5),
                "created_at": r.get::<_, String>(6),
            })
        }))
    }

    /// Get the latest version number of a flow.
    pub async fn get_latest_flow_version(&self, flow_id: &str) -> Result<Option<i32>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                "SELECT MAX(version) FROM flow_versions WHERE flow_id = $1",
                &[&flow_id],
            )
            .await
            .map_err(|e| format!("Failed to get latest version: {}", e))?;

        Ok(row.and_then(|r| r.get::<_, Option<i32>>(0)))
    }

    /// List all versions of a flow.
    pub async fn list_flow_versions(&self, flow_id: &str) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, flow_id, version, message, created_by, created_at
                FROM flow_versions
                WHERE flow_id = $1
                ORDER BY version DESC
                "#,
                &[&flow_id],
            )
            .await
            .map_err(|e| format!("Failed to list flow versions: {}", e))?;

        let results = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "flow_id": r.get::<_, String>(1),
                    "version": r.get::<_, i32>(2),
                    "message": r.get::<_, Option<String>>(3),
                    "created_by": r.get::<_, Option<String>>(4),
                    "created_at": r.get::<_, String>(5),
                })
            })
            .collect();

        Ok(results)
    }

    /// Delete a specific version of a flow.
    pub async fn delete_flow_version(&self, flow_id: &str, version: i32) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute(
                "DELETE FROM flow_versions WHERE flow_id = $1 AND version = $2",
                &[&flow_id, &version],
            )
            .await
            .map_err(|e| format!("Failed to delete flow version: {}", e))?;

        Ok(affected > 0)
    }

    /// Import multiple flows in a batch.
    pub async fn import_flows(&self, flows: &[serde_json::Value]) -> Result<u64, String> {
        let mut imported = 0u64;
        for flow in flows {
            self.save_flow(flow).await?;
            imported += 1;
        }
        Ok(imported)
    }
}
