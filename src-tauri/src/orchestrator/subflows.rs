//! Subflow/Nested Workflow Support Module
//!
//! Implements workflow composition patterns inspired by Dify and n8n.
//! Subflows allow reusable workflow components with defined inputs and outputs.
//!
//! ## Key Concepts
//!
//! - **Subflow**: A reusable workflow with defined input/output ports
//! - **InputPort**: Defines expected inputs with type and validation
//! - **OutputPort**: Defines outputs produced by the subflow
//! - **SubflowLibrary**: Registry of reusable subflow templates
//!
//! ## Example
//!
//! ```rust
//! use qontinui_runner::orchestrator::subflows::*;
//!
//! // Define a reusable "run tests" subflow
//! let run_tests = SubflowDef::new("run_tests", "Run Test Suite")
//!     .with_input(InputPort::required("project_path", PortType::String))
//!     .with_input(InputPort::optional("test_filter", PortType::String))
//!     .with_output(OutputPort::new("passed", PortType::Boolean))
//!     .with_output(OutputPort::new("results", PortType::Json));
//! ```

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Port Types
// ============================================================================

/// Type of data a port accepts or produces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum PortType {
    /// String value.
    String,
    /// Integer value.
    Integer,
    /// Floating point value.
    Float,
    /// Boolean value.
    Boolean,
    /// JSON object.
    Json,
    /// Array of values.
    Array(Box<PortType>),
    /// File path.
    FilePath,
    /// Any type (no validation).
    #[default]
    Any,
    /// Custom type with validation.
    Custom { name: String },
}


impl PortType {
    /// Validate a value against this type.
    pub fn validate(&self, value: &serde_json::Value) -> bool {
        match self {
            Self::String => value.is_string(),
            Self::Integer => value.is_i64() || value.is_u64(),
            Self::Float => value.is_f64() || value.is_i64(),
            Self::Boolean => value.is_boolean(),
            Self::Json => value.is_object(),
            Self::Array(inner) => {
                if let Some(arr) = value.as_array() {
                    arr.iter().all(|v| inner.validate(v))
                } else {
                    false
                }
            }
            Self::FilePath => value.is_string(),
            Self::Any => true,
            Self::Custom { .. } => true, // Custom validation not implemented here
        }
    }

    /// Get a human-readable description.
    pub fn description(&self) -> String {
        match self {
            Self::String => "string".to_string(),
            Self::Integer => "integer".to_string(),
            Self::Float => "float".to_string(),
            Self::Boolean => "boolean".to_string(),
            Self::Json => "JSON object".to_string(),
            Self::Array(inner) => format!("array of {}", inner.description()),
            Self::FilePath => "file path".to_string(),
            Self::Any => "any".to_string(),
            Self::Custom { name } => name.clone(),
        }
    }
}

// ============================================================================
// Input Port
// ============================================================================

/// Definition of an input port for a subflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputPort {
    /// Port name.
    pub name: String,
    /// Port type.
    pub port_type: PortType,
    /// Whether this input is required.
    pub required: bool,
    /// Default value if not provided.
    pub default_value: Option<serde_json::Value>,
    /// Description for documentation.
    pub description: Option<String>,
    /// Validation rules.
    pub validation: Option<ValidationRules>,
}

/// Validation rules for inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationRules {
    /// Minimum value (for numbers).
    pub min: Option<f64>,
    /// Maximum value (for numbers).
    pub max: Option<f64>,
    /// Minimum length (for strings/arrays).
    pub min_length: Option<usize>,
    /// Maximum length (for strings/arrays).
    pub max_length: Option<usize>,
    /// Pattern to match (for strings).
    pub pattern: Option<String>,
    /// Allowed values (enum).
    pub allowed_values: Option<Vec<serde_json::Value>>,
}

impl InputPort {
    /// Create a required input port.
    pub fn required(name: impl Into<String>, port_type: PortType) -> Self {
        Self {
            name: name.into(),
            port_type,
            required: true,
            default_value: None,
            description: None,
            validation: None,
        }
    }

