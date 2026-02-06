//! Execution Context Module
//!
//! Provides the foundational context types for all execution events.
//! This is the single source of truth for task/phase/iteration tracking.

#![allow(dead_code)]

use crate::unified_workflow_executor::WorkflowPhase;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

// ============================================================================
// Core Execution Context
// ============================================================================

/// Common execution context - the "where" of execution.
///
/// This struct captures the essential context that applies to ALL execution events,
/// whether they are step executions, AI sessions, or any other tracked activity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionContext {
    // ========================================================================
    // Core Identity
    // ========================================================================
    /// The task run ID this execution belongs to.
    /// Links to the `task_runs` table in the database.
    pub task_run_id: String,

    /// Which workflow phase this execution is part of.
    pub phase: WorkflowPhase,

    /// Iteration number within the phase (1-indexed).
    /// None for setup/completion phases which don't iterate.
    pub iteration: Option<u32>,

    // ========================================================================
    // Hierarchy & Nesting
    // ========================================================================
    /// Parent execution context (for nested workflows/sub-tasks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_task_run_id: Option<String>,

    /// Root task run ID (top of hierarchy). Same as task_run_id if this is root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_task_run_id: Option<String>,

    /// Nesting depth (0 = top level).
    #[serde(default)]
    pub depth: u32,

    // ========================================================================
    // Correlation & Tracing
    // ========================================================================
    /// Unique trace ID for distributed tracing (spans multiple services).
    /// Use for correlating events across runner, web backend, and external systems.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,

    /// Span ID for this specific execution unit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,

    // ========================================================================
    // Business Context
    // ========================================================================
    /// Project ID this execution belongs to (from qontinui-web).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,

    /// Workspace/organization ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,

    /// User ID who triggered the execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triggered_by: Option<String>,

    // ========================================================================
    // Retry & Recovery
    // ========================================================================
    /// Retry attempt number (0 = first attempt, 1 = first retry, etc.).
    #[serde(default)]
    pub retry_attempt: u32,

    /// ID of original execution this is retrying (if this is a retry).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<String>,

    // ========================================================================
    // Environment
    // ========================================================================
    /// Environment identifier (dev, staging, prod).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,

    /// Runner/agent ID that's executing this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,

    /// Git commit, tag, or version reference being tested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_ref: Option<String>,
}

impl ExecutionContext {
    /// Create a new execution context with required fields.
    pub fn new(task_run_id: impl Into<String>, phase: WorkflowPhase) -> Self {
        let task_run_id = task_run_id.into();
        Self {
            task_run_id: task_run_id.clone(),
            phase,
            iteration: None,
            parent_task_run_id: None,
            root_task_run_id: Some(task_run_id), // Default: this is root
            depth: 0,
            trace_id: None,
            span_id: None,
            project_id: None,
            workspace_id: None,
            triggered_by: None,
            retry_attempt: 0,
            retry_of: None,
            environment: None,
            runner_id: None,
            version_ref: None,
        }
    }

    /// Create context for setup phase.
    pub fn setup(task_run_id: impl Into<String>) -> Self {
        Self::new(task_run_id, WorkflowPhase::Setup)
    }

    /// Create context for verification phase.
    pub fn verification(task_run_id: impl Into<String>, iteration: u32) -> Self {
        let mut ctx = Self::new(task_run_id, WorkflowPhase::Verification);
        ctx.iteration = Some(iteration);
        ctx
    }

    /// Create context for agentic phase.
    pub fn agentic(task_run_id: impl Into<String>, iteration: u32) -> Self {
        let mut ctx = Self::new(task_run_id, WorkflowPhase::Agentic);
        ctx.iteration = Some(iteration);
        ctx
    }

    /// Create context for completion phase.
    pub fn completion(task_run_id: impl Into<String>) -> Self {
        Self::new(task_run_id, WorkflowPhase::Completion)
    }

    // ========================================================================
    // Builder Methods
    // ========================================================================

    /// Set iteration.
    pub fn with_iteration(mut self, iteration: u32) -> Self {
        self.iteration = Some(iteration);
        self
    }

