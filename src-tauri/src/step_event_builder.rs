//! Step Event Builder Module
//!
//! Provides a builder for creating consistent task run events across all step types.
//! This replaces scattered inline event creation with a unified, maintainable pattern.

use crate::database::CreateTaskRunEventInput;
use crate::step_metadata::{StepDetails, StepMetadata};
use serde_json::Value;

/// Builder for creating consistent step execution events.
///
/// This builder ensures that all required fields are present and properly formatted
/// for the Timeline widget and other consumers.
///
/// # Example
///
/// ```ignore
/// let metadata = StepMetadata::setup(task_run_id, StepType::ShellCommand, "Install deps", 0);
/// let details = StepDetails::shell_command("npm install".to_string(), None, None);
///
/// let event = StepEventBuilder::new(task_run_id, metadata)
///     .with_details(details)
///     .with_workflow_name("My Workflow")
///     .build_start();
/// ```
pub struct StepEventBuilder {
    /// Task run ID this event belongs to
    task_run_id: String,

    /// Required step metadata
    metadata: StepMetadata,

    /// Optional step-specific details
    details: StepDetails,

    /// Optional workflow name for context
    workflow_name: Option<String>,

    /// Optional action ID for linking related events
    action_id: Option<String>,

    /// Optional state name for GUI automation context
    state_name: Option<String>,

    /// Additional data to merge into the event
    extra_data: Option<serde_json::Map<String, Value>>,
}

impl StepEventBuilder {
    /// Create a new event builder with required fields.
    pub fn new(task_run_id: impl Into<String>, metadata: StepMetadata) -> Self {
        Self {
            task_run_id: task_run_id.into(),
            metadata,
            details: StepDetails::default(),
            workflow_name: None,
            action_id: None,
            state_name: None,
            extra_data: None,
        }
    }

    /// Add step-specific details.
    pub fn with_details(mut self, details: StepDetails) -> Self {
        self.details = details;
        self
    }

    /// Set the workflow name for context.
    pub fn with_workflow_name(mut self, name: impl Into<String>) -> Self {
        self.workflow_name = Some(name.into());
        self
    }

    /// Set a custom action ID for linking events.
    pub fn with_action_id(mut self, id: impl Into<String>) -> Self {
        self.action_id = Some(id.into());
        self
    }

    /// Set the state name for GUI automation context.
    pub fn with_state_name(mut self, name: impl Into<String>) -> Self {
        self.state_name = Some(name.into());
        self
    }

    /// Add extra data to merge into the event.
    pub fn with_extra_data(mut self, data: serde_json::Map<String, Value>) -> Self {
        self.extra_data = Some(data);
        self
    }

    /// Add a single extra field to the event data.
    pub fn with_extra_field(mut self, key: impl Into<String>, value: Value) -> Self {
        if self.extra_data.is_none() {
            self.extra_data = Some(serde_json::Map::new());
        }
        if let Some(ref mut data) = self.extra_data {
            data.insert(key.into(), value);
        }
        self
    }

    /// Generate the action ID if not explicitly set.
    fn generate_action_id(&self) -> String {
        if let Some(ref id) = self.action_id {
            return id.clone();
        }

        let phase = self.metadata.phase().as_str();
        let step_type = self.metadata.step_type.as_str();
        let step_index = self.metadata.step_index;

        if let Some(iteration) = self.metadata.iteration() {
            format!(
                "{}-{}-{}-{}-{}",
                phase, step_type, self.task_run_id, iteration, step_index
            )
        } else {
            format!(
                "{}-{}-{}-{}",
                phase, step_type, self.task_run_id, step_index
            )
        }
    }

    /// Build the event data JSON object.
    fn build_data(&self) -> String {
        // Start with metadata as the base
        let mut data = match self.metadata.to_json() {
            Value::Object(obj) => obj,
            _ => serde_json::Map::new(),
        };

        // Note: We no longer add a separate "type" field for AI steps.
        // The "step_type" field from metadata is sufficient and consistent
        // with the format used by log_step_event in step_executor.rs.
        // This consistency allows the Timeline widget to properly aggregate
        // start/complete events for the same step.

        // Merge in step-specific details
        self.details.merge_into(&mut data);

        // Merge extra data if present
        if let Some(ref extra) = self.extra_data {
            for (key, value) in extra {
                data.insert(key.clone(), value.clone());
            }
        }

        serde_json::to_string(&Value::Object(data)).unwrap_or_default()
    }

