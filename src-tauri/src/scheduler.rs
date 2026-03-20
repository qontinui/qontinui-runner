//! CI/CD Scheduler
//!
//! This module provides scheduling functionality for workflows and prompts,
//! enabling automated execution at specified times with completion tracking.

// Allow dead code - these are public API functions that may not be called yet
// but are part of the complete scheduler interface
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tracing::{error, info};
use uuid::Uuid;

use crate::database::CheckpointDb;

// ============================================================================
// Schedule Expression Types
// ============================================================================

/// How a task should be scheduled
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum ScheduleExpression {
    /// Run once at a specific datetime (ISO 8601)
    Once(String),
    /// Cron expression (e.g., "0 9 * * *" for 9 AM daily)
    Cron(String),
    /// Interval in seconds (for testing/debugging)
    Interval(u64),
}

impl Default for ScheduleExpression {
    fn default() -> Self {
        ScheduleExpression::Once(chrono::Utc::now().to_rfc3339())
    }
}

// ============================================================================
// Schedule Conditions
// ============================================================================

/// Condition that requires the runner to be idle (not executing workflows or AI tasks)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IdleCondition {
    pub enabled: bool,
}

/// A single repository to monitor for inactivity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryWatch {
    /// Path to the repository directory
    pub path: String,
    /// Minutes of inactivity required before condition is met
    pub inactive_minutes: u32,
}

/// Condition that requires repositories to have no file modifications for a period
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RepositoryInactiveCondition {
    pub enabled: bool,
    /// List of repositories to watch (ALL must be inactive for condition to be met)
    #[serde(default)]
    pub repositories: Vec<RepositoryWatch>,
}

/// Conditions that must ALL be met before task execution
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScheduleConditions {
    /// Require runner to be idle (not executing workflows or AI tasks)
    #[serde(default)]
    pub require_idle: Option<IdleCondition>,
    /// Require repository file inactivity
    #[serde(default)]
    pub require_repo_inactive: Option<RepositoryInactiveCondition>,
    /// Maximum time to wait for conditions (minutes). None = wait indefinitely
    #[serde(default)]
    pub timeout_minutes: Option<u32>,
}

/// Status of condition checking for a deferred task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionStatus {
    /// Time when conditions started being checked (ISO 8601)
    pub waiting_since: String,
    /// Current idle condition status (None if not checked, Some(true) if idle, Some(false) if busy)
    #[serde(default)]
    pub idle_met: Option<bool>,
    /// Current repo inactive status per repository: (path, is_inactive)
    #[serde(default)]
    pub repo_inactive_met: Option<Vec<(String, bool)>>,
    /// Whether timeout has been exceeded
    #[serde(default)]
    pub timed_out: bool,
}

impl Default for ConditionStatus {
    fn default() -> Self {
        Self {
            waiting_since: chrono::Utc::now().to_rfc3339(),
            idle_met: None,
            repo_inactive_met: None,
            timed_out: false,
        }
    }
}

// ============================================================================
// Task Type Definitions
// ============================================================================

/// Type of task to schedule
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "task_type")]
pub enum ScheduledTaskType {
    /// Run a workflow from loaded config or unified workflow by ID
    Workflow {
        workflow_name: String,
        #[serde(default)]
        config_path: Option<String>,
        #[serde(default)]
        monitor_index: Option<i32>,
        /// If set, run unified workflow by ID instead of legacy workflow by name
        #[serde(default)]
        workflow_id: Option<String>,
    },
    /// Run a prompt from Prompt Library
    Prompt {
        prompt_id: String,
        /// Optional override for max_sessions (None = use prompt's setting)
        #[serde(default)]
        max_sessions: Option<u32>,
    },
    /// Trigger auto-fix (check findings and fix auto-fixable items)
    AutoFix {
        #[serde(default = "default_true")]
        check_findings: bool,
        #[serde(default)]
        force_run: bool,
    },
}

fn default_true() -> bool {
    true
}

impl Default for ScheduledTaskType {
    fn default() -> Self {
        ScheduledTaskType::AutoFix {
            check_findings: true,
            force_run: false,
        }
    }
}

// ============================================================================
// Task Status
// ============================================================================

/// Status of a scheduled task execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ScheduledTaskStatus {
    #[default]
    Pending,
    Running,
    Completed,
    Failed,
    /// Skipped because already completed and skip_if_completed=true
    Skipped,
    Cancelled,
}