    /// Set parent context (for nested workflows).
    pub fn with_parent(mut self, parent_task_run_id: impl Into<String>) -> Self {
        self.parent_task_run_id = Some(parent_task_run_id.into());
        self.depth = self.depth.saturating_add(1);
        self
    }

    /// Set root task run ID.
    pub fn with_root(mut self, root_task_run_id: impl Into<String>) -> Self {
        self.root_task_run_id = Some(root_task_run_id.into());
        self
    }

    /// Set nesting depth.
    pub fn with_depth(mut self, depth: u32) -> Self {
        self.depth = depth;
        self
    }

    /// Set trace ID for distributed tracing.
    pub fn with_trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }

    /// Set span ID.
    pub fn with_span_id(mut self, span_id: impl Into<String>) -> Self {
        self.span_id = Some(span_id.into());
        self
    }

    /// Set project ID.
    pub fn with_project_id(mut self, project_id: impl Into<String>) -> Self {
        self.project_id = Some(project_id.into());
        self
    }

    /// Set workspace ID.
    pub fn with_workspace_id(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = Some(workspace_id.into());
        self
    }

    /// Set triggered by user.
    pub fn with_triggered_by(mut self, user_id: impl Into<String>) -> Self {
        self.triggered_by = Some(user_id.into());
        self
    }

    /// Set retry information.
    pub fn with_retry(mut self, attempt: u32, original_id: Option<String>) -> Self {
        self.retry_attempt = attempt;
        self.retry_of = original_id;
        self
    }

    /// Set environment.
    pub fn with_environment(mut self, env: impl Into<String>) -> Self {
        self.environment = Some(env.into());
        self
    }

    /// Set runner ID.
    pub fn with_runner_id(mut self, runner_id: impl Into<String>) -> Self {
        self.runner_id = Some(runner_id.into());
        self
    }

    /// Set version reference.
    pub fn with_version_ref(mut self, version_ref: impl Into<String>) -> Self {
        self.version_ref = Some(version_ref.into());
        self
    }

    // ========================================================================
    // Utility Methods
    // ========================================================================

    /// Generate a session ID that includes phase suffix.
    /// Format: `{task_run_id}-{phase}` or `{task_run_id}-{phase}-{iteration}`
    pub fn session_id(&self) -> String {
        match self.iteration {
            Some(iter) => format!("{}-{}-{}", self.task_run_id, self.phase.as_str(), iter),
            None => format!("{}-{}", self.task_run_id, self.phase.as_str()),
        }
    }

    /// Check if this is a root execution (not nested).
    pub fn is_root(&self) -> bool {
        self.depth == 0 && self.parent_task_run_id.is_none()
    }

    /// Check if this is a retry.
    pub fn is_retry(&self) -> bool {
        self.retry_attempt > 0
    }

    /// Create a child context for nested execution.
    pub fn child(&self, child_task_run_id: impl Into<String>, phase: WorkflowPhase) -> Self {
        let child_id = child_task_run_id.into();
        Self {
            task_run_id: child_id,
            phase,
            iteration: None,
            parent_task_run_id: Some(self.task_run_id.clone()),
            root_task_run_id: self.root_task_run_id.clone(),
            depth: self.depth + 1,
            trace_id: self.trace_id.clone(),
            span_id: None, // Child gets its own span
            project_id: self.project_id.clone(),
            workspace_id: self.workspace_id.clone(),
            triggered_by: self.triggered_by.clone(),
            retry_attempt: 0,
            retry_of: None,
            environment: self.environment.clone(),
            runner_id: self.runner_id.clone(),
            version_ref: self.version_ref.clone(),
        }
    }

    /// Convert to JSON object for event data.
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }

    /// Merge context fields into an existing JSON object.
    pub fn merge_into(&self, obj: &mut serde_json::Map<String, Value>) {
        obj.insert("task_run_id".to_string(), json!(self.task_run_id));
        obj.insert("phase".to_string(), json!(self.phase.as_str()));
        if let Some(iteration) = self.iteration {
            obj.insert("iteration".to_string(), json!(iteration));
        }
        if let Some(ref parent) = self.parent_task_run_id {
            obj.insert("parent_task_run_id".to_string(), json!(parent));
        }
        if let Some(ref root) = self.root_task_run_id {
            obj.insert("root_task_run_id".to_string(), json!(root));
        }
        if self.depth > 0 {
            obj.insert("depth".to_string(), json!(self.depth));
        }
        if let Some(ref trace_id) = self.trace_id {
            obj.insert("trace_id".to_string(), json!(trace_id));
        }
        if let Some(ref span_id) = self.span_id {
            obj.insert("span_id".to_string(), json!(span_id));
        }
        if let Some(ref project_id) = self.project_id {
            obj.insert("project_id".to_string(), json!(project_id));
        }
        if let Some(ref workspace_id) = self.workspace_id {
            obj.insert("workspace_id".to_string(), json!(workspace_id));
        }
        if let Some(ref triggered_by) = self.triggered_by {
            obj.insert("triggered_by".to_string(), json!(triggered_by));
        }
        if self.retry_attempt > 0 {
            obj.insert("retry_attempt".to_string(), json!(self.retry_attempt));
        }
        if let Some(ref retry_of) = self.retry_of {
            obj.insert("retry_of".to_string(), json!(retry_of));
        }
        if let Some(ref env) = self.environment {
            obj.insert("environment".to_string(), json!(env));
        }
        if let Some(ref runner_id) = self.runner_id {
            obj.insert("runner_id".to_string(), json!(runner_id));
        }
        if let Some(ref version_ref) = self.version_ref {
            obj.insert("version_ref".to_string(), json!(version_ref));
        }
    }
}

