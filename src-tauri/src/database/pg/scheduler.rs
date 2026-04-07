//! PostgreSQL scheduler operations (raw SQL).
//!
//! Covers: scheduled_tasks CRUD, execution helpers, scheduler_history,
//! and scheduler_settings.

use super::PgDb;
use crate::scheduler::{
    ConditionScheduleConfig, ScheduleExpression, ScheduledTask, ScheduledTaskStatus,
    ScheduledTaskType, SchedulerSettings, TaskExecutionRecord,
};
use tracing::warn;

// ============================================================================
// Helpers
// ============================================================================

/// Column list for SELECT queries on scheduled_tasks.
const SELECT_TASK_COLS: &str = r#"
    id, name, description, enabled,
    schedule_type, schedule_value, task_config,
    skip_if_completed, auto_fix_on_failure, success_criteria,
    created_at, modified_at, next_run, last_run_id
"#;

/// Map a tokio_postgres Row to a ScheduledTask.
fn row_to_scheduled_task(row: &tokio_postgres::Row) -> ScheduledTask {
    let schedule_type: String = row.get(4);
    let schedule_value: String = row.get(5);
    let task_config_json: String = row.get(6);

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
            let config: ConditionScheduleConfig =
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

    ScheduledTask {
        id: row.get(0),
        name: row.get(1),
        description: row.get(2),
        enabled: row.get(3),
        schedule,
        task,
        skip_if_completed: row.get(7),
        auto_fix_on_failure: row.get(8),
        success_criteria: row.get(9),
        created_at: row.get(10),
        modified_at: row.get(11),
        next_run: row.get(12),
        last_run: None,
        conditions: None,
        condition_status: None,
    }
}

/// Convert a ScheduleExpression to (type, value) strings for storage.
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

/// Map a tokio_postgres Row to a TaskExecutionRecord.
fn row_to_execution_record(row: &tokio_postgres::Row) -> TaskExecutionRecord {
    let status_str: String = row.get(4);
    let status = match status_str.as_str() {
        "pending" => ScheduledTaskStatus::Pending,
        "running" => ScheduledTaskStatus::Running,
        "completed" => ScheduledTaskStatus::Completed,
        "failed" => ScheduledTaskStatus::Failed,
        "skipped" => ScheduledTaskStatus::Skipped,
        "cancelled" => ScheduledTaskStatus::Cancelled,
        _ => ScheduledTaskStatus::Failed,
    };

    TaskExecutionRecord {
        execution_id: row.get(0),
        session_id: row.get(1),
        started_at: row.get(2),
        ended_at: row.get(3),
        status,
        success: row.get(5),
        error_message: row.get(6),
        triggered_auto_fix: row.get(7),
        auto_fix_session_id: row.get(8),
    }
}

impl PgDb {
    // ========================================================================
    // Task CRUD
    // ========================================================================

