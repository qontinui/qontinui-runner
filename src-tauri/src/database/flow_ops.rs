//! Flow Designer CRUD operations, version history, and execution tracking.
//!
//! Contains all CheckpointDb methods related to the Flow Designer.

use chrono::Utc;
use rusqlite::{params, Result as SqliteResult};

use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Flow Designer Operations
    // ========================================================================

    /// Save a flow definition
    pub fn save_flow(&self, flow: &serde_json::Value) -> Result<String, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let id = flow["id"].as_str().ok_or("Flow must have an id")?;
        let name = flow["name"].as_str().ok_or("Flow must have a name")?;
        let description = flow["description"].as_str();
        let steps = serde_json::to_string(&flow["steps"]).map_err(|e| e.to_string())?;
        let start_step = flow["start_step"].as_str();
        let timeout_secs = flow["timeout_secs"].as_i64().map(|v| v as i32);
        let inputs = serde_json::to_string(&flow["inputs"]).ok();
        let outputs = serde_json::to_string(&flow["outputs"]).ok();
        let tags = serde_json::to_string(&flow["tags"]).ok();
        let version = flow["version"].as_str().unwrap_or("1.0.0");

        conn.execute(
            r#"
            INSERT INTO orchestrator_flows (
                id, name, description, steps, start_step, timeout_secs,
                inputs, outputs, tags, version, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
            ON CONFLICT(id) DO UPDATE SET
                name = ?2,
                description = ?3,
                steps = ?4,
                start_step = ?5,
                timeout_secs = ?6,
                inputs = ?7,
                outputs = ?8,
                tags = ?9,
                version = ?10,
                updated_at = ?11
            "#,
            params![
                id,
                name,
                description,
                steps,
                start_step,
                timeout_secs,
                inputs,
                outputs,
                tags,
                version,
                now
            ],
        )
        .map_err(|e| format!("Failed to save flow: {}", e))?;

        Ok(id.to_string())
    }

    /// Get a flow by ID
    pub fn get_flow(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<serde_json::Value> = conn.query_row(
            r#"
            SELECT id, name, description, steps, start_step, timeout_secs,
                   inputs, outputs, tags, version, created_at, updated_at
            FROM orchestrator_flows
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "steps": row.get::<_, String>(3)?.parse::<serde_json::Value>().ok(),
                    "start_step": row.get::<_, Option<String>>(4)?,
                    "timeout_secs": row.get::<_, Option<i32>>(5)?,
                    "inputs": row.get::<_, Option<String>>(6)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "outputs": row.get::<_, Option<String>>(7)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "tags": row.get::<_, Option<String>>(8)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "version": row.get::<_, String>(9)?,
                    "created_at": row.get::<_, String>(10)?,
                    "updated_at": row.get::<_, String>(11)?,
                }))
            },
        );

        match result {
            Ok(flow) => Ok(Some(flow)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get flow: {}", e)),
        }
    }

    /// List all flows (summaries)
    pub fn list_flows(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description,
                       json_array_length(json_extract(steps, '$')) as step_count,
                       tags, version
                FROM orchestrator_flows
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "step_count": row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    "tags": row.get::<_, Option<String>>(4)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "version": row.get::<_, String>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to list flows: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Delete a flow
    pub fn delete_flow(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let affected = conn
            .execute("DELETE FROM orchestrator_flows WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete flow: {}", e))?;

        Ok(affected > 0)
    }

    // ========================================================================
    // Flow Version History
    // ========================================================================

    /// Create a new version snapshot of a flow.
    /// Automatically increments version number.
    pub fn create_flow_version(
        &self,
        flow_id: &str,
        definition: &serde_json::Value,
        message: Option<&str>,
        created_by: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Get the next version number for this flow
        let next_version: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) + 1 FROM flow_versions WHERE flow_id = ?1",
                params![flow_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to get next version number: {}", e))?;

        let id = format!("{}_v{}", flow_id, next_version);
        let definition_json = serde_json::to_string(definition)
            .map_err(|e| format!("Failed to serialize flow definition: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO flow_versions (id, flow_id, version, definition, message, created_by, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![id, flow_id, next_version, definition_json, message, created_by, now],
        )
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

    /// List all versions of a flow.
    pub fn list_flow_versions(&self, flow_id: &str) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, flow_id, version, message, created_by, created_at
                FROM flow_versions
                WHERE flow_id = ?1
                ORDER BY version DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(params![flow_id], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "version": row.get::<_, i32>(2)?,
                    "message": row.get::<_, Option<String>>(3)?,
                    "created_by": row.get::<_, Option<String>>(4)?,
                    "created_at": row.get::<_, String>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to list flow versions: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get a specific version of a flow (full definition).
    pub fn get_flow_version(
        &self,
        flow_id: &str,
        version: i32,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<serde_json::Value> = conn.query_row(
            r#"
            SELECT id, flow_id, version, definition, message, created_by, created_at
            FROM flow_versions
            WHERE flow_id = ?1 AND version = ?2
            "#,
            params![flow_id, version],
            |row| {
                let definition_str: String = row.get(3)?;
                let definition: serde_json::Value =
                    serde_json::from_str(&definition_str).unwrap_or(serde_json::Value::Null);

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "version": row.get::<_, i32>(2)?,
                    "definition": definition,
                    "message": row.get::<_, Option<String>>(4)?,
                    "created_by": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            },
        );

        match result {
            Ok(version_data) => Ok(Some(version_data)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get flow version: {}", e)),
        }
    }

    /// Restore a flow to a specific version.
    /// Creates a new version first as backup, then updates the flow.
    pub fn restore_flow_version(
        &self,
        flow_id: &str,
        version: i32,
        created_by: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        // Get the version to restore
        let version_data = self
            .get_flow_version(flow_id, version)?
            .ok_or_else(|| format!("Version {} not found for flow {}", version, flow_id))?;

        let definition = version_data["definition"].clone();

        // Get current flow for backup
        let current_flow = self.get_flow(flow_id)?;

        // Create backup version of current state before restoring
        if let Some(current) = current_flow {
            self.create_flow_version(
                flow_id,
                &current,
                Some(&format!("Backup before restoring to version {}", version)),
                created_by,
            )?;
        }

        // Update the flow with the restored definition
        self.save_flow(&definition)?;

        // Create a new version marking the restore
        let new_version = self.create_flow_version(
            flow_id,
            &definition,
            Some(&format!("Restored from version {}", version)),
            created_by,
        )?;

        Ok(serde_json::json!({
            "flow_id": flow_id,
            "restored_from_version": version,
            "new_version": new_version["version"],
            "definition": definition
        }))
    }

    /// Compare two versions of a flow.
    /// Returns the definitions of both versions for client-side diff.
    pub fn compare_flow_versions(
        &self,
        flow_id: &str,
        version1: i32,
        version2: i32,
    ) -> Result<serde_json::Value, String> {
        let v1 = self
            .get_flow_version(flow_id, version1)?
            .ok_or_else(|| format!("Version {} not found", version1))?;

        let v2 = self
            .get_flow_version(flow_id, version2)?
            .ok_or_else(|| format!("Version {} not found", version2))?;

        Ok(serde_json::json!({
            "flow_id": flow_id,
            "version1": {
                "version": version1,
                "definition": v1["definition"],
                "message": v1["message"],
                "created_at": v1["created_at"],
                "created_by": v1["created_by"]
            },
            "version2": {
                "version": version2,
                "definition": v2["definition"],
                "message": v2["message"],
                "created_at": v2["created_at"],
                "created_by": v2["created_by"]
            }
        }))
    }

    /// Delete a specific version of a flow.
    pub fn delete_flow_version(&self, flow_id: &str, version: i32) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let affected = conn
            .execute(
                "DELETE FROM flow_versions WHERE flow_id = ?1 AND version = ?2",
                params![flow_id, version],
            )
            .map_err(|e| format!("Failed to delete flow version: {}", e))?;

        Ok(affected > 0)
    }

    /// Get the latest version number of a flow.
    pub fn get_latest_flow_version(&self, flow_id: &str) -> Result<Option<i32>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<i32> = conn.query_row(
            "SELECT MAX(version) FROM flow_versions WHERE flow_id = ?1",
            params![flow_id],
            |row| row.get(0),
        );

        match result {
            Ok(version) => Ok(Some(version)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get latest version: {}", e)),
        }
    }

    /// Save flow execution state
    pub fn save_flow_execution(&self, execution: &serde_json::Value) -> Result<(), String> {
        let conn = self.get_conn()?;

        let instance_id = execution["instance_id"]
            .as_str()
            .ok_or("Execution must have instance_id")?;
        let flow_id = execution["flow_id"]
            .as_str()
            .ok_or("Execution must have flow_id")?;
        let current_step = execution["current_step"].as_str();
        let status = execution["status"].as_str().unwrap_or("pending");
        let context = serde_json::to_string(&execution["context"]).ok();
        let history = serde_json::to_string(&execution["history"]).ok();
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
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(instance_id) DO UPDATE SET
                current_step = ?3,
                status = ?4,
                context = ?5,
                history = ?6,
                error = ?7,
                completed_at = ?9
            "#,
            params![instance_id, flow_id, current_step, status, context, history, error, started_at, completed_at],
        )
        .map_err(|e| format!("Failed to save flow execution: {}", e))?;

        Ok(())
    }

    /// Get flow execution by instance ID
    pub fn get_flow_execution(
        &self,
        instance_id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<serde_json::Value> = conn.query_row(
            r#"
            SELECT instance_id, flow_id, current_step, status, context, history, error, started_at, completed_at
            FROM flow_executions
            WHERE instance_id = ?1
            "#,
            params![instance_id],
            |row| {
                Ok(serde_json::json!({
                    "instance_id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "current_step": row.get::<_, Option<String>>(2)?,
                    "status": row.get::<_, String>(3)?,
                    "context": row.get::<_, Option<String>>(4)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "history": row.get::<_, Option<String>>(5)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "error": row.get::<_, Option<String>>(6)?,
                    "started_at": row.get::<_, String>(7)?,
                    "completed_at": row.get::<_, Option<String>>(8)?,
                }))
            },
        );

        match result {
            Ok(exec) => Ok(Some(exec)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get flow execution: {}", e)),
        }
    }

    /// List flow executions
    pub fn list_flow_executions(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT instance_id, flow_id, current_step, status, started_at, completed_at
                FROM flow_executions
                ORDER BY started_at DESC
                LIMIT 50
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                Ok(serde_json::json!({
                    "instance_id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "current_step": row.get::<_, Option<String>>(2)?,
                    "status": row.get::<_, String>(3)?,
                    "started_at": row.get::<_, String>(4)?,
                    "completed_at": row.get::<_, Option<String>>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to list flow executions: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    // ========================================================================
    // Enhanced Flow Queries (Tag Filtering, Execution Filtering)
    // ========================================================================

    /// Get flows filtered by tag.
    pub fn get_flows_by_tag(&self, tag: &str) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        // Use JSON search to find flows with the specified tag
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description,
                       json_array_length(json_extract(steps, '$')) as step_count,
                       tags, version
                FROM orchestrator_flows
                WHERE json_extract(tags, '$') LIKE ?1
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let search_pattern = format!("%\"{}%", tag);
        let results = stmt
            .query_map(params![search_pattern], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "step_count": row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    "tags": row.get::<_, Option<String>>(4)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "version": row.get::<_, String>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to get flows by tag: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get flow executions filtered by flow ID and/or status.
    pub fn get_flow_executions_filtered(
        &self,
        flow_id: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        // Build dynamic WHERE clause
        let mut conditions = Vec::new();
        if flow_id.is_some() {
            conditions.push("flow_id = ?1");
        }
        if status.is_some() {
            conditions.push("status = ?2");
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let query = format!(
            r#"
            SELECT instance_id, flow_id, current_step, status, started_at, completed_at
            FROM flow_executions
            {}
            ORDER BY started_at DESC
            LIMIT 100
            "#,
            where_clause
        );

        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(
                params![flow_id.unwrap_or(""), status.unwrap_or("")],
                |row| {
                    Ok(serde_json::json!({
                        "instance_id": row.get::<_, String>(0)?,
                        "flow_id": row.get::<_, String>(1)?,
                        "current_step": row.get::<_, Option<String>>(2)?,
                        "status": row.get::<_, String>(3)?,
                        "started_at": row.get::<_, String>(4)?,
                        "completed_at": row.get::<_, Option<String>>(5)?,
                    }))
                },
            )
            .map_err(|e| format!("Failed to get flow executions: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get flow executions with pagination support.
    pub fn get_flow_executions_paginated(
        &self,
        flow_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let query = if flow_id.is_some() {
            r#"
            SELECT instance_id, flow_id, current_step, status, started_at, completed_at
            FROM flow_executions
            WHERE flow_id = ?1
            ORDER BY started_at DESC
            LIMIT ?2 OFFSET ?3
            "#
        } else {
            r#"
            SELECT instance_id, flow_id, current_step, status, started_at, completed_at
            FROM flow_executions
            ORDER BY started_at DESC
            LIMIT ?2 OFFSET ?3
            "#
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = if let Some(fid) = flow_id {
            stmt.query_map(params![fid, limit, offset], |row| {
                Ok(serde_json::json!({
                    "instance_id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "current_step": row.get::<_, Option<String>>(2)?,
                    "status": row.get::<_, String>(3)?,
                    "started_at": row.get::<_, String>(4)?,
                    "completed_at": row.get::<_, Option<String>>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to get flow executions: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        } else {
            stmt.query_map(params!["", limit, offset], |row| {
                Ok(serde_json::json!({
                    "instance_id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "current_step": row.get::<_, Option<String>>(2)?,
                    "status": row.get::<_, String>(3)?,
                    "started_at": row.get::<_, String>(4)?,
                    "completed_at": row.get::<_, Option<String>>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to get flow executions: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        };

        Ok(results)
    }

    /// Get total count of flow executions (for pagination).
    pub fn get_flow_executions_count(&self, flow_id: Option<&str>) -> Result<i64, String> {
        let conn = self.get_conn()?;
        if let Some(fid) = flow_id {
            conn.query_row(
                "SELECT COUNT(*) FROM flow_executions WHERE flow_id = ?1",
                params![fid],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to get count: {}", e))
        } else {
            conn.query_row("SELECT COUNT(*) FROM flow_executions", [], |row| row.get(0))
                .map_err(|e| format!("Failed to get count: {}", e))
        }
    }
}
