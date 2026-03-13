//! Flow Orchestration Mode Module
//!
//! Implements an alternative orchestration paradigm inspired by CrewAI's Flows.
//! Provides deterministic, step-by-step workflow execution alongside the
//! verification-driven mode.
//!
//! ## Key Concepts
//!
//! - **Flow**: A sequence of steps with explicit transitions
//! - **FlowStep**: Individual units of work (agent, tool, conditional, etc.)
//! - **FlowState**: Runtime state during flow execution
//!
//! ## Example
//!
//! ```rust
//! use qontinui_runner::orchestrator::flow::*;
//!
//! let flow = Flow::new("code_review_flow")
//!     .add_step(FlowStep::agent("analyze", "code_reviewer", "Review the code"))
//!     .add_step(FlowStep::conditional("has_issues",

#![allow(dead_code)]
//!         Condition::output_equals("analyze.issues_count", 0),
//!         "complete",
//!         "fix_issues"
//!     ))
//!     .add_step(FlowStep::agent("fix_issues", "debugger", "Fix the issues"))
//!     .with_start("analyze");
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Flow Definition
// ============================================================================

/// A flow-based workflow definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flow {
    /// Unique identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Description.
    pub description: Option<String>,
    /// Steps in the flow.
    pub steps: HashMap<String, FlowStep>,
    /// Starting step ID.
    pub start_step: Option<String>,
    /// Global timeout in seconds.
    pub timeout_secs: Option<u64>,
    /// Input parameters.
    pub inputs: Vec<FlowInput>,
    /// Output definitions.
    pub outputs: Vec<FlowOutput>,
    /// Tags.
    pub tags: Vec<String>,
    /// Version.
    pub version: String,
}

/// Input parameter for a flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowInput {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
    pub default_value: Option<serde_json::Value>,
}

/// Output definition for a flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowOutput {
    pub name: String,
    pub description: Option<String>,
    pub source: String, // e.g., "step_name.output_field"
}

impl Default for Flow {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            description: None,
            steps: HashMap::new(),
            start_step: None,
            timeout_secs: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            tags: Vec::new(),
            version: "1.0.0".to_string(),
        }
    }
}

