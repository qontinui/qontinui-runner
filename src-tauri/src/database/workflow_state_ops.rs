//! Workflow state management operations: execution state, step checkpoints, and progress markers.
//!
//! Contains all CheckpointDb methods related to workflow state tracking.

use chrono::Utc;
use rusqlite::{params, OptionalExtension};

use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Workflow Execution State Operations
    // ========================================================================

    /// Save or update workflow execution state.
    pub fn save_workflow_execution_state(
        &self,
        execution_id: &str,
        workflow_type: &str,
        state_name: &str,
        state_data: Option<&str>,
        phase: Option<&str>,
        iteration: Option<u32>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO workflow_execution_state (
                execution_id, workflow_type, state_name, state_data, phase, iteration, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(execution_id) DO UPDATE SET
                workflow_type = ?2,
                state_name = ?3,
                state_data = ?4,
                phase = ?5,
                iteration = ?6,
                updated_at = ?7
            "#,
            params![
                execution_id,
                workflow_type,
                state_name,
                state_data,
                phase,
                iteration.map(|i| i as i64),
                now,
            ],
        )
        .map_err(|e| format!("Failed to save workflow execution state: {}", e))?;

        Ok(())
    }

    /// Get workflow execution state by execution_id.
    pub fn get_workflow_execution_state(
        &self,
        execution_id: &str,
    ) -> Result<Option<crate::workflow_state::WorkflowExecutionStateRecord>, String> {
        let conn = self.get_conn()?;

        let result = conn
            .query_row(
                r#"
                SELECT execution_id, workflow_type, state_name, state_data, phase, iteration, updated_at
                FROM workflow_execution_state
                WHERE execution_id = ?1
                "#,
                params![execution_id],
                |row| {
                    Ok(crate::workflow_state::WorkflowExecutionStateRecord {
                        execution_id: row.get(0)?,
                        workflow_type: row.get(1)?,
                        state_name: row.get(2)?,
                        state_data: row.get(3)?,
                        phase: row.get(4)?,
                        iteration: row.get::<_, Option<i64>>(5)?.map(|i| i as u32),
                        updated_at: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(|e| format!("Failed to get workflow execution state: {}", e))?;

        Ok(result)
    }

    /// Delete workflow execution state.
    pub fn delete_workflow_execution_state(&self, execution_id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;

        conn.execute(
            "DELETE FROM workflow_execution_state WHERE execution_id = ?1",
            params![execution_id],
        )
        .map_err(|e| format!("Failed to delete workflow execution state: {}", e))?;

        Ok(())
    }

    // ========================================================================
    // Workflow Step Checkpoint Operations
    // ========================================================================

    /// Save or update a workflow step checkpoint.
    pub fn save_workflow_step_checkpoint(
        &self,
        checkpoint: &crate::workflow_state::StepCheckpoint,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;

        conn.execute(
            r#"
            INSERT INTO workflow_step_checkpoints (
                id, execution_id, workflow_type, phase, iteration, step_index,
                stage_index, step_type, step_name, status, result_json, step_config_json,
                started_at, completed_at, duration_ms, error
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, COALESCE(?7, 0), ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
            ON CONFLICT(execution_id, phase, iteration, step_index, stage_index) DO UPDATE SET
                status = ?10,
                result_json = ?11,
                step_config_json = COALESCE(?12, step_config_json),
                started_at = COALESCE(?13, started_at),
                completed_at = ?14,
                duration_ms = ?15,
                error = ?16
            "#,
            params![
                checkpoint.id,
                checkpoint.execution_id,
                checkpoint.workflow_type,
                checkpoint.phase,
                checkpoint.iteration.map(|i| i as i64),
                checkpoint.step_index as i64,
                checkpoint.stage_index.map(|i| i as i64),
                checkpoint.step_type,
                checkpoint.step_name,
                checkpoint.status.to_string(),
                checkpoint.result_json,
                checkpoint.step_config_json,
                checkpoint.started_at,
                checkpoint.completed_at,
                checkpoint.duration_ms,
                checkpoint.error,
            ],
        )
        .map_err(|e| format!("Failed to save workflow step checkpoint: {}", e))?;

        Ok(())
    }

    /// Atomically save both workflow execution state and a step checkpoint.
    ///
    /// This is critical for ensuring data consistency when a step completes and the
    /// workflow state advances. If either operation fails, both are rolled back.
    ///
    /// # Arguments
    /// * `execution_id` - The execution/task run ID
    /// * `workflow_type` - Type of workflow (e.g., "unified")
    /// * `state_name` - Name of the new workflow state
    /// * `state_data` - Serialized state data (JSON)
    /// * `phase` - Current phase name
    /// * `iteration` - Current iteration number
    /// * `checkpoint` - The step checkpoint to save
    pub fn save_state_and_checkpoint_atomic(
        &self,
        execution_id: &str,
        workflow_type: &str,
        state_name: &str,
        state_data: Option<&str>,
        phase: Option<&str>,
        iteration: Option<u32>,
        checkpoint: &crate::workflow_state::StepCheckpoint,
    ) -> Result<(), String> {
        self.transaction(|conn| {
            let now = chrono::Utc::now().to_rfc3339();

            // Save workflow execution state
            conn.execute(
                r#"
                INSERT INTO workflow_execution_state (
                    execution_id, workflow_type, state_name, state_data, phase, iteration, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(execution_id) DO UPDATE SET
                    workflow_type = ?2,
                    state_name = ?3,
                    state_data = ?4,
                    phase = ?5,
                    iteration = ?6,
                    updated_at = ?7
                "#,
                params![
                    execution_id,
                    workflow_type,
                    state_name,
                    state_data,
                    phase,
                    iteration.map(|i| i as i64),
                    now,
                ],
            )?;

            // Save step checkpoint
            conn.execute(
                r#"
                INSERT INTO workflow_step_checkpoints (
                    id, execution_id, workflow_type, phase, iteration, step_index,
                    stage_index, step_type, step_name, status, result_json, step_config_json,
                    started_at, completed_at, duration_ms, error
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, COALESCE(?7, 0), ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
                ON CONFLICT(execution_id, phase, iteration, step_index, stage_index) DO UPDATE SET
                    status = ?10,
                    result_json = ?11,
                    step_config_json = COALESCE(?12, step_config_json),
                    started_at = COALESCE(?13, started_at),
                    completed_at = ?14,
                    duration_ms = ?15,
                    error = ?16
                "#,
                params![
                    checkpoint.id,
                    checkpoint.execution_id,
                    checkpoint.workflow_type,
                    checkpoint.phase,
                    checkpoint.iteration.map(|i| i as i64),
                    checkpoint.step_index as i64,
                    checkpoint.stage_index.map(|i| i as i64),
                    checkpoint.step_type,
                    checkpoint.step_name,
                    checkpoint.status.to_string(),
                    checkpoint.result_json,
                    checkpoint.step_config_json,
                    checkpoint.started_at,
                    checkpoint.completed_at,
                    checkpoint.duration_ms,
                    checkpoint.error,
                ],
            )?;

            Ok(())
        })
    }

    /// Get workflow step checkpoints for a given execution, phase, and iteration.
    pub fn get_workflow_step_checkpoints(
        &self,
        execution_id: &str,
        phase: &str,
        iteration: Option<u32>,
    ) -> Result<Vec<crate::workflow_state::StepCheckpoint>, String> {
        let conn = self.get_conn()?;

        let query = if iteration.is_some() {
            r#"
            SELECT id, execution_id, workflow_type, phase, iteration, step_index,
                   stage_index, step_type, step_name, status, result_json, step_config_json,
                   started_at, completed_at, duration_ms, error
            FROM workflow_step_checkpoints
            WHERE execution_id = ?1 AND phase = ?2 AND iteration = ?3
            ORDER BY step_index ASC
            "#
        } else {
            r#"
            SELECT id, execution_id, workflow_type, phase, iteration, step_index,
                   stage_index, step_type, step_name, status, result_json, step_config_json,
                   started_at, completed_at, duration_ms, error
            FROM workflow_step_checkpoints
            WHERE execution_id = ?1 AND phase = ?2 AND iteration IS NULL
            ORDER BY step_index ASC
            "#
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let row_mapper =
            |row: &rusqlite::Row| -> rusqlite::Result<crate::workflow_state::StepCheckpoint> {
                let status_str: String = row.get(9)?;
                let status = status_str
                    .parse()
                    .unwrap_or(crate::workflow_state::StepCheckpointStatus::Pending);

                Ok(crate::workflow_state::StepCheckpoint {
                    id: row.get(0)?,
                    execution_id: row.get(1)?,
                    workflow_type: row.get(2)?,
                    phase: row.get(3)?,
                    iteration: row.get::<_, Option<i64>>(4)?.map(|i| i as u32),
                    step_index: row.get::<_, i64>(5)? as usize,
                    stage_index: row.get::<_, Option<i64>>(6)?.map(|i| i as u32),
                    step_type: row.get(7)?,
                    step_name: row.get(8)?,
                    status,
                    result_json: row.get(10)?,
                    step_config_json: row.get(11)?,
                    started_at: row.get(12)?,
                    completed_at: row.get(13)?,
                    duration_ms: row.get(14)?,
                    error: row.get(15)?,
                })
            };

        let checkpoints: Vec<crate::workflow_state::StepCheckpoint> = if let Some(iter) = iteration
        {
            stmt.query_map(params![execution_id, phase, iter as i64], row_mapper)
                .map_err(|e| format!("Failed to get step checkpoints: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            stmt.query_map(params![execution_id, phase], row_mapper)
                .map_err(|e| format!("Failed to get step checkpoints: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        };

        Ok(checkpoints)
    }

    /// Get workflow step checkpoints with cursor-based pagination.
    ///
    /// This is optimized for handling runs with 1000+ steps without loading all data at once.
    /// Uses step_index as the cursor for efficient pagination.
    ///
    /// # Arguments
    /// * `execution_id` - The execution/task run ID
    /// * `cursor` - Optional step_index to start from (exclusive). None means start from beginning.
    /// * `limit` - Maximum number of checkpoints to return
    ///
    /// # Returns
    /// A tuple of (checkpoints, next_cursor). If next_cursor is Some, there are more results.
    pub fn get_workflow_step_checkpoints_paginated(
        &self,
        execution_id: &str,
        cursor: Option<i64>,
        limit: usize,
    ) -> Result<(Vec<crate::workflow_state::StepCheckpoint>, Option<i64>), String> {
        let conn = self.get_conn()?;

        // Use cursor-based pagination for efficiency
        // The idx_step_checkpoints_cursor index on (execution_id, step_index) makes this fast
        let query = if cursor.is_some() {
            r#"
            SELECT id, execution_id, workflow_type, phase, iteration, step_index,
                   stage_index, step_type, step_name, status, result_json, step_config_json,
                   started_at, completed_at, duration_ms, error
            FROM workflow_step_checkpoints
            WHERE execution_id = ?1 AND step_index > ?2
            ORDER BY step_index ASC
            LIMIT ?3
            "#
        } else {
            r#"
            SELECT id, execution_id, workflow_type, phase, iteration, step_index,
                   stage_index, step_type, step_name, status, result_json, step_config_json,
                   started_at, completed_at, duration_ms, error
            FROM workflow_step_checkpoints
            WHERE execution_id = ?1
            ORDER BY step_index ASC
            LIMIT ?2
            "#
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let row_mapper =
            |row: &rusqlite::Row| -> rusqlite::Result<crate::workflow_state::StepCheckpoint> {
                let status_str: String = row.get(9)?;
                let status = status_str
                    .parse()
                    .unwrap_or(crate::workflow_state::StepCheckpointStatus::Pending);

                Ok(crate::workflow_state::StepCheckpoint {
                    id: row.get(0)?,
                    execution_id: row.get(1)?,
                    workflow_type: row.get(2)?,
                    phase: row.get(3)?,
                    iteration: row.get::<_, Option<i64>>(4)?.map(|i| i as u32),
                    step_index: row.get::<_, i64>(5)? as usize,
                    stage_index: row.get::<_, Option<i64>>(6)?.map(|i| i as u32),
                    step_type: row.get(7)?,
                    step_name: row.get(8)?,
                    status,
                    result_json: row.get(10)?,
                    step_config_json: row.get(11)?,
                    started_at: row.get(12)?,
                    completed_at: row.get(13)?,
                    duration_ms: row.get(14)?,
                    error: row.get(15)?,
                })
            };

        // Request one more than limit to check if there are more results
        let fetch_limit = (limit + 1) as i64;

        let checkpoints: Vec<crate::workflow_state::StepCheckpoint> =
            if let Some(cursor_val) = cursor {
                stmt.query_map(params![execution_id, cursor_val, fetch_limit], row_mapper)
                    .map_err(|e| format!("Failed to get step checkpoints: {}", e))?
                    .filter_map(|r| r.ok())
                    .collect()
            } else {
                stmt.query_map(params![execution_id, fetch_limit], row_mapper)
                    .map_err(|e| format!("Failed to get step checkpoints: {}", e))?
                    .filter_map(|r| r.ok())
                    .collect()
            };

        // Determine if there are more results and what the next cursor should be
        let (result_checkpoints, next_cursor) = if checkpoints.len() > limit {
            // There are more results; return only `limit` items
            let mut result = checkpoints;
            result.truncate(limit);
            let last_step_index = result.last().map(|cp| cp.step_index as i64);
            (result, last_step_index)
        } else {
            // No more results
            (checkpoints, None)
        };

        Ok((result_checkpoints, next_cursor))
    }

    /// Delete workflow step checkpoints.
    pub fn delete_workflow_step_checkpoints(
        &self,
        execution_id: &str,
        phase: Option<&str>,
        iteration: Option<u32>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;

        match (phase, iteration) {
            (Some(p), Some(i)) => {
                conn.execute(
                    "DELETE FROM workflow_step_checkpoints WHERE execution_id = ?1 AND phase = ?2 AND iteration = ?3",
                    params![execution_id, p, i as i64],
                )
            }
            (Some(p), None) => {
                conn.execute(
                    "DELETE FROM workflow_step_checkpoints WHERE execution_id = ?1 AND phase = ?2",
                    params![execution_id, p],
                )
            }
            (None, _) => {
                conn.execute(
                    "DELETE FROM workflow_step_checkpoints WHERE execution_id = ?1",
                    params![execution_id],
                )
            }
        }
        .map_err(|e| format!("Failed to delete step checkpoints: {}", e))?;

        Ok(())
    }

    // ========================================================================
    // Step Progress Marker Operations
    // ========================================================================

    /// Save a progress marker for a step checkpoint.
    ///
    /// Progress markers track intra-step progress, such as "analyzed 50/100 files".
    /// This is useful for long-running AI operations where you want to show progress
    /// and enable resume from the last known position.
    pub fn save_step_progress_marker(
        &self,
        checkpoint_id: &str,
        marker_type: &str,
        current_value: u64,
        total_value: Option<u64>,
        description: Option<&str>,
        data_json: Option<&str>,
    ) -> Result<i64, String> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO step_progress_markers (
                checkpoint_id, marker_type, current_value, total_value,
                description, data_json, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![
                checkpoint_id,
                marker_type,
                current_value as i64,
                total_value.map(|v| v as i64),
                description,
                data_json,
                now,
            ],
        )
        .map_err(|e| format!("Failed to save step progress marker: {}", e))?;

        Ok(conn.last_insert_rowid())
    }

    /// Get the latest progress marker for a step checkpoint.
    ///
    /// Returns the most recent progress marker for the given checkpoint_id,
    /// which can be used to resume from the last known position.
    pub fn get_latest_step_progress_marker(
        &self,
        checkpoint_id: &str,
    ) -> Result<Option<crate::workflow_state::StepProgressMarker>, String> {
        let conn = self.get_conn()?;

        let result = conn.query_row(
            r#"
                SELECT id, checkpoint_id, marker_type, current_value, total_value,
                       description, data_json, created_at
                FROM step_progress_markers
                WHERE checkpoint_id = ?1
                ORDER BY id DESC
                LIMIT 1
                "#,
            params![checkpoint_id],
            |row| {
                Ok(crate::workflow_state::StepProgressMarker {
                    id: row.get(0)?,
                    checkpoint_id: row.get(1)?,
                    marker_type: row.get(2)?,
                    current_value: row.get::<_, i64>(3)? as u64,
                    total_value: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                    description: row.get(5)?,
                    data_json: row.get(6)?,
                    created_at: row.get(7)?,
                })
            },
        );

        match result {
            Ok(marker) => Ok(Some(marker)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get step progress marker: {}", e)),
        }
    }

    /// Get all progress markers for a step checkpoint.
    ///
    /// Returns all progress markers in order of creation (oldest first).
    pub fn get_step_progress_markers(
        &self,
        checkpoint_id: &str,
    ) -> Result<Vec<crate::workflow_state::StepProgressMarker>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, checkpoint_id, marker_type, current_value, total_value,
                       description, data_json, created_at
                FROM step_progress_markers
                WHERE checkpoint_id = ?1
                ORDER BY id ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let markers = stmt
            .query_map(params![checkpoint_id], |row| {
                Ok(crate::workflow_state::StepProgressMarker {
                    id: row.get(0)?,
                    checkpoint_id: row.get(1)?,
                    marker_type: row.get(2)?,
                    current_value: row.get::<_, i64>(3)? as u64,
                    total_value: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                    description: row.get(5)?,
                    data_json: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })
            .map_err(|e| format!("Failed to get step progress markers: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(markers)
    }

    /// Delete all progress markers for a step checkpoint.
    pub fn delete_step_progress_markers(&self, checkpoint_id: &str) -> Result<usize, String> {
        let conn = self.get_conn()?;

        let deleted = conn
            .execute(
                "DELETE FROM step_progress_markers WHERE checkpoint_id = ?1",
                params![checkpoint_id],
            )
            .map_err(|e| format!("Failed to delete step progress markers: {}", e))?;

        Ok(deleted)
    }

    // ========================================================================
    // Full Workflow State (for frontend restart recovery)
    // ========================================================================

    /// Get all workflow step checkpoints for an execution (all phases).
    ///
    /// This is used by the full-state endpoint to return all checkpoints for restart recovery.
    pub fn get_all_workflow_step_checkpoints(
        &self,
        execution_id: &str,
    ) -> Result<Vec<crate::workflow_state::StepCheckpoint>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, execution_id, workflow_type, phase, iteration, step_index,
                       stage_index, step_type, step_name, status, result_json, step_config_json,
                       started_at, completed_at, duration_ms, error
                FROM workflow_step_checkpoints
                WHERE execution_id = ?1
                ORDER BY COALESCE(stage_index, 0), phase, COALESCE(iteration, 0), step_index ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let row_mapper =
            |row: &rusqlite::Row| -> rusqlite::Result<crate::workflow_state::StepCheckpoint> {
                let status_str: String = row.get(9)?;
                let status = status_str
                    .parse()
                    .unwrap_or(crate::workflow_state::StepCheckpointStatus::Pending);

                Ok(crate::workflow_state::StepCheckpoint {
                    id: row.get(0)?,
                    execution_id: row.get(1)?,
                    workflow_type: row.get(2)?,
                    phase: row.get(3)?,
                    iteration: row.get::<_, Option<i64>>(4)?.map(|i| i as u32),
                    step_index: row.get::<_, i64>(5)? as usize,
                    stage_index: row.get::<_, Option<i64>>(6)?.map(|i| i as u32),
                    step_type: row.get(7)?,
                    step_name: row.get(8)?,
                    status,
                    result_json: row.get(10)?,
                    step_config_json: row.get(11)?,
                    started_at: row.get(12)?,
                    completed_at: row.get(13)?,
                    duration_ms: row.get(14)?,
                    error: row.get(15)?,
                })
            };

        let checkpoints: Vec<crate::workflow_state::StepCheckpoint> = stmt
            .query_map(params![execution_id], row_mapper)
            .map_err(|e| format!("Failed to get all step checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(checkpoints)
    }

    /// Get the progress marker for the currently running step (if any).
    ///
    /// Finds the checkpoint that is in "running" status and returns its latest progress marker.
    pub fn get_current_step_progress(
        &self,
        execution_id: &str,
    ) -> Result<Option<crate::workflow_state::StepProgressMarker>, String> {
        let conn = self.get_conn()?;

        // First find the running checkpoint
        let running_checkpoint_id: Option<String> = conn
            .query_row(
                r#"
                SELECT id FROM workflow_step_checkpoints
                WHERE execution_id = ?1 AND status = 'running'
                ORDER BY step_index DESC
                LIMIT 1
                "#,
                params![execution_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("Failed to find running checkpoint: {}", e))?;

        match running_checkpoint_id {
            Some(checkpoint_id) => self.get_latest_step_progress_marker(&checkpoint_id),
            None => Ok(None),
        }
    }
}
