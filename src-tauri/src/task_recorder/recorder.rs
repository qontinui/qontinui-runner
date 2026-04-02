//! TaskRecorder - Unified task execution recorder.
//!
//! Provides the main interface for creating and managing task runs.

#![allow(dead_code)]

use crate::database::CreateTaskRunInput;
use std::sync::Arc;
use tracing::{debug, info};

/// Task type determines the nature of the task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskType {
    /// Standard AI task with prompt
    #[default]
    Task,
    /// Pure GUI automation task
    Automation,
    /// Scheduler-triggered task
    Scheduled,
}

impl TaskType {
    /// Convert to database string value.
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskType::Task => "task",
            TaskType::Automation => "automation",
            TaskType::Scheduled => "scheduled",
        }
    }
}

/// Configuration for creating a new task.
#[derive(Debug, Clone)]
pub struct TaskConfig {
    /// Human-readable name for the task
    pub task_name: String,
    /// Task prompt (None for pure automation tasks)
    pub prompt: Option<String>,
    /// Type of task
    pub task_type: TaskType,
    /// Maximum number of AI sessions allowed (None = unlimited)
    pub max_sessions: Option<u32>,
    /// Whether to auto-continue on session completion
    pub auto_continue: bool,
    /// Config ID for automation-enabled tasks
    pub config_id: Option<String>,
    /// Workflow name being executed
    pub workflow_name: Option<String>,
    /// JSON-encoded execution steps to run before each AI session
    pub execution_steps_json: Option<String>,
    /// JSON-encoded log sources to capture during execution
    pub log_sources_json: Option<String>,
    /// Workspace/organization ID (from web backend when task is created via API)
    pub workspace_id: Option<String>,
    /// User ID who triggered the task (from web backend when task is created via API)
    pub triggered_by: Option<String>,
    /// Bridge ID handling this task (for multi-bridge scenarios)
    pub bridge_id: Option<String>,
}

impl TaskConfig {
    /// Create a new task configuration for an AI task.
    pub fn ai_task(task_name: &str, prompt: &str) -> Self {
        Self {
            task_name: task_name.to_string(),
            prompt: Some(prompt.to_string()),
            task_type: TaskType::Task,
            max_sessions: None,
            auto_continue: true,
            config_id: None,
            workflow_name: None,
            execution_steps_json: None,
            log_sources_json: None,
            workspace_id: None,
            triggered_by: None,
            bridge_id: None,
        }
    }

    /// Create a new task configuration for an automation task.
    pub fn automation_task(task_name: &str, config_id: &str, workflow_name: Option<&str>) -> Self {
        Self {
            task_name: task_name.to_string(),
            prompt: None,
            task_type: TaskType::Automation,
            max_sessions: None,
            auto_continue: false,
            config_id: Some(config_id.to_string()),
            workflow_name: workflow_name.map(|s| s.to_string()),
            execution_steps_json: None,
            log_sources_json: None,
            workspace_id: None,
            triggered_by: None,
            bridge_id: None,
        }
    }

    /// Create a new task configuration for a mixed AI+automation task.
    pub fn mixed_task(
        task_name: &str,
        prompt: &str,
        config_id: &str,
        workflow_name: Option<&str>,
    ) -> Self {
        Self {
            task_name: task_name.to_string(),
            prompt: Some(prompt.to_string()),
            task_type: TaskType::Task,
            max_sessions: None,
            auto_continue: true,
            config_id: Some(config_id.to_string()),
            workflow_name: workflow_name.map(|s| s.to_string()),
            execution_steps_json: None,
            log_sources_json: None,
            workspace_id: None,
            triggered_by: None,
            bridge_id: None,
        }
    }

    /// Set the maximum number of sessions.
    pub fn with_max_sessions(mut self, max: u32) -> Self {
        self.max_sessions = Some(max);
        self
    }

    /// Set auto-continue behavior.
    pub fn with_auto_continue(mut self, auto_continue: bool) -> Self {
        self.auto_continue = auto_continue;
        self
    }

    /// Set execution steps JSON.
    pub fn with_execution_steps(mut self, steps_json: String) -> Self {
        self.execution_steps_json = Some(steps_json);
        self
    }

    /// Set log sources JSON.
    pub fn with_log_sources(mut self, log_sources_json: String) -> Self {
        self.log_sources_json = Some(log_sources_json);
        self
    }