impl Flow {
    /// Create a new flow.
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            id: id.clone(),
            name: id,
            ..Default::default()
        }
    }

    /// Set the name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set the description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Add a step.
    pub fn add_step(mut self, step: FlowStep) -> Self {
        let id = step.id.clone();
        // Auto-set start if first step
        if self.start_step.is_none() {
            self.start_step = Some(id.clone());
        }
        self.steps.insert(id, step);
        self
    }

    /// Set the starting step.
    pub fn with_start(mut self, step_id: impl Into<String>) -> Self {
        self.start_step = Some(step_id.into());
        self
    }

    /// Set timeout.
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = Some(secs);
        self
    }

    /// Add an input parameter.
    pub fn with_input(mut self, name: impl Into<String>, required: bool) -> Self {
        self.inputs.push(FlowInput {
            name: name.into(),
            description: None,
            required,
            default_value: None,
        });
        self
    }

    /// Add an output definition.
    pub fn with_output(mut self, name: impl Into<String>, source: impl Into<String>) -> Self {
        self.outputs.push(FlowOutput {
            name: name.into(),
            description: None,
            source: source.into(),
        });
        self
    }

    /// Validate the flow structure.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        if self.steps.is_empty() {
            errors.push("Flow has no steps".to_string());
        }

        if self.start_step.is_none() {
            errors.push("Flow has no start step".to_string());
        } else if let Some(start) = &self.start_step {
            if !self.steps.contains_key(start) {
                errors.push(format!("Start step '{}' not found", start));
            }
        }

        // Check all transitions point to valid steps
        for (step_id, step) in &self.steps {
            for next in step.get_next_steps() {
                if next != "end" && next != "fail" && !self.steps.contains_key(&next) {
                    errors.push(format!(
                        "Step '{}' references unknown step '{}'",
                        step_id, next
                    ));
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Get step by ID.
    pub fn get_step(&self, id: &str) -> Option<&FlowStep> {
        self.steps.get(id)
    }

    /// Get step count.
    pub fn step_count(&self) -> usize {
        self.steps.len()
    }
}

// ============================================================================
// Flow Step
// ============================================================================

/// A step in a flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowStep {
    /// Step ID.
    pub id: String,
    /// Step name.
    pub name: String,
    /// Step type.
    pub step_type: StepType,
    /// Description.
    pub description: Option<String>,
    /// Timeout for this step in seconds.
    pub timeout_secs: Option<u64>,
    /// Whether to continue on error.
    pub continue_on_error: bool,
    /// Retry configuration.
    pub retry_count: u32,
    /// Input mappings from flow context.
    pub input_mappings: HashMap<String, String>,
    /// Condition for executing this step.
    pub condition: Option<Condition>,
}

/// Types of flow steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StepType {
    /// Execute an agent with a role.
    Agent {
        role: String,
        prompt: String,
        max_iterations: Option<u32>,
        next: String,
    },
    /// Execute a tool.
    Tool {
        tool_id: String,
        inputs: HashMap<String, serde_json::Value>,
        next: String,
    },
    /// Conditional branch.
    Conditional {
        condition: Condition,
        then_step: String,
        else_step: String,
    },
    /// Parallel execution.
    Parallel {
        branches: Vec<String>,
        merge_strategy: ParallelMerge,
        next: String,
    },
    /// Wait for input.
    HumanInput {
        prompt: String,
        options: Vec<String>,
        next: String,
    },
    /// Transform data.
    Transform {
        expression: String,
        output_key: String,
        next: String,
    },
    /// Delay/wait.
    Wait { seconds: u64, next: String },
    /// Loop construct.
    Loop {
        over: String, // expression evaluating to array
        body_step: String,
        next: String,
    },
    /// End the flow successfully.
    End,
    /// End the flow with failure.
    Fail { error: String },
}

/// Merge strategy for parallel steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ParallelMerge {
    #[default]
    WaitAll,
    WaitAny,
    WaitN(usize),
}


impl FlowStep {
    /// Create an agent step.
    pub fn agent(
        id: impl Into<String>,
        role: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Self {
        let id = id.into();
        Self {
            name: id.clone(),
            id,
            step_type: StepType::Agent {
                role: role.into(),
                prompt: prompt.into(),
                max_iterations: None,
                next: "end".to_string(),
            },
            description: None,
            timeout_secs: None,
            continue_on_error: false,
            retry_count: 0,
            input_mappings: HashMap::new(),
            condition: None,
        }
    }

    /// Create a tool step.
    pub fn tool(id: impl Into<String>, tool_id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            name: id.clone(),
            id,
            step_type: StepType::Tool {
                tool_id: tool_id.into(),
                inputs: HashMap::new(),
                next: "end".to_string(),
            },
            description: None,
            timeout_secs: None,
            continue_on_error: false,
            retry_count: 0,
            input_mappings: HashMap::new(),
            condition: None,
        }
    }

    /// Create a conditional step.
    pub fn conditional(
        id: impl Into<String>,
        condition: Condition,
        then_step: impl Into<String>,
        else_step: impl Into<String>,
    ) -> Self {
        let id = id.into();
        Self {
            name: id.clone(),
            id,
            step_type: StepType::Conditional {
                condition,
                then_step: then_step.into(),
                else_step: else_step.into(),
            },
            description: None,
            timeout_secs: None,
            continue_on_error: false,
            retry_count: 0,
            input_mappings: HashMap::new(),
            condition: None,
        }
    }

    /// Create a parallel step.
    pub fn parallel(id: impl Into<String>, branches: Vec<String>) -> Self {
        let id = id.into();
        Self {
            name: id.clone(),
            id,
            step_type: StepType::Parallel {
                branches,
                merge_strategy: ParallelMerge::default(),
                next: "end".to_string(),
            },
            description: None,
            timeout_secs: None,
            continue_on_error: false,
            retry_count: 0,
            input_mappings: HashMap::new(),
            condition: None,
        }
    }

