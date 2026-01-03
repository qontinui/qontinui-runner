//! CI/CD Scheduler
//!
//! This module provides scheduling functionality for workflows and prompts,
//! enabling automated execution at specified times with completion tracking.

// Allow dead code - these are public API functions that may not be called yet
// but are part of the complete scheduler interface
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tracing::{error, info};
use uuid::Uuid;

const SCHEDULER_FILE: &str = "scheduler.json";

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdleCondition {
    pub enabled: bool,
}

impl Default for IdleCondition {
    fn default() -> Self {
        Self { enabled: false }
    }
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryInactiveCondition {
    pub enabled: bool,
    /// List of repositories to watch (ALL must be inactive for condition to be met)
    #[serde(default)]
    pub repositories: Vec<RepositoryWatch>,
}

impl Default for RepositoryInactiveCondition {
    fn default() -> Self {
        Self {
            enabled: false,
            repositories: Vec::new(),
        }
    }
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
    /// Run a workflow from loaded config
    Workflow {
        workflow_name: String,
        #[serde(default)]
        config_path: Option<String>,
        #[serde(default)]
        monitor_index: Option<i32>,
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

// ============================================================================
// Scheduler State
// ============================================================================

/// Full scheduler state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerState {
    /// Version of the scheduler format (for future migrations)
    #[serde(default = "default_version")]
    pub version: String,
    /// All scheduled tasks
    #[serde(default)]
    pub tasks: Vec<ScheduledTask>,
    /// Execution history (last N runs per task)
    #[serde(default)]
    pub history: HashMap<String, Vec<TaskExecutionRecord>>,
    /// Max history entries per task
    #[serde(default = "default_max_history")]
    pub max_history_per_task: usize,
    /// Global scheduler settings
    #[serde(default)]
    pub settings: SchedulerSettings,
}

fn default_version() -> String {
    "1.0.0".to_string()
}

fn default_max_history() -> usize {
    50
}

impl Default for SchedulerState {
    fn default() -> Self {
        Self {
            version: default_version(),
            tasks: Vec::new(),
            history: HashMap::new(),
            max_history_per_task: default_max_history(),
            settings: SchedulerSettings::default(),
        }
    }
}

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
// File Operations
// ============================================================================

/// Get the scheduler file path in the app data directory
fn get_scheduler_path() -> Result<PathBuf, String> {
    let app_data_dir = dirs::config_dir()
        .ok_or("Failed to get config directory")?
        .join("com.qontinui.runner");

    // Create directory if it doesn't exist
    if !app_data_dir.exists() {
        fs::create_dir_all(&app_data_dir)
            .map_err(|e| format!("Failed to create app data directory: {}", e))?;
    }

    Ok(app_data_dir.join(SCHEDULER_FILE))
}

/// Load the scheduler state from disk
pub fn load_scheduler_state() -> SchedulerState {
    match get_scheduler_path() {
        Ok(path) => {
            if path.exists() {
                match fs::read_to_string(&path) {
                    Ok(contents) => match serde_json::from_str(&contents) {
                        Ok(state) => {
                            info!("Loaded scheduler state from {:?}", path);
                            state
                        }
                        Err(e) => {
                            error!("Failed to parse scheduler file: {}", e);
                            SchedulerState::default()
                        }
                    },
                    Err(e) => {
                        error!("Failed to read scheduler file: {}", e);
                        SchedulerState::default()
                    }
                }
            } else {
                info!("No scheduler file found, using empty state");
                SchedulerState::default()
            }
        }
        Err(e) => {
            error!("Failed to get scheduler path: {}", e);
            SchedulerState::default()
        }
    }
}

/// Save the scheduler state to disk
pub fn save_scheduler_state(state: &SchedulerState) -> Result<(), String> {
    let path = get_scheduler_path()?;

    let contents = serde_json::to_string_pretty(state)
        .map_err(|e| format!("Failed to serialize scheduler state: {}", e))?;

    fs::write(&path, contents).map_err(|e| format!("Failed to write scheduler file: {}", e))?;

    info!("Saved scheduler state to {:?}", path);
    Ok(())
}

// ============================================================================
// CRUD Operations
// ============================================================================

/// Get all scheduled tasks
pub fn get_all_tasks() -> Vec<ScheduledTask> {
    load_scheduler_state().tasks
}

/// Get a task by ID
pub fn get_task(id: &str) -> Option<ScheduledTask> {
    load_scheduler_state()
        .tasks
        .into_iter()
        .find(|t| t.id == id)
}

/// Create a new scheduled task
#[allow(clippy::too_many_arguments)]
pub fn create_task(
    name: String,
    description: Option<String>,
    schedule: ScheduleExpression,
    task: ScheduledTaskType,
    skip_if_completed: bool,
    auto_fix_on_failure: bool,
    success_criteria: Option<String>,
    conditions: Option<ScheduleConditions>,
) -> Result<ScheduledTask, String> {
    let mut state = load_scheduler_state();

    let mut scheduled_task = ScheduledTask::new(name, description, schedule, task);
    scheduled_task.skip_if_completed = skip_if_completed;
    scheduled_task.auto_fix_on_failure = auto_fix_on_failure;
    scheduled_task.success_criteria = success_criteria;
    scheduled_task.conditions = conditions;

    let created = scheduled_task.clone();
    state.tasks.push(scheduled_task);
    save_scheduler_state(&state)?;

    info!("Created scheduled task: {} ({})", created.name, created.id);
    Ok(created)
}

/// Update an existing task
#[allow(clippy::too_many_arguments)]
pub fn update_task(
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
    let mut state = load_scheduler_state();

    let scheduled_task = state
        .tasks
        .iter_mut()
        .find(|t| t.id == id)
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
    let updated = scheduled_task.clone();

    save_scheduler_state(&state)?;
    info!("Updated scheduled task: {} ({})", updated.name, updated.id);
    Ok(updated)
}

/// Delete a task
pub fn delete_task(id: &str) -> Result<(), String> {
    let mut state = load_scheduler_state();

    let initial_len = state.tasks.len();
    state.tasks.retain(|t| t.id != id);

    if state.tasks.len() == initial_len {
        return Err(format!("Task not found: {}", id));
    }

    // Also remove history
    state.history.remove(id);

    save_scheduler_state(&state)?;
    info!("Deleted scheduled task: {}", id);
    Ok(())
}

/// Record an execution
pub fn record_execution(task_id: &str, record: TaskExecutionRecord) -> Result<(), String> {
    let mut state = load_scheduler_state();

    // Update last_run on the task
    if let Some(task) = state.tasks.iter_mut().find(|t| t.id == task_id) {
        task.last_run = Some(record.clone());
        task.touch();
    }

    // Add to history
    let history = state.history.entry(task_id.to_string()).or_default();
    history.push(record);

    // Trim history if needed
    let max = state.max_history_per_task;
    if history.len() > max {
        let to_remove = history.len() - max;
        history.drain(0..to_remove);
    }

    save_scheduler_state(&state)?;
    Ok(())
}

/// Get execution history for a task
pub fn get_task_history(task_id: &str) -> Vec<TaskExecutionRecord> {
    load_scheduler_state()
        .history
        .get(task_id)
        .cloned()
        .unwrap_or_default()
}

/// Update the condition status for a task
pub fn update_task_condition_status(task_id: &str, status: ConditionStatus) -> Result<(), String> {
    let mut state = load_scheduler_state();

    if let Some(task) = state.tasks.iter_mut().find(|t| t.id == task_id) {
        task.condition_status = Some(status);
        task.touch();
    }

    save_scheduler_state(&state)?;
    Ok(())
}

/// Clear the condition status for a task (after execution or timeout)
pub fn clear_task_condition_status(task_id: &str) -> Result<(), String> {
    let mut state = load_scheduler_state();

    if let Some(task) = state.tasks.iter_mut().find(|t| t.id == task_id) {
        task.condition_status = None;
        task.touch();
    }

    save_scheduler_state(&state)?;
    Ok(())
}

/// Get scheduler settings
pub fn get_scheduler_settings() -> SchedulerSettings {
    load_scheduler_state().settings
}

/// Update scheduler settings
pub fn update_scheduler_settings(settings: SchedulerSettings) -> Result<(), String> {
    let mut state = load_scheduler_state();
    state.settings = settings;
    save_scheduler_state(&state)?;
    info!("Updated scheduler settings");
    Ok(())
}

/// Get current scheduler status
pub fn get_scheduler_status() -> SchedulerStatus {
    let state = load_scheduler_state();

    let running_tasks = state
        .tasks
        .iter()
        .filter(|t| {
            t.last_run
                .as_ref()
                .map(|r| r.status == ScheduledTaskStatus::Running)
                .unwrap_or(false)
        })
        .count() as u32;

    let pending_tasks = state.tasks.iter().filter(|t| t.enabled).count() as u32;

    // Find next task to run
    let next_task = state
        .tasks
        .iter()
        .filter(|t| t.enabled && t.next_run.is_some())
        .min_by_key(|t| t.next_run.as_ref().unwrap())
        .map(|t| NextTaskInfo {
            id: t.id.clone(),
            name: t.name.clone(),
            next_run: t.next_run.clone().unwrap(),
        });

    SchedulerStatus {
        enabled: state.settings.enabled,
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
pub fn update_all_next_runs() -> Result<(), String> {
    let mut state = load_scheduler_state();
    let now = chrono::Utc::now();

    for task in state.tasks.iter_mut() {
        if task.enabled {
            task.next_run = compute_next_run(&task.schedule, now).map(|dt| dt.to_rfc3339());
        } else {
            task.next_run = None;
        }
    }

    save_scheduler_state(&state)?;
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