    /// Build a "start" event for when a step begins execution.
    pub fn build_start(&self) -> CreateTaskRunEventInput {
        CreateTaskRunEventInput {
            task_run_id: self.task_run_id.clone(),
            event_type: "step_execution".to_string(),
            event_subtype: Some("start".to_string()),
            message: format!(
                "{} '{}' started",
                self.metadata.step_type.as_str(),
                self.metadata.step_name
            ),
            data: Some(self.build_data()),
            workflow_name: self.workflow_name.clone(),
            state_name: self.state_name.clone(),
            action_id: Some(self.generate_action_id()),
            timestamp: chrono::Utc::now().to_rfc3339(),
            duration_ms: None,
        }
    }

    /// Build a "complete" event for when a step finishes successfully.
    pub fn build_complete(&self, duration_ms: i64) -> CreateTaskRunEventInput {
        // Update details with duration for the event data
        let mut details = self.details.clone();
        details.duration_ms = Some(duration_ms);

        let builder_with_duration = StepEventBuilder {
            task_run_id: self.task_run_id.clone(),
            metadata: self.metadata.clone(),
            details,
            workflow_name: self.workflow_name.clone(),
            action_id: self.action_id.clone(),
            state_name: self.state_name.clone(),
            extra_data: self.extra_data.clone(),
        };

        CreateTaskRunEventInput {
            task_run_id: self.task_run_id.clone(),
            event_type: "step_execution".to_string(),
            event_subtype: Some("complete".to_string()),
            message: format!(
                "{} '{}' completed ({}ms)",
                self.metadata.step_type.as_str(),
                self.metadata.step_name,
                duration_ms
            ),
            data: Some(builder_with_duration.build_data()),
            workflow_name: self.workflow_name.clone(),
            state_name: self.state_name.clone(),
            action_id: Some(self.generate_action_id()),
            timestamp: chrono::Utc::now().to_rfc3339(),
            duration_ms: Some(duration_ms),
        }
    }

    /// Build an "error" event for when a step fails.
    pub fn build_error(
        &self,
        duration_ms: i64,
        error_message: Option<&str>,
    ) -> CreateTaskRunEventInput {
        // Update details with duration and error for the event data
        let mut details = self.details.clone();
        details.duration_ms = Some(duration_ms);
        if let Some(err) = error_message {
            details.error = Some(err.to_string());
        }

        let builder_with_error = StepEventBuilder {
            task_run_id: self.task_run_id.clone(),
            metadata: self.metadata.clone(),
            details,
            workflow_name: self.workflow_name.clone(),
            action_id: self.action_id.clone(),
            state_name: self.state_name.clone(),
            extra_data: self.extra_data.clone(),
        };

        let msg = if let Some(err) = error_message {
            format!(
                "{} '{}' failed: {} ({}ms)",
                self.metadata.step_type.as_str(),
                self.metadata.step_name,
                err,
                duration_ms
            )
        } else {
            format!(
                "{} '{}' failed ({}ms)",
                self.metadata.step_type.as_str(),
                self.metadata.step_name,
                duration_ms
            )
        };

        CreateTaskRunEventInput {
            task_run_id: self.task_run_id.clone(),
            event_type: "step_execution".to_string(),
            event_subtype: Some("error".to_string()),
            message: msg,
            data: Some(builder_with_error.build_data()),
            workflow_name: self.workflow_name.clone(),
            state_name: self.state_name.clone(),
            action_id: Some(self.generate_action_id()),
            timestamp: chrono::Utc::now().to_rfc3339(),
            duration_ms: Some(duration_ms),
        }
    }
}

