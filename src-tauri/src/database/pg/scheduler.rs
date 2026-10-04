//! PostgreSQL scheduler operations (raw SQL).
//!
//! Covers: scheduled_tasks CRUD, execution helpers, scheduler_history,
//! and scheduler_settings.

use super::PgDb;
use crate::scheduler::{
    scheduled_task_type_default, CatchUpPolicy, ConditionScheduleConfig, ConditionStatus,
    ScheduleConditions, ScheduleExpression, ScheduledTask, ScheduledTaskStatus, ScheduledTaskType,
    SchedulerSettings, TaskExecutionRecord,
};
use chrono::{DateTime, Utc};
use tracing::warn;

// ============================================================================
// Helpers
// ============================================================================

/// Column list for SELECT queries on scheduled_tasks.
///
/// Order matters — `row_to_scheduled_task` indexes by position. The trailing
/// four columns (`catch_up_policy`, `catch_up_grace_seconds`,
/// `consecutive_launch_failures`, `launch_failure_backoff_seconds`) were
/// added by the v12 (Phase A) additive migration in `schema.pg.sql` and
/// self-heal on next runner restart per
/// `proj_pg_schema_drift_audit.md`.
///
/// `conditions` (JSONB, qontinui-web revision
/// `sched_cond_01_scheduled_tasks_conditions`; self-healed on an embedded PG by
/// `PgDb::verify_and_provision`) and `condition_status` (TEXT holding JSON)
/// are the last two. Before plan
/// `2026-09-29-quiet-is-measured-by-session-existence-and-machine-wide-so-a-24x7-box-never-gets-one`
/// Phase 4c neither was read, so every task loaded with no conditions and
/// every schedule condition was inert.
const SELECT_TASK_COLS: &str = r#"
    id, name, description, enabled,
    schedule_type, schedule_value, task_config,
    skip_if_completed, auto_fix_on_failure, success_criteria,
    created_at, modified_at, next_run, last_run_id,
    catch_up_policy, catch_up_grace_seconds,
    consecutive_launch_failures, launch_failure_backoff_seconds,
    conditions, condition_status
"#;

/// Whether this store round-trips a task's `ScheduleConditions`: true iff the
/// column list it reads names a `conditions` column (it does since Phase 4c;
/// see `crate::scheduler_service::enforced_conditions`, which serves the
/// scheduler's condition list on `/health` only while this holds). Derived
/// from [`SELECT_TASK_COLS`] rather than declared, so it cannot disagree with
/// the query that decides it.
pub fn task_store_persists_conditions() -> bool {
    SELECT_TASK_COLS
        .split(|c: char| c == ',' || c.is_whitespace())
        .any(|column| column == "conditions")
}

/// Parse a `catch_up_policy` text value from the DB into the typed enum.
/// Falls back to [`CatchUpPolicy::default`] for unknown / null values so
/// rows written before this column existed still load.
fn parse_catch_up_policy(raw: &str) -> CatchUpPolicy {
    match raw {
        "run" => CatchUpPolicy::Run,
        "skip" => CatchUpPolicy::Skip,
        "run_once" => CatchUpPolicy::RunOnce,
        other => {
            warn!(
                "Unknown catch_up_policy value '{}'; defaulting to RunOnce",
                other
            );
            CatchUpPolicy::default()
        }
    }
}

/// Render a [`CatchUpPolicy`] as the snake_case discriminator string used
/// for the TEXT column. Mirrors the `#[serde(rename_all = "snake_case")]`
/// on the type so DB and JSON wire-format stay aligned.
fn catch_up_policy_to_str(policy: CatchUpPolicy) -> &'static str {
    match policy {
        CatchUpPolicy::Run => "run",
        CatchUpPolicy::Skip => "skip",
        CatchUpPolicy::RunOnce => "run_once",
    }
}

/// Parse a `modified_at` token (as read into `ScheduledTask::modified_at`)
/// for a conditional write.
fn parse_modified_at(raw: &str) -> Result<DateTime<Utc>, String> {
    raw.parse()
        .map_err(|e| format!("expected modified_at '{raw}': {e}"))
}

/// Serialize a task's conditions for the `conditions` JSONB column.
fn conditions_to_json(
    conditions: Option<&ScheduleConditions>,
) -> Result<Option<serde_json::Value>, String> {
    conditions
        .map(serde_json::to_value)
        .transpose()
        .map_err(|e| format!("Serialize conditions: {e}"))
}

/// Parse the `conditions` column. An unparseable value is an ERROR, never
/// "no conditions": reading it as `None` would run the task ungated, so the
/// caller refuses to load that task instead (fail closed).
fn conditions_from_json(
    task_id: &str,
    raw: Option<serde_json::Value>,
) -> Result<Option<ScheduleConditions>, String> {
    raw.map(|value| {
        serde_json::from_value(value).map_err(|e| {
            format!("task {task_id}: stored conditions are unreadable ({e}); refusing to run it")
        })
    })
    .transpose()
}

