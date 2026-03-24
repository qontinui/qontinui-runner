//! Scheduler DB operations on CheckpointDb.
//!
//! Replaces the JSON-file-based scheduler persistence with SQLite-backed
//! CRUD for scheduled_tasks, scheduler_history, and scheduler_settings.

use rusqlite::{params, OptionalExtension};
use tracing::warn;

use super::CheckpointDb;
use crate::scheduler::{
    ScheduleExpression, ScheduledTask, ScheduledTaskStatus, ScheduledTaskType, SchedulerSettings,
    TaskExecutionRecord,
};

// ============================================================================
// Helper: row → ScheduledTask
// ============================================================================

/// Map a rusqlite Row to a ScheduledTask.
///
/// Expected column order (by index) — matches the actual table schema:
///  0  id
///  1  name
///  2  description
///  3  enabled
///  4  schedule_type
///  5  schedule_value
///  6  task_config        (JSON)
///  7  skip_if_completed
///  8  auto_fix_on_failure
///  9  success_criteria
/// 10  created_at
/// 11  modified_at
/// 12  next_run
/// 13  last_run_id
fn row_to_scheduled_task(row: &rusqlite::Row) -> rusqlite::Result<ScheduledTask> {
    let schedule_type: String = row.get(4)?;
    let schedule_value: String = row.get(5)?;
    let task_config_json: String = row.get(6)?;

    // Strip surrounding quotes from schedule_value if present (legacy data)
    let schedule_value = schedule_value.trim_matches('"').to_string();

    let schedule = match schedule_type.to_lowercase().as_str() {
        "cron" => ScheduleExpression::Cron(schedule_value),
        "once" => ScheduleExpression::Once(schedule_value),
        "interval" => {
            let secs: u64 = match schedule_value.parse() {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        "Invalid schedule interval value '{}': {}; defaulting to 60s",
                        schedule_value, e
                    );
                    60
                }
            };
            ScheduleExpression::Interval(secs)
        }
        "condition" => {
            let config: crate::scheduler::ConditionScheduleConfig =
                serde_json::from_str(&schedule_value).unwrap_or_default();
            ScheduleExpression::Condition(config)
        }
        _ => ScheduleExpression::Once(schedule_value),
    };

    let task: ScheduledTaskType = match serde_json::from_str(&task_config_json) {
        Ok(t) => t,
        Err(e) => {
            warn!(
                "Failed to deserialize task config '{}': {}; using default",
                task_config_json, e
            );
            ScheduledTaskType::default()
        }
    };

    Ok(ScheduledTask {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        enabled: row.get(3)?,
        schedule,
        task,
        skip_if_completed: row.get(7)?,
        auto_fix_on_failure: row.get(8)?,
        success_criteria: row.get(9)?,
        created_at: row.get(10)?,
        modified_at: row.get(11)?,
        next_run: row.get(12)?,
        // last_run is not stored inline; the caller can look it up via history
        // We store last_run_id and leave last_run as None here.
        last_run: None,
        conditions: None,
        condition_status: None,
    })
}

// ============================================================================
// Helper: ScheduleExpression → (type, value)
// ============================================================================

fn schedule_to_parts(schedule: &ScheduleExpression) -> (&'static str, String) {
    match schedule {
        ScheduleExpression::Cron(s) => ("cron", s.clone()),
        ScheduleExpression::Once(s) => ("once", s.clone()),
        ScheduleExpression::Interval(n) => ("interval", n.to_string()),
        ScheduleExpression::Condition(config) => (
            "condition",
            serde_json::to_string(config).unwrap_or_else(|_| "{}".to_string()),
        ),
    }
}

const SELECT_TASK_COLS: &str = r#"
    id, name, description, enabled,
    schedule_type, schedule_value, task_config,
    skip_if_completed, auto_fix_on_failure, success_criteria,
    created_at, modified_at, next_run, last_run_id
"#;

// ============================================================================
// impl CheckpointDb — Task CRUD
// ============================================================================

impl CheckpointDb {
    // -----------------------------------------------------------------------
    // Task CRUD
    // -----------------------------------------------------------------------

