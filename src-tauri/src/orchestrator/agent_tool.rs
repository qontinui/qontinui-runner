//! Agent-as-Tool Pattern Module
//!
//! Implements the agent-as-tool pattern from AutoGen, enabling workers to delegate
//! to specialized sub-agents as if they were tools.
//!
//! ## Key Concepts
//!
//! - **AgentTool**: Wraps an agent role as an invocable tool
//! - **ToolInvocation**: Request to invoke a tool (agent or otherwise)
//! - **ToolResult**: Structured result from tool invocation
//!
//! ## Example
//!
//! ```rust
//! use qontinui_runner::orchestrator::agent_tool::*;
//!
//! // Create an agent tool from a role
//! let security_scanner = AgentTool::from_role("security_analyst")
//!     .with_description("Scan code for security vulnerabilities")
//!     .with_input("file_path", ToolInputType::String);
//!
//! // Invoke the tool
//! let invocation = ToolInvocation::new("security_scanner")
//!     .with_input("file_path", json!("src/auth.rs"));
//! ```

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Tool Input/Output Types
// ============================================================================

/// Type of a tool input or output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolInputType {
    /// String value.
    String,
    /// Integer value.
    Integer,
    /// Float value.
    Float,
    /// Boolean value.
    Boolean,
    /// JSON object.
    Object,
    /// Array of values.
    Array,
    /// File path.
    FilePath,
    /// Code snippet.
    Code,
    /// Any type.
    Any,
}

impl Default for ToolInputType {
    fn default() -> Self {
        Self::Any
    }
}

/// Definition of a tool input parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInput {
    /// Parameter name.
    pub name: String,
    /// Parameter type.
    pub input_type: ToolInputType,
    /// Description.
    pub description: Option<String>,
    /// Whether this input is required.
    pub required: bool,
    /// Default value.
    pub default_value: Option<serde_json::Value>,
    /// Example value.
    pub example: Option<serde_json::Value>,
}

impl ToolInput {
    /// Create a required input.
    pub fn required(name: impl Into<String>, input_type: ToolInputType) -> Self {
        Self {
            name: name.into(),
            input_type,
            description: None,
            required: true,
            default_value: None,
            example: None,
        }
    }

    /// Create an optional input.
    pub fn optional(name: impl Into<String>, input_type: ToolInputType) -> Self {
        Self {
            name: name.into(),
            input_type,
            description: None,
            required: false,
            default_value: None,
            example: None,
        }
    }

    /// Set description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set default value.
    pub fn with_default(mut self, value: serde_json::Value) -> Self {
        self.default_value = Some(value);
        self
    }

    /// Set example.
    pub fn with_example(mut self, value: serde_json::Value) -> Self {
        self.example = Some(value);
        self
    }
}

/// Definition of a tool output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    /// Output name.
    pub name: String,
    /// Output type.
    pub output_type: ToolInputType,
    /// Description.
    pub description: Option<String>,
}

impl ToolOutput {
    /// Create a new output.
    pub fn new(name: impl Into<String>, output_type: ToolInputType) -> Self {
        Self {
            name: name.into(),
            output_type,
            description: None,
        }
    }

    /// Set description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

// ============================================================================
// Agent Tool
// ============================================================================

/// An agent wrapped as a tool for use by other agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTool {
    /// Tool identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Description of what this tool does.
    pub description: String,
    /// Agent role ID to use.
    pub agent_role: String,
    /// Input parameters.
    pub inputs: Vec<ToolInput>,
    /// Output definitions.
    pub outputs: Vec<ToolOutput>,
    /// Maximum iterations for the agent.
    pub max_iterations: u32,
    /// Timeout in seconds.
    pub timeout_secs: u64,
    /// Whether to include conversation context.
    pub include_context: bool,
    /// Custom prompt template.
    pub prompt_template: Option<String>,
    /// Tags for categorization.
    pub tags: Vec<String>,
}

impl Default for AgentTool {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            agent_role: String::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            max_iterations: 3,
            timeout_secs: 300,
            include_context: true,
            prompt_template: None,
            tags: Vec::new(),
        }
    }
}