/// Parse the `condition_status` column. It is transient wait state (when the
/// wait began, the last sub-condition results), so an unreadable value is
/// logged and dropped — the wait restarts — rather than blocking the task.
fn condition_status_from_text(task_id: &str, raw: Option<String>) -> Option<ConditionStatus> {
    let raw = raw?;
    match serde_json::from_str(&raw) {
        Ok(status) => Some(status),
        Err(e) => {
            warn!(
                "task {task_id}: stored condition_status is unreadable ({e}); restarting the wait"
            );
            None
        }
    }
}

/// A scheduled-task row as stored, for the task-management API: the task,
/// plus why its stored `conditions` could not be read, if they could not.
/// When `conditions_error` is `Some`, `task.conditions` is `None` — which is
/// NOT "no conditions", so only the API sees this shape (to show the row and
/// let a PUT replace the conditions); everything that RUNS tasks goes through
/// [`PgDb::get_all_scheduled_tasks`] / [`PgDb::get_scheduled_task`], which
/// refuse such a row (fail closed).
#[derive(Debug, Clone)]
pub struct StoredScheduledTask {
    pub task: ScheduledTask,
    pub conditions_error: Option<String>,
}

impl StoredScheduledTask {
    /// The task for something that will RUN it: `Err` when its conditions are
    /// unreadable.
    fn runnable(self) -> Result<ScheduledTask, String> {
        match self.conditions_error {
            None => Ok(self.task),
            Some(e) => Err(e),
        }
    }
}

/// Map rows to runnable tasks, dropping (with an ERROR log) any whose
/// conditions are unreadable — such a task must not run, and running it
/// ungated is the only alternative a loader has.
fn rows_to_scheduled_tasks(rows: &[tokio_postgres::Row]) -> Vec<ScheduledTask> {
    rows.iter()
        .filter_map(|row| match row_to_scheduled_task(row).runnable() {
            Ok(task) => Some(task),
            Err(e) => {
                tracing::error!("scheduler: {e}");
                None
            }
        })
        .collect()
}

/// Map a tokio_postgres Row to a [`StoredScheduledTask`]. Unreadable
/// `conditions` (see [`conditions_from_json`]) land in `conditions_error`.
#[expect(
    clippy::disallowed_methods,
    reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
)]
fn row_to_scheduled_task(row: &tokio_postgres::Row) -> StoredScheduledTask {
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
            scheduled_task_type_default()
        }
    };

    let created: DateTime<Utc> = row.get(10);
    let modified: DateTime<Utc> = row.get(11);
    let next: Option<DateTime<Utc>> = row.get(12);
    let catch_up_policy_raw: String = row.get(14);
    let catch_up_grace_seconds: i32 = row.get(15);
    let consecutive_launch_failures: i32 = row.get(16);
    let launch_failure_backoff_seconds: i32 = row.get(17);
    let id: String = row.get(0);
    let (conditions, conditions_error) = match conditions_from_json(&id, row.get(18)) {
        Ok(conditions) => (conditions, None),
        Err(e) => (None, Some(e)),
    };
    let condition_status = condition_status_from_text(&id, row.get(19));

    let task = ScheduledTask {
        id,
        name: row.get(1),
        description: row.get(2),
        enabled: row.get(3),
        schedule,
        task,
        skip_if_completed: row.get(7),
        auto_fix_on_failure: row.get(8),
        success_criteria: row.get(9),
        created_at: created.to_rfc3339(),
        modified_at: modified.to_rfc3339(),
        next_run: next.map(|dt| dt.to_rfc3339()),
        last_run: None,
        conditions,
        condition_status,
        catch_up_policy: parse_catch_up_policy(&catch_up_policy_raw),
        // INTEGER columns surface as i32; clamp negatives to 0 defensively
        // (NOT NULL DEFAULT in schema, but drift audits remind us that
        // pre-migration rows can carry surprising values).
        catch_up_grace_seconds: catch_up_grace_seconds.max(0) as u32,
        consecutive_launch_failures: consecutive_launch_failures.max(0) as u32,
        launch_failure_backoff_seconds: launch_failure_backoff_seconds.max(0) as u32,
    };
    StoredScheduledTask {
        task,
        conditions_error,
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
///
/// `started_at` and `ended_at` are TIMESTAMPTZ in the live DB (migrated
/// from TEXT by v10). Read as `DateTime<Utc>` and format to ISO 8601 so
/// the JSON-serialised struct stays backwards-compatible.
///
/// `scheduled_for` (column 9) and `catch_up_run` (column 10) were added
/// by the v12 (Phase A) additive migration. `scheduled_for` is nullable
/// for backward compatibility with rows written before the column
/// existed; `catch_up_run` is NOT NULL DEFAULT false in the schema so
/// pre-existing rows surface as `false` here.
#[expect(
    clippy::disallowed_methods,
    reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
)]
fn row_to_execution_record(row: &tokio_postgres::Row) -> TaskExecutionRecord {
    let status_str: String = row.get(4);
    let status = parse_status(&status_str);

    let started: DateTime<Utc> = row.get(2);
    let ended: Option<DateTime<Utc>> = row.get(3);
    let scheduled_for: Option<DateTime<Utc>> = row.get(9);
    let catch_up_run: bool = row.get(10);

    TaskExecutionRecord {
        execution_id: row.get(0),
        session_id: row.get(1),
        started_at: started.to_rfc3339(),
        ended_at: ended.map(|dt| dt.to_rfc3339()),
        status,
        success: row.get(5),
        error_message: row.get(6),
        triggered_auto_fix: row.get(7),
        auto_fix_session_id: row.get(8),
        scheduled_for: scheduled_for.map(|dt| dt.to_rfc3339()),
        catch_up_run,
    }
}