// ============================================================================
// Execution Record
// ============================================================================

/// Record of a single task execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskExecutionRecord {
    /// Unique ID for this execution
    pub execution_id: String,
    /// Session ID if this triggered an AI session (for success tracking)
    #[serde(default)]
    pub session_id: Option<String>,
    /// ISO 8601 timestamp when execution started
    pub started_at: String,
    /// ISO 8601 timestamp when execution ended
    #[serde(default)]
    pub ended_at: Option<String>,
    /// Current status
    pub status: ScheduledTaskStatus,
    /// Whether the task succeeded (read from session checkpoint)
    #[serde(default)]
    pub success: bool,
    /// Error message if failed
    #[serde(default)]
    pub error_message: Option<String>,
    /// Whether auto-fix was triggered after this execution
    #[serde(default)]
    pub triggered_auto_fix: bool,
    /// Session ID of the auto-fix session if triggered
    #[serde(default)]
    pub auto_fix_session_id: Option<String>,
}

impl TaskExecutionRecord {
    /// Create a new execution record
    pub fn new() -> Self {
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

    /// Mark execution as completed
    pub fn complete(&mut self, success: bool, error_message: Option<String>) {
        self.ended_at = Some(chrono::Utc::now().to_rfc3339());
        self.success = success;
        self.error_message = error_message;
        self.status = if success {
            ScheduledTaskStatus::Completed
        } else {
            ScheduledTaskStatus::Failed
        };
    }

    /// Mark that auto-fix was triggered
    pub fn mark_auto_fix_triggered(&mut self, session_id: String) {
        self.triggered_auto_fix = true;
        self.auto_fix_session_id = Some(session_id);
    }
}

impl Default for TaskExecutionRecord {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Scheduled Task
// ============================================================================

/// A scheduled task definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTask {
    /// Unique identifier (UUID v4)
    pub id: String,
    /// Display name for the task
    pub name: String,
    /// Optional description
    #[serde(default)]
    pub description: Option<String>,
    /// Whether the task is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Schedule configuration
    pub schedule: ScheduleExpression,
    /// Task type and configuration
    pub task: ScheduledTaskType,
    /// Whether to skip if task has already succeeded
    #[serde(default)]
    pub skip_if_completed: bool,
    /// Auto-trigger auto-fix on failure
    #[serde(default)]
    pub auto_fix_on_failure: bool,
    /// Success criteria description (for reference)
    #[serde(default)]
    pub success_criteria: Option<String>,
    /// ISO 8601 timestamp of creation
    pub created_at: String,
    /// ISO 8601 timestamp of last modification
    pub modified_at: String,
    /// Last execution record
    #[serde(default)]
    pub last_run: Option<TaskExecutionRecord>,
    /// Next scheduled run time (computed)
    #[serde(default)]
    pub next_run: Option<String>,
    /// Optional conditions that must be met before execution
    #[serde(default)]
    pub conditions: Option<ScheduleConditions>,
    /// Status when task is waiting for conditions to be met
    #[serde(default)]
    pub condition_status: Option<ConditionStatus>,
}

impl ScheduledTask {
    /// Create a new scheduled task
    pub fn new(
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

    /// Check if task has conditions that need to be evaluated
    pub fn has_conditions(&self) -> bool {
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

    /// Check if task is currently waiting for conditions
    pub fn is_waiting_for_conditions(&self) -> bool {
        self.condition_status.is_some()
    }

    /// Check if task should be skipped (completed and skip_if_completed is true)
    pub fn should_skip(&self) -> bool {
        if !self.skip_if_completed {
            return false;
        }
        self.last_run.as_ref().map(|r| r.success).unwrap_or(false)
    }

    /// Update the modified timestamp
    pub fn touch(&mut self) {
        self.modified_at = chrono::Utc::now().to_rfc3339();
    }
}

// ============================================================================
// Scheduler Settings
// ============================================================================

/// Global scheduler settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerSettings {
    /// Scheduler enabled globally
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Maximum concurrent scheduled tasks
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    /// Default auto-fix on failure setting for new tasks
    #[serde(default)]
    pub default_auto_fix_on_failure: bool,
    /// Timezone for schedule interpretation (default: local)
    #[serde(default)]
    pub timezone: Option<String>,
}

fn default_max_concurrent() -> u32 {
    1
}

impl Default for SchedulerSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_concurrent: 1,
            default_auto_fix_on_failure: false,
            timezone: None,
        }
    }
}