// ============================================================================
// AI Session Context
// ============================================================================

/// Agent role in the orchestrator system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// Planning agent - creates verification plans
    Planner,
    /// Worker agent - executes tasks
    Worker,
    /// Verifier agent - checks success criteria
    Verifier,
    /// Orchestrator agent - coordinates other agents
    Orchestrator,
    /// General/unspecified agent
    General,
}

impl AgentRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentRole::Planner => "planner",
            AgentRole::Worker => "worker",
            AgentRole::Verifier => "verifier",
            AgentRole::Orchestrator => "orchestrator",
            AgentRole::General => "general",
        }
    }
}

/// AI session context - extends ExecutionContext with session-specific fields.
///
/// Used for tracking AI output streams and grouping output by session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiSessionContext {
    // ========================================================================
    // Core Context
    // ========================================================================
    /// The execution context (task run, phase, iteration).
    #[serde(flatten)]
    pub context: ExecutionContext,

    /// Unique session identifier.
    /// Typically includes phase suffix: `{task_run_id}-{phase}-{iteration}`
    pub session_id: String,

    /// Human-readable session name for display.
    /// Example: "My Task - Iteration 3"
    pub session_name: String,

    // ========================================================================
    // AI-Specific
    // ========================================================================
    /// AI model identifier (e.g., "claude-3-opus", "claude-3-5-sonnet").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,

    /// Model generation parameters (temperature, max_tokens, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_params: Option<Value>,

    /// Total tokens consumed (input + output).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<u64>,

    /// Input tokens consumed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,

    /// Output tokens generated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,

    /// Estimated cost in cents (USD).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_cents: Option<u32>,

    // ========================================================================
    // Agent Role
    // ========================================================================
    /// Agent role in the orchestrator system.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<AgentRole>,

    /// Agent persona/instructions version identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_version: Option<String>,

    // ========================================================================
    // Conversation Context
    // ========================================================================
    /// Number of conversation turns in this session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_count: Option<u32>,

    /// Whether this is a continuation of a previous session.
    #[serde(default)]
    pub is_continuation: bool,

    /// Previous session ID (for continuations).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continued_from: Option<String>,

    /// MCP tools available to this session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_tools: Option<Vec<String>>,
}

impl AiSessionContext {
    /// Create a new AI session context.
    pub fn new(context: ExecutionContext, session_name: impl Into<String>) -> Self {
        let session_id = context.session_id();
        Self {
            context,
            session_id,
            session_name: session_name.into(),
            model_id: None,
            model_params: None,
            tokens_used: None,
            input_tokens: None,
            output_tokens: None,
            cost_cents: None,
            agent_role: None,
            agent_version: None,
            turn_count: None,
            is_continuation: false,
            continued_from: None,
            available_tools: None,
        }
    }

