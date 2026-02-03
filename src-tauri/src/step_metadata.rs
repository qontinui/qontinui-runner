//! Step Metadata Module
//!
//! Provides structured metadata for step events, ensuring consistency
//! across all step types and phases.

#![allow(dead_code)]

use crate::execution_context::ExecutionContext;
use crate::runtime_env::ExecutionContextExt;
use crate::step_types::StepType;
use crate::unified_workflow_executor::WorkflowPhase;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Required metadata for every step event.
///
/// This struct ensures that all step events contain the essential fields
/// needed for aggregation, Timeline display, and debugging.
///
/// Uses composition with `ExecutionContext` to share common fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepMetadata {
    // ========================================================================
    // Core Context (from ExecutionContext)
    // ========================================================================
    /// The execution context (task run, phase, iteration).
    #[serde(flatten)]
    pub context: ExecutionContext,

    // ========================================================================
    // Step Identity
    // ========================================================================
    /// Unique step ID for this execution instance.
    /// Generated automatically if not provided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,

    /// The type of step being executed.
    pub step_type: StepType,

    /// Human-readable name of the step.
    pub step_name: String,

    /// Index of this step within its phase (0-indexed).
    ///
    /// This field is REQUIRED for proper Timeline widget aggregation.
    /// Missing step_index was a common source of bugs.
    pub step_index: usize,

    // ========================================================================
    // Step Hierarchy
    // ========================================================================
    /// Parent step ID (for nested steps within a composite step).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_step_id: Option<String>,

    // ========================================================================
    // Timing & Performance
    // ========================================================================
    /// Expected duration in milliseconds (for progress estimation).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_duration_ms: Option<u64>,

    /// Timeout in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,

    // ========================================================================
    // Dependencies
    // ========================================================================
    /// Step IDs that must complete before this one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<String>>,

    /// Whether this step can run in parallel with others.
    #[serde(default)]
    pub parallelizable: bool,

    // ========================================================================
    // Criticality & Error Handling
    // ========================================================================
    /// Whether failure should stop the workflow.
    #[serde(default = "default_is_critical")]
    pub is_critical: bool,

    /// Whether to continue execution even if this step fails.
    #[serde(default)]
    pub continue_on_failure: bool,

    // ========================================================================
    // Reporting & Categorization
    // ========================================================================
    /// Category for grouping in reports (e.g., "setup", "test", "cleanup").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,

    /// Custom tags for filtering and categorization.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,

    /// Description of what this step does (for reports).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn default_is_critical() -> bool {
    true // Steps are critical by default
}

impl StepMetadata {
    /// Create new step metadata with all required fields.
    ///
    /// Automatically sets `expected_duration_ms` based on the step type's default.
    pub fn new(
        context: ExecutionContext,
        step_type: StepType,
        step_name: impl Into<String>,
        step_index: usize,
    ) -> Self {
        Self {
            context,
            step_id: None,
            step_type,
            step_name: step_name.into(),
            step_index,
            parent_step_id: None,
            expected_duration_ms: step_type.default_expected_duration_ms(),
            timeout_ms: None,
            depends_on: None,
            parallelizable: false,
            is_critical: true,
            continue_on_failure: false,
            category: None,
            tags: None,
            description: None,
        }
    }

    /// Create step metadata for a setup phase step.
    pub fn setup(
        task_run_id: impl Into<String>,
        step_type: StepType,
        step_name: impl Into<String>,
        step_index: usize,
    ) -> Self {
        Self::new(
            ExecutionContext::setup(task_run_id).with_runtime_env(),
            step_type,
            step_name,
            step_index,
        )
        .with_category("setup")
    }

    /// Create step metadata for a verification phase step.
    pub fn verification(
        task_run_id: impl Into<String>,
        step_type: StepType,
        step_name: impl Into<String>,
        step_index: usize,
        iteration: u32,
    ) -> Self {
        Self::new(
            ExecutionContext::verification(task_run_id, iteration).with_runtime_env(),
            step_type,
            step_name,
            step_index,
        )
        .with_category("verification")
    }