/// Helper function to categorize steps into automation and AI steps.
///
/// This replaces the fragile string-based partition logic:
/// ```ignore
/// // Before (fragile)
/// let (automation_steps, prompt_steps) = all_steps.into_iter().partition(|s| s.step_type != "prompt");
///
/// // After (robust)
/// let (automation_steps, ai_steps) = categorize_steps(all_steps);
/// ```
pub fn categorize_steps<T, F>(steps: Vec<T>, get_step_type: F) -> (Vec<T>, Vec<T>)
where
    F: Fn(&T) -> &str,
{
    use crate::step_types::StepType;

    steps.into_iter().partition(|s| {
        let step_type_str = get_step_type(s);
        StepType::from_str_compat(step_type_str)
            .map(|t| !t.requires_ai())
            .unwrap_or(true) // Unknown types default to automation
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_context::ExecutionContext;
    use crate::step_types::StepType;

    #[test]
    fn test_build_start_event() {
        let metadata = StepMetadata::new(
            ExecutionContext::setup("task-123"),
            StepType::ShellCommand,
            "Install dependencies",
            0,
        );

        let event = StepEventBuilder::new("task-123", metadata)
            .with_workflow_name("Test Workflow")
            .build_start();

        assert_eq!(event.task_run_id, "task-123");
        assert_eq!(event.event_type, "step_execution");
        assert_eq!(event.event_subtype, Some("start".to_string()));
        assert!(event.message.contains("Install dependencies"));
        assert!(event.message.contains("started"));
        assert_eq!(event.workflow_name, Some("Test Workflow".to_string()));
        assert!(event.duration_ms.is_none());

        // Verify data contains required fields
        let data: serde_json::Value = serde_json::from_str(event.data.as_ref().unwrap()).unwrap();
        assert_eq!(data["step_type"], "shell_command");
        assert_eq!(data["step_index"], 0);
        assert_eq!(data["phase"], "setup");
        assert_eq!(data["task_run_id"], "task-123");
    }

    #[test]
    fn test_build_complete_event() {
        let metadata =
            StepMetadata::verification("task-123", StepType::Playwright, "Login Test", 2, 1);
        let details = StepDetails::playwright("script-456".to_string());

        let event = StepEventBuilder::new("task-123", metadata)
            .with_details(details)
            .build_complete(1500);

        assert_eq!(event.event_subtype, Some("complete".to_string()));
        assert!(event.message.contains("completed"));
        assert!(event.message.contains("1500ms"));
        assert_eq!(event.duration_ms, Some(1500));

        // Verify data contains step-specific fields
        let data: serde_json::Value = serde_json::from_str(event.data.as_ref().unwrap()).unwrap();
        assert_eq!(data["playwright_script_id"], "script-456");
        assert_eq!(data["iteration"], 1);
    }

    #[test]
    fn test_build_error_event() {
        let metadata = StepMetadata::setup("task-123", StepType::ShellCommand, "Build", 0);

        let event = StepEventBuilder::new("task-123", metadata)
            .build_error(500, Some("Command failed with exit code 1"));

        assert_eq!(event.event_subtype, Some("error".to_string()));
        assert!(event.message.contains("failed"));
        assert!(event.message.contains("exit code 1"));

        // Verify error is in data
        let data: serde_json::Value = serde_json::from_str(event.data.as_ref().unwrap()).unwrap();
        assert!(data["error"].as_str().unwrap().contains("exit code 1"));
    }

    #[test]
    fn test_ai_step_type_field() {
        let metadata = StepMetadata::agentic("task-123", StepType::AiSession, "Fix issues", 0, 1);

        let event = StepEventBuilder::new("task-123", metadata).build_start();

        // Verify step_type is set correctly (no separate "type" field anymore)
        let data: serde_json::Value = serde_json::from_str(event.data.as_ref().unwrap()).unwrap();
        assert_eq!(data["step_type"], "ai_session");
        assert_eq!(data["step_name"], "Fix issues");
        // The "type" field is no longer added for consistency with log_step_event
        assert!(data.get("type").is_none());
    }

    #[test]
    fn test_categorize_steps() {
        struct MockStep {
            step_type: String,
        }

        let steps = vec![
            MockStep {
                step_type: "shell_command".to_string(),
            },
            MockStep {
                step_type: "prompt".to_string(),
            },
            MockStep {
                step_type: "playwright".to_string(),
            },
            MockStep {
                step_type: "ai_session".to_string(),
            },
        ];

        let (automation, ai) = categorize_steps(steps, |s| &s.step_type);

        assert_eq!(automation.len(), 2);
        assert_eq!(ai.len(), 2);
        assert_eq!(automation[0].step_type, "shell_command");
        assert_eq!(automation[1].step_type, "playwright");
        assert_eq!(ai[0].step_type, "prompt");
        assert_eq!(ai[1].step_type, "ai_session");
    }
}