    /// Set workspace ID (from web backend).
    pub fn with_workspace_id(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = Some(workspace_id.into());
        self
    }

    /// Set the user ID who triggered the task (from web backend).
    pub fn with_triggered_by(mut self, user_id: impl Into<String>) -> Self {
        self.triggered_by = Some(user_id.into());
        self
    }

    /// Set the bridge ID handling this task (for multi-bridge scenarios).
    pub fn with_bridge_id(mut self, bridge_id: impl Into<String>) -> Self {
        self.bridge_id = Some(bridge_id.into());
        self
    }
}

/// Handle to an active task run.
///
/// This handle provides methods to update the task as it executes.
#[derive(Clone)]
pub struct TaskRunHandle {
    /// Task run ID
    pub id: String,
}

impl std::fmt::Debug for TaskRunHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskRunHandle")
            .field("id", &self.id)
            .finish()
    }
}

impl TaskRunHandle {
    /// Get the task run ID.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Append output to the task log.
    ///
    /// Returns true if [TASK_COMPLETE] was detected in the output.
    pub fn append_output(&self, _output: &str) -> Result<bool, String> {
        Err("SQLite removed".into())
    }

    /// Append output to the task log and increment the session count.
    ///
    /// Returns true if [TASK_COMPLETE] was detected in the output.
    pub fn append_output_with_session_increment(&self, _output: &str) -> Result<bool, String> {
        Err("SQLite removed".into())
    }

    /// Complete the task with success.
    pub fn complete(&self) -> Result<(), String> {
        Err("SQLite removed".into())
    }

    /// Complete the task with failure.
    pub fn fail(&self, _error_message: &str) -> Result<(), String> {
        Err("SQLite removed".into())
    }

    /// Stop the task.
    pub fn stop(&self) -> Result<(), String> {
        Err("SQLite removed".into())
    }

    /// Update the task summary.
    pub fn set_summary(
        &self,
        _summary: &str,
        _goal_achieved: bool,
        _remaining_work: Option<&str>,
    ) -> Result<(), String> {
        Err("SQLite removed".into())
    }
}

/// TaskRecorder - Unified task execution recorder.
///
/// This is the main entry point for creating and managing task runs.
/// All task execution should go through TaskRecorder to ensure consistent
/// recording and tracking.
pub struct TaskRecorder;

impl TaskRecorder {
    /// Create a new TaskRecorder.
    pub fn new() -> Self {
        Self
    }

    /// Start a new task run.
    ///
    /// Returns a handle that can be used to update the task as it executes.
    /// SQLite removed — returns Err.
    pub fn start_task(&self, _config: TaskConfig) -> Result<TaskRunHandle, String> {
        Err("SQLite removed".into())
    }

    /// Start a new task run with a specific ID.
    pub fn start_task_with_id(
        &self,
        _id: &str,
        _config: TaskConfig,
    ) -> Result<TaskRunHandle, String> {
        Err("SQLite removed".into())
    }

    /// Get an existing task run handle.
    pub fn get_task_handle(&self, _id: &str) -> Result<TaskRunHandle, String> {
        Err("SQLite removed".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_type_as_str() {
        assert_eq!(TaskType::Task.as_str(), "task");
        assert_eq!(TaskType::Automation.as_str(), "automation");
        assert_eq!(TaskType::Scheduled.as_str(), "scheduled");
    }

    #[test]
    fn test_task_config_builder() {
        let config = TaskConfig::ai_task("Test Task", "Do something")
            .with_max_sessions(5)
            .with_auto_continue(false);

        assert_eq!(config.task_name, "Test Task");
        assert_eq!(config.prompt, Some("Do something".to_string()));
        assert_eq!(config.max_sessions, Some(5));
        assert!(!config.auto_continue);
    }

    #[test]
    fn test_automation_config() {
        let config = TaskConfig::automation_task("Auto Task", "config-123", Some("workflow-1"));

        assert_eq!(config.task_name, "Auto Task");
        assert_eq!(config.prompt, None);
        assert_eq!(config.task_type, TaskType::Automation);
        assert_eq!(config.config_id, Some("config-123".to_string()));
        assert_eq!(config.workflow_name, Some("workflow-1".to_string()));
    }
}
