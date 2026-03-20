//! Orchestrator checkpoint operations.
//!
//! Contains all CheckpointDb methods related to orchestrator checkpoints (save, get, filter, paginate).

use rusqlite::params;

use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Orchestrator Checkpoint Operations
    // ========================================================================

    /// Save an orchestrator checkpoint
    pub fn save_orchestrator_checkpoint(
        &self,
        id: &str,
        task_id: &str,
        iteration: u32,
        trigger: &str,
        state: &serde_json::Value,
        name: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();
        let state_json = state.to_string();

        conn.execute(
            r#"
            INSERT INTO orchestrator_checkpoints (id, task_id, iteration, trigger, state, name, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![id, task_id, iteration, trigger, state_json, name, now],
        )
        .map_err(|e| format!("Failed to save orchestrator checkpoint: {}", e))?;

        Ok(())
    }

    /// Get checkpoints for a task
    pub fn get_orchestrator_checkpoints(
        &self,
        task_id: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let query = if task_id.is_some() {
            r#"
            SELECT id, task_id, iteration, trigger, state, name, created_at
            FROM orchestrator_checkpoints
            WHERE task_id = ?1
            ORDER BY iteration ASC
            "#
        } else {
            r#"
            SELECT id, task_id, iteration, trigger, state, name, created_at
            FROM orchestrator_checkpoints
            ORDER BY created_at DESC
            LIMIT 100
            "#
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = if let Some(tid) = task_id {
            stmt.query_map(params![tid], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "trigger": row.get::<_, String>(3)?,
                    "state": row.get::<_, String>(4)?.parse::<serde_json::Value>().ok(),
                    "name": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            })
            .map_err(|e| format!("Failed to get checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        } else {
            stmt.query_map([], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "trigger": row.get::<_, String>(3)?,
                    "state": row.get::<_, String>(4)?.parse::<serde_json::Value>().ok(),
                    "name": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            })
            .map_err(|e| format!("Failed to get checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        };

        Ok(results)
    }

    /// Get a single checkpoint by ID
    pub fn get_orchestrator_checkpoint(
        &self,
        id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let result: rusqlite::Result<serde_json::Value> = conn.query_row(
            r#"
            SELECT id, task_id, iteration, trigger, state, name, created_at
            FROM orchestrator_checkpoints
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "trigger": row.get::<_, String>(3)?,
                    "state": row.get::<_, String>(4)?.parse::<serde_json::Value>().ok(),
                    "name": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            },
        );

        match result {
            Ok(cp) => Ok(Some(cp)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get checkpoint: {}", e)),
        }
    }

    /// Delete a checkpoint
    pub fn delete_orchestrator_checkpoint(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let affected = conn
            .execute(
                "DELETE FROM orchestrator_checkpoints WHERE id = ?1",
                params![id],
            )
            .map_err(|e| format!("Failed to delete checkpoint: {}", e))?;

        Ok(affected > 0)
    }

    /// Get all unique task IDs that have checkpoints
    pub fn get_checkpoint_task_ids(&self) -> Result<Vec<String>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare("SELECT DISTINCT task_id FROM orchestrator_checkpoints ORDER BY task_id")
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| format!("Failed to get task IDs: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    // ========================================================================
    // Enhanced Checkpoint Queries (Filtering, Pagination)
    // ========================================================================

    /// Get checkpoints with optional filtering by task ID, trigger type, and date.
    pub fn get_checkpoints_filtered(
        &self,
        task_id: Option<&str>,
        trigger: Option<&str>,
        since: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        // Build dynamic WHERE clause
        let mut conditions = Vec::new();
        if task_id.is_some() {
            conditions.push("task_id = ?1");
        }
        if trigger.is_some() {
            conditions.push("trigger = ?2");
        }
        if since.is_some() {
            conditions.push("created_at >= ?3");
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

        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(
                params![
                    task_id.unwrap_or(""),
                    trigger.unwrap_or(""),
                    since.unwrap_or("")
                ],
                |row| {
                    Ok(serde_json::json!({
                        "id": row.get::<_, String>(0)?,
                        "task_id": row.get::<_, String>(1)?,
                        "iteration": row.get::<_, i64>(2)?,
                        "trigger": row.get::<_, String>(3)?,
                        "state": row.get::<_, String>(4)?.parse::<serde_json::Value>().ok(),
                        "name": row.get::<_, Option<String>>(5)?,
                        "created_at": row.get::<_, String>(6)?,
                    }))
                },
            )
            .map_err(|e| format!("Failed to get checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get checkpoints with pagination support.
    pub fn get_checkpoints_paginated(
        &self,
        task_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let query = if task_id.is_some() {
            r#"
            SELECT id, task_id, iteration, trigger, state, name, created_at
            FROM orchestrator_checkpoints
            WHERE task_id = ?1
            ORDER BY iteration ASC
            LIMIT ?2 OFFSET ?3
            "#
        } else {
            r#"
            SELECT id, task_id, iteration, trigger, state, name, created_at
            FROM orchestrator_checkpoints
            ORDER BY created_at DESC
            LIMIT ?2 OFFSET ?3
            "#
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = if let Some(tid) = task_id {
            stmt.query_map(params![tid, limit, offset], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "trigger": row.get::<_, String>(3)?,
                    "state": row.get::<_, String>(4)?.parse::<serde_json::Value>().ok(),
                    "name": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            })
            .map_err(|e| format!("Failed to get checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        } else {
            stmt.query_map(params!["", limit, offset], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "trigger": row.get::<_, String>(3)?,
                    "state": row.get::<_, String>(4)?.parse::<serde_json::Value>().ok(),
                    "name": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            })
            .map_err(|e| format!("Failed to get checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        };

        Ok(results)
    }

    /// Get total count of checkpoints (for pagination).
    pub fn get_checkpoints_count(&self, task_id: Option<&str>) -> Result<i64, String> {
        let conn = self.get_conn()?;
        if let Some(tid) = task_id {
            conn.query_row(
                "SELECT COUNT(*) FROM orchestrator_checkpoints WHERE task_id = ?1",
                params![tid],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to get count: {}", e))
        } else {
            conn.query_row("SELECT COUNT(*) FROM orchestrator_checkpoints", [], |row| {
                row.get(0)
            })
            .map_err(|e| format!("Failed to get count: {}", e))
        }
    }
}
