//! PostgreSQL workflow execution state operations via Clorinde-generated queries.

use super::PgDb;
use crate::workflow_state::{
    StepCheckpoint, StepCheckpointStatus, StepProgressMarker, WorkflowExecutionStateRecord,
};

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Map a Clorinde step checkpoint row to the domain type.
macro_rules! row_to_step_checkpoint {
    ($r:expr) => {{
        let status = $r.status.parse().unwrap_or(StepCheckpointStatus::Pending);
        StepCheckpoint {
            id: $r.id,
            execution_id: $r.execution_id,
            workflow_type: $r.workflow_type,
            phase: $r.phase,
            iteration: if $r.iteration == 0 {
                None
            } else {
                Some($r.iteration as u32)
            },
            step_index: $r.step_index as usize,
            stage_index: if $r.stage_index == 0 {
                None
            } else {
                Some($r.stage_index as u32)
            },
            step_type: $r.step_type,
            step_name: non_empty($r.step_name),
            status,
            result_json: non_empty($r.result_json),
            step_config_json: non_empty($r.step_config_json),
            started_at: non_empty($r.started_at),
            completed_at: non_empty($r.completed_at),
            duration_ms: if $r.duration_ms == 0 {
                None
            } else {
                Some($r.duration_ms as i64)
            },
            error: non_empty($r.error),
        }
    }};
}

impl PgDb {
    /// Save or update workflow execution state (upsert).
    pub async fn save_workflow_execution_state(
        &self,
        execution_id: &str,
        workflow_type: &str,
        state_name: &str,
        state_data: Option<&str>,
        phase: Option<&str>,
        iteration: Option<u32>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let iter_i32: Option<i32> = iteration.map(|i| i as i32);

        conn.execute(
            r#"INSERT INTO workflow_execution_state
               (execution_id, workflow_type, state_name, state_data, phase, iteration, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, NOW())
               ON CONFLICT (execution_id) DO UPDATE SET
                   workflow_type = $2,
                   state_name = $3,
                   state_data = $4,
                   phase = $5,
                   iteration = $6,
                   updated_at = NOW()"#,
            &[
                &execution_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &workflow_type as &(dyn tokio_postgres::types::ToSql + Sync),
                &state_name as &(dyn tokio_postgres::types::ToSql + Sync),
                &state_data as &(dyn tokio_postgres::types::ToSql + Sync),
                &phase as &(dyn tokio_postgres::types::ToSql + Sync),
                &iter_i32 as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG save_workflow_execution_state: {}", e))?;

        Ok(())
    }

    /// Get workflow execution state.
    pub async fn get_workflow_execution_state(
        &self,
        execution_id: &str,
    ) -> Result<Option<WorkflowExecutionStateRecord>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = qontinui_db::queries::workflow_state::get_workflow_execution_state()
            .bind(&conn, &execution_id)
            .opt()
            .await
            .map_err(|e| format!("PG get_workflow_execution_state: {}", e))?;

        Ok(row.map(|r| WorkflowExecutionStateRecord {
            execution_id: r.execution_id,
            workflow_type: r.workflow_type,
            state_name: r.state_name,
            state_data: non_empty(r.state_data),
            phase: non_empty(r.phase),
            iteration: if r.iteration == 0 {
                None
            } else {
                Some(r.iteration as u32)
            },
            updated_at: r.updated_at.to_rfc3339(),
        }))
    }

    /// Get step checkpoints with cursor-based pagination.
    pub async fn get_workflow_step_checkpoints_paginated(
        &self,
        execution_id: &str,
        cursor: Option<i64>,
        limit: usize,
    ) -> Result<(Vec<StepCheckpoint>, Option<i64>), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let fetch_limit = (limit + 1) as i64; // Fetch one extra to detect more results

        let checkpoints: Vec<StepCheckpoint> = if let Some(cursor_val) = cursor {
            let rows =
                qontinui_db::queries::workflow_state::get_step_checkpoints_paginated_with_cursor()
                    .bind(&conn, &execution_id, &(cursor_val as i32), &fetch_limit)
                    .all()
                    .await
                    .map_err(|e| format!("PG get_step_checkpoints: {}", e))?;
            rows.into_iter()
                .map(|r| row_to_step_checkpoint!(r))
                .collect()
        } else {
            let rows =
                qontinui_db::queries::workflow_state::get_step_checkpoints_paginated_no_cursor()
                    .bind(&conn, &execution_id, &fetch_limit)
                    .all()
                    .await
                    .map_err(|e| format!("PG get_step_checkpoints: {}", e))?;
            rows.into_iter()
                .map(|r| row_to_step_checkpoint!(r))
                .collect()
        };

        // Determine next cursor
        if checkpoints.len() > limit {
            let mut result = checkpoints;
            result.truncate(limit);
            let next_cursor = result.last().map(|cp| cp.step_index as i64);
            Ok((result, next_cursor))
        } else {
            Ok((checkpoints, None))
        }
    }