    /// Create a human input step.
    pub fn human_input(id: impl Into<String>, prompt: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            name: id.clone(),
            id,
            step_type: StepType::HumanInput {
                prompt: prompt.into(),
                options: Vec::new(),
                next: "end".to_string(),
            },
            description: None,
            timeout_secs: None,
            continue_on_error: false,
            retry_count: 0,
            input_mappings: HashMap::new(),
            condition: None,
        }
    }

    /// Create an end step.
    pub fn end(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            name: id.clone(),
            id,
            step_type: StepType::End,
            description: None,
            timeout_secs: None,
            continue_on_error: false,
            retry_count: 0,
            input_mappings: HashMap::new(),
            condition: None,
        }
    }

    /// Create a fail step.
    pub fn fail(id: impl Into<String>, error: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            name: id.clone(),
            id,
            step_type: StepType::Fail {
                error: error.into(),
            },
            description: None,
            timeout_secs: None,
            continue_on_error: false,
            retry_count: 0,
            input_mappings: HashMap::new(),
            condition: None,
        }
    }

    /// Set the next step.
    pub fn then(mut self, next: impl Into<String>) -> Self {
        match &mut self.step_type {
            StepType::Agent { next: n, .. }
            | StepType::Tool { next: n, .. }
            | StepType::Parallel { next: n, .. }
            | StepType::HumanInput { next: n, .. }
            | StepType::Transform { next: n, .. }
            | StepType::Wait { next: n, .. }
            | StepType::Loop { next: n, .. } => {
                *n = next.into();
            }
            _ => {}
        }
        self
    }

    /// Set description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set timeout.
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = Some(secs);
        self
    }

    /// Set retry count.
    pub fn with_retry(mut self, count: u32) -> Self {
        self.retry_count = count;
        self
    }

    /// Allow continue on error.
    pub fn continue_on_error(mut self) -> Self {
        self.continue_on_error = true;
        self
    }

    /// Add input mapping.
    pub fn with_input(mut self, name: impl Into<String>, source: impl Into<String>) -> Self {
        self.input_mappings.insert(name.into(), source.into());
        self
    }

    /// Add execution condition.
    pub fn when(mut self, condition: Condition) -> Self {
        self.condition = Some(condition);
        self
    }

    /// Get next steps this step can transition to.
    pub fn get_next_steps(&self) -> Vec<String> {
        match &self.step_type {
            StepType::Agent { next, .. }
            | StepType::Tool { next, .. }
            | StepType::Parallel { next, .. }
            | StepType::HumanInput { next, .. }
            | StepType::Transform { next, .. }
            | StepType::Wait { next, .. }
            | StepType::Loop { next, .. } => vec![next.clone()],
            StepType::Conditional {
                then_step,
                else_step,
                ..
            } => vec![then_step.clone(), else_step.clone()],
            StepType::End | StepType::Fail { .. } => vec![],
        }
    }
}

// ============================================================================
// Conditions
// ============================================================================

/// A condition for flow control.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Condition {
    /// Check if a value equals something.
    Equals {
        left: String,
        right: serde_json::Value,
    },
    /// Check if a value doesn't equal something.
    NotEquals {
        left: String,
        right: serde_json::Value,
    },
    /// Check if a value is greater than.
    GreaterThan { left: String, right: f64 },
    /// Check if a value is less than.
    LessThan { left: String, right: f64 },
    /// Check if a value is true/truthy.
    IsTrue { value: String },
    /// Check if a value is false/falsy.
    IsFalse { value: String },
    /// Check if a value is null/missing.
    IsNull { value: String },
    /// Check if a value is not null.
    IsNotNull { value: String },
    /// Check if a string contains a substring.
    Contains { value: String, substring: String },
    /// Logical AND of conditions.
    And { conditions: Vec<Condition> },
    /// Logical OR of conditions.
    Or { conditions: Vec<Condition> },
    /// Logical NOT of a condition.
    Not { condition: Box<Condition> },
    /// Custom expression.
    Expression { expr: String },
}