impl AgentTool {
    /// Create an agent tool from a role ID.
    pub fn from_role(role_id: impl Into<String>) -> Self {
        let role_id = role_id.into();
        Self {
            id: role_id.clone(),
            agent_role: role_id,
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
        self.description = description.into();
        self
    }

    /// Add an input parameter.
    pub fn with_input(mut self, name: impl Into<String>, input_type: ToolInputType) -> Self {
        self.inputs.push(ToolInput::required(name, input_type));
        self
    }

    /// Add an optional input parameter.
    pub fn with_optional_input(
        mut self,
        name: impl Into<String>,
        input_type: ToolInputType,
    ) -> Self {
        self.inputs.push(ToolInput::optional(name, input_type));
        self
    }

    /// Add an output definition.
    pub fn with_output(mut self, name: impl Into<String>, output_type: ToolInputType) -> Self {
        self.outputs.push(ToolOutput::new(name, output_type));
        self
    }

    /// Set max iterations.
    pub fn with_max_iterations(mut self, max: u32) -> Self {
        self.max_iterations = max;
        self
    }

    /// Set timeout.
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// Disable context inclusion.
    pub fn without_context(mut self) -> Self {
        self.include_context = false;
        self
    }

    /// Set a custom prompt template.
    pub fn with_prompt_template(mut self, template: impl Into<String>) -> Self {
        self.prompt_template = Some(template.into());
        self
    }

    /// Add a tag.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Validate inputs against the tool's schema.
    pub fn validate_inputs(
        &self,
        inputs: &HashMap<String, serde_json::Value>,
    ) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        for input_def in &self.inputs {
            match inputs.get(&input_def.name) {
                None if input_def.required && input_def.default_value.is_none() => {
                    errors.push(format!("Missing required input: {}", input_def.name));
                }
                Some(value) => {
                    if !self.validate_type(value, &input_def.input_type) {
                        errors.push(format!(
                            "Invalid type for input '{}': expected {:?}",
                            input_def.name, input_def.input_type
                        ));
                    }
                }
                _ => {}
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Validate a value against an expected type.
    fn validate_type(&self, value: &serde_json::Value, expected: &ToolInputType) -> bool {
        match expected {
            ToolInputType::String | ToolInputType::FilePath | ToolInputType::Code => {
                value.is_string()
            }
            ToolInputType::Integer => value.is_i64() || value.is_u64(),
            ToolInputType::Float => value.is_f64() || value.is_i64(),
            ToolInputType::Boolean => value.is_boolean(),
            ToolInputType::Object => value.is_object(),
            ToolInputType::Array => value.is_array(),
            ToolInputType::Any => true,
        }
    }

    /// Build a prompt for the agent from inputs.
    pub fn build_prompt(&self, inputs: &HashMap<String, serde_json::Value>) -> String {
        if let Some(template) = &self.prompt_template {
            let mut prompt = template.clone();
            for (key, value) in inputs {
                let placeholder = format!("{{{{{}}}}}", key);
                let value_str = match value {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                prompt = prompt.replace(&placeholder, &value_str);
            }
            prompt
        } else {
            // Default prompt format
            let mut prompt = format!("# Task: {}\n\n", self.description);
            prompt.push_str("## Inputs\n");
            for (key, value) in inputs {
                prompt.push_str(&format!("- {}: {}\n", key, value));
            }
            prompt
        }
    }

    /// Generate a JSON schema for this tool (for LLM function calling).
    pub fn to_json_schema(&self) -> serde_json::Value {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();

        for input in &self.inputs {
            let prop_type = match input.input_type {
                ToolInputType::String | ToolInputType::FilePath | ToolInputType::Code => "string",
                ToolInputType::Integer => "integer",
                ToolInputType::Float => "number",
                ToolInputType::Boolean => "boolean",
                ToolInputType::Object => "object",
                ToolInputType::Array => "array",
                ToolInputType::Any => "string",
            };

            let mut prop = serde_json::json!({
                "type": prop_type,
            });

            if let Some(desc) = &input.description {
                prop["description"] = serde_json::Value::String(desc.clone());
            }

            properties.insert(input.name.clone(), prop);

            if input.required {
                required.push(serde_json::Value::String(input.name.clone()));
            }
        }

        serde_json::json!({
            "name": self.id,
            "description": self.description,
            "parameters": {
                "type": "object",
                "properties": properties,
                "required": required,
            }
        })
    }
}

// ============================================================================
// Tool Invocation
// ============================================================================

/// A request to invoke a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInvocation {
    /// Invocation ID.
    pub id: String,
    /// Tool ID to invoke.
    pub tool_id: String,
    /// Input values.
    pub inputs: HashMap<String, serde_json::Value>,
    /// Caller context (for nested invocations).
    pub caller_context: Option<CallerContext>,
    /// When the invocation was created.
    pub created_at: String,
}

/// Context from the calling agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallerContext {
    /// Calling agent/worker ID.
    pub caller_id: String,
    /// Task ID.
    pub task_id: Option<String>,
    /// Iteration number.
    pub iteration: Option<u32>,
    /// Additional context.
    pub context: Option<String>,
}

impl ToolInvocation {
    /// Create a new invocation.
    pub fn new(tool_id: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            tool_id: tool_id.into(),
            inputs: HashMap::new(),
            caller_context: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Add an input value.
    pub fn with_input(mut self, name: impl Into<String>, value: serde_json::Value) -> Self {
        self.inputs.insert(name.into(), value);
        self
    }

    /// Set caller context.
    pub fn with_caller(mut self, context: CallerContext) -> Self {
        self.caller_context = Some(context);
        self
    }
}

// ============================================================================
// Tool Result
// ============================================================================

/// Result of a tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Invocation ID this result is for.
    pub invocation_id: String,
    /// Tool ID.
    pub tool_id: String,
    /// Whether the invocation succeeded.
    pub success: bool,
    /// Output values.
    pub outputs: HashMap<String, serde_json::Value>,
    /// Error message if failed.
    pub error: Option<String>,
    /// Execution duration in milliseconds.
    pub duration_ms: u64,
    /// Number of iterations used (for agent tools).
    pub iterations_used: Option<u32>,
    /// When completed.
    pub completed_at: String,
}

impl ToolResult {
    /// Create a successful result.
    pub fn success(invocation_id: impl Into<String>, tool_id: impl Into<String>) -> Self {
        Self {
            invocation_id: invocation_id.into(),
            tool_id: tool_id.into(),
            success: true,
            outputs: HashMap::new(),
            error: None,
            duration_ms: 0,
            iterations_used: None,
            completed_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Create a failed result.
    pub fn failure(
        invocation_id: impl Into<String>,
        tool_id: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            invocation_id: invocation_id.into(),
            tool_id: tool_id.into(),
            success: false,
            outputs: HashMap::new(),
            error: Some(error.into()),
            duration_ms: 0,
            iterations_used: None,
            completed_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Add an output.
    pub fn with_output(mut self, name: impl Into<String>, value: serde_json::Value) -> Self {
        self.outputs.insert(name.into(), value);
        self
    }

    /// Set duration.
    pub fn with_duration(mut self, ms: u64) -> Self {
        self.duration_ms = ms;
        self
    }

    /// Set iterations used.
    pub fn with_iterations(mut self, iterations: u32) -> Self {
        self.iterations_used = Some(iterations);
        self
    }
}

// ============================================================================
// Tool Registry
// ============================================================================

/// Registry of available tools (including agent tools).
#[derive(Debug, Default)]
pub struct ToolRegistry {
    /// Agent tools by ID.
    agent_tools: HashMap<String, AgentTool>,
    /// Tool aliases.
    aliases: HashMap<String, String>,
}

impl ToolRegistry {
    /// Create a new registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an agent tool.
    pub fn register(&mut self, tool: AgentTool) {
        self.agent_tools.insert(tool.id.clone(), tool);
    }

    /// Register an alias for a tool.
    pub fn add_alias(&mut self, alias: impl Into<String>, tool_id: impl Into<String>) {
        self.aliases.insert(alias.into(), tool_id.into());
    }

    /// Get a tool by ID or alias.
    pub fn get(&self, id: &str) -> Option<&AgentTool> {
        self.agent_tools.get(id).or_else(|| {
            self.aliases
                .get(id)
                .and_then(|real_id| self.agent_tools.get(real_id))
        })
    }

    /// List all tool IDs.
    pub fn list(&self) -> Vec<&str> {
        self.agent_tools.keys().map(|s| s.as_str()).collect()
    }

    /// Find tools by tag.
    pub fn find_by_tag(&self, tag: &str) -> Vec<&AgentTool> {
        self.agent_tools
            .values()
            .filter(|t| t.tags.iter().any(|t| t == tag))
            .collect()
    }

    /// Get all tools as JSON schemas (for LLM function calling).
    pub fn to_json_schemas(&self) -> Vec<serde_json::Value> {
        self.agent_tools
            .values()
            .map(|t| t.to_json_schema())
            .collect()
    }

    /// Create registry with predefined agent tools.
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();

        // Security scanner tool
        registry.register(
            AgentTool::from_role("security_analyst")
                .with_name("security_scanner")
                .with_description("Scan code for security vulnerabilities")
                .with_input("file_path", ToolInputType::FilePath)
                .with_optional_input("scan_type", ToolInputType::String)
                .with_output("vulnerabilities", ToolInputType::Array)
                .with_output("severity", ToolInputType::String)
                .with_tag("security"),
        );

        // Code reviewer tool
        registry.register(
            AgentTool::from_role("code_reviewer")
                .with_name("code_review")
                .with_description("Review code for quality and best practices")
                .with_input("code", ToolInputType::Code)
                .with_optional_input("focus_area", ToolInputType::String)
                .with_output("suggestions", ToolInputType::Array)
                .with_output("score", ToolInputType::Integer)
                .with_tag("review"),
        );

        // Test writer tool
        registry.register(
            AgentTool::from_role("test_engineer")
                .with_name("test_writer")
                .with_description("Write unit tests for code")
                .with_input("function_code", ToolInputType::Code)
                .with_optional_input("test_framework", ToolInputType::String)
                .with_output("test_code", ToolInputType::Code)
                .with_output("coverage_estimate", ToolInputType::Float)
                .with_tag("testing"),
        );

        // Documentation tool
        registry.register(
            AgentTool::from_role("documentation_writer")
                .with_name("doc_generator")
                .with_description("Generate documentation for code")
                .with_input("code", ToolInputType::Code)
                .with_optional_input("format", ToolInputType::String)
                .with_output("documentation", ToolInputType::String)
                .with_tag("documentation"),
        );

        registry
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_tool_creation() {
        let tool = AgentTool::from_role("debugger")
            .with_name("Debug Tool")
            .with_description("Debug code issues")
            .with_input("error_message", ToolInputType::String)
            .with_output("root_cause", ToolInputType::String);

        assert_eq!(tool.agent_role, "debugger");
        assert_eq!(tool.inputs.len(), 1);
        assert_eq!(tool.outputs.len(), 1);
    }

    #[test]
    fn test_input_validation() {
        let tool = AgentTool::from_role("test")
            .with_input("required_str", ToolInputType::String)
            .with_optional_input("optional_int", ToolInputType::Integer);

        let mut inputs = HashMap::new();
        inputs.insert("required_str".to_string(), serde_json::json!("value"));

        assert!(tool.validate_inputs(&inputs).is_ok());

        let empty_inputs = HashMap::new();
        assert!(tool.validate_inputs(&empty_inputs).is_err());
    }

    #[test]
    fn test_prompt_building() {
        let tool = AgentTool::from_role("test")
            .with_description("Test task")
            .with_prompt_template("Analyze {{file}} for {{issue}}");

        let mut inputs = HashMap::new();
        inputs.insert("file".to_string(), serde_json::json!("main.rs"));
        inputs.insert("issue".to_string(), serde_json::json!("bugs"));

        let prompt = tool.build_prompt(&inputs);
        assert!(prompt.contains("main.rs"));
        assert!(prompt.contains("bugs"));
    }

    #[test]
    fn test_json_schema_generation() {
        let tool = AgentTool::from_role("test")
            .with_description("Test tool")
            .with_input("input1", ToolInputType::String)
            .with_optional_input("input2", ToolInputType::Integer);

        let schema = tool.to_json_schema();
        assert!(schema["name"].as_str().is_some());
        assert!(schema["parameters"]["properties"]["input1"].is_object());
    }

    #[test]
    fn test_tool_invocation() {
        let invocation = ToolInvocation::new("my_tool")
            .with_input("param1", serde_json::json!("value1"))
            .with_input("param2", serde_json::json!(42));

        assert_eq!(invocation.tool_id, "my_tool");
        assert_eq!(invocation.inputs.len(), 2);
    }

    #[test]
    fn test_tool_result() {
        let result = ToolResult::success("inv-1", "tool-1")
            .with_output("result", serde_json::json!("success"))
            .with_duration(1500)
            .with_iterations(2);

        assert!(result.success);
        assert_eq!(result.duration_ms, 1500);
        assert_eq!(result.iterations_used, Some(2));
    }

    #[test]
    fn test_tool_registry() {
        let registry = ToolRegistry::with_defaults();

        // Tools are registered by role ID, not by name
        assert!(registry.get("security_analyst").is_some());
        assert!(registry.get("code_reviewer").is_some());

        let security_tools = registry.find_by_tag("security");
        assert!(!security_tools.is_empty());
    }

    #[test]
    fn test_tool_alias() {
        let mut registry = ToolRegistry::new();
        registry.register(AgentTool::from_role("test").with_name("Original"));
        registry.add_alias("alias", "test");

        assert!(registry.get("test").is_some());
        assert!(registry.get("alias").is_some());
    }
}