    /// Create context for setup phase AI session.
    pub fn setup(task_run_id: impl Into<String>, workflow_name: &str) -> Self {
        let context = ExecutionContext::setup(task_run_id);
        let session_name = format!("{} - Setup", workflow_name);
        Self::new(context, session_name)
    }

    /// Create context for verification phase AI session.
    pub fn verification(
        task_run_id: impl Into<String>,
        workflow_name: &str,
        iteration: u32,
    ) -> Self {
        let context = ExecutionContext::verification(task_run_id, iteration);
        let session_name = format!("{} - Verification {}", workflow_name, iteration);
        Self::new(context, session_name)
    }

    /// Create context for agentic phase AI session.
    pub fn agentic(task_run_id: impl Into<String>, workflow_name: &str, iteration: u32) -> Self {
        let context = ExecutionContext::agentic(task_run_id, iteration);
        let session_name = format!("{} - Iteration {}", workflow_name, iteration);
        Self::new(context, session_name)
    }

    /// Create context for completion phase AI session.
    pub fn completion(task_run_id: impl Into<String>, workflow_name: &str) -> Self {
        let context = ExecutionContext::completion(task_run_id);
        let session_name = format!("{} - Completion", workflow_name);
        Self::new(context, session_name)
    }

    // ========================================================================
    // Builder Methods
    // ========================================================================

    /// Set model ID.
    pub fn with_model(mut self, model_id: impl Into<String>) -> Self {
        self.model_id = Some(model_id.into());
        self
    }

    /// Set model parameters.
    pub fn with_model_params(mut self, params: Value) -> Self {
        self.model_params = Some(params);
        self
    }

    /// Set token usage.
    pub fn with_tokens(mut self, input: u64, output: u64) -> Self {
        self.input_tokens = Some(input);
        self.output_tokens = Some(output);
        self.tokens_used = Some(input + output);
        self
    }

    /// Set cost in cents.
    pub fn with_cost(mut self, cost_cents: u32) -> Self {
        self.cost_cents = Some(cost_cents);
        self
    }

    /// Set agent role.
    pub fn with_agent_role(mut self, role: AgentRole) -> Self {
        self.agent_role = Some(role);
        self
    }

    /// Set agent version.
    pub fn with_agent_version(mut self, version: impl Into<String>) -> Self {
        self.agent_version = Some(version.into());
        self
    }

    /// Set turn count.
    pub fn with_turn_count(mut self, count: u32) -> Self {
        self.turn_count = Some(count);
        self
    }

    /// Mark as continuation of another session.
    pub fn with_continuation_of(mut self, previous_session_id: impl Into<String>) -> Self {
        self.is_continuation = true;
        self.continued_from = Some(previous_session_id.into());
        self
    }

    /// Set available tools.
    pub fn with_tools(mut self, tools: Vec<String>) -> Self {
        self.available_tools = Some(tools);
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

    /// Convert to JSON object for event data.
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }
}

// ============================================================================
// Step Outcome Types
// ============================================================================

/// Outcome category for step execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepOutcome {
    /// Step completed successfully
    Success,
    /// Step failed with an error
    Failure,
    /// Step timed out
    Timeout,
    /// Step was skipped (dependency not met, condition false)
    Skipped,
    /// Step was cancelled by user or system
    Cancelled,
    /// Step is still running
    Running,
    /// Step is pending (not started)
    Pending,
}

impl StepOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            StepOutcome::Success => "success",
            StepOutcome::Failure => "failure",
            StepOutcome::Timeout => "timeout",
            StepOutcome::Skipped => "skipped",
            StepOutcome::Cancelled => "cancelled",
            StepOutcome::Running => "running",
            StepOutcome::Pending => "pending",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            StepOutcome::Success
                | StepOutcome::Failure
                | StepOutcome::Timeout
                | StepOutcome::Skipped
                | StepOutcome::Cancelled
        )
    }

    pub fn is_success(&self) -> bool {
        matches!(self, StepOutcome::Success)
    }

    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            StepOutcome::Failure | StepOutcome::Timeout | StepOutcome::Cancelled
        )
    }
}