    /// Create step metadata for an agentic phase step.
    pub fn agentic(
        task_run_id: impl Into<String>,
        step_type: StepType,
        step_name: impl Into<String>,
        step_index: usize,
        iteration: u32,
    ) -> Self {
        Self::new(
            ExecutionContext::agentic(task_run_id, iteration).with_runtime_env(),
            step_type,
            step_name,
            step_index,
        )
        .with_category("agentic")
    }

    /// Create step metadata for a completion phase step.
    pub fn completion(
        task_run_id: impl Into<String>,
        step_type: StepType,
        step_name: impl Into<String>,
        step_index: usize,
    ) -> Self {
        Self::new(
            ExecutionContext::completion(task_run_id).with_runtime_env(),
            step_type,
            step_name,
            step_index,
        )
        .with_category("completion")
    }

    // ========================================================================
    // Builder Methods
    // ========================================================================

    /// Set the unique step ID.
    pub fn with_step_id(mut self, step_id: impl Into<String>) -> Self {
        self.step_id = Some(step_id.into());
        self
    }

    /// Generate and set a unique step ID based on context.
    pub fn with_generated_step_id(mut self) -> Self {
        let phase = self.context.phase.as_str();
        let iter_part = self.context.iteration.map(|i| format!("-{}", i)).unwrap_or_default();
        self.step_id = Some(format!(
            "{}-{}{}-{}-{}",
            self.context.task_run_id,
            phase,
            iter_part,
            self.step_type.as_str(),
            self.step_index
        ));
        self
    }

    /// Set parent step ID (for nested steps).
    pub fn with_parent_step(mut self, parent_step_id: impl Into<String>) -> Self {
        self.parent_step_id = Some(parent_step_id.into());
        self
    }

    /// Set expected duration for progress estimation.
    pub fn with_expected_duration(mut self, duration_ms: u64) -> Self {
        self.expected_duration_ms = Some(duration_ms);
        self
    }

    /// Set timeout.
    pub fn with_timeout(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }

    /// Set dependencies on other steps.
    pub fn with_dependencies(mut self, depends_on: Vec<String>) -> Self {
        self.depends_on = Some(depends_on);
        self
    }

    /// Add a single dependency.
    pub fn depends_on_step(mut self, step_id: impl Into<String>) -> Self {
        self.depends_on
            .get_or_insert_with(Vec::new)
            .push(step_id.into());
        self
    }

    /// Mark as parallelizable.
    pub fn parallelizable(mut self) -> Self {
        self.parallelizable = true;
        self
    }

    /// Mark as non-critical (failure won't stop workflow).
    pub fn non_critical(mut self) -> Self {
        self.is_critical = false;
        self
    }

    /// Set continue on failure.
    pub fn with_continue_on_failure(mut self) -> Self {
        self.continue_on_failure = true;
        self
    }

    /// Set category.
    pub fn with_category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }

    /// Set tags.
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = Some(tags);
        self
    }

    /// Add a single tag.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.get_or_insert_with(Vec::new).push(tag.into());
        self
    }

    /// Set description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    // ========================================================================
    // Accessor Methods
    // ========================================================================

    /// Get the task run ID.
    pub fn task_run_id(&self) -> &str {
        &self.context.task_run_id
    }

    /// Get the phase.
    pub fn phase(&self) -> WorkflowPhase {
        self.context.phase
    }

    /// Get the iteration (if any).
    pub fn iteration(&self) -> Option<u32> {
        self.context.iteration
    }

    /// Get or generate step ID.
    pub fn get_step_id(&self) -> String {
        if let Some(ref id) = self.step_id {
            id.clone()
        } else {
            let phase = self.context.phase.as_str();
            let iter_part = self.context.iteration.map(|i| format!("-{}", i)).unwrap_or_default();
            format!(
                "{}-{}{}-{}-{}",
                self.context.task_run_id,
                phase,
                iter_part,
                self.step_type.as_str(),
                self.step_index
            )
        }
    }

    /// Convert metadata to JSON object for event data.
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| {
            // Fallback to manual construction if serialization fails
            let mut obj = json!({
                "task_run_id": self.context.task_run_id,
                "step_type": self.step_type.as_str(),
                "step_name": self.step_name,
                "step_index": self.step_index,
                "phase": self.context.phase.as_str(),
            });

            if let Some(iteration) = self.context.iteration {
                obj["iteration"] = json!(iteration);
            }

            obj
        })
    }
}