    /// Create an optional input port.
    pub fn optional(name: impl Into<String>, port_type: PortType) -> Self {
        Self {
            name: name.into(),
            port_type,
            required: false,
            default_value: None,
            description: None,
            validation: None,
        }
    }

    /// Set a default value.
    pub fn with_default(mut self, value: serde_json::Value) -> Self {
        self.default_value = Some(value);
        self
    }

    /// Set description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set validation rules.
    pub fn with_validation(mut self, validation: ValidationRules) -> Self {
        self.validation = Some(validation);
        self
    }

    /// Validate an input value.
    pub fn validate(&self, value: Option<&serde_json::Value>) -> Result<(), String> {
        match value {
            None => {
                if self.required && self.default_value.is_none() {
                    Err(format!("Required input '{}' is missing", self.name))
                } else {
                    Ok(())
                }
            }
            Some(v) => {
                if !self.port_type.validate(v) {
                    Err(format!(
                        "Input '{}' has wrong type, expected {}",
                        self.name,
                        self.port_type.description()
                    ))
                } else {
                    self.validate_rules(v)
                }
            }
        }
    }

    /// Validate against rules.
    fn validate_rules(&self, value: &serde_json::Value) -> Result<(), String> {
        if let Some(rules) = &self.validation {
            // Check min/max for numbers
            if let Some(n) = value.as_f64() {
                if let Some(min) = rules.min {
                    if n < min {
                        return Err(format!(
                            "Input '{}' value {} is below minimum {}",
                            self.name, n, min
                        ));
                    }
                }
                if let Some(max) = rules.max {
                    if n > max {
                        return Err(format!(
                            "Input '{}' value {} is above maximum {}",
                            self.name, n, max
                        ));
                    }
                }
            }

            // Check length for strings
            if let Some(s) = value.as_str() {
                if let Some(min_len) = rules.min_length {
                    if s.len() < min_len {
                        return Err(format!(
                            "Input '{}' length {} is below minimum {}",
                            self.name,
                            s.len(),
                            min_len
                        ));
                    }
                }
                if let Some(max_len) = rules.max_length {
                    if s.len() > max_len {
                        return Err(format!(
                            "Input '{}' length {} is above maximum {}",
                            self.name,
                            s.len(),
                            max_len
                        ));
                    }
                }
            }

            // Check allowed values
            if let Some(allowed) = &rules.allowed_values {
                if !allowed.contains(value) {
                    return Err(format!("Input '{}' value not in allowed values", self.name));
                }
            }
        }
        Ok(())
    }
}

// ============================================================================
// Output Port
// ============================================================================

/// Definition of an output port for a subflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputPort {
    /// Port name.
    pub name: String,
    /// Port type.
    pub port_type: PortType,
    /// Description for documentation.
    pub description: Option<String>,
    /// Whether this output is always produced.
    pub guaranteed: bool,
}

impl OutputPort {
    /// Create an output port.
    pub fn new(name: impl Into<String>, port_type: PortType) -> Self {
        Self {
            name: name.into(),
            port_type,
            description: None,
            guaranteed: true,
        }
    }

    /// Mark as optional output.
    pub fn optional(mut self) -> Self {
        self.guaranteed = false;
        self
    }

    /// Set description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

// ============================================================================
// Subflow Definition
// ============================================================================

/// Definition of a reusable subflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubflowDef {
    /// Unique identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Description.
    pub description: Option<String>,
    /// Version.
    pub version: String,
    /// Input ports.
    pub inputs: Vec<InputPort>,
    /// Output ports.
    pub outputs: Vec<OutputPort>,
    /// Tags for categorization.
    pub tags: Vec<String>,
    /// Author.
    pub author: Option<String>,
    /// Inner steps (workflow definition).
    pub steps: Vec<SubflowStep>,
    /// Default timeout in seconds.
    pub timeout_secs: Option<u64>,
    /// Whether this subflow can run in parallel.
    pub parallelizable: bool,
}

impl Default for SubflowDef {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            description: None,
            version: "1.0.0".to_string(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            tags: Vec::new(),
            author: None,
            steps: Vec::new(),
            timeout_secs: None,
            parallelizable: false,
        }
    }
}

