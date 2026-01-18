//! Execution Context Propagation Module
//!
//! Provides runtime context management for passing data between execution steps.
//! Supports expression syntax for variable substitution in prompts and configurations.
//!
//! Inspired by n8n's execution context propagation pattern.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, warn};

// ============================================================================
// Runtime Context
// ============================================================================

/// Runtime context for passing data between execution steps.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeContext {
    /// User-defined variables that can be referenced in expressions
    pub variables: HashMap<String, serde_json::Value>,
    /// Outputs from completed steps, indexed by step_id
    pub step_outputs: HashMap<String, StepOutput>,
    /// Current iteration number (1-based)
    pub iteration: u32,
    /// Task run ID for context
    pub task_run_id: Option<String>,
}

/// Output from a completed execution step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepOutput {
    /// Unique identifier for the step
    pub step_id: String,
    /// Human-readable name for the step
    pub step_name: String,
    /// The output value from this step
    pub output: serde_json::Value,
    /// Variables extracted from this step's output
    pub extracted_variables: HashMap<String, serde_json::Value>,
    /// Timestamp when step completed
    pub completed_at: String,
}

impl RuntimeContext {
    /// Create a new empty runtime context.
    pub fn new() -> Self {
        Self {
            variables: HashMap::new(),
            step_outputs: HashMap::new(),
            iteration: 1,
            task_run_id: None,
        }
    }

    /// Create a context with a task run ID.
    pub fn with_task_run_id(task_run_id: &str) -> Self {
        Self {
            task_run_id: Some(task_run_id.to_string()),
            ..Default::default()
        }
    }

    /// Set a variable in the context.
    pub fn set_variable(&mut self, name: &str, value: serde_json::Value) {
        debug!("Setting context variable: {} = {:?}", name, value);
        self.variables.insert(name.to_string(), value);
    }

    /// Get a variable from the context.
    pub fn get_variable(&self, name: &str) -> Option<&serde_json::Value> {
        self.variables.get(name)
    }

    /// Record the output of a completed step.
    pub fn record_step_output(&mut self, output: StepOutput) {
        debug!(
            "Recording step output: {} ({})",
            output.step_id, output.step_name
        );
        self.step_outputs.insert(output.step_id.clone(), output);
    }

    /// Get the output of a specific step.
    pub fn get_step_output(&self, step_id: &str) -> Option<&StepOutput> {
        self.step_outputs.get(step_id)
    }

    /// Get the most recent step output.
    pub fn get_previous_output(&self) -> Option<&StepOutput> {
        // Find the step with the latest timestamp
        self.step_outputs
            .values()
            .max_by_key(|s| &s.completed_at)
    }

    /// Increment the iteration counter.
    pub fn increment_iteration(&mut self) {
        self.iteration += 1;
        debug!("Incremented iteration to {}", self.iteration);
    }

    /// Set the iteration number.
    pub fn set_iteration(&mut self, iteration: u32) {
        self.iteration = iteration;
    }

    /// Serialize the context to JSON for storage.
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|e| format!("Failed to serialize context: {}", e))
    }

    /// Deserialize context from JSON.
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| format!("Failed to deserialize context: {}", e))
    }
}

// ============================================================================
// Expression Evaluator
// ============================================================================

/// Evaluates expressions in text and substitutes values from the runtime context.
///
/// Supported expression syntax:
/// - `{{variable_name}}` - Direct variable lookup
/// - `{{steps.step_id.output}}` - Output from a specific step
/// - `{{steps.step_id.output.field}}` - Field from step output (if JSON object)
/// - `{{$previous}}` - Output from the previous step
/// - `{{$previous.field}}` - Field from previous step output
/// - `{{$iteration}}` - Current iteration number
/// - `{{$task_run_id}}` - Current task run ID
pub struct ExpressionEvaluator {
    /// Pattern for matching expressions: {{...}}
    expression_pattern: Regex,
}

impl Default for ExpressionEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

impl ExpressionEvaluator {
    /// Create a new expression evaluator.
    pub fn new() -> Self {
        Self {
            // Match {{...}} patterns, capturing the inner content
            expression_pattern: Regex::new(r"\{\{([^}]+)\}\}").unwrap(),
        }
    }

    /// Evaluate all expressions in the given text using the provided context.
    ///
    /// Returns the text with all expressions replaced by their values.
    /// Unknown expressions are left unchanged with a warning logged.
    pub fn evaluate(&self, text: &str, context: &RuntimeContext) -> String {
        let mut result = text.to_string();
        let mut replacements = Vec::new();

        // Find all expressions and their replacements
        for cap in self.expression_pattern.captures_iter(text) {
            let full_match = cap.get(0).unwrap().as_str();
            let expression = cap.get(1).unwrap().as_str().trim();

            if let Some(value) = self.resolve_expression(expression, context) {
                replacements.push((full_match.to_string(), value));
            } else {
                warn!("Unable to resolve expression: {}", expression);
            }
        }

        // Apply replacements
        for (pattern, replacement) in replacements {
            result = result.replace(&pattern, &replacement);
        }

        result
    }