/// Step-specific details that vary by step type.
///
/// This struct contains optional fields that are populated based on the step type.
/// Not all fields apply to all step types.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StepDetails {
    // ========================================================================
    // Command Step Fields
    // ========================================================================
    /// Shell command that was executed
    pub command: Option<String>,

    /// Working directory for command execution
    pub working_directory: Option<String>,

    /// Exit code from command execution
    pub exit_code: Option<i32>,

    /// Standard output from command
    pub stdout: Option<String>,

    /// Standard error from command
    pub stderr: Option<String>,

    /// Shell type used (powershell, bash, etc.)
    pub shell_type: Option<String>,

    // ========================================================================
    // GUI Step Fields
    // ========================================================================
    /// Action type (click, double_click, right_click, type, etc.)
    pub action_type: Option<String>,

    /// Target image name for visual automation
    pub target_image_name: Option<String>,

    /// Monitor index used for execution
    pub monitor_index: Option<i32>,

    // ========================================================================
    // Test Step Fields
    // ========================================================================
    /// Playwright script ID
    pub playwright_script_id: Option<String>,

    /// Verification test ID
    pub test_id: Option<String>,

    /// Number of assertions that passed
    pub assertions_passed: Option<usize>,

    /// Number of assertions that failed
    pub assertions_failed: Option<usize>,

    /// Total number of assertions
    pub assertions_total: Option<usize>,

    /// Test failure message
    pub failure_message: Option<String>,

    // ========================================================================
    // AI Step Fields
    // ========================================================================
    /// AI session ID
    pub session_id: Option<String>,

    /// Length of AI output in characters
    pub output_length: Option<usize>,

    // ========================================================================
    // API Request Fields
    // ========================================================================
    /// HTTP method (GET, POST, etc.)
    pub http_method: Option<String>,

    /// Request URL
    pub url: Option<String>,

    /// HTTP status code
    pub status_code: Option<u16>,

    // ========================================================================
    // MCP Call Fields
    // ========================================================================
    /// MCP server ID
    pub mcp_server_id: Option<String>,

    /// MCP tool name
    pub mcp_tool_name: Option<String>,

    // ========================================================================
    // Error Fields
    // ========================================================================
    /// Error message if step failed
    pub error: Option<String>,

    // ========================================================================
    // Duration Fields
    // ========================================================================
    /// Execution duration in milliseconds
    pub duration_ms: Option<i64>,
}

impl StepDetails {
    /// Create empty details.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Create details for a shell command step.
    pub fn shell_command(
        command: String,
        working_directory: Option<String>,
        shell_type: Option<String>,
    ) -> Self {
        Self {
            command: Some(command),
            working_directory,
            shell_type,
            ..Default::default()
        }
    }

    /// Create details for a shell command completion.
    pub fn shell_command_complete(
        command: String,
        working_directory: Option<String>,
        shell_type: Option<String>,
        exit_code: i32,
        stdout: Option<String>,
        stderr: Option<String>,
        duration_ms: i64,
    ) -> Self {
        Self {
            command: Some(command),
            working_directory,
            shell_type,
            exit_code: Some(exit_code),
            stdout,
            stderr,
            duration_ms: Some(duration_ms),
            ..Default::default()
        }
    }

    /// Create details for a GUI action step.
    pub fn gui_action(
        action_type: String,
        target_image_name: Option<String>,
        monitor_index: Option<i32>,
    ) -> Self {
        Self {
            action_type: Some(action_type),
            target_image_name,
            monitor_index,
            ..Default::default()
        }
    }

    /// Create details for a Playwright test step.
    pub fn playwright(script_id: String) -> Self {
        Self {
            playwright_script_id: Some(script_id),
            ..Default::default()
        }
    }

    /// Create details for a test step completion.
    pub fn test_complete(
        test_id: Option<String>,
        passed: usize,
        failed: usize,
        total: usize,
        failure_message: Option<String>,
    ) -> Self {
        Self {
            test_id,
            assertions_passed: Some(passed),
            assertions_failed: Some(failed),
            assertions_total: Some(total),
            failure_message,
            ..Default::default()
        }
    }