impl Condition {
    /// Create an equals condition.
    pub fn equals(left: impl Into<String>, right: serde_json::Value) -> Self {
        Self::Equals {
            left: left.into(),
            right,
        }
    }

    /// Create an is_true condition.
    pub fn is_true(value: impl Into<String>) -> Self {
        Self::IsTrue {
            value: value.into(),
        }
    }

    /// Create an is_false condition.
    pub fn is_false(value: impl Into<String>) -> Self {
        Self::IsFalse {
            value: value.into(),
        }
    }

    /// Create an AND condition.
    pub fn and(conditions: Vec<Condition>) -> Self {
        Self::And { conditions }
    }

    /// Create an OR condition.
    pub fn or(conditions: Vec<Condition>) -> Self {
        Self::Or { conditions }
    }

    /// Create a NOT condition.
    pub fn not(condition: Condition) -> Self {
        Self::Not {
            condition: Box::new(condition),
        }
    }

    /// Evaluate condition against context.
    pub fn evaluate(&self, context: &HashMap<String, serde_json::Value>) -> bool {
        match self {
            Self::Equals { left, right } => context.get(left) == Some(right),
            Self::NotEquals { left, right } => context.get(left) != Some(right),
            Self::GreaterThan { left, right } => context
                .get(left)
                .and_then(|v| v.as_f64())
                .map(|v| v > *right)
                .unwrap_or(false),
            Self::LessThan { left, right } => context
                .get(left)
                .and_then(|v| v.as_f64())
                .map(|v| v < *right)
                .unwrap_or(false),
            Self::IsTrue { value } => context
                .get(value)
                .map(|v| v.as_bool().unwrap_or(false))
                .unwrap_or(false),
            Self::IsFalse { value } => context
                .get(value)
                .map(|v| !v.as_bool().unwrap_or(true))
                .unwrap_or(true),
            Self::IsNull { value } => context.get(value).map(|v| v.is_null()).unwrap_or(true),
            Self::IsNotNull { value } => context.get(value).map(|v| !v.is_null()).unwrap_or(false),
            Self::Contains { value, substring } => context
                .get(value)
                .and_then(|v| v.as_str())
                .map(|s| s.contains(substring))
                .unwrap_or(false),
            Self::And { conditions } => conditions.iter().all(|c| c.evaluate(context)),
            Self::Or { conditions } => conditions.iter().any(|c| c.evaluate(context)),
            Self::Not { condition } => !condition.evaluate(context),
            Self::Expression { .. } => true, // Custom expressions not evaluated here
        }
    }
}

// ============================================================================
// Flow State
// ============================================================================

/// Runtime state during flow execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowState {
    /// Flow instance ID.
    pub instance_id: String,
    /// Flow definition ID.
    pub flow_id: String,
    /// Current step ID.
    pub current_step: Option<String>,
    /// Step execution history.
    pub history: Vec<StepExecution>,
    /// Flow context (variables, outputs).
    pub context: HashMap<String, serde_json::Value>,
    /// Flow status.
    pub status: FlowStatus,
    /// When started.
    pub started_at: String,
    /// When completed.
    pub completed_at: Option<String>,
    /// Error if failed.
    pub error: Option<String>,
}

/// Status of a flow execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowStatus {
    Pending,
    Running,
    WaitingForInput,
    Completed,
    Failed,
    Cancelled,
}

/// Record of a step execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepExecution {
    /// Step ID.
    pub step_id: String,
    /// When started.
    pub started_at: String,
    /// When completed.
    pub completed_at: Option<String>,
    /// Duration in milliseconds.
    pub duration_ms: Option<u64>,
    /// Whether successful.
    pub success: bool,
    /// Error if failed.
    pub error: Option<String>,
    /// Output values.
    pub outputs: HashMap<String, serde_json::Value>,
}

