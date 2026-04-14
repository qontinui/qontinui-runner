//! CI/CD Scheduler
//!
//! This module provides scheduling functionality for workflows and prompts,
//! enabling automated execution at specified times with completion tracking.
//!
//! The DTO shape of the scheduler types lives in the `qontinui-types` crate
//! (`qontinui_types::scheduler`) and is re-exported via `pub use` below.
//! Runner-specific behavior (constructors, condition evaluation, rearm checks,
//! `Default` impls that need a timestamp, next-run computation) stays here.
//! Because of Rust's orphan rule we can't define inherent `impl` blocks or
//! `impl Default` on the foreign types, so the methods/defaults are exposed
//! through extension traits (`ScheduledTaskExt`, `TaskExecutionRecordExt`)
//! and free `*_default` helpers. The traits are re-exported so callers can
//! keep using `ScheduledTask::new(...)`, `task.should_skip()`, etc. verbatim
//! as long as the scheduler module's contents are brought into scope.

// Allow dead code - these are public API functions that may not be called yet
// but are part of the complete scheduler interface
#![allow(dead_code)]

pub use qontinui_types::scheduler::*;

use tracing::{error, info};
use uuid::Uuid;

// ============================================================================
// Default helpers (orphan rule prevents `impl Default` for foreign types)
// ============================================================================

/// Default [`ScheduleExpression`] — a one-shot run scheduled for "now".
///
/// Previously `impl Default for ScheduleExpression`. Moved to a free helper
/// because `ScheduleExpression` is now defined in `qontinui-types` and Rust's
/// orphan rule forbids an `impl Default` here. Use this helper everywhere the
/// old `ScheduleExpression::default()` was used.
pub fn schedule_expression_default() -> ScheduleExpression {
    ScheduleExpression::Once(chrono::Utc::now().to_rfc3339())
}

/// Default [`ConditionStatus`] — waiting as of right now, no sub-conditions
/// evaluated yet, not timed out.
pub fn condition_status_default() -> ConditionStatus {
    ConditionStatus {
        waiting_since: chrono::Utc::now().to_rfc3339(),
        idle_met: None,
        repo_inactive_met: None,
        timed_out: false,
    }
}

/// Default [`ScheduledTaskType`] — a no-op AutoFix with `check_findings=true`.
pub fn scheduled_task_type_default() -> ScheduledTaskType {
    ScheduledTaskType::AutoFix {
        check_findings: true,
        force_run: false,
    }
}

// ============================================================================
// TaskExecutionRecord behavior (extension trait)
// ============================================================================

/// Runner-side behavior for [`TaskExecutionRecord`].
///
/// Exposed as a trait because `TaskExecutionRecord` is defined in
/// `qontinui-types` and we cannot add inherent methods to it here. Import this
/// trait (or use `crate::scheduler::*`) to call these methods exactly as if
/// they were inherent.
pub trait TaskExecutionRecordExt: Sized {
    /// Create a new execution record stamped with the current time and a
    /// fresh UUID v4 for `execution_id`.
    fn new() -> Self;
    /// Mark execution as completed, setting `ended_at`, `success`,
    /// `error_message`, and `status` accordingly.
    fn complete(&mut self, success: bool, error_message: Option<String>);
    /// Mark that auto-fix was triggered for this execution, recording the
    /// auto-fix session id.
    fn mark_auto_fix_triggered(&mut self, session_id: String);
    /// Runner-side default that matches the pre-refactor `Default` impl
    /// (delegates to [`TaskExecutionRecordExt::new`]).
    fn runner_default() -> Self {
        Self::new()
    }
}

impl TaskExecutionRecordExt for TaskExecutionRecord {
    fn new() -> Self {
        Self {
            execution_id: Uuid::new_v4().to_string(),
            session_id: None,
            started_at: chrono::Utc::now().to_rfc3339(),
            ended_at: None,
            status: ScheduledTaskStatus::Running,
            success: false,
            error_message: None,
            triggered_auto_fix: false,
            auto_fix_session_id: None,
        }
    }