impl SubflowDef {
    /// Create a new subflow definition.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            ..Default::default()
        }
    }

    /// Add an input port.
    pub fn with_input(mut self, input: InputPort) -> Self {
        self.inputs.push(input);
        self
    }

    /// Add an output port.
    pub fn with_output(mut self, output: OutputPort) -> Self {
        self.outputs.push(output);
        self
    }

    /// Set description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Add a tag.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Add a step.
    pub fn with_step(mut self, step: SubflowStep) -> Self {
        self.steps.push(step);
        self
    }

    /// Set timeout.
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = Some(secs);
        self
    }

    /// Mark as parallelizable.
    pub fn parallelizable(mut self) -> Self {
        self.parallelizable = true;
        self
    }

    /// Validate inputs against this subflow's schema.
    pub fn validate_inputs(
        &self,
        inputs: &HashMap<String, serde_json::Value>,
    ) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        for input_def in &self.inputs {
            if let Err(e) = input_def.validate(inputs.get(&input_def.name)) {
                errors.push(e);
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Get required input names.
    pub fn required_inputs(&self) -> Vec<&str> {
        self.inputs
            .iter()
            .filter(|i| i.required)
            .map(|i| i.name.as_str())
            .collect()
    }

    /// Get output names.
    pub fn output_names(&self) -> Vec<&str> {
        self.outputs.iter().map(|o| o.name.as_str()).collect()
    }
}

// ============================================================================
// Subflow Step
// ============================================================================

/// A step within a subflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubflowStep {
    /// Step ID.
    pub id: String,
    /// Step name.
    pub name: String,
    /// Type of step.
    pub step_type: StepType,
    /// Input mappings (from subflow inputs or previous step outputs).
    pub input_mappings: HashMap<String, String>,
    /// Condition for execution.
    pub condition: Option<StepCondition>,
    /// Whether to continue on error.
    pub continue_on_error: bool,
    /// Retry configuration.
    pub retry: Option<SubflowRetryConfig>,
}

/// Types of subflow steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StepType {
    /// Execute a worker.
    Worker { domain: String, prompt: String },
    /// Execute a nested subflow.
    Subflow { subflow_id: String },
    /// Conditional branch.
    Branch {
        conditions: Vec<BranchCondition>,
        default_branch: Option<String>,
    },
    /// Parallel execution.
    Parallel { branches: Vec<String> },
    /// Wait/delay.
    Wait { seconds: u64 },
    /// Transform data.
    Transform { expression: String },
    /// Custom step.
    Custom { handler: String },
}

/// Condition for step execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepCondition {
    /// Expression to evaluate.
    pub expression: String,
    /// Comparison operator.
    pub operator: ComparisonOp,
    /// Value to compare against.
    pub value: serde_json::Value,
}

/// Comparison operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonOp {
    Equals,
    NotEquals,
    GreaterThan,
    LessThan,
    GreaterThanOrEquals,
    LessThanOrEquals,
    Contains,
    StartsWith,
    EndsWith,
    IsNull,
    IsNotNull,
}

/// Condition for branching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchCondition {
    /// Condition to evaluate.
    pub condition: StepCondition,
    /// Target step ID if condition matches.
    pub target_step: String,
}

/// Retry configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubflowRetryConfig {
    /// Maximum retries.
    pub max_retries: u32,
    /// Initial delay in milliseconds.
    pub initial_delay_ms: u64,
    /// Backoff multiplier.
    pub backoff_multiplier: f64,
    /// Maximum delay in milliseconds.
    pub max_delay_ms: u64,
}

impl Default for SubflowRetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay_ms: 1000,
            backoff_multiplier: 2.0,
            max_delay_ms: 30000,
        }
    }
}