    /// Retrieve all scheduled tasks.
    pub fn get_all_scheduled_tasks(&self) -> Result<Vec<ScheduledTask>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {} FROM scheduled_tasks ORDER BY created_at",
                SELECT_TASK_COLS
            ))
            .map_err(|e| format!("Failed to prepare get_all_scheduled_tasks: {e}"))?;

        let tasks = stmt
            .query_map([], row_to_scheduled_task)
            .map_err(|e| format!("Failed to query scheduled_tasks: {e}"))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(tasks)
    }

    /// Retrieve a single scheduled task by ID, or None if not found.
    pub fn get_scheduled_task(&self, id: &str) -> Result<Option<ScheduledTask>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {} FROM scheduled_tasks WHERE id = ?1",
                SELECT_TASK_COLS
            ))
            .map_err(|e| format!("Failed to prepare get_scheduled_task: {e}"))?;

        let task = stmt
            .query_row(params![id], row_to_scheduled_task)
            .optional()
            .map_err(|e| format!("Failed to query scheduled_task {id}: {e}"))?;

        Ok(task)
    }

    /// Insert a new scheduled task.
    pub fn insert_scheduled_task(&self, task: &ScheduledTask) -> Result<(), String> {
        let conn = self.get_conn()?;
        let (stype, svalue) = schedule_to_parts(&task.schedule);
        let task_config =
            serde_json::to_string(&task.task).map_err(|e| format!("Serialize task_config: {e}"))?;
        let last_run_id = task.last_run.as_ref().map(|r| r.execution_id.clone());

        conn.execute(
            r#"
            INSERT INTO scheduled_tasks
                (id, name, description, enabled,
                 schedule_type, schedule_value, task_config,
                 skip_if_completed, auto_fix_on_failure, success_criteria,
                 created_at, modified_at, next_run, last_run_id)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            "#,
            params![
                task.id,
                task.name,
                task.description,
                task.enabled,
                stype,
                svalue,
                task_config,
                task.skip_if_completed,
                task.auto_fix_on_failure,
                task.success_criteria,
                task.created_at,
                task.modified_at,
                task.next_run,
                last_run_id,
            ],
        )
        .map_err(|e| format!("Failed to insert scheduled_task {}: {e}", task.id))?;

        Ok(())
    }

    /// Update an existing scheduled task (full replacement of mutable fields).
    pub fn update_scheduled_task(&self, task: &ScheduledTask) -> Result<(), String> {
        let conn = self.get_conn()?;
        let (stype, svalue) = schedule_to_parts(&task.schedule);
        let task_config =
            serde_json::to_string(&task.task).map_err(|e| format!("Serialize task_config: {e}"))?;
        let last_run_id = task.last_run.as_ref().map(|r| r.execution_id.clone());

        conn.execute(
            r#"
            UPDATE scheduled_tasks SET
                name = ?1,
                description = ?2,
                enabled = ?3,
                schedule_type = ?4,
                schedule_value = ?5,
                task_config = ?6,
                skip_if_completed = ?7,
                auto_fix_on_failure = ?8,
                success_criteria = ?9,
                modified_at = ?10,
                next_run = ?11,
                last_run_id = ?12
            WHERE id = ?13
            "#,
            params![
                task.name,
                task.description,
                task.enabled,
                stype,
                svalue,
                task_config,
                task.skip_if_completed,
                task.auto_fix_on_failure,
                task.success_criteria,
                task.modified_at,
                task.next_run,
                last_run_id,
                task.id,
            ],
        )
        .map_err(|e| format!("Failed to update scheduled_task {}: {e}", task.id))?;

        Ok(())
    }

    /// Delete a scheduled task by ID. History is cascade-deleted by FK.
    pub fn delete_scheduled_task(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute("DELETE FROM scheduled_tasks WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete scheduled_task {id}: {e}"))?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Execution helpers (single-column updates)
    // -----------------------------------------------------------------------

    /// Set or clear the last_run_id on a task.
    pub fn update_task_last_run(
        &self,
        task_id: &str,
        last_run_id: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE scheduled_tasks SET last_run_id = ?1, modified_at = ?2 WHERE id = ?3",
            params![last_run_id, chrono::Utc::now().to_rfc3339(), task_id],
        )
        .map_err(|e| format!("Failed to update last_run_id for {task_id}: {e}"))?;
        Ok(())
    }

    /// Set or clear the next_run timestamp on a task.
    pub fn update_task_next_run(
        &self,
        task_id: &str,
        next_run: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE scheduled_tasks SET next_run = ?1, modified_at = ?2 WHERE id = ?3",
            params![next_run, chrono::Utc::now().to_rfc3339(), task_id],
        )
        .map_err(|e| format!("Failed to update next_run for {task_id}: {e}"))?;
        Ok(())
    }

    /// Set or clear the condition_status JSON on a task.
    ///
    /// Note: The `condition_status` column may not exist in older DB schemas.
    /// This is a transient state that resets on restart, so failure is tolerable.
    pub fn update_task_condition_status(
        &self,
        task_id: &str,
        status: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        // Try updating condition_status; silently ignore if column doesn't exist
        let result = conn.execute(
            "UPDATE scheduled_tasks SET condition_status = ?1, modified_at = ?2 WHERE id = ?3",
            params![status, chrono::Utc::now().to_rfc3339(), task_id],
        );
        if let Err(e) = result {
            tracing::debug!("condition_status update skipped (column may not exist): {e}");
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // History
    // -----------------------------------------------------------------------

    /// Insert an execution record into scheduler_history.
    pub fn insert_execution_record(
        &self,
        task_id: &str,
        record: &TaskExecutionRecord,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let status = match &record.status {
            ScheduledTaskStatus::Pending => "pending",
            ScheduledTaskStatus::Running => "running",
            ScheduledTaskStatus::Completed => "completed",
            ScheduledTaskStatus::Failed => "failed",
            ScheduledTaskStatus::Skipped => "skipped",
            ScheduledTaskStatus::Cancelled => "cancelled",
        };

        conn.execute(
            r#"
            INSERT INTO scheduler_history
                (id, task_id, session_id, started_at, ended_at,
                 status, success, error_message,
                 triggered_auto_fix, auto_fix_session_id)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                record.execution_id,
                task_id,
                record.session_id,
                record.started_at,
                record.ended_at,
                status,
                record.success,
                record.error_message,
                record.triggered_auto_fix,
                record.auto_fix_session_id,
            ],
        )
        .map_err(|e| format!("Failed to insert execution record: {e}"))?;

        Ok(())
    }

    /// Get execution history for a task, most recent first, limited to `limit` rows.
    pub fn get_execution_history(
        &self,
        task_id: &str,
        limit: usize,
    ) -> Result<Vec<TaskExecutionRecord>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, session_id, started_at, ended_at,
                       status, success, error_message,
                       triggered_auto_fix, auto_fix_session_id
                FROM scheduler_history
                WHERE task_id = ?1
                ORDER BY started_at DESC
                LIMIT ?2
                "#,
            )
            .map_err(|e| format!("Failed to prepare get_execution_history: {e}"))?;

        let records = stmt
            .query_map(params![task_id, limit as i64], |row| {
                let status_str: String = row.get(4)?;
                let status = match status_str.as_str() {
                    "pending" => ScheduledTaskStatus::Pending,
                    "running" => ScheduledTaskStatus::Running,
                    "completed" => ScheduledTaskStatus::Completed,
                    "failed" => ScheduledTaskStatus::Failed,
                    "skipped" => ScheduledTaskStatus::Skipped,
                    "cancelled" => ScheduledTaskStatus::Cancelled,
                    _ => ScheduledTaskStatus::Failed,
                };

                Ok(TaskExecutionRecord {
                    execution_id: row.get(0)?,
                    session_id: row.get(1)?,
                    started_at: row.get(2)?,
                    ended_at: row.get(3)?,
                    status,
                    success: row.get(5)?,
                    error_message: row.get(6)?,
                    triggered_auto_fix: row.get(7)?,
                    auto_fix_session_id: row.get(8)?,
                })
            })
            .map_err(|e| format!("Failed to query execution history: {e}"))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(records)
    }

    /// Trim execution history for a task, keeping only the most recent `max_entries`.
    pub fn trim_execution_history(&self, task_id: &str, max_entries: usize) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"
            DELETE FROM scheduler_history
            WHERE task_id = ?1
              AND id NOT IN (
                  SELECT id FROM scheduler_history
                  WHERE task_id = ?1
                  ORDER BY started_at DESC
                  LIMIT ?2
              )
            "#,
            params![task_id, max_entries as i64],
        )
        .map_err(|e| format!("Failed to trim execution history for {task_id}: {e}"))?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Settings
    // -----------------------------------------------------------------------

    /// Get global scheduler settings. Returns defaults if no row exists.
    pub fn get_scheduler_settings(&self) -> Result<SchedulerSettings, String> {
        let conn = self.get_conn()?;
        let result = conn
            .query_row(
                "SELECT enabled, max_concurrent, default_auto_fix_on_failure, timezone FROM scheduler_settings WHERE id = 1",
                [],
                |row| {
                    Ok(SchedulerSettings {
                        enabled: row.get(0)?,
                        max_concurrent: row.get::<_, i64>(1)? as u32,
                        default_auto_fix_on_failure: row.get(2)?,
                        timezone: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(|e| format!("Failed to query scheduler_settings: {e}"))?;

        Ok(result.unwrap_or_default())
    }

    /// Upsert global scheduler settings (single-row table, id=1).
    pub fn update_scheduler_settings(&self, settings: &SchedulerSettings) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"
            INSERT INTO scheduler_settings (id, enabled, max_concurrent, default_auto_fix_on_failure, timezone)
            VALUES (1, ?1, ?2, ?3, ?4)
            ON CONFLICT(id) DO UPDATE SET
                enabled = excluded.enabled,
                max_concurrent = excluded.max_concurrent,
                default_auto_fix_on_failure = excluded.default_auto_fix_on_failure,
                timezone = excluded.timezone
            "#,
            params![
                settings.enabled,
                settings.max_concurrent as i64,
                settings.default_auto_fix_on_failure,
                settings.timezone,
            ],
        )
        .map_err(|e| format!("Failed to update scheduler_settings: {e}"))?;

        Ok(())
    }
}