    fn complete(&mut self, success: bool, error_message: Option<String>) {
        self.ended_at = Some(chrono::Utc::now().to_rfc3339());
        self.success = success;
        self.error_message = error_message;
        self.status = if success {
            ScheduledTaskStatus::Completed
        } else {
            ScheduledTaskStatus::Failed
        };
    }

    fn mark_auto_fix_triggered(&mut self, session_id: String) {
        self.triggered_auto_fix = true;
        self.auto_fix_session_id = Some(session_id);
    }
}

// ============================================================================
// ScheduledTask behavior (extension trait)
// ============================================================================

/// Runner-side behavior for [`ScheduledTask`].
///
/// Exposed as a trait for the same orphan-rule reason as
/// [`TaskExecutionRecordExt`]. Import this trait (or `crate::scheduler::*`)
/// to call `ScheduledTask::new(...)`, `task.should_skip()`, etc. exactly as
/// if they were inherent.
pub trait ScheduledTaskExt: Sized {
    /// Create a new scheduled task with default flags, a fresh UUID, and
    /// `created_at`/`modified_at` stamped to now.
    fn new(
        name: String,
        description: Option<String>,
        schedule: ScheduleExpression,
        task: ScheduledTaskType,
    ) -> Self;
    /// Whether the task has at least one active condition that needs to be
    /// evaluated before it can run.
    fn has_conditions(&self) -> bool;
    /// Whether the task is currently parked waiting for conditions to be
    /// met (i.e. has a non-`None` `condition_status`).
    fn is_waiting_for_conditions(&self) -> bool;
    /// Whether the task should be skipped this cycle because it has already
    /// succeeded and `skip_if_completed` is `true`.
    fn should_skip(&self) -> bool;
    /// For `Condition`-scheduled tasks: whether the rearm delay has elapsed
    /// since the last execution completed.
    fn is_rearm_ready(&self) -> bool;
    /// Update the `modified_at` timestamp to "now".
    fn touch(&mut self);
}

impl ScheduledTaskExt for ScheduledTask {
    fn new(
        name: String,
        description: Option<String>,
        schedule: ScheduleExpression,
        task: ScheduledTaskType,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            description,
            enabled: true,
            schedule,
            task,
            skip_if_completed: false,
            auto_fix_on_failure: false,
            success_criteria: None,
            created_at: now.clone(),
            modified_at: now,
            last_run: None,
            next_run: None,
            conditions: None,
            condition_status: None,
        }
    }

    fn has_conditions(&self) -> bool {
        match &self.conditions {
            Some(cond) => {
                let idle_enabled = cond
                    .require_idle
                    .as_ref()
                    .map(|c| c.enabled)
                    .unwrap_or(false);
                let repo_enabled = cond
                    .require_repo_inactive
                    .as_ref()
                    .map(|c| c.enabled && !c.repositories.is_empty())
                    .unwrap_or(false);
                idle_enabled || repo_enabled
            }
            None => false,
        }
    }

    fn is_waiting_for_conditions(&self) -> bool {
        self.condition_status.is_some()
    }

    fn should_skip(&self) -> bool {
        if !self.skip_if_completed {
            return false;
        }
        self.last_run.as_ref().map(|r| r.success).unwrap_or(false)
    }

    fn is_rearm_ready(&self) -> bool {
        match &self.schedule {
            ScheduleExpression::Condition(config) => match &self.last_run {
                Some(record) => match &record.ended_at {
                    Some(ended) => match chrono::DateTime::parse_from_rfc3339(ended) {
                        Ok(ended_dt) => {
                            let elapsed = chrono::Utc::now() - ended_dt.with_timezone(&chrono::Utc);
                            elapsed >= chrono::Duration::minutes(config.rearm_delay_minutes as i64)
                        }
                        Err(_) => true,
                    },
                    None => false,
                },
                None => true,
            },
            _ => true,
        }
    }

    fn touch(&mut self) {
        self.modified_at = chrono::Utc::now().to_rfc3339();
    }
}