impl SubflowStep {
    /// Create a new worker step.
    pub fn worker(
        id: impl Into<String>,
        name: impl Into<String>,
        domain: &str,
        prompt: &str,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            step_type: StepType::Worker {
                domain: domain.to_string(),
                prompt: prompt.to_string(),
            },
            input_mappings: HashMap::new(),
            condition: None,
            continue_on_error: false,
            retry: None,
        }
    }

    /// Create a nested subflow step.
    pub fn subflow(id: impl Into<String>, name: impl Into<String>, subflow_id: &str) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            step_type: StepType::Subflow {
                subflow_id: subflow_id.to_string(),
            },
            input_mappings: HashMap::new(),
            condition: None,
            continue_on_error: false,
            retry: None,
        }
    }

    /// Add an input mapping.
    pub fn with_input_mapping(
        mut self,
        step_input: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        self.input_mappings.insert(step_input.into(), source.into());
        self
    }

    /// Set condition.
    pub fn with_condition(mut self, condition: StepCondition) -> Self {
        self.condition = Some(condition);
        self
    }

    /// Set continue on error.
    pub fn continue_on_error(mut self) -> Self {
        self.continue_on_error = true;
        self
    }

    /// Set retry config.
    pub fn with_retry(mut self, retry: SubflowRetryConfig) -> Self {
        self.retry = Some(retry);
        self
    }
}

// ============================================================================
// Subflow Instance
// ============================================================================

/// A running instance of a subflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubflowInstance {
    /// Instance ID.
    pub id: String,
    /// Subflow definition ID.
    pub subflow_id: String,
    /// Parent workflow/task ID.
    pub parent_id: Option<String>,
    /// Input values.
    pub inputs: HashMap<String, serde_json::Value>,
    /// Output values (populated as steps complete).
    pub outputs: HashMap<String, serde_json::Value>,
    /// Current status.
    pub status: SubflowStatus,
    /// Current step index.
    pub current_step: usize,
    /// Step results.
    pub step_results: HashMap<String, StepResult>,
    /// When started.
    pub started_at: String,
    /// When completed (if finished).
    pub completed_at: Option<String>,
    /// Error message (if failed).
    pub error: Option<String>,
}

/// Status of a subflow instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubflowStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    Paused,
}

/// Result of a step execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    /// Step ID.
    pub step_id: String,
    /// Whether the step succeeded.
    pub success: bool,
    /// Output values.
    pub outputs: HashMap<String, serde_json::Value>,
    /// Error message if failed.
    pub error: Option<String>,
    /// Duration in milliseconds.
    pub duration_ms: u64,
    /// Retry count.
    pub retries: u32,
}

impl SubflowInstance {
    /// Create a new instance.
    pub fn new(subflow_id: impl Into<String>, inputs: HashMap<String, serde_json::Value>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            subflow_id: subflow_id.into(),
            parent_id: None,
            inputs,
            outputs: HashMap::new(),
            status: SubflowStatus::Pending,
            current_step: 0,
            step_results: HashMap::new(),
            started_at: chrono::Utc::now().to_rfc3339(),
            completed_at: None,
            error: None,
        }
    }

    /// Set parent ID.
    pub fn with_parent(mut self, parent_id: impl Into<String>) -> Self {
        self.parent_id = Some(parent_id.into());
        self
    }

    /// Mark as started.
    pub fn start(&mut self) {
        self.status = SubflowStatus::Running;
    }

    /// Record a step result.
    pub fn record_step_result(&mut self, result: StepResult) {
        let success = result.success;
        self.step_results.insert(result.step_id.clone(), result);
        if !success {
            self.status = SubflowStatus::Failed;
        }
    }

    /// Mark as completed.
    pub fn complete(&mut self, outputs: HashMap<String, serde_json::Value>) {
        self.outputs = outputs;
        self.status = SubflowStatus::Completed;
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Mark as failed.
    pub fn fail(&mut self, error: impl Into<String>) {
        self.status = SubflowStatus::Failed;
        self.error = Some(error.into());
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Check if finished.
    pub fn is_finished(&self) -> bool {
        matches!(
            self.status,
            SubflowStatus::Completed | SubflowStatus::Failed | SubflowStatus::Cancelled
        )
    }
}

// ============================================================================
// Subflow Library
// ============================================================================

/// Library of reusable subflow definitions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubflowLibrary {
    /// Subflows by ID.
    subflows: HashMap<String, SubflowDef>,
    /// Category index.
    category_index: HashMap<String, Vec<String>>,
}