/// Structured error information for step failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepError {
    /// Error code or category.
    pub code: String,

    /// Human-readable error message.
    pub message: String,

    /// Detailed error information (stack trace, context).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,

    /// Whether this error is retryable.
    #[serde(default)]
    pub retryable: bool,

    /// Suggested action to resolve the error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

impl StepError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
            retryable: false,
            suggestion: None,
        }
    }

    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }

    pub fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }
}

/// Artifact generated by step execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    /// Artifact type (screenshot, log, report, video, etc.).
    pub artifact_type: String,

    /// Relative path to the artifact file.
    pub path: String,

    /// MIME type of the artifact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,

    /// Size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,

    /// Human-readable description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Artifact {
    pub fn new(artifact_type: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            artifact_type: artifact_type.into(),
            path: path.into(),
            mime_type: None,
            size_bytes: None,
            description: None,
        }
    }

    pub fn screenshot(path: impl Into<String>) -> Self {
        Self::new("screenshot", path).with_mime_type("image/png")
    }

    pub fn log(path: impl Into<String>) -> Self {
        Self::new("log", path).with_mime_type("text/plain")
    }

    pub fn video(path: impl Into<String>) -> Self {
        Self::new("video", path).with_mime_type("video/webm")
    }

    pub fn with_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }

    pub fn with_size(mut self, size_bytes: u64) -> Self {
        self.size_bytes = Some(size_bytes);
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// Result of step execution with detailed metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    /// Unique step ID.
    pub step_id: String,

    /// Whether the step succeeded.
    pub success: bool,

    // ========================================================================
    // Timing
    // ========================================================================
    /// Timestamp when step started (Unix ms).
    pub started_at: i64,

    /// Timestamp when step completed (Unix ms).
    pub completed_at: i64,

    /// Total duration in milliseconds.
    pub duration_ms: u64,

    /// Time spent waiting (queued, blocked by dependencies) in ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_time_ms: Option<u64>,

    // ========================================================================
    // Outcome Details
    // ========================================================================
    /// Outcome category.
    pub outcome: StepOutcome,

    /// Structured error info (if failed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<StepError>,

    // ========================================================================
    // Artifacts
    // ========================================================================
    /// Paths to generated artifacts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Vec<Artifact>>,

    // ========================================================================
    // Metrics
    // ========================================================================
    /// Custom metrics captured during execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<HashMap<String, f64>>,

    // ========================================================================
    // Retry Info
    // ========================================================================
    /// Whether this result is from a retry.
    #[serde(default)]
    pub is_retry: bool,

    /// Number of retries before this result.
    #[serde(default)]
    pub retry_count: u32,
}

impl StepResult {
    pub fn new(step_id: impl Into<String>) -> Self {
        let now = chrono::Utc::now().timestamp_millis();
        Self {
            step_id: step_id.into(),
            success: false,
            started_at: now,
            completed_at: now,
            duration_ms: 0,
            wait_time_ms: None,
            outcome: StepOutcome::Pending,
            error: None,
            artifacts: None,
            metrics: None,
            is_retry: false,
            retry_count: 0,
        }
    }

    /// Mark as success.
    pub fn succeeded(mut self) -> Self {
        self.success = true;
        self.outcome = StepOutcome::Success;
        self.completed_at = chrono::Utc::now().timestamp_millis();
        self.duration_ms = (self.completed_at - self.started_at) as u64;
        self
    }

    /// Mark as failed with error.
    pub fn failed(mut self, error: StepError) -> Self {
        self.success = false;
        self.outcome = StepOutcome::Failure;
        self.error = Some(error);
        self.completed_at = chrono::Utc::now().timestamp_millis();
        self.duration_ms = (self.completed_at - self.started_at) as u64;
        self
    }

    /// Mark as timed out.
    pub fn timed_out(mut self) -> Self {
        self.success = false;
        self.outcome = StepOutcome::Timeout;
        self.error = Some(StepError::new("TIMEOUT", "Step execution timed out"));
        self.completed_at = chrono::Utc::now().timestamp_millis();
        self.duration_ms = (self.completed_at - self.started_at) as u64;
        self
    }