// SchedulerState was removed — all persistence is now handled by CheckpointDb.

// ============================================================================
// Schedule Computation
// ============================================================================

/// Compute the next run time for a schedule expression
pub fn compute_next_run(
    schedule: &ScheduleExpression,
    from: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    match schedule {
        ScheduleExpression::Once(datetime_str) => {
            // Parse the datetime and return if it's in the future
            chrono::DateTime::parse_from_rfc3339(datetime_str)
                .ok()
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .filter(|dt| *dt > from)
        }
        ScheduleExpression::Cron(cron_expr) => {
            // Use the cron crate to compute next run (requires 6 or 7 field expressions).
            // Auto-normalize standard 5-field cron (min hr dom mon dow) by prepending "0" for seconds.
            use cron::Schedule;
            use std::str::FromStr;

            let normalized = if cron_expr.split_whitespace().count() == 5 {
                format!("0 {}", cron_expr)
            } else {
                cron_expr.clone()
            };

            Schedule::from_str(&normalized)
                .ok()
                .and_then(|schedule| schedule.after(&from).next())
        }
        ScheduleExpression::Interval(seconds) => {
            // Next run is from + interval
            Some(from + chrono::Duration::seconds(*seconds as i64))
        }
        ScheduleExpression::Condition(_) => Some(from),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cron_parsing() {
        // Test daily at 9 AM (6-field with seconds)
        let schedule = ScheduleExpression::Cron("0 0 9 * * *".to_string());
        let now = chrono::Utc::now();
        let next = compute_next_run(&schedule, now);
        assert!(next.is_some());
    }

    #[test]
    fn test_cron_5_field_normalization() {
        // Standard 5-field cron (no seconds) should be auto-normalized
        let schedule = ScheduleExpression::Cron("0 3 * * *".to_string());
        let now = chrono::Utc::now();
        let next = compute_next_run(&schedule, now);
        assert!(
            next.is_some(),
            "5-field cron '0 3 * * *' should be normalized and parsed"
        );
    }

    #[test]
    fn test_interval_schedule() {
        let schedule = ScheduleExpression::Interval(3600); // 1 hour
        let now = chrono::Utc::now();
        let next = compute_next_run(&schedule, now);
        assert!(next.is_some());
        assert!(next.unwrap() > now);
    }

    #[test]
    fn test_once_schedule_future() {
        let future = chrono::Utc::now() + chrono::Duration::hours(1);
        let schedule = ScheduleExpression::Once(future.to_rfc3339());
        let now = chrono::Utc::now();
        let next = compute_next_run(&schedule, now);
        assert!(next.is_some());
    }

    #[test]
    fn test_once_schedule_past() {
        let past = chrono::Utc::now() - chrono::Duration::hours(1);
        let schedule = ScheduleExpression::Once(past.to_rfc3339());
        let now = chrono::Utc::now();
        let next = compute_next_run(&schedule, now);
        assert!(next.is_none());
    }

    #[test]
    fn test_task_should_skip() {
        let mut task = ScheduledTask::new(
            "Test".to_string(),
            None,
            schedule_expression_default(),
            scheduled_task_type_default(),
        );

        // No skip when skip_if_completed is false
        task.skip_if_completed = false;
        assert!(!task.should_skip());

        // No skip when no last_run
        task.skip_if_completed = true;
        assert!(!task.should_skip());

        // No skip when last_run was not successful
        let mut record = TaskExecutionRecord::new();
        record.success = false;
        task.last_run = Some(record);
        assert!(!task.should_skip());

        // Skip when last_run was successful
        let mut record = TaskExecutionRecord::new();
        record.success = true;
        task.last_run = Some(record);
        assert!(task.should_skip());
    }
}