impl SubflowLibrary {
    /// Create a new library.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a subflow.
    pub fn register(&mut self, subflow: SubflowDef) {
        let id = subflow.id.clone();
        for tag in &subflow.tags {
            self.category_index
                .entry(tag.clone())
                .or_default()
                .push(id.clone());
        }
        self.subflows.insert(id, subflow);
    }

    /// Get a subflow by ID.
    pub fn get(&self, id: &str) -> Option<&SubflowDef> {
        self.subflows.get(id)
    }

    /// List all subflow IDs.
    pub fn list(&self) -> Vec<&str> {
        self.subflows.keys().map(|s| s.as_str()).collect()
    }

    /// Find subflows by tag.
    pub fn find_by_tag(&self, tag: &str) -> Vec<&SubflowDef> {
        self.category_index
            .get(tag)
            .map(|ids| ids.iter().filter_map(|id| self.subflows.get(id)).collect())
            .unwrap_or_default()
    }

    /// Create a new instance of a subflow.
    pub fn instantiate(
        &self,
        subflow_id: &str,
        inputs: HashMap<String, serde_json::Value>,
    ) -> Result<SubflowInstance, String> {
        let subflow = self
            .get(subflow_id)
            .ok_or_else(|| format!("Subflow '{}' not found", subflow_id))?;

        // Validate inputs
        if let Err(errors) = subflow.validate_inputs(&inputs) {
            return Err(format!("Input validation failed: {}", errors.join(", ")));
        }

        Ok(SubflowInstance::new(subflow_id, inputs))
    }