// SchedulerState was removed — all persistence is now handled by CheckpointDb.

// ============================================================================
// Scheduler Status (for API responses)
// ============================================================================

/// Current scheduler status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerStatus {
    pub enabled: bool,
    pub running_tasks: u32,
    pub pending_tasks: u32,
    pub next_task: Option<NextTaskInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextTaskInfo {
    pub id: String,
    pub name: String,
    pub next_run: String,
}

// ============================================================================
// CRUD Operations (backed by CheckpointDb)
// ============================================================================

/// Get all scheduled tasks
pub fn get_all_tasks(db: &CheckpointDb) -> Vec<ScheduledTask> {
    match db.get_all_scheduled_tasks() {
        Ok(tasks) => tasks,
        Err(e) => {
            error!("Failed to get all scheduled tasks: {}", e);
            Vec::new()
        }
    }
}

/// Get a task by ID
pub fn get_task(db: &CheckpointDb, id: &str) -> Option<ScheduledTask> {
    match db.get_scheduled_task(id) {
        Ok(task) => task,
        Err(e) => {
            error!("Failed to get scheduled task {}: {}", id, e);
            None
        }
    }
}

/// Create a new scheduled task
#[allow(clippy::too_many_arguments)]
pub fn create_task(
    db: &CheckpointDb,
    name: String,
    description: Option<String>,
    schedule: ScheduleExpression,
    task: ScheduledTaskType,
    skip_if_completed: bool,
    auto_fix_on_failure: bool,
    success_criteria: Option<String>,
    conditions: Option<ScheduleConditions>,
) -> Result<ScheduledTask, String> {
    let mut scheduled_task = ScheduledTask::new(name, description, schedule, task);
    scheduled_task.skip_if_completed = skip_if_completed;
    scheduled_task.auto_fix_on_failure = auto_fix_on_failure;
    scheduled_task.success_criteria = success_criteria;
    scheduled_task.conditions = conditions;

    db.insert_scheduled_task(&scheduled_task)?;

    info!("Created scheduled task: {} ({})", scheduled_task.name, scheduled_task.id);
    Ok(scheduled_task)
}

/// Update an existing task
#[allow(clippy::too_many_arguments)]
pub fn update_task(
    db: &CheckpointDb,
    id: &str,
    name: Option<String>,
    description: Option<Option<String>>,
    enabled: Option<bool>,
    schedule: Option<ScheduleExpression>,
    task: Option<ScheduledTaskType>,
    skip_if_completed: Option<bool>,
    auto_fix_on_failure: Option<bool>,
    success_criteria: Option<Option<String>>,
    conditions: Option<Option<ScheduleConditions>>,
) -> Result<ScheduledTask, String> {
    let mut scheduled_task = db
        .get_scheduled_task(id)?
        .ok_or_else(|| format!("Task not found: {}", id))?;

    if let Some(name) = name {
        scheduled_task.name = name;
    }
    if let Some(description) = description {
        scheduled_task.description = description;
    }
    if let Some(enabled) = enabled {
        scheduled_task.enabled = enabled;
    }
    if let Some(schedule) = schedule {
        scheduled_task.schedule = schedule;
    }
    if let Some(task) = task {
        scheduled_task.task = task;
    }
    if let Some(skip_if_completed) = skip_if_completed {
        scheduled_task.skip_if_completed = skip_if_completed;
    }
    if let Some(auto_fix_on_failure) = auto_fix_on_failure {
        scheduled_task.auto_fix_on_failure = auto_fix_on_failure;
    }
    if let Some(success_criteria) = success_criteria {
        scheduled_task.success_criteria = success_criteria;
    }
    if let Some(conditions) = conditions {
        scheduled_task.conditions = conditions;
        // Clear condition_status when conditions change
        scheduled_task.condition_status = None;
    }

    scheduled_task.touch();
    db.update_scheduled_task(&scheduled_task)?;

    info!("Updated scheduled task: {} ({})", scheduled_task.name, scheduled_task.id);
    Ok(scheduled_task)
}