/// Parse the TEXT status column into the typed enum, mapping unknown
/// strings to `Failed` as a defensive default.
fn parse_status(raw: &str) -> ScheduledTaskStatus {
    match raw {
        "pending" => ScheduledTaskStatus::Pending,
        "running" => ScheduledTaskStatus::Running,
        "completed" => ScheduledTaskStatus::Completed,
        "failed" => ScheduledTaskStatus::Failed,
        "launch_failed" => ScheduledTaskStatus::LaunchFailed,
        "skipped" => ScheduledTaskStatus::Skipped,
        "cancelled" => ScheduledTaskStatus::Cancelled,
        "missed_runner_down" => ScheduledTaskStatus::MissedRunnerDown,
        _ => ScheduledTaskStatus::Failed,
    }
}

/// Render a [`ScheduledTaskStatus`] as the snake_case discriminator string
/// stored in the TEXT `status` column. Mirrors `#[serde(rename_all =
/// "snake_case")]` on the enum.
fn status_to_str(status: &ScheduledTaskStatus) -> &'static str {
    match status {
        ScheduledTaskStatus::Pending => "pending",
        ScheduledTaskStatus::Running => "running",
        ScheduledTaskStatus::Completed => "completed",
        ScheduledTaskStatus::Failed => "failed",
        ScheduledTaskStatus::LaunchFailed => "launch_failed",
        ScheduledTaskStatus::Skipped => "skipped",
        ScheduledTaskStatus::Cancelled => "cancelled",
        ScheduledTaskStatus::MissedRunnerDown => "missed_runner_down",
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
            .map_err(|e| crate::database::pg::pg_err("PG get_all_scheduled_tasks", &e))?;

        Ok(rows_to_scheduled_tasks(&rows))
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

        row.as_ref()
            .map(|row| row_to_scheduled_task(row).runnable())
            .transpose()
    }

    /// Every scheduled-task row as stored, INCLUDING rows whose conditions
    /// are unreadable (flagged, not dropped). For the task-management API
    /// only — never for anything that runs a task.
    pub async fn get_all_stored_scheduled_tasks(&self) -> Result<Vec<StoredScheduledTask>, String> {
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
            .map_err(|e| crate::database::pg::pg_err("PG get_all_stored_scheduled_tasks", &e))?;
        Ok(rows.iter().map(row_to_scheduled_task).collect())
    }

    /// One scheduled-task row as stored, flagged rather than refused when
    /// its conditions are unreadable. For the task-management API only.
    pub async fn get_stored_scheduled_task(
        &self,
        id: &str,
    ) -> Result<Option<StoredScheduledTask>, String> {
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
            .map_err(|e| format!("PG get_stored_scheduled_task {}: {}", id, e))?;
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
        let catch_up_policy_str = catch_up_policy_to_str(task.catch_up_policy).to_string();
        // Cast u32 → i32 for PG INTEGER columns. Schema defaults are well
        // under 2^31, so saturating is a safety belt rather than an
        // expected path.
        let catch_up_grace_seconds_i32: i32 =
            task.catch_up_grace_seconds.min(i32::MAX as u32) as i32;
        let consecutive_launch_failures_i32: i32 =
            task.consecutive_launch_failures.min(i32::MAX as u32) as i32;
        let launch_failure_backoff_seconds_i32: i32 =
            task.launch_failure_backoff_seconds.min(i32::MAX as u32) as i32;
        let conditions_json = conditions_to_json(task.conditions.as_ref())?;
        let condition_status_text: Option<String> = task
            .condition_status
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("Serialize condition_status: {e}"))?;

        conn.execute(
            r#"
            INSERT INTO scheduled_tasks
                (id, name, description, enabled,
                 schedule_type, schedule_value, task_config,
                 skip_if_completed, auto_fix_on_failure, success_criteria,
                 created_at, modified_at, next_run, last_run_id,
                 catch_up_policy, catch_up_grace_seconds,
                 consecutive_launch_failures, launch_failure_backoff_seconds,
                 conditions, condition_status)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                    $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)
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
                &task
                    .created_at
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
                &task
                    .modified_at
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
                &task
                    .next_run
                    .as_deref()
                    .and_then(|s| s.parse::<DateTime<Utc>>().ok()),
                &last_run_id,
                &catch_up_policy_str,
                &catch_up_grace_seconds_i32,
                &consecutive_launch_failures_i32,
                &launch_failure_backoff_seconds_i32,
                &conditions_json,
                &condition_status_text,
            ],
        )
        .await
        .map_err(|e| format!("PG insert_scheduled_task {}: {}", task.id, e))?;

        Ok(())
    }

    /// Update an existing scheduled task's USER-editable fields as a
    /// CONDITIONAL write: it lands only if the row's `modified_at` still
    /// equals `expected_modified_at`, the value the caller read before
    /// building `task`. Returns `Ok(false)` when it did not (another user edit
    /// moved the row since), so a caller holding a stale snapshot cannot
    /// overwrite a newer change; the HTTP handler answers that with 409.
    ///
    /// Executor-owned columns (`last_run_id`, `consecutive_launch_failures`)
    /// are NOT written — a user edit must not reset a run the executor
    /// recorded after the edit's read. `conditions` / `condition_status` are
    /// written only when `write_conditions` is set (the request carried
    /// `conditions`): otherwise the stored values are left exactly as they are,
    /// which is also what lets a PUT repair other fields of a row whose stored
    /// conditions are unreadable without the loader ever parsing them.
    pub async fn update_scheduled_task(
        &self,
        task: &ScheduledTask,
        expected_modified_at: &str,
        write_conditions: bool,
    ) -> Result<bool, String> {
        let expected = parse_modified_at(expected_modified_at)?;
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let (stype, svalue) = schedule_to_parts(&task.schedule);
        let task_config = serde_json::to_string(&task.task)
            .map_err(|e| format!("Serialize task_config: {}", e))?;
        let catch_up_policy_str = catch_up_policy_to_str(task.catch_up_policy).to_string();
        let catch_up_grace_seconds_i32: i32 =
            task.catch_up_grace_seconds.min(i32::MAX as u32) as i32;
        let launch_failure_backoff_seconds_i32: i32 =
            task.launch_failure_backoff_seconds.min(i32::MAX as u32) as i32;
        let conditions_json = conditions_to_json(task.conditions.as_ref())?;
        let condition_status_text: Option<String> = task
            .condition_status
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("Serialize condition_status: {e}"))?;

        let updated = conn
            .execute(
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
                catch_up_policy = $12,
                catch_up_grace_seconds = $13,
                launch_failure_backoff_seconds = $14,
                conditions = CASE WHEN $17 THEN $18 ELSE conditions END,
                condition_status = CASE WHEN $17 THEN $19 ELSE condition_status END
            WHERE id = $15 AND modified_at = $16
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
                    &task
                        .modified_at
                        .parse::<DateTime<Utc>>()
                        .unwrap_or_else(|_| Utc::now()),
                    &task
                        .next_run
                        .as_deref()
                        .and_then(|s| s.parse::<DateTime<Utc>>().ok()),
                    &catch_up_policy_str,
                    &catch_up_grace_seconds_i32,
                    &launch_failure_backoff_seconds_i32,
                    &task.id,
                    &expected,
                    &write_conditions,
                    &conditions_json,
                    &condition_status_text,
                ],
            )
            .await
            .map_err(|e| format!("PG update_scheduled_task {}: {}", task.id, e))?;

        Ok(updated == 1)
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

    // Every write below is an EXECUTOR write, not a user edit, so none of them
    // bumps `modified_at`: `modified_at` is the last USER modification, the
    // token `update_scheduled_task`'s conditional write (and these) compare
    // against. The two a tick derives from a task snapshot — `next_run` and
    // `condition_status` — are CONDITIONAL on the snapshot's `modified_at`:
    // if a user PUT landed since the snapshot was read they write nothing
    // and return `Ok(false)`, and the caller re-reads on its next tick instead
    // of overwriting the user's change with state derived from the old row.

    /// Set or clear the last_run_id on a task (executor-owned; never written
    /// by a user PUT).
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
        conn.execute(
            "UPDATE scheduled_tasks SET last_run_id = $1 WHERE id = $2",
            &[
                &last_run_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &task_id,
            ],
        )
        .await
        .map_err(|e| format!("PG update_task_last_run for {}: {}", task_id, e))?;
        Ok(())
    }

    /// Set or clear the next_run timestamp on a task, only if the row's
    /// `modified_at` still equals `expected_modified_at` (the snapshot the
    /// value was computed from). `Ok(false)` = a user edit landed since; the
    /// edit recomputed `next_run` itself, so nothing is lost.
    pub async fn update_task_next_run(
        &self,
        task_id: &str,
        next_run: Option<&str>,
        expected_modified_at: &str,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let next: Option<DateTime<Utc>> = next_run.and_then(|s| s.parse().ok());
        let expected = parse_modified_at(expected_modified_at)?;
        let updated = conn
            .execute(
                "UPDATE scheduled_tasks SET next_run = $1 WHERE id = $2 AND modified_at = $3",
                &[
                    &next as &(dyn tokio_postgres::types::ToSql + Sync),
                    &task_id,
                    &expected,
                ],
            )
            .await
            .map_err(|e| format!("PG update_task_next_run for {}: {}", task_id, e))?;
        Ok(updated == 1)
    }

    /// Persist `consecutive_launch_failures` for a task without rewriting
    /// the rest of the row. Used by the Phase C launch-failure path to
    /// bump the counter on `LaunchFailed` and to zero it on the first
    /// successful launch. Other columns (especially `next_run`) are
    /// updated separately so callers can sequence the writes
    /// independently and one transient PG error doesn't roll back the
    /// reschedule. Executor-owned: a user PUT never writes this column.
    pub async fn update_task_launch_failure_counter(
        &self,
        task_id: &str,
        consecutive_launch_failures: u32,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let counter_i32: i32 = consecutive_launch_failures.min(i32::MAX as u32) as i32;
        conn.execute(
            "UPDATE scheduled_tasks SET consecutive_launch_failures = $1 WHERE id = $2",
            &[
                &counter_i32 as &(dyn tokio_postgres::types::ToSql + Sync),
                &task_id,
            ],
        )
        .await
        .map_err(|e| {
            format!(
                "PG update_task_launch_failure_counter for {}: {}",
                task_id, e
            )
        })?;
        Ok(())
    }

    /// Set or clear the condition_status JSON on a task, only if the row's
    /// `modified_at` still equals `expected_modified_at` (the snapshot the
    /// status was evaluated against). `Ok(false)` = a user edit landed since;
    /// the caller re-evaluates on its next tick.
    pub async fn update_task_condition_status(
        &self,
        task_id: &str,
        status: Option<&str>,
        expected_modified_at: &str,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let expected = parse_modified_at(expected_modified_at)?;
        let updated = conn
            .execute(
                "UPDATE scheduled_tasks SET condition_status = $1 WHERE id = $2 AND modified_at = $3",
                &[
                    &status as &(dyn tokio_postgres::types::ToSql + Sync),
                    &task_id,
                    &expected,
                ],
            )
            .await
            .map_err(|e| format!("PG update_task_condition_status for {}: {}", task_id, e))?;
        Ok(updated == 1)
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
        let status = status_to_str(&record.status).to_string();

        // Parse ISO 8601 strings back to DateTime<Utc> for the TIMESTAMPTZ
        // columns. The struct stores strings for JSON serialisation but the
        // DB expects native timestamps after the v10 TEXT → TIMESTAMPTZ migration.
        let started: DateTime<Utc> = record
            .started_at
            .parse::<DateTime<Utc>>()
            .unwrap_or_else(|_| Utc::now());
        let ended: Option<DateTime<Utc>> = record
            .ended_at
            .as_deref()
            .and_then(|s| s.parse::<DateTime<Utc>>().ok());
        // `scheduled_for` is optional and nullable — the column was added
        // by v12 (Phase A) and pre-existing records will write `NULL`.
        let scheduled_for: Option<DateTime<Utc>> = record
            .scheduled_for
            .as_deref()
            .and_then(|s| s.parse::<DateTime<Utc>>().ok());

        conn.execute(
            r#"
            INSERT INTO scheduler_history
                (execution_id, task_id, session_id, started_at, ended_at,
                 status, success, error_message,
                 triggered_auto_fix, auto_fix_session_id,
                 scheduled_for, catch_up_run)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            "#,
            &[
                &record.execution_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &task_id,
                &record.session_id,
                &started,
                &ended,
                &status,
                &record.success,
                &record.error_message,
                &record.triggered_auto_fix,
                &record.auto_fix_session_id,
                &scheduled_for,
                &record.catch_up_run,
            ],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG insert_execution_record", &e))?;

        Ok(())
    }

    /// Update an existing execution record with a final outcome.
    ///
    /// Used by the scheduler after polling an asynchronously-launched workflow
    /// to backfill the real success/error/ended_at values once the underlying
    /// task_run leaves the `running` state. Before this method existed, the
    /// scheduler only ever wrote a single "launched successfully" row, which
    /// lied about the true outcome for any workflow whose launch endpoint was
    /// non-blocking.
    pub async fn update_execution_record(
        &self,
        execution_id: &str,
        record: &TaskExecutionRecord,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let status = status_to_str(&record.status).to_string();

        let ended: Option<DateTime<Utc>> = record
            .ended_at
            .as_deref()
            .and_then(|s| s.parse::<DateTime<Utc>>().ok());

        // `scheduled_for` and `catch_up_run` are launch-time properties
        // (set on INSERT). On UPDATE we COALESCE so callers that *do*
        // supply them can backfill historical rows, while callers that
        // leave them as the struct default (`None` / `false`) won't clobber
        // values written at insert.
        let scheduled_for: Option<DateTime<Utc>> = record
            .scheduled_for
            .as_deref()
            .and_then(|s| s.parse::<DateTime<Utc>>().ok());

        conn.execute(
            r#"
            UPDATE scheduler_history
            SET ended_at = $2,
                status = $3,
                success = $4,
                error_message = $5,
                triggered_auto_fix = $6,
                auto_fix_session_id = $7,
                session_id = COALESCE($8, session_id),
                scheduled_for = COALESCE($9, scheduled_for),
                catch_up_run = catch_up_run OR $10
            WHERE execution_id = $1
            "#,
            &[
                &execution_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &ended,
                &status,
                &record.success,
                &record.error_message,
                &record.triggered_auto_fix,
                &record.auto_fix_session_id,
                &record.session_id,
                &scheduled_for,
                &record.catch_up_run,
            ],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG update_execution_record", &e))?;

        Ok(())
    }

    /// Returns the candidate slots that have **no** matching
    /// `scheduler_history` row for the given task — i.e. the slots the
    /// catch-up reconciler should act on.
    ///
    /// The DB only returns timestamps that *do* match, and we diff against
    /// the supplied candidates client-side so the order/cardinality of the
    /// caller's input is preserved.
    ///
    /// NULL `scheduled_for` rows (legacy data written before the v12
    /// migration) deliberately do **not** match anything: they're recorded
    /// without a known slot, so we can't suppress catch-up on their behalf.
    /// The `WHERE scheduled_for = ANY($2)` predicate already excludes NULLs
    /// because SQL equality with NULL yields NULL (not true), so this falls
    /// out for free — but it's worth calling out.
    ///
    /// All timestamps round-trip as TIMESTAMPTZ, so the caller may pass
    /// either UTC or any other offset; PostgreSQL normalizes internally.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn find_missed_slots(
        &self,
        task_id: &str,
        candidate_slots: &[chrono::DateTime<chrono::Utc>],
    ) -> Result<Vec<chrono::DateTime<chrono::Utc>>, String> {
        if candidate_slots.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Pass the candidate slot list as a TIMESTAMPTZ[] parameter and let
        // PG do the matching. Never string-interpolate timestamps.
        let candidates_vec: Vec<DateTime<Utc>> = candidate_slots.to_vec();
        let rows = conn
            .query(
                r#"
                SELECT DISTINCT scheduled_for
                FROM scheduler_history
                WHERE task_id = $1
                  AND scheduled_for = ANY($2::timestamptz[])
                "#,
                &[&task_id, &candidates_vec],
            )
            .await
            .map_err(|e| format!("PG find_missed_slots {}: {}", task_id, e))?;

        // Collect the slots that *did* have history rows.
        let matched: std::collections::HashSet<DateTime<Utc>> = rows
            .iter()
            .filter_map(|r| r.get::<_, Option<DateTime<Utc>>>(0))
            .collect();

        // Return the candidates that are *not* in the matched set, preserving
        // input order so the most-recent slot ends up last.
        Ok(candidate_slots
            .iter()
            .filter(|slot| !matched.contains(slot))
            .copied()
            .collect())
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
                SELECT execution_id, session_id, started_at, ended_at,
                       status, success, error_message,
                       triggered_auto_fix, auto_fix_session_id,
                       scheduled_for, catch_up_run
                FROM scheduler_history
                WHERE task_id = $1
                ORDER BY started_at DESC
                LIMIT $2
                "#,
                &[&task_id, &limit],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_execution_history", &e))?;

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
              AND execution_id NOT IN (
                  SELECT execution_id FROM scheduler_history
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
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
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
            .map_err(|e| crate::database::pg::pg_err("PG get_scheduler_settings", &e))?;

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
        .map_err(|e| crate::database::pg::pg_err("PG update_scheduler_settings", &e))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::{ProbeCondition, ScheduledTaskExt};

    fn probe_conditions() -> ScheduleConditions {
        ScheduleConditions {
            require_probe: Some(ProbeCondition {
                enabled: true,
                command: vec!["sh".into(), "-c".into(), "exit 0".into()],
                poll_seconds: 300,
                timeout_seconds: 60,
            }),
            timeout_minutes: Some(120),
            ..Default::default()
        }
    }

    #[test]
    fn the_store_reads_the_conditions_columns() {
        assert!(task_store_persists_conditions());
        // `condition_status` alone must not satisfy the column check.
        assert!(!"condition_status"
            .split(|c: char| c == ',' || c.is_whitespace())
            .any(|column| column == "conditions"));
    }

    #[test]
    fn conditions_round_trip_through_the_column_encoding() {
        let conditions = probe_conditions();
        let json = conditions_to_json(Some(&conditions)).unwrap();
        assert_eq!(json.as_ref().unwrap()["requireProbe"]["pollSeconds"], 300);
        let back = conditions_from_json("t", json).unwrap().unwrap();
        assert_eq!(back.require_probe, conditions.require_probe);
        assert_eq!(back.timeout_minutes, Some(120));
        assert_eq!(conditions_to_json(None).unwrap(), None);
        assert!(conditions_from_json("t", None).unwrap().is_none());
    }

    /// An unreadable `conditions` value is an error, never "no conditions" —
    /// the latter would run the task ungated.
    #[test]
    fn unreadable_conditions_fail_closed() {
        let bad = serde_json::json!({ "requireProbe": { "enabled": "yes" } });
        let err = conditions_from_json("t1", Some(bad)).unwrap_err();
        assert!(err.contains("t1") && err.contains("refusing"), "{err}");
    }

    #[test]
    fn an_unreadable_condition_status_restarts_the_wait() {
        assert!(condition_status_from_text("t", Some("not json".into())).is_none());
        let status = crate::scheduler::condition_status_default();
        let text = serde_json::to_string(&status).unwrap();
        let back = condition_status_from_text("t", Some(text)).unwrap();
        assert_eq!(back.waiting_since, status.waiting_since);
    }

    /// The two executor writes a tick derives from a snapshot are conditional
    /// on its `modified_at`, and no executor write bumps `modified_at`.
    #[test]
    fn executor_writes_are_conditional_and_do_not_bump_modified_at() {
        let src = include_str!("scheduler.rs");
        let (_, executor) = src
            .split_once("// Every write below is an EXECUTOR write")
            .expect("executor section");
        let (executor, _) = executor.split_once("// History").expect("section end");
        assert!(executor.contains(
            "UPDATE scheduled_tasks SET next_run = $1 WHERE id = $2 AND modified_at = $3"
        ));
        assert!(executor.contains(
            "UPDATE scheduled_tasks SET condition_status = $1 WHERE id = $2 AND modified_at = $3"
        ));
        assert!(
            !executor.contains("modified_at = $2"),
            "no executor write bumps modified_at"
        );
        let heal = include_str!("mod.rs");
        let (_, heal) = heal
            .split_once("project.scheduled_tasks.conditions (JSONB)")
            .expect("self-heal");
        let (heal, _) = heal.split_once("END $$;").expect("self-heal block");
        assert!(
            heal.contains("information_schema.columns")
                && heal.contains("column_name = 'conditions'"),
            "the ALTER must be guarded by a column-existence check"
        );
    }

    // PG-gated, per the `database/pg/*` convention (see event_log.rs):
    //   DATABASE_URL=... cargo test --bin qontinui-runner \
    //     database::pg::scheduler -- --ignored
    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn conditions_and_condition_status_survive_insert_read_and_update() {
        let db = PgDb::new_for_test().await;
        let mut task = ScheduledTask::new(
            "phase-4c round trip".to_string(),
            None,
            ScheduleExpression::Cron("0 20 7 * * *".to_string()),
            scheduled_task_type_default(),
        );
        task.conditions = Some(probe_conditions());
        db.insert_scheduled_task(&task).await.expect("insert");

        let read = db
            .get_scheduled_task(&task.id)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(
            read.conditions
                .as_ref()
                .and_then(|c| c.require_probe.clone()),
            task.conditions
                .as_ref()
                .and_then(|c| c.require_probe.clone()),
            "require_probe must survive insert -> read"
        );
        assert!(read.has_conditions());
        assert!(db
            .get_all_scheduled_tasks()
            .await
            .expect("list")
            .iter()
            .any(|t| t.id == task.id && t.conditions.is_some()));

        // condition_status written by the tick path reads back, and the
        // executor write does not move modified_at.
        let mut status = crate::scheduler::condition_status_default();
        status.probe_met = Some(false);
        status.probe_detail = Some("exit 1".to_string());
        assert!(db
            .update_task_condition_status(
                &task.id,
                Some(&serde_json::to_string(&status).unwrap()),
                &read.modified_at,
            )
            .await
            .expect("status write"));
        let after_status = db.get_scheduled_task(&task.id).await.unwrap().unwrap();
        assert_eq!(
            after_status
                .condition_status
                .as_ref()
                .and_then(|s| s.probe_detail.clone()),
            Some("exit 1".to_string())
        );
        assert_eq!(after_status.modified_at, read.modified_at);

        // A user edit (conditional on what it read) lands; a PUT that does not
        // carry conditions leaves them untouched.
        let mut renamed = after_status.clone();
        renamed.name = "renamed".to_string();
        renamed.conditions = None;
        renamed.touch();
        assert!(db
            .update_scheduled_task(&renamed, &after_status.modified_at, false)
            .await
            .expect("rename"));
        let after_rename = db.get_scheduled_task(&task.id).await.unwrap().unwrap();
        assert_eq!(after_rename.name, "renamed");
        assert!(
            after_rename.conditions.is_some(),
            "a PUT without conditions keeps them"
        );

        // A tick holding the PRE-edit snapshot can no longer write.
        assert!(!db
            .update_task_condition_status(&task.id, None, &after_status.modified_at)
            .await
            .expect("stale status write"));
        assert!(!db
            .update_task_next_run(&task.id, None, &after_status.modified_at)
            .await
            .expect("stale next_run write"));
        // A second user write against the stale snapshot is refused too.
        assert!(!db
            .update_scheduled_task(&renamed, &after_status.modified_at, false)
            .await
            .expect("stale update"));

        // A PUT carrying conditions (here: none) replaces them.
        let mut cleared = after_rename.clone();
        cleared.conditions = None;
        cleared.condition_status = None;
        cleared.touch();
        assert!(db
            .update_scheduled_task(&cleared, &after_rename.modified_at, true)
            .await
            .expect("clear"));
        let read = db.get_scheduled_task(&task.id).await.unwrap().unwrap();
        assert!(read.conditions.is_none() && read.condition_status.is_none());

        db.delete_scheduled_task(&task.id).await.expect("cleanup");
    }

    /// A row whose stored conditions are unreadable: the scheduler refuses it
    /// (fail closed), the API sees it flagged, and a PUT that carries
    /// conditions repairs it without the stored value ever being parsed.
    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn an_unreadable_conditions_row_is_flagged_refused_and_repairable() {
        let db = PgDb::new_for_test().await;
        let task = ScheduledTask::new(
            "phase-4c unreadable conditions".to_string(),
            None,
            ScheduleExpression::Cron("0 20 7 * * *".to_string()),
            scheduled_task_type_default(),
        );
        db.insert_scheduled_task(&task).await.expect("insert");
        {
            let conn = db.pool.get().await.expect("pool");
            let bad = serde_json::json!({ "requireProbe": { "enabled": "yes" } });
            conn.execute(
                "UPDATE scheduled_tasks SET conditions = $1 WHERE id = $2",
                &[&bad as &(dyn tokio_postgres::types::ToSql + Sync), &task.id],
            )
            .await
            .expect("corrupt the conditions");
        }

        // Fail closed for everything that runs tasks.
        assert!(db.get_scheduled_task(&task.id).await.is_err());
        assert!(!db
            .get_all_scheduled_tasks()
            .await
            .unwrap()
            .iter()
            .any(|t| t.id == task.id));

        // Visible, flagged, to the API.
        let stored = db
            .get_stored_scheduled_task(&task.id)
            .await
            .unwrap()
            .expect("row");
        assert!(stored.conditions_error.is_some());
        assert!(stored.task.conditions.is_none());
        assert!(db
            .get_all_stored_scheduled_tasks()
            .await
            .unwrap()
            .iter()
            .any(|t| t.task.id == task.id && t.conditions_error.is_some()));

        // A PUT without conditions does not erase the unreadable value (that
        // would read as "ungated").
        let mut renamed = stored.task.clone();
        renamed.name = "still unreadable".to_string();
        renamed.touch();
        assert!(db
            .update_scheduled_task(&renamed, &stored.task.modified_at, false)
            .await
            .unwrap());
        assert!(db.get_scheduled_task(&task.id).await.is_err());

        // A PUT carrying conditions repairs it.
        let stored = db
            .get_stored_scheduled_task(&task.id)
            .await
            .unwrap()
            .unwrap();
        let mut repaired = stored.task.clone();
        repaired.conditions = Some(probe_conditions());
        repaired.touch();
        assert!(db
            .update_scheduled_task(&repaired, &stored.task.modified_at, true)
            .await
            .unwrap());
        let read = db.get_scheduled_task(&task.id).await.unwrap().expect("row");
        assert_eq!(
            read.conditions.and_then(|c| c.require_probe),
            probe_conditions().require_probe
        );

        db.delete_scheduled_task(&task.id).await.expect("cleanup");
    }
}