    /// Mark as skipped.
    pub fn skipped(mut self, reason: impl Into<String>) -> Self {
        self.success = true; // Skipped steps don't count as failures
        self.outcome = StepOutcome::Skipped;
        self.error = Some(StepError::new("SKIPPED", reason));
        self.completed_at = chrono::Utc::now().timestamp_millis();
        self.duration_ms = (self.completed_at - self.started_at) as u64;
        self
    }

    /// Add artifact.
    pub fn with_artifact(mut self, artifact: Artifact) -> Self {
        self.artifacts.get_or_insert_with(Vec::new).push(artifact);
        self
    }

    /// Add metric.
    pub fn with_metric(mut self, name: impl Into<String>, value: f64) -> Self {
        self.metrics
            .get_or_insert_with(HashMap::new)
            .insert(name.into(), value);
        self
    }

    /// Set retry info.
    pub fn with_retry_info(mut self, retry_count: u32) -> Self {
        self.is_retry = retry_count > 0;
        self.retry_count = retry_count;
        self
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execution_context_session_id() {
        let ctx = ExecutionContext::agentic("task-123", 2);
        assert_eq!(ctx.session_id(), "task-123-agentic-2");

        let ctx = ExecutionContext::setup("task-456");
        assert_eq!(ctx.session_id(), "task-456-setup");
    }

    #[test]
    fn test_execution_context_hierarchy() {
        let parent = ExecutionContext::agentic("parent-task", 1)
            .with_project_id("proj-123")
            .with_trace_id("trace-abc");

        // Parent is at depth 0 (root)
        assert_eq!(parent.depth, 0);

        let child = parent.child("child-task", WorkflowPhase::Setup);

        assert_eq!(child.parent_task_run_id, Some("parent-task".to_string()));
        // Child is at depth 1 (parent.depth + 1)
        assert_eq!(child.depth, 1);
        assert_eq!(child.project_id, Some("proj-123".to_string()));
        assert_eq!(child.trace_id, Some("trace-abc".to_string()));
    }

    #[test]
    fn test_execution_context_retry() {
        let ctx = ExecutionContext::agentic("task-123", 1)
            .with_retry(2, Some("original-task".to_string()));

        assert!(ctx.is_retry());
        assert_eq!(ctx.retry_attempt, 2);
        assert_eq!(ctx.retry_of, Some("original-task".to_string()));
    }

    #[test]
    fn test_ai_session_context() {
        let ctx = AiSessionContext::agentic("task-123", "My Workflow", 3)
            .with_model("claude-3-opus")
            .with_agent_role(AgentRole::Worker)
            .with_tokens(1000, 500);

        assert_eq!(ctx.task_run_id(), "task-123");
        assert_eq!(ctx.phase(), WorkflowPhase::Agentic);
        assert_eq!(ctx.iteration(), Some(3));
        assert_eq!(ctx.session_id, "task-123-agentic-3");
        assert_eq!(ctx.session_name, "My Workflow - Iteration 3");
        assert_eq!(ctx.model_id, Some("claude-3-opus".to_string()));
        assert_eq!(ctx.tokens_used, Some(1500));
    }

    #[test]
    fn test_step_result() {
        let result = StepResult::new("step-1")
            .with_metric("assertions", 5.0)
            .with_artifact(Artifact::screenshot("screenshots/test.png"))
            .succeeded();

        assert!(result.success);
        assert_eq!(result.outcome, StepOutcome::Success);
        assert!(result.metrics.as_ref().unwrap().contains_key("assertions"));
        assert_eq!(result.artifacts.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_step_error() {
        let error = StepError::new("ASSERTION_FAILED", "Expected 5, got 3")
            .with_details("at test.ts:42")
            .retryable()
            .with_suggestion("Check the test data");

        assert_eq!(error.code, "ASSERTION_FAILED");
        assert!(error.retryable);
        assert!(error.suggestion.is_some());
    }

    #[test]
    fn test_execution_context_to_json() {
        let ctx = ExecutionContext::verification("task-789", 1)
            .with_project_id("proj-123")
            .with_environment("staging");

        let json = ctx.to_json();

        assert_eq!(json["task_run_id"], "task-789");
        assert_eq!(json["phase"], "verification");
        assert_eq!(json["iteration"], 1);
        assert_eq!(json["project_id"], "proj-123");
        assert_eq!(json["environment"], "staging");
    }
}