    /// Retrieve all scheduled tasks.
    pub async fn get_all_scheduled_tasks(&self) -> Result<Vec<ScheduledTask>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                &format!(
                    "SELECT {} FROM scheduled_tasks ORDER BY created_at",
                    SELECT_TASK_COLS
                ),
                &[],
            )
            .await
            .map_err(|e| format!("PG get_all_scheduled_tasks: {}", e))?;

        Ok(rows.iter().map(row_to_scheduled_task).collect())
    }

    /// Retrieve a single scheduled task by ID, or None if not found.
    pub async fn get_scheduled_task(&self, id: &str) -> Result<Option<ScheduledTask>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                &format!(
                    "SELECT {} FROM scheduled_tasks WHERE id = $1",
                    SELECT_TASK_COLS
                ),
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_scheduled_task {}: {}", id, e))?;

        Ok(row.as_ref().map(row_to_scheduled_task))
    }

    /// Insert a new scheduled task.
    pub async fn insert_scheduled_task(&self, task: &ScheduledTask) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let (stype, svalue) = schedule_to_parts(&task.schedule);
        let task_config = serde_json::to_string(&task.task)
            .map_err(|e| format!("Serialize task_config: {}", e))?;
        let last_run_id = task.last_run.as_ref().map(|r| r.execution_id.clone());

        conn.execute(
            r#"
            INSERT INTO scheduled_tasks
                (id, name, description, enabled,
                 schedule_type, schedule_value, task_config,
                 skip_if_completed, auto_fix_on_failure, success_criteria,
                 created_at, modified_at, next_run, last_run_id)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
            "#,
            &[
                &task.id as &(dyn tokio_postgres::types::ToSql + Sync),
                &task.name,
                &task.description,
                &task.enabled,
                &stype.to_string(),
                &svalue,
                &task_config,
                &task.skip_if_completed,
                &task.auto_fix_on_failure,
                &task.success_criteria,
                &task.created_at,
                &task.modified_at,
                &task.next_run,
                &last_run_id,
            ],
        )
        .await
        .map_err(|e| format!("PG insert_scheduled_task {}: {}", task.id, e))?;

        Ok(())
    }

    /// Update an existing scheduled task (full replacement of mutable fields).
    pub async fn update_scheduled_task(&self, task: &ScheduledTask) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let (stype, svalue) = schedule_to_parts(&task.schedule);
        let task_config = serde_json::to_string(&task.task)
            .map_err(|e| format!("Serialize task_config: {}", e))?;
        let last_run_id = task.last_run.as_ref().map(|r| r.execution_id.clone());

        conn.execute(
            r#"
            UPDATE scheduled_tasks SET
                name = $1,
                description = $2,
                enabled = $3,
                schedule_type = $4,
                schedule_value = $5,
                task_config = $6,
                skip_if_completed = $7,
                auto_fix_on_failure = $8,
                success_criteria = $9,
                modified_at = $10,
                next_run = $11,
                last_run_id = $12
            WHERE id = $13
            "#,
            &[
                &task.name as &(dyn tokio_postgres::types::ToSql + Sync),
                &task.description,
                &task.enabled,
                &stype.to_string(),
                &svalue,
                &task_config,
                &task.skip_if_completed,
                &task.auto_fix_on_failure,
                &task.success_criteria,
                &task.modified_at,
                &task.next_run,
                &last_run_id,
                &task.id,
            ],
        )
        .await
        .map_err(|e| format!("PG update_scheduled_task {}: {}", task.id, e))?;

        Ok(())
    }

    /// Delete a scheduled task by ID. History is cascade-deleted by FK.
    pub async fn delete_scheduled_task(&self, id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute("DELETE FROM scheduled_tasks WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("PG delete_scheduled_task {}: {}", id, e))?;
        Ok(())
    }

    // ========================================================================
    // Execution helpers (single-column updates)
    // ========================================================================

    /// Set or clear the last_run_id on a task.
    pub async fn update_task_last_run(
        &self,
        task_id: &str,
        last_run_id: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE scheduled_tasks SET last_run_id = $1, modified_at = $2 WHERE id = $3",
            &[
                &last_run_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &now as &(dyn tokio_postgres::types::ToSql + Sync),
                &task_id,
            ],
        )
        .await
        .map_err(|e| format!("PG update_task_last_run for {}: {}", task_id, e))?;
        Ok(())
    }

    /// Set or clear the next_run timestamp on a task.
    pub async fn update_task_next_run(
        &self,
        task_id: &str,
        next_run: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE scheduled_tasks SET next_run = $1, modified_at = $2 WHERE id = $3",
            &[
                &next_run as &(dyn tokio_postgres::types::ToSql + Sync),
                &now as &(dyn tokio_postgres::types::ToSql + Sync),
                &task_id,
            ],
        )
        .await
        .map_err(|e| format!("PG update_task_next_run for {}: {}", task_id, e))?;
        Ok(())
    }

    /// Set or clear the condition_status JSON on a task.
    pub async fn update_task_condition_status(
        &self,
        task_id: &str,
        status: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        let result = conn
            .execute(
                "UPDATE scheduled_tasks SET condition_status = $1, modified_at = $2 WHERE id = $3",
                &[
                    &status as &(dyn tokio_postgres::types::ToSql + Sync),
                    &now as &(dyn tokio_postgres::types::ToSql + Sync),
                    &task_id,
                ],
            )
            .await;
        if let Err(e) = result {
            tracing::debug!("PG condition_status update skipped: {}", e);
        }
        Ok(())
    }

    // ========================================================================
    // History
    // ========================================================================

    /// Insert an execution record into scheduler_history.
    pub async fn insert_execution_record(
        &self,
        task_id: &str,
        record: &TaskExecutionRecord,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
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
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            "#,
            &[
                &record.execution_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &task_id,
                &record.session_id,
                &record.started_at,
                &record.ended_at,
                &status.to_string(),
                &record.success,
                &record.error_message,
                &record.triggered_auto_fix,
                &record.auto_fix_session_id,
            ],
        )
        .await
        .map_err(|e| format!("PG insert_execution_record: {}", e))?;

        Ok(())
    }

    /// Get execution history for a task, most recent first, limited to `limit` rows.
    pub async fn get_execution_history(
        &self,
        task_id: &str,
        limit: i64,
    ) -> Result<Vec<TaskExecutionRecord>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, session_id, started_at, ended_at,
                       status, success, error_message,
                       triggered_auto_fix, auto_fix_session_id
                FROM scheduler_history
                WHERE task_id = $1
                ORDER BY started_at DESC
                LIMIT $2
                "#,
                &[&task_id, &limit],
            )
            .await
            .map_err(|e| format!("PG get_execution_history: {}", e))?;

        Ok(rows.iter().map(row_to_execution_record).collect())
    }

    /// Trim execution history for a task, keeping only the most recent `max_entries`.
    pub async fn trim_execution_history(
        &self,
        task_id: &str,
        max_entries: i64,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            r#"
            DELETE FROM scheduler_history
            WHERE task_id = $1
              AND id NOT IN (
                  SELECT id FROM scheduler_history
                  WHERE task_id = $1
                  ORDER BY started_at DESC
                  LIMIT $2
              )
            "#,
            &[&task_id, &max_entries],
        )
        .await
        .map_err(|e| format!("PG trim_execution_history for {}: {}", task_id, e))?;

        Ok(())
    }

    // ========================================================================
    // Scheduler Settings (singleton table, id=1)
    // ========================================================================

    /// Get global scheduler settings. Returns defaults if no row exists.
    pub async fn get_scheduler_settings(&self) -> Result<SchedulerSettings, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                "SELECT enabled, max_concurrent, default_auto_fix_on_failure, timezone FROM scheduler_settings WHERE id = 1",
                &[],
            )
            .await
            .map_err(|e| format!("PG get_scheduler_settings: {}", e))?;

        Ok(row
            .map(|r| SchedulerSettings {
                enabled: r.get(0),
                max_concurrent: r.get::<_, i32>(1) as u32,
                default_auto_fix_on_failure: r.get(2),
                timezone: r.get(3),
            })
            .unwrap_or_default())
    }

    /// Upsert global scheduler settings (single-row table, id=1).
    pub async fn update_scheduler_settings(
        &self,
        settings: &SchedulerSettings,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let max_concurrent = settings.max_concurrent as i32;

        conn.execute(
            r#"
            INSERT INTO scheduler_settings (id, enabled, max_concurrent, default_auto_fix_on_failure, timezone)
            VALUES (1, $1, $2, $3, $4)
            ON CONFLICT(id) DO UPDATE SET
                enabled = EXCLUDED.enabled,
                max_concurrent = EXCLUDED.max_concurrent,
                default_auto_fix_on_failure = EXCLUDED.default_auto_fix_on_failure,
                timezone = EXCLUDED.timezone
            "#,
            &[
                &settings.enabled as &(dyn tokio_postgres::types::ToSql + Sync),
                &max_concurrent as &(dyn tokio_postgres::types::ToSql + Sync),
                &settings.default_auto_fix_on_failure as &(dyn tokio_postgres::types::ToSql + Sync),
                &settings.timezone as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG update_scheduler_settings: {}", e))?;

        Ok(())
    }
}