    /// Create details for an AI session step.
    pub fn ai_session(session_id: String) -> Self {
        Self {
            session_id: Some(session_id),
            ..Default::default()
        }
    }

    /// Create details for an AI session completion.
    pub fn ai_session_complete(session_id: String, output_length: usize, duration_ms: i64) -> Self {
        Self {
            session_id: Some(session_id),
            output_length: Some(output_length),
            duration_ms: Some(duration_ms),
            ..Default::default()
        }
    }

    /// Create details for an API request step.
    pub fn api_request(method: String, url: String) -> Self {
        Self {
            http_method: Some(method),
            url: Some(url),
            ..Default::default()
        }
    }

    /// Create details for an MCP call step.
    pub fn mcp_call(server_id: String, tool_name: String) -> Self {
        Self {
            mcp_server_id: Some(server_id),
            mcp_tool_name: Some(tool_name),
            ..Default::default()
        }
    }

    /// Create details with an error.
    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }

    /// Add duration to existing details.
    pub fn with_duration(mut self, duration_ms: i64) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }

    /// Merge details into a JSON object, filtering out None values.
    pub fn merge_into(&self, obj: &mut serde_json::Map<String, Value>) {
        if let Some(ref command) = self.command {
            obj.insert("command".to_string(), json!(command));
        }
        if let Some(ref wd) = self.working_directory {
            obj.insert("working_directory".to_string(), json!(wd));
        }
        if let Some(code) = self.exit_code {
            obj.insert("exit_code".to_string(), json!(code));
        }
        if let Some(ref out) = self.stdout {
            obj.insert("stdout".to_string(), json!(out));
        }
        if let Some(ref err) = self.stderr {
            obj.insert("stderr".to_string(), json!(err));
        }
        if let Some(ref shell) = self.shell_type {
            obj.insert("shell_type".to_string(), json!(shell));
        }
        if let Some(ref at) = self.action_type {
            obj.insert("action_type".to_string(), json!(at));
        }
        if let Some(ref tin) = self.target_image_name {
            obj.insert("target_image_name".to_string(), json!(tin));
        }
        if let Some(mi) = self.monitor_index {
            obj.insert("monitor_index".to_string(), json!(mi));
        }
        if let Some(ref psid) = self.playwright_script_id {
            obj.insert("playwright_script_id".to_string(), json!(psid));
        }
        if let Some(ref tid) = self.test_id {
            obj.insert("test_id".to_string(), json!(tid));
        }
        if let Some(ap) = self.assertions_passed {
            obj.insert("assertions_passed".to_string(), json!(ap));
        }
        if let Some(af) = self.assertions_failed {
            obj.insert("assertions_failed".to_string(), json!(af));
        }
        if let Some(at) = self.assertions_total {
            obj.insert("assertions_total".to_string(), json!(at));
        }
        if let Some(ref fm) = self.failure_message {
            obj.insert("failure_message".to_string(), json!(fm));
        }
        if let Some(ref sid) = self.session_id {
            obj.insert("session_id".to_string(), json!(sid));
        }
        if let Some(ol) = self.output_length {
            obj.insert("output_length".to_string(), json!(ol));
        }
        if let Some(ref method) = self.http_method {
            obj.insert("http_method".to_string(), json!(method));
        }
        if let Some(ref u) = self.url {
            obj.insert("url".to_string(), json!(u));
        }
        if let Some(sc) = self.status_code {
            obj.insert("status_code".to_string(), json!(sc));
        }
        if let Some(ref msid) = self.mcp_server_id {
            obj.insert("mcp_server_id".to_string(), json!(msid));
        }
        if let Some(ref mtn) = self.mcp_tool_name {
            obj.insert("mcp_tool_name".to_string(), json!(mtn));
        }
        if let Some(ref e) = self.error {
            obj.insert("error".to_string(), json!(e));
        }
        if let Some(d) = self.duration_ms {
            obj.insert("duration_ms".to_string(), json!(d));
        }
    }

    /// Convert details to JSON object.
    pub fn to_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        self.merge_into(&mut obj);
        Value::Object(obj)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_step_metadata_to_json() {
        let metadata = StepMetadata::verification(
            "task-123",
            StepType::Playwright,
            "Login Test",
            2,
            1,
        );
        let json = metadata.to_json();

        assert_eq!(json["task_run_id"], "task-123");
        assert_eq!(json["step_type"], "playwright");
        assert_eq!(json["step_name"], "Login Test");
        assert_eq!(json["step_index"], 2);
        assert_eq!(json["phase"], "verification");
        assert_eq!(json["iteration"], 1);
        assert_eq!(json["category"], "verification");
    }

    #[test]
    fn test_step_metadata_accessors() {
        let metadata = StepMetadata::agentic(
            "task-456",
            StepType::AiSession,
            "AI Analysis",
            0,
            3,
        );

        assert_eq!(metadata.task_run_id(), "task-456");
        assert_eq!(metadata.phase(), WorkflowPhase::Agentic);
        assert_eq!(metadata.iteration(), Some(3));
    }

    #[test]
    fn test_step_metadata_builder() {
        let metadata = StepMetadata::setup("task-789", StepType::ShellCommand, "Build", 0)
            .with_expected_duration(5000)
            .with_timeout(30000)
            .with_tags(vec!["build".to_string(), "npm".to_string()])
            .non_critical()
            .parallelizable();

        // with_expected_duration should override the default
        assert_eq!(metadata.expected_duration_ms, Some(5000));
        assert_eq!(metadata.timeout_ms, Some(30000));
        assert!(!metadata.is_critical);
        assert!(metadata.parallelizable);
        assert_eq!(metadata.tags.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn test_step_metadata_default_expected_duration() {
        // ShellCommand default is 30_000ms
        let shell_metadata = StepMetadata::setup("task-123", StepType::ShellCommand, "Build", 0);
        assert_eq!(shell_metadata.expected_duration_ms, Some(30_000));

        // AiSession default is 300_000ms (5 minutes)
        let ai_metadata = StepMetadata::agentic("task-123", StepType::AiSession, "AI Analysis", 0, 1);
        assert_eq!(ai_metadata.expected_duration_ms, Some(300_000));

        // Playwright default is 60_000ms
        let playwright_metadata = StepMetadata::verification("task-123", StepType::Playwright, "Login Test", 0, 1);
        assert_eq!(playwright_metadata.expected_duration_ms, Some(60_000));

        // Action default is 5_000ms
        let action_metadata = StepMetadata::new(
            ExecutionContext::setup("task-123").with_runtime_env(),
            StepType::Action,
            "Click Button",
            0,
        );
        assert_eq!(action_metadata.expected_duration_ms, Some(5_000));
    }

    #[test]
    fn test_step_metadata_dependencies() {
        let metadata = StepMetadata::verification("task-123", StepType::Playwright, "Test", 2, 1)
            .depends_on_step("step-1")
            .depends_on_step("step-2");

        assert_eq!(metadata.depends_on.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn test_step_metadata_get_step_id() {
        let metadata = StepMetadata::agentic("task-123", StepType::AiSession, "Test", 0, 2);
        let step_id = metadata.get_step_id();

        assert!(step_id.contains("task-123"));
        assert!(step_id.contains("agentic"));
        assert!(step_id.contains("ai_session"));
    }

    #[test]
    fn test_step_details_shell_command() {
        let details = StepDetails::shell_command_complete(
            "npm test".to_string(),
            Some("/app".to_string()),
            Some("bash".to_string()),
            0,
            Some("Tests passed".to_string()),
            None,
            1500,
        );

        let json = details.to_json();
        assert_eq!(json["command"], "npm test");
        assert_eq!(json["exit_code"], 0);
        assert_eq!(json["duration_ms"], 1500);
    }

    #[test]
    fn test_step_details_merge() {
        let metadata = StepMetadata::setup(
            "task-789",
            StepType::ShellCommand,
            "Build",
            0,
        );
        let details = StepDetails::shell_command("npm run build".to_string(), None, None);

        // Convert metadata to JSON object
        let mut combined = metadata.to_json();
        let obj = combined.as_object_mut().unwrap();

        // Merge in details
        details.merge_into(obj);

        assert_eq!(combined["task_run_id"], "task-789");
        assert_eq!(combined["step_type"], "shell_command");
        assert_eq!(combined["command"], "npm run build");
    }
}