    /// Get current step progress for an execution.
    pub async fn get_current_step_progress(
        &self,
        execution_id: &str,
    ) -> Result<Option<StepProgressMarker>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Find the running checkpoint
        let running_id = qontinui_db::queries::workflow_state::get_running_checkpoint_id()
            .bind(&conn, &execution_id)
            .opt()
            .await
            .map_err(|e| format!("PG get_running_checkpoint: {}", e))?;

        match running_id {
            Some(checkpoint_id) => {
                let cp_opt: Option<&str> = Some(checkpoint_id.as_str());
                let marker =
                    qontinui_db::queries::workflow_state::get_latest_step_progress_marker()
                        .bind(&conn, &cp_opt)
                        .opt()
                        .await
                        .map_err(|e| format!("PG get_latest_progress: {}", e))?;

                Ok(marker.map(|r| StepProgressMarker {
                    id: r.id,
                    checkpoint_id: r.checkpoint_id,
                    marker_type: r.marker_type,
                    current_value: r.current_value as u64,
                    total_value: if r.total_value == 0 {
                        None
                    } else {
                        Some(r.total_value as u64)
                    },
                    description: non_empty(r.description),
                    data_json: non_empty(r.data_json),
                    created_at: r.created_at.to_rfc3339(),
                }))
            }
            None => Ok(None),
        }
    }

    /// Save a progress marker for a step checkpoint.
    ///
    /// Progress markers track intra-step progress, such as "analyzed 50/100 files".
    /// Returns the generated marker ID.
    pub async fn save_step_progress_marker(
        &self,
        checkpoint_id: &str,
        marker_type: &str,
        current_value: u64,
        total_value: Option<u64>,
        description: Option<&str>,
        data_json: Option<&str>,
    ) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let current_i64 = current_value as i64;
        let total_i64: Option<i64> = total_value.map(|v| v as i64);

        let row = conn
            .query_one(
                r#"INSERT INTO step_progress_markers
                   (checkpoint_id, marker_type, current_value, total_value, description, data_json)
                   VALUES ($1, $2, $3, $4, $5, $6)
                   RETURNING id"#,
                &[
                    &checkpoint_id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &marker_type as &(dyn tokio_postgres::types::ToSql + Sync),
                    &current_i64 as &(dyn tokio_postgres::types::ToSql + Sync),
                    &total_i64 as &(dyn tokio_postgres::types::ToSql + Sync),
                    &description as &(dyn tokio_postgres::types::ToSql + Sync),
                    &data_json as &(dyn tokio_postgres::types::ToSql + Sync),
                ],
            )
            .await
            .map_err(|e| format!("PG save_step_progress_marker: {}", e))?;

        Ok(row.get::<_, i64>(0))
    }

    /// Store (upsert) a verification phase result for a given task run and iteration.
    pub async fn store_verification_phase_result(
        &self,
        task_run_id: &str,
        iteration: u32,
        result: &serde_json::Value,
    ) -> Result<String, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let all_passed = result
            .get("all_passed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let total_steps = result
            .get("total_steps")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as i32;
        let passed_steps = result
            .get("passed_steps")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as i32;
        let failed_steps = result
            .get("failed_steps")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as i32;
        let skipped_steps = result
            .get("skipped_steps")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as i32;
        let total_duration_ms = result
            .get("total_duration_ms")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let critical_failure = result
            .get("critical_failure")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // CR-5: `workflow_verification_phase_results.result_json` is `jsonb`
        // after `PgDb::new()`'s self-bootstrap (`mod.rs` CR-5 block). Bind
        // the `serde_json::Value` directly so tokio-postgres maps it via the
        // `with-serde_json-1` ToSql impl — NOT `to_string(...)` (which would
        // store an escaped string literal and break §5.16 JSONB-path indexes).
        let result_json: &serde_json::Value = result;
        let iter_i32 = iteration as i32;

        // Check for existing record
        let existing = conn
            .query_opt(
                "SELECT id FROM workflow_verification_phase_results WHERE task_run_id = $1 AND iteration = $2",
                &[&task_run_id, &iter_i32],
            )
            .await
            .map_err(|e| format!("PG store_verification_phase_result query: {}", e))?;

        let id = if let Some(row) = existing {
            let existing_id: String = row.get(0);
            conn.execute(
                r#"UPDATE workflow_verification_phase_results
                   SET all_passed = $1, total_steps = $2, passed_steps = $3, failed_steps = $4,
                       skipped_steps = $5, total_duration_ms = $6, critical_failure = $7, result_json = $8
                   WHERE task_run_id = $9 AND iteration = $10"#,
                &[
                    &all_passed, &total_steps, &passed_steps, &failed_steps,
                    &skipped_steps, &total_duration_ms, &critical_failure, &result_json,
                    &task_run_id, &iter_i32,
                ],
            )
            .await
            .map_err(|e| format!("PG store_verification_phase_result update: {}", e))?;
            existing_id
        } else {
            let new_id = uuid::Uuid::new_v4().to_string();
            conn.execute(
                r#"INSERT INTO workflow_verification_phase_results
                   (id, task_run_id, iteration, all_passed, total_steps, passed_steps,
                    failed_steps, skipped_steps, total_duration_ms, critical_failure, result_json)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
                &[
                    &new_id,
                    &task_run_id,
                    &iter_i32,
                    &all_passed,
                    &total_steps,
                    &passed_steps,
                    &failed_steps,
                    &skipped_steps,
                    &total_duration_ms,
                    &critical_failure,
                    &result_json,
                ],
            )
            .await
            .map_err(|e| format!("PG store_verification_phase_result insert: {}", e))?;
            new_id
        };

        tracing::info!(
            "Stored verification phase result for task {} iteration {}: all_passed={}, {}/{} steps",
            task_run_id,
            iteration,
            all_passed,
            passed_steps,
            total_steps
        );
        Ok(id)
    }

    /// Get verification phase result JSON for a specific iteration.
    pub async fn get_verification_phase_result(
        &self,
        task_run_id: &str,
        iteration: u32,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let iter_i32 = iteration as i32;

        let row = conn
            .query_opt(
                "SELECT result_json FROM workflow_verification_phase_results WHERE task_run_id = $1 AND iteration = $2 LIMIT 1",
                &[&task_run_id, &iter_i32],
            )
            .await
            .map_err(|e| format!("PG get_verification_phase_result: {}", e))?;

        match row {
            Some(r) => {
                let json_str: String = r.get(0);
                let parsed: serde_json::Value = serde_json::from_str(&json_str)
                    .map_err(|e| format!("Failed to parse verification result JSON: {}", e))?;
                Ok(Some(parsed))
            }
            None => Ok(None),
        }
    }

    /// Store constraint evaluation results for a task run iteration (upsert: deletes old, inserts new).
    pub async fn store_constraint_results(
        &self,
        task_run_id: &str,
        iteration: u32,
        results: &[crate::constraint_engine::ConstraintResult],
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let iter_i64 = iteration as i64;

        // Delete existing results for this (task_run_id, iteration)
        conn.execute(
            "DELETE FROM workflow_constraint_results WHERE task_run_id = $1 AND iteration = $2",
            &[&task_run_id, &iter_i64],
        )
        .await
        .map_err(|e| format!("PG store_constraint_results delete: {}", e))?;

        for result in results {
            let severity_str = serde_json::to_value(result.severity)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| format!("{:?}", result.severity).to_lowercase());

            let violations_json: Option<String> = if result.violations.is_empty() {
                None
            } else {
                Some(
                    serde_json::to_string(&result.violations)
                        .map_err(|e| format!("Failed to serialize constraint violations: {}", e))?,
                )
            };

            let iter_i32 = iteration as i32;
            conn.execute(
                r#"INSERT INTO workflow_constraint_results
                   (task_run_id, iteration, constraint_id, constraint_name, passed, severity, violations_json)
                   VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
                &[
                    &task_run_id, &iter_i32,
                    &result.constraint_id, &result.constraint_name,
                    &result.passed, &severity_str, &violations_json,
                ],
            )
            .await
            .map_err(|e| format!("PG store_constraint_results insert: {}", e))?;
        }

        let failed_count = results.iter().filter(|r| !r.passed).count();
        tracing::info!(
            "Stored {} constraint results for task {} iteration {} ({} failed)",
            results.len(),
            task_run_id,
            iteration,
            failed_count
        );
        Ok(())
    }

    // =========================================================================
    // Sync wrappers (block_in_place) for CheckpointManager
    // =========================================================================

    /// Save a workflow step checkpoint (sync wrapper).
    pub fn save_workflow_step_checkpoint_sync(
        &self,
        checkpoint: &StepCheckpoint,
    ) -> Result<(), String> {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(self.save_workflow_step_checkpoint_async(checkpoint))
        })
    }

    /// Save a workflow step checkpoint (async).
    pub async fn save_workflow_step_checkpoint_async(
        &self,
        checkpoint: &StepCheckpoint,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let iter_i32: Option<i32> = checkpoint.iteration.map(|i| i as i32);
        let step_index_i32 = checkpoint.step_index as i32;
        let stage_index_i32: Option<i32> = checkpoint.stage_index.map(|i| i as i32);
        let duration_ms_i64: Option<i64> = checkpoint.duration_ms;
        let status_str = checkpoint.status.to_string();

        conn.execute(
            r#"INSERT INTO workflow_step_checkpoints (
                id, execution_id, workflow_type, phase, iteration, step_index,
                stage_index, step_type, step_name, status, result_json, step_config_json,
                started_at, completed_at, duration_ms, error
            ) VALUES ($1, $2, $3, $4, $5, $6, COALESCE($7, 0), $8, $9, $10, $11, $12, $13, $14, $15, $16)
            ON CONFLICT(execution_id, phase, iteration, step_index, stage_index) DO UPDATE SET
                status = $10,
                result_json = $11,
                step_config_json = COALESCE($12, workflow_step_checkpoints.step_config_json),
                started_at = COALESCE($13, workflow_step_checkpoints.started_at),
                completed_at = $14,
                duration_ms = $15,
                error = $16"#,
            &[
                &checkpoint.id as &(dyn tokio_postgres::types::ToSql + Sync),
                &checkpoint.execution_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &checkpoint.workflow_type as &(dyn tokio_postgres::types::ToSql + Sync),
                &checkpoint.phase as &(dyn tokio_postgres::types::ToSql + Sync),
                &iter_i32 as &(dyn tokio_postgres::types::ToSql + Sync),
                &step_index_i32 as &(dyn tokio_postgres::types::ToSql + Sync),
                &stage_index_i32 as &(dyn tokio_postgres::types::ToSql + Sync),
                &checkpoint.step_type as &(dyn tokio_postgres::types::ToSql + Sync),
                &checkpoint.step_name as &(dyn tokio_postgres::types::ToSql + Sync),
                &status_str as &(dyn tokio_postgres::types::ToSql + Sync),
                &checkpoint.result_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &checkpoint.step_config_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &checkpoint.started_at as &(dyn tokio_postgres::types::ToSql + Sync),
                &checkpoint.completed_at as &(dyn tokio_postgres::types::ToSql + Sync),
                &duration_ms_i64 as &(dyn tokio_postgres::types::ToSql + Sync),
                &checkpoint.error as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG save_workflow_step_checkpoint: {}", e))?;

        Ok(())
    }

    /// Get workflow step checkpoints by phase/iteration (sync wrapper).
    pub fn get_workflow_step_checkpoints_sync(
        &self,
        execution_id: &str,
        phase: &str,
        iteration: Option<u32>,
    ) -> Result<Vec<StepCheckpoint>, String> {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.get_workflow_step_checkpoints_by_phase(
                execution_id,
                phase,
                iteration,
            ))
        })
    }

    /// Get workflow step checkpoints by phase and iteration (async).
    pub async fn get_workflow_step_checkpoints_by_phase(
        &self,
        execution_id: &str,
        phase: &str,
        iteration: Option<u32>,
    ) -> Result<Vec<StepCheckpoint>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let iter_i32: Option<i32> = iteration.map(|i| i as i32);

        // Cast TIMESTAMPTZ columns to TEXT so they deserialize into String without
        // panicking on driver-type drift (same pattern as step_type_knowledge.rs fix).
        let rows = if let Some(iter_val) = iter_i32 {
            conn.query(
                r#"SELECT id, execution_id, workflow_type, phase, iteration, step_index,
                          stage_index, step_type, step_name, status, result_json, step_config_json,
                          started_at::TEXT, completed_at::TEXT, duration_ms, error
                   FROM workflow_step_checkpoints
                   WHERE execution_id = $1 AND phase = $2 AND iteration = $3
                   ORDER BY step_index ASC"#,
                &[&execution_id, &phase, &iter_val],
            )
            .await
        } else {
            conn.query(
                r#"SELECT id, execution_id, workflow_type, phase, iteration, step_index,
                          stage_index, step_type, step_name, status, result_json, step_config_json,
                          started_at::TEXT, completed_at::TEXT, duration_ms, error
                   FROM workflow_step_checkpoints
                   WHERE execution_id = $1 AND phase = $2 AND iteration IS NULL
                   ORDER BY step_index ASC"#,
                &[&execution_id, &phase],
            )
            .await
        }
        .map_err(|e| format!("PG get_workflow_step_checkpoints_by_phase: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| {
                // try_get so a future column-type drift warns instead of panicking the row mapper.
                let status_str: String = r.try_get(9).unwrap_or_else(|e| {
                    tracing::warn!("workflow_step_checkpoints status decode drift: {}", e);
                    String::new()
                });
                let status = status_str.parse().unwrap_or(StepCheckpointStatus::Pending);
                let iter_raw: Option<i32> = r.get(4);
                let step_idx: i32 = r.get(5);
                let stage_idx: Option<i32> = r.get(6);
                let dur: Option<i64> = r.get(14);

                StepCheckpoint {
                    id: r.get(0),
                    execution_id: r.get(1),
                    workflow_type: r.get(2),
                    phase: r.get(3),
                    iteration: iter_raw.map(|i| i as u32),
                    step_index: step_idx as usize,
                    stage_index: stage_idx.map(|i| i as u32),
                    step_type: r.get(7),
                    step_name: r.get(8),
                    status,
                    result_json: r.get(10),
                    step_config_json: r.get(11),
                    started_at: r.try_get(12).unwrap_or_else(|e| {
                        tracing::warn!("started_at decode drift: {}", e);
                        None
                    }),
                    completed_at: r.try_get(13).unwrap_or_else(|e| {
                        tracing::warn!("completed_at decode drift: {}", e);
                        None
                    }),
                    duration_ms: dur,
                    error: r.get(15),
                }
            })
            .collect())
    }

    /// Get all workflow step checkpoints for an execution (async).
    pub async fn get_all_workflow_step_checkpoints(
        &self,
        execution_id: &str,
    ) -> Result<Vec<StepCheckpoint>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        // Cast TIMESTAMPTZ columns to TEXT + try_get below to guard against
        // driver-type drift (same pattern as step_type_knowledge.rs fix).
        let rows = conn
            .query(
                r#"SELECT id, execution_id, workflow_type, phase, iteration, step_index,
                      stage_index, step_type, step_name, status, result_json, step_config_json,
                      started_at::TEXT, completed_at::TEXT, duration_ms, error
               FROM workflow_step_checkpoints
               WHERE execution_id = $1
               ORDER BY step_index ASC"#,
                &[&execution_id],
            )
            .await
            .map_err(|e| format!("PG get_all_workflow_step_checkpoints: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let status_str: String = r.try_get(9).unwrap_or_else(|e| {
                    tracing::warn!("workflow_step_checkpoints status decode drift: {}", e);
                    String::new()
                });
                let status = status_str.parse().unwrap_or(StepCheckpointStatus::Pending);
                let iter_raw: Option<i32> = r.get(4);
                let step_idx: i32 = r.get(5);
                let stage_idx: Option<i32> = r.get(6);
                let dur: Option<i64> = r.get(14);

                StepCheckpoint {
                    id: r.get(0),
                    execution_id: r.get(1),
                    workflow_type: r.get(2),
                    phase: r.get(3),
                    iteration: iter_raw.map(|i| i as u32),
                    step_index: step_idx as usize,
                    stage_index: stage_idx.map(|i| i as u32),
                    step_type: r.get(7),
                    step_name: r.get(8),
                    status,
                    result_json: r.get(10),
                    step_config_json: r.get(11),
                    started_at: r.try_get(12).unwrap_or_else(|e| {
                        tracing::warn!("started_at decode drift: {}", e);
                        None
                    }),
                    completed_at: r.try_get(13).unwrap_or_else(|e| {
                        tracing::warn!("completed_at decode drift: {}", e);
                        None
                    }),
                    duration_ms: dur,
                    error: r.get(15),
                }
            })
            .collect())
    }

    /// Delete workflow step checkpoints (sync wrapper).
    pub fn delete_workflow_step_checkpoints_sync(
        &self,
        execution_id: &str,
        phase: Option<&str>,
        iteration: Option<u32>,
    ) -> Result<(), String> {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.delete_workflow_step_checkpoints_async(
                execution_id,
                phase,
                iteration,
            ))
        })
    }

    /// Delete workflow step checkpoints (async).
    pub async fn delete_workflow_step_checkpoints_async(
        &self,
        execution_id: &str,
        phase: Option<&str>,
        iteration: Option<u32>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let iter_i32: Option<i32> = iteration.map(|i| i as i32);

        match (phase, iter_i32) {
            (Some(p), Some(i)) => {
                conn.execute(
                    "DELETE FROM workflow_step_checkpoints WHERE execution_id = $1 AND phase = $2 AND iteration = $3",
                    &[&execution_id as &(dyn tokio_postgres::types::ToSql + Sync),
                      &p as &(dyn tokio_postgres::types::ToSql + Sync),
                      &i as &(dyn tokio_postgres::types::ToSql + Sync)],
                ).await
            }
            (Some(p), None) => {
                conn.execute(
                    "DELETE FROM workflow_step_checkpoints WHERE execution_id = $1 AND phase = $2",
                    &[&execution_id as &(dyn tokio_postgres::types::ToSql + Sync),
                      &p as &(dyn tokio_postgres::types::ToSql + Sync)],
                ).await
            }
            (None, _) => {
                conn.execute(
                    "DELETE FROM workflow_step_checkpoints WHERE execution_id = $1",
                    &[&execution_id as &(dyn tokio_postgres::types::ToSql + Sync)],
                ).await
            }
        }
        .map_err(|e| format!("PG delete_workflow_step_checkpoints: {}", e))?;

        Ok(())
    }
}