impl FlowState {
    /// Create a new flow state.
    pub fn new(flow: &Flow) -> Self {
        Self {
            instance_id: uuid::Uuid::new_v4().to_string(),
            flow_id: flow.id.clone(),
            current_step: flow.start_step.clone(),
            history: Vec::new(),
            context: HashMap::new(),
            status: FlowStatus::Pending,
            started_at: chrono::Utc::now().to_rfc3339(),
            completed_at: None,
            error: None,
        }
    }

    /// Start the flow.
    pub fn start(&mut self) {
        self.status = FlowStatus::Running;
    }

    /// Set current step.
    pub fn set_current_step(&mut self, step_id: impl Into<String>) {
        self.current_step = Some(step_id.into());
    }

    /// Record step start.
    pub fn step_started(&mut self, step_id: impl Into<String>) {
        self.history.push(StepExecution {
            step_id: step_id.into(),
            started_at: chrono::Utc::now().to_rfc3339(),
            completed_at: None,
            duration_ms: None,
            success: false,
            error: None,
            outputs: HashMap::new(),
        });
    }

    /// Record step completion.
    pub fn step_completed(&mut self, success: bool, outputs: HashMap<String, serde_json::Value>) {
        if let Some(exec) = self.history.last_mut() {
            let now = chrono::Utc::now();
            exec.completed_at = Some(now.to_rfc3339());
            exec.success = success;
            exec.outputs = outputs.clone();

            // Calculate duration from started_at
            if let Ok(started) = chrono::DateTime::parse_from_rfc3339(&exec.started_at) {
                let duration = now.signed_duration_since(started);
                exec.duration_ms = Some(duration.num_milliseconds().max(0) as u64);
            }

            // Merge outputs into context
            for (key, value) in outputs {
                let context_key = format!("{}.{}", exec.step_id, key);
                self.context.insert(context_key, value);
            }
        }
    }

    /// Record step failure.
    pub fn step_failed(&mut self, error: impl Into<String>) {
        if let Some(exec) = self.history.last_mut() {
            let now = chrono::Utc::now();
            exec.completed_at = Some(now.to_rfc3339());
            exec.success = false;
            exec.error = Some(error.into());

            // Calculate duration from started_at
            if let Ok(started) = chrono::DateTime::parse_from_rfc3339(&exec.started_at) {
                let duration = now.signed_duration_since(started);
                exec.duration_ms = Some(duration.num_milliseconds().max(0) as u64);
            }
        }
    }