/// Delete a task
pub fn delete_task(db: &CheckpointDb, id: &str) -> Result<(), String> {
    // Verify task exists first
    db.get_scheduled_task(id)?
        .ok_or_else(|| format!("Task not found: {}", id))?;

    db.delete_scheduled_task(id)?;
    info!("Deleted scheduled task: {}", id);
    Ok(())
}

/// Record an execution
pub fn record_execution(db: &CheckpointDb, task_id: &str, record: TaskExecutionRecord) -> Result<(), String> {
    // Update last_run on the task
    db.update_task_last_run(task_id, Some(&record.execution_id))?;

    // Add to history
    db.insert_execution_record(task_id, &record)?;

    // Trim history if needed (keep max 50 entries)
    db.trim_execution_history(task_id, 50)?;

    Ok(())
}

/// Get execution history for a task
pub fn get_task_history(db: &CheckpointDb, task_id: &str) -> Vec<TaskExecutionRecord> {
    match db.get_execution_history(task_id, 50) {
        Ok(records) => records,
        Err(e) => {
            error!("Failed to get task history for {}: {}", task_id, e);
            Vec::new()
        }
    }
}

/// Update the condition status for a task
pub fn update_task_condition_status(db: &CheckpointDb, task_id: &str, status: ConditionStatus) -> Result<(), String> {
    let status_json = serde_json::to_string(&status)
        .map_err(|e| format!("Failed to serialize condition status: {}", e))?;
    db.update_task_condition_status(task_id, Some(&status_json))
}

/// Clear the condition status for a task (after execution or timeout)
pub fn clear_task_condition_status(db: &CheckpointDb, task_id: &str) -> Result<(), String> {
    db.update_task_condition_status(task_id, None)
}

/// Get scheduler settings
pub fn get_scheduler_settings(db: &CheckpointDb) -> SchedulerSettings {
    match db.get_scheduler_settings() {
        Ok(settings) => settings,
        Err(e) => {
            error!("Failed to get scheduler settings: {}", e);
            SchedulerSettings::default()
        }
    }
}

/// Update scheduler settings
pub fn update_scheduler_settings(db: &CheckpointDb, settings: SchedulerSettings) -> Result<(), String> {
    db.update_scheduler_settings(&settings)?;
    info!("Updated scheduler settings");
    Ok(())
}

/// Get current scheduler status
pub fn get_scheduler_status(db: &CheckpointDb) -> SchedulerStatus {
    let tasks = get_all_tasks(db);
    let settings = get_scheduler_settings(db);

    let running_tasks = tasks
        .iter()
        .filter(|t| {
            t.last_run
                .as_ref()
                .map(|r| r.status == ScheduledTaskStatus::Running)
                .unwrap_or(false)
        })
        .count() as u32;

    let pending_tasks = tasks.iter().filter(|t| t.enabled).count() as u32;

    // Find next task to run
    let next_task = tasks
        .iter()
        .filter(|t| t.enabled && t.next_run.is_some())
        .min_by_key(|t| t.next_run.as_ref().unwrap())
        .map(|t| NextTaskInfo {
            id: t.id.clone(),
            name: t.name.clone(),
            next_run: t.next_run.clone().unwrap(),
        });

    SchedulerStatus {
        enabled: settings.enabled,
        running_tasks,
        pending_tasks,
        next_task,
    }
}

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
            // Use the cron crate to compute next run
            use cron::Schedule;
            use std::str::FromStr;

            Schedule::from_str(cron_expr)
                .ok()
                .and_then(|schedule| schedule.after(&from).next())
        }
        ScheduleExpression::Interval(seconds) => {
            // Next run is from + interval
            Some(from + chrono::Duration::seconds(*seconds as i64))
        }
    }
}

/// Update next_run for all tasks
pub fn update_all_next_runs(db: &CheckpointDb) -> Result<(), String> {
    let tasks = get_all_tasks(db);
    let now = chrono::Utc::now();

    for task in &tasks {
        let next_run = if task.enabled {
            compute_next_run(&task.schedule, now).map(|dt| dt.to_rfc3339())
        } else {
            None
        };
        db.update_task_next_run(&task.id, next_run.as_deref())?;
    }

    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cron_parsing() {
        // Test daily at 9 AM
        let schedule = ScheduleExpression::Cron("0 0 9 * * *".to_string());
        let now = chrono::Utc::now();
        let next = compute_next_run(&schedule, now);
        assert!(next.is_some());
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
            ScheduleExpression::default(),
            ScheduledTaskType::default(),
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