    /// Create library with predefined subflows.
    pub fn with_defaults() -> Self {
        let mut library = Self::new();

        // Run tests subflow
        library.register(
            SubflowDef::new("run_tests", "Run Test Suite")
                .with_description("Execute tests and collect results")
                .with_input(InputPort::required("project_path", PortType::FilePath))
                .with_input(
                    InputPort::optional("test_filter", PortType::String)
                        .with_description("Filter to run specific tests"),
                )
                .with_input(
                    InputPort::optional("timeout_secs", PortType::Integer)
                        .with_default(serde_json::json!(300)),
                )
                .with_output(OutputPort::new("passed", PortType::Boolean))
                .with_output(OutputPort::new("total_tests", PortType::Integer))
                .with_output(OutputPort::new(
                    "failed_tests",
                    PortType::Array(Box::new(PortType::String)),
                ))
                .with_tag("testing")
                .with_tag("ci"),
        );

        // Lint code subflow
        library.register(
            SubflowDef::new("lint_code", "Lint Code")
                .with_description("Run linting tools on the codebase")
                .with_input(InputPort::required("project_path", PortType::FilePath))
                .with_input(
                    InputPort::optional("fix", PortType::Boolean)
                        .with_default(serde_json::json!(false)),
                )
                .with_output(OutputPort::new("issues_found", PortType::Integer))
                .with_output(OutputPort::new("issues_fixed", PortType::Integer))
                .with_output(OutputPort::new(
                    "issues",
                    PortType::Array(Box::new(PortType::Json)),
                ))
                .with_tag("linting")
                .with_tag("code-quality"),
        );

        // Build project subflow
        library.register(
            SubflowDef::new("build_project", "Build Project")
                .with_description("Build the project and check for errors")
                .with_input(InputPort::required("project_path", PortType::FilePath))
                .with_input(
                    InputPort::optional("build_mode", PortType::String)
                        .with_default(serde_json::json!("debug")),
                )
                .with_output(OutputPort::new("success", PortType::Boolean))
                .with_output(OutputPort::new(
                    "errors",
                    PortType::Array(Box::new(PortType::String)),
                ))
                .with_output(OutputPort::new(
                    "warnings",
                    PortType::Array(Box::new(PortType::String)),
                ))
                .with_tag("build")
                .with_tag("ci"),
        );

        library
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_port_type_validation() {
        assert!(PortType::String.validate(&serde_json::json!("hello")));
        assert!(!PortType::String.validate(&serde_json::json!(123)));

        assert!(PortType::Integer.validate(&serde_json::json!(42)));
        assert!(!PortType::Integer.validate(&serde_json::json!("42")));

        assert!(PortType::Boolean.validate(&serde_json::json!(true)));
        assert!(!PortType::Boolean.validate(&serde_json::json!("true")));
    }

    #[test]
    fn test_array_port_type() {
        let port_type = PortType::Array(Box::new(PortType::String));
        assert!(port_type.validate(&serde_json::json!(["a", "b", "c"])));
        assert!(!port_type.validate(&serde_json::json!([1, 2, 3])));
    }

    #[test]
    fn test_input_port_validation() {
        let port = InputPort::required("name", PortType::String);
        assert!(port.validate(Some(&serde_json::json!("test"))).is_ok());
        assert!(port.validate(None).is_err());
    }

    #[test]
    fn test_input_port_with_validation_rules() {
        let port = InputPort::required("age", PortType::Integer).with_validation(ValidationRules {
            min: Some(0.0),
            max: Some(120.0),
            min_length: None,
            max_length: None,
            pattern: None,
            allowed_values: None,
        });

        assert!(port.validate(Some(&serde_json::json!(25))).is_ok());
        assert!(port.validate(Some(&serde_json::json!(-5))).is_err());
        assert!(port.validate(Some(&serde_json::json!(150))).is_err());
    }

    #[test]
    fn test_subflow_definition() {
        let subflow = SubflowDef::new("test_flow", "Test Flow")
            .with_input(InputPort::required("input1", PortType::String))
            .with_output(OutputPort::new("output1", PortType::Boolean))
            .with_tag("testing");

        assert_eq!(subflow.inputs.len(), 1);
        assert_eq!(subflow.outputs.len(), 1);
        assert!(subflow.tags.contains(&"testing".to_string()));
    }

    #[test]
    fn test_subflow_input_validation() {
        let subflow = SubflowDef::new("test", "Test")
            .with_input(InputPort::required("name", PortType::String))
            .with_input(InputPort::optional("count", PortType::Integer));

        let mut inputs = HashMap::new();
        inputs.insert("name".to_string(), serde_json::json!("test"));

        assert!(subflow.validate_inputs(&inputs).is_ok());

        let empty_inputs = HashMap::new();
        assert!(subflow.validate_inputs(&empty_inputs).is_err());
    }

    #[test]
    fn test_subflow_instance() {
        let mut inputs = HashMap::new();
        inputs.insert("path".to_string(), serde_json::json!("/project"));

        let mut instance = SubflowInstance::new("run_tests", inputs);
        assert_eq!(instance.status, SubflowStatus::Pending);

        instance.start();
        assert_eq!(instance.status, SubflowStatus::Running);

        let mut outputs = HashMap::new();
        outputs.insert("passed".to_string(), serde_json::json!(true));
        instance.complete(outputs);

        assert_eq!(instance.status, SubflowStatus::Completed);
        assert!(instance.is_finished());
    }

    #[test]
    fn test_subflow_library() {
        let library = SubflowLibrary::with_defaults();

        assert!(library.get("run_tests").is_some());
        assert!(library.get("lint_code").is_some());
        assert!(library.get("build_project").is_some());

        let testing_flows = library.find_by_tag("testing");
        assert!(!testing_flows.is_empty());
    }

    #[test]
    fn test_subflow_library_instantiate() {
        let library = SubflowLibrary::with_defaults();

        let mut inputs = HashMap::new();
        inputs.insert(
            "project_path".to_string(),
            serde_json::json!("/path/to/project"),
        );

        let instance = library.instantiate("run_tests", inputs);
        assert!(instance.is_ok());

        // Missing required input
        let empty_inputs = HashMap::new();
        let instance = library.instantiate("run_tests", empty_inputs);
        assert!(instance.is_err());
    }

    #[test]
    fn test_step_creation() {
        let step = SubflowStep::worker("step1", "Run Tests", "testing", "Run all unit tests")
            .with_input_mapping("path", "inputs.project_path")
            .with_retry(SubflowRetryConfig::default());

        assert_eq!(step.id, "step1");
        assert!(step.retry.is_some());
        assert!(step.input_mappings.contains_key("path"));
    }
}