    /// Complete the flow.
    pub fn complete(&mut self) {
        self.status = FlowStatus::Completed;
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());
        self.current_step = None;
    }

    /// Fail the flow.
    pub fn fail(&mut self, error: impl Into<String>) {
        self.status = FlowStatus::Failed;
        self.error = Some(error.into());
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Set context value.
    pub fn set(&mut self, key: impl Into<String>, value: serde_json::Value) {
        self.context.insert(key.into(), value);
    }

    /// Get context value.
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.context.get(key)
    }

    /// Get step execution count.
    pub fn execution_count(&self) -> usize {
        self.history.len()
    }

    /// Check if flow is finished.
    pub fn is_finished(&self) -> bool {
        matches!(
            self.status,
            FlowStatus::Completed | FlowStatus::Failed | FlowStatus::Cancelled
        )
    }

    /// Calculate total flow duration in seconds.
    ///
    /// Returns None if the flow hasn't completed or if timestamps are invalid.
    pub fn total_duration_secs(&self) -> Option<f64> {
        let completed_at = self.completed_at.as_ref()?;

        // Parse the ISO timestamps
        let start = chrono::DateTime::parse_from_rfc3339(&self.started_at).ok()?;
        let end = chrono::DateTime::parse_from_rfc3339(completed_at).ok()?;

        // Calculate duration
        let duration = end.signed_duration_since(start);
        Some(duration.num_milliseconds() as f64 / 1000.0)
    }

    /// Calculate average step duration in seconds.
    ///
    /// Only considers steps that have a recorded duration.
    pub fn average_step_duration_secs(&self) -> Option<f64> {
        let durations: Vec<_> = self
            .history
            .iter()
            .filter_map(|step| step.duration_ms)
            .collect();

        if durations.is_empty() {
            None
        } else {
            let sum: u64 = durations.iter().sum();
            Some(sum as f64 / durations.len() as f64 / 1000.0)
        }
    }

    /// Calculate total step execution time in seconds.
    ///
    /// This sums the duration of all completed steps.
    pub fn total_step_duration_secs(&self) -> f64 {
        let sum: u64 = self
            .history
            .iter()
            .filter_map(|step| step.duration_ms)
            .sum();
        sum as f64 / 1000.0
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flow_creation() {
        let flow = Flow::new("test_flow")
            .with_name("Test Flow")
            .add_step(FlowStep::agent("step1", "debugger", "Debug code"))
            .add_step(FlowStep::end("done"));

        assert_eq!(flow.step_count(), 2);
        assert_eq!(flow.start_step, Some("step1".to_string()));
    }

    #[test]
    fn test_flow_validation() {
        let valid_flow = Flow::new("valid")
            .add_step(FlowStep::agent("start", "role", "prompt").then("end"))
            .add_step(FlowStep::end("end"));

        assert!(valid_flow.validate().is_ok());

        let invalid_flow = Flow::new("invalid")
            .add_step(FlowStep::agent("start", "role", "prompt").then("nonexistent"));

        assert!(invalid_flow.validate().is_err());
    }

    #[test]
    fn test_step_transitions() {
        let step = FlowStep::conditional(
            "check",
            Condition::is_true("value"),
            "then_step",
            "else_step",
        );

        let nexts = step.get_next_steps();
        assert_eq!(nexts.len(), 2);
        assert!(nexts.contains(&"then_step".to_string()));
        assert!(nexts.contains(&"else_step".to_string()));
    }

    #[test]
    fn test_condition_evaluation() {
        let mut context = HashMap::new();
        context.insert("flag".to_string(), serde_json::json!(true));
        context.insert("count".to_string(), serde_json::json!(10));

        assert!(Condition::is_true("flag").evaluate(&context));
        assert!(Condition::equals("count", serde_json::json!(10)).evaluate(&context));
        assert!(Condition::GreaterThan {
            left: "count".to_string(),
            right: 5.0,
        }
        .evaluate(&context));
    }

    #[test]
    fn test_condition_logic() {
        let mut context = HashMap::new();
        context.insert("a".to_string(), serde_json::json!(true));
        context.insert("b".to_string(), serde_json::json!(false));

        let and_cond = Condition::and(vec![Condition::is_true("a"), Condition::is_true("b")]);
        assert!(!and_cond.evaluate(&context));

        let or_cond = Condition::or(vec![Condition::is_true("a"), Condition::is_true("b")]);
        assert!(or_cond.evaluate(&context));
    }

    #[test]
    fn test_flow_state() {
        let flow = Flow::new("test").add_step(FlowStep::agent("step1", "role", "prompt"));
        let mut state = FlowState::new(&flow);

        assert_eq!(state.status, FlowStatus::Pending);
        state.start();
        assert_eq!(state.status, FlowStatus::Running);

        state.step_started("step1");
        let mut outputs = HashMap::new();
        outputs.insert("result".to_string(), serde_json::json!("success"));
        state.step_completed(true, outputs);

        assert_eq!(state.execution_count(), 1);
        assert!(state.get("step1.result").is_some());
    }

    #[test]
    fn test_parallel_step() {
        let step = FlowStep::parallel(
            "parallel_work",
            vec!["branch_a".to_string(), "branch_b".to_string()],
        )
        .then("merge");

        if let StepType::Parallel { branches, next, .. } = &step.step_type {
            assert_eq!(branches.len(), 2);
            assert_eq!(next, "merge");
        } else {
            panic!("Expected Parallel step type");
        }
    }

    #[test]
    fn test_input_mappings() {
        let step = FlowStep::agent("process", "role", "prompt")
            .with_input("file", "input.file_path")
            .with_input("mode", "config.mode");

        assert_eq!(step.input_mappings.len(), 2);
    }
}
