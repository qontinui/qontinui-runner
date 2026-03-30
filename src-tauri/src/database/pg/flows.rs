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
                       inputs, outputs, tags, version, created_at::TEXT, updated_at::TEXT
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
                SELECT instance_id, flow_id, current_step, status, context, history, error, started_at::TEXT, completed_at::TEXT
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
                SELECT instance_id, flow_id, current_step, status, started_at::TEXT, completed_at::TEXT
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
                SELECT id, flow_id, version, definition, message, created_by, created_at::TEXT
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
                SELECT id, flow_id, version, message, created_by, created_at::TEXT
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

    /// Get flow executions with optional filtering.
    pub async fn get_flow_executions_filtered(
        &self,
        flow_id: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let (query, params): (String, Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>>) =
            match (flow_id, status) {
                (Some(fid), Some(st)) => (
                    "SELECT instance_id, flow_id, current_step, status, started_at, completed_at FROM flow_executions WHERE flow_id = $1 AND status = $2 ORDER BY started_at DESC".to_string(),
                    vec![Box::new(fid.to_string()), Box::new(st.to_string())],
                ),
                (Some(fid), None) => (
                    "SELECT instance_id, flow_id, current_step, status, started_at, completed_at FROM flow_executions WHERE flow_id = $1 ORDER BY started_at DESC".to_string(),
                    vec![Box::new(fid.to_string())],
                ),
                (None, Some(st)) => (
                    "SELECT instance_id, flow_id, current_step, status, started_at, completed_at FROM flow_executions WHERE status = $1 ORDER BY started_at DESC".to_string(),
                    vec![Box::new(st.to_string())],
                ),
                (None, None) => (
                    "SELECT instance_id, flow_id, current_step, status, started_at, completed_at FROM flow_executions ORDER BY started_at DESC".to_string(),
                    vec![],
                ),
            };

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();

        let rows = conn
            .query(&query, &param_refs)
            .await
            .map_err(|e| format!("PG get_flow_executions_filtered: {}", e))?;

        let results = rows
            .iter()
            .map(|r| {
                let started: chrono::DateTime<chrono::Utc> = r.get(4);
                let completed: Option<chrono::DateTime<chrono::Utc>> = r.get(5);
                serde_json::json!({
                    "instance_id": r.get::<_, String>(0),
                    "flow_id": r.get::<_, String>(1),
                    "current_step": r.get::<_, Option<String>>(2),
                    "status": r.get::<_, String>(3),
                    "started_at": started.to_rfc3339(),
                    "completed_at": completed.map(|dt| dt.to_rfc3339()),
                })
            })
            .collect();

        Ok(results)
    }

    /// Get flow executions with pagination.
    pub async fn get_flow_executions_paginated(
        &self,
        flow_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = if let Some(fid) = flow_id {
            conn.query(
                r#"SELECT instance_id, flow_id, current_step, status, started_at, completed_at
                FROM flow_executions WHERE flow_id = $1 ORDER BY started_at DESC LIMIT $2 OFFSET $3"#,
                &[&fid, &limit, &offset],
            )
            .await
        } else {
            conn.query(
                r#"SELECT instance_id, flow_id, current_step, status, started_at, completed_at
                FROM flow_executions ORDER BY started_at DESC LIMIT $1 OFFSET $2"#,
                &[&limit, &offset],
            )
            .await
        }
        .map_err(|e| format!("PG get_flow_executions_paginated: {}", e))?;

        let results = rows
            .iter()
            .map(|r| {
                let started: chrono::DateTime<chrono::Utc> = r.get(4);
                let completed: Option<chrono::DateTime<chrono::Utc>> = r.get(5);
                serde_json::json!({
                    "instance_id": r.get::<_, String>(0),
                    "flow_id": r.get::<_, String>(1),
                    "current_step": r.get::<_, Option<String>>(2),
                    "status": r.get::<_, String>(3),
                    "started_at": started.to_rfc3339(),
                    "completed_at": completed.map(|dt| dt.to_rfc3339()),
                })
            })
            .collect();

        Ok(results)
    }

    /// Get total count of flow executions.
    pub async fn get_flow_executions_count(&self, flow_id: Option<&str>) -> Result<i64, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let count: i64 = if let Some(fid) = flow_id {
            let row = conn
                .query_one(
                    "SELECT COUNT(*) FROM flow_executions WHERE flow_id = $1",
                    &[&fid],
                )
                .await
                .map_err(|e| format!("PG get_flow_executions_count: {}", e))?;
            row.get(0)
        } else {
            let row = conn
                .query_one("SELECT COUNT(*) FROM flow_executions", &[])
                .await
                .map_err(|e| format!("PG get_flow_executions_count: {}", e))?;
            row.get(0)
        };

        Ok(count)
    }

    /// Restore a flow to a specific version.
    pub async fn restore_flow_version(
        &self,
        flow_id: &str,
        version: i32,
        created_by: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        // Get the version to restore
        let version_data = self
            .get_flow_version(flow_id, version)
            .await?
            .ok_or_else(|| format!("Version {} not found for flow {}", version, flow_id))?;

        let definition = &version_data["definition"];

        // Create a backup version of the current state first
        if let Some(current_flow) = self.get_flow(flow_id).await? {
            self.create_flow_version(
                flow_id,
                &current_flow,
                Some(&format!("Auto-backup before restoring v{}", version)),
                created_by,
            )
            .await?;
        }

        // Update the flow with the restored definition
        self.save_flow(definition).await?;

        // Create a new version entry for the restoration
        let restored = self
            .create_flow_version(
                flow_id,
                definition,
                Some(&format!("Restored from v{}", version)),
                created_by,
            )
            .await?;

        Ok(restored)
    }

    /// Compare two versions of a flow.
    pub async fn compare_flow_versions(
        &self,
        flow_id: &str,
        version1: i32,
        version2: i32,
    ) -> Result<serde_json::Value, String> {
        let v1 = self
            .get_flow_version(flow_id, version1)
            .await?
            .ok_or_else(|| format!("Version {} not found", version1))?;

        let v2 = self
            .get_flow_version(flow_id, version2)
            .await?
            .ok_or_else(|| format!("Version {} not found", version2))?;

        Ok(serde_json::json!({
            "flow_id": flow_id,
            "version1": v1,
            "version2": v2,
        }))
    }
}