    /// Check if text contains any expressions.
    pub fn has_expressions(&self, text: &str) -> bool {
        self.expression_pattern.is_match(text)
    }

    /// List all expressions found in the text.
    pub fn find_expressions(&self, text: &str) -> Vec<String> {
        self.expression_pattern
            .captures_iter(text)
            .filter_map(|cap| cap.get(1).map(|m| m.as_str().trim().to_string()))
            .collect()
    }

    /// Resolve a single expression to its string value.
    fn resolve_expression(&self, expression: &str, context: &RuntimeContext) -> Option<String> {
        debug!("Resolving expression: {}", expression);

        // Handle special variables
        if expression.starts_with('$') {
            return self.resolve_special_variable(expression, context);
        }

        // Handle step outputs: steps.step_id.output[.field]
        if expression.starts_with("steps.") {
            return self.resolve_step_expression(expression, context);
        }

        // Handle direct variable lookup
        if let Some(value) = context.get_variable(expression) {
            return Some(self.value_to_string(value));
        }

        None
    }

    /// Resolve special variables ($iteration, $previous, $task_run_id).
    fn resolve_special_variable(
        &self,
        expression: &str,
        context: &RuntimeContext,
    ) -> Option<String> {
        if expression == "$iteration" {
            return Some(context.iteration.to_string());
        }

        if expression == "$task_run_id" {
            return context.task_run_id.clone();
        }

        if expression == "$previous" {
            if let Some(prev) = context.get_previous_output() {
                return Some(self.value_to_string(&prev.output));
            }
            return None;
        }

        if expression.starts_with("$previous.") {
            let field_path = &expression[10..]; // Skip "$previous."
            if let Some(prev) = context.get_previous_output() {
                return self.extract_field(&prev.output, field_path);
            }
            return None;
        }

        None
    }

    /// Resolve step output expressions (steps.step_id.output[.field]).
    fn resolve_step_expression(&self, expression: &str, context: &RuntimeContext) -> Option<String> {
        let parts: Vec<&str> = expression.splitn(4, '.').collect();

        if parts.len() < 3 {
            warn!("Invalid step expression format: {}", expression);
            return None;
        }

        let step_id = parts[1];
        let property = parts[2];

        let step_output = context.get_step_output(step_id)?;

        match property {
            "output" => {
                if parts.len() > 3 {
                    // Extract nested field: steps.step_id.output.field
                    self.extract_field(&step_output.output, parts[3])
                } else {
                    Some(self.value_to_string(&step_output.output))
                }
            }
            "step_name" => Some(step_output.step_name.clone()),
            "step_id" => Some(step_output.step_id.clone()),
            "completed_at" => Some(step_output.completed_at.clone()),
            _ => {
                // Try to get from extracted_variables
                step_output
                    .extracted_variables
                    .get(property)
                    .map(|v| self.value_to_string(v))
            }
        }
    }

    /// Extract a field from a JSON value using dot notation.
    fn extract_field(&self, value: &serde_json::Value, field_path: &str) -> Option<String> {
        let mut current = value;

        for part in field_path.split('.') {
            current = match current {
                serde_json::Value::Object(map) => map.get(part)?,
                serde_json::Value::Array(arr) => {
                    let index: usize = part.parse().ok()?;
                    arr.get(index)?
                }
                _ => return None,
            };
        }

        Some(self.value_to_string(current))
    }

    /// Convert a JSON value to a string representation.
    fn value_to_string(&self, value: &serde_json::Value) -> String {
        match value {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => "null".to_string(),
            // For arrays and objects, serialize to JSON
            _ => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
        }
    }
}

// ============================================================================
// Variable Extraction
// ============================================================================

/// Extract variables from AI output based on patterns.
///
/// Looks for patterns like:
/// - `VARIABLE_NAME: value`
/// - `[VARIABLE_NAME]: value`
/// - JSON objects with extractable fields
pub fn extract_variables_from_output(
    output: &str,
    patterns: Option<&[String]>,
) -> HashMap<String, serde_json::Value> {
    let mut variables = HashMap::new();

    // Try to extract variables using patterns
    if let Some(patterns) = patterns {
        for pattern in patterns {
            if let Some(value) = extract_pattern_value(output, pattern) {
                variables.insert(pattern.clone(), serde_json::Value::String(value));
            }
        }
    }

    // Try to extract [VARIABLE]: value patterns
    let bracket_pattern = Regex::new(r"\[([A-Z_]+)\]:\s*(.+)").unwrap();
    for cap in bracket_pattern.captures_iter(output) {
        let name = cap.get(1).unwrap().as_str();
        let value = cap.get(2).unwrap().as_str().trim();
        variables.insert(
            name.to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }

    // Try to extract VARIABLE_NAME: value patterns (all caps)
    let caps_pattern = Regex::new(r"^([A-Z][A-Z_]+):\s*(.+)$").unwrap();
    for line in output.lines() {
        if let Some(cap) = caps_pattern.captures(line.trim()) {
            let name = cap.get(1).unwrap().as_str();
            let value = cap.get(2).unwrap().as_str().trim();
            if !variables.contains_key(name) {
                variables.insert(
                    name.to_string(),
                    serde_json::Value::String(value.to_string()),
                );
            }
        }
    }

    variables
}

/// Extract a value matching a specific pattern from output.
fn extract_pattern_value(output: &str, pattern: &str) -> Option<String> {
    // Look for pattern followed by colon and value
    let regex_str = format!(r"{}:\s*(.+)", regex::escape(pattern));
    if let Ok(re) = Regex::new(&regex_str) {
        if let Some(cap) = re.captures(output) {
            return cap.get(1).map(|m| m.as_str().trim().to_string());
        }
    }
    None
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_variable_substitution() {
        let mut context = RuntimeContext::new();
        context.set_variable("name", serde_json::Value::String("Alice".to_string()));
        context.set_variable("count", serde_json::json!(42));

        let evaluator = ExpressionEvaluator::new();
        let result = evaluator.evaluate("Hello {{name}}, count is {{count}}", &context);

        assert_eq!(result, "Hello Alice, count is 42");
    }

    #[test]
    fn test_iteration_variable() {
        let mut context = RuntimeContext::new();
        context.set_iteration(5);

        let evaluator = ExpressionEvaluator::new();
        let result = evaluator.evaluate("This is iteration {{$iteration}}", &context);

        assert_eq!(result, "This is iteration 5");
    }

    #[test]
    fn test_step_output_reference() {
        let mut context = RuntimeContext::new();
        context.record_step_output(StepOutput {
            step_id: "step1".to_string(),
            step_name: "First Step".to_string(),
            output: serde_json::json!({"result": "success", "data": {"value": 123}}),
            extracted_variables: HashMap::new(),
            completed_at: "2024-01-01T00:00:00Z".to_string(),
        });

        let evaluator = ExpressionEvaluator::new();

        // Test full output
        let result = evaluator.evaluate("Step output: {{steps.step1.output}}", &context);
        assert!(result.contains("success"));

        // Test nested field
        let result = evaluator.evaluate("Value: {{steps.step1.output.data.value}}", &context);
        assert_eq!(result, "Value: 123");
    }

    #[test]
    fn test_previous_output() {
        let mut context = RuntimeContext::new();
        context.record_step_output(StepOutput {
            step_id: "step1".to_string(),
            step_name: "First".to_string(),
            output: serde_json::json!("first output"),
            extracted_variables: HashMap::new(),
            completed_at: "2024-01-01T00:00:00Z".to_string(),
        });
        context.record_step_output(StepOutput {
            step_id: "step2".to_string(),
            step_name: "Second".to_string(),
            output: serde_json::json!({"status": "done"}),
            extracted_variables: HashMap::new(),
            completed_at: "2024-01-01T00:01:00Z".to_string(),
        });

        let evaluator = ExpressionEvaluator::new();
        let result = evaluator.evaluate("Previous status: {{$previous.status}}", &context);

        assert_eq!(result, "Previous status: done");
    }

    #[test]
    fn test_find_expressions() {
        let evaluator = ExpressionEvaluator::new();
        let expressions = evaluator.find_expressions("Hello {{name}}, {{$iteration}} times");

        assert_eq!(expressions.len(), 2);
        assert!(expressions.contains(&"name".to_string()));
        assert!(expressions.contains(&"$iteration".to_string()));
    }

    #[test]
    fn test_extract_variables_from_output() {
        let output = r#"
Processing complete.
[STATUS]: success
[COUNT]: 42
RESULT: all tests passed
"#;

        let variables = extract_variables_from_output(output, None);

        assert_eq!(
            variables.get("STATUS"),
            Some(&serde_json::Value::String("success".to_string()))
        );
        assert_eq!(
            variables.get("COUNT"),
            Some(&serde_json::Value::String("42".to_string()))
        );
        assert_eq!(
            variables.get("RESULT"),
            Some(&serde_json::Value::String("all tests passed".to_string()))
        );
    }
}
