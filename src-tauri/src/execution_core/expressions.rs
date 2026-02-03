//! Expression Evaluation
//!
//! Provides expression evaluation for variable resolution in flows.
//! Supports dot notation, array access, and simple transformations.

use std::collections::HashMap;

/// Evaluate a simple expression against a context.
///
/// Supports:
/// - Variable references: `foo`, `foo.bar`, `foo.0` (array index)
/// - Context prefix: `context.foo` (removes prefix)
/// - Literal fallback: returns the expression as a string if not found
///
/// # Examples
///
/// ```ignore
/// let mut context = HashMap::new();
/// context.insert("name".to_string(), json!("Alice"));
/// context.insert("user".to_string(), json!({"name": "Bob", "age": 30}));
///
/// assert_eq!(evaluate_expression("name", &context), json!("Alice"));
/// assert_eq!(evaluate_expression("user.name", &context), json!("Bob"));
/// assert_eq!(evaluate_expression("user.age", &context), json!(30));
/// assert_eq!(evaluate_expression("unknown", &context), json!("unknown"));
/// ```
pub fn evaluate_expression(
    expression: &str,
    context: &HashMap<String, serde_json::Value>,
) -> serde_json::Value {
    let expression = expression.trim();

    // Handle "context." prefix (strip it)
    let key = if let Some(stripped) = expression.strip_prefix("context.") {
        stripped
    } else {
        expression
    };

    // Try to get value from context
    if let Some(value) = get_value_by_path(key, context) {
        return value;
    }

    // Return expression as literal string if not found
    serde_json::json!(expression)
}

/// Get a value from context by path, supporting dot notation.
///
/// Supports:
/// - Simple keys: `foo`
/// - Nested objects: `foo.bar.baz`
/// - Array indices: `items.0`, `data.items.2.name`
pub fn get_value_by_path(
    path: &str,
    context: &HashMap<String, serde_json::Value>,
) -> Option<serde_json::Value> {
    // First try direct lookup
    if let Some(value) = context.get(path) {
        return Some(value.clone());
    }

    // No dots means no nested access needed
    if !path.contains('.') {
        return None;
    }

    // Split path and traverse
    let parts: Vec<&str> = path.split('.').collect();
    let first = parts.first()?;

    let mut value = context.get(*first)?.clone();

    for part in parts.iter().skip(1) {
        value = get_nested_value(&value, part)?;
    }

    Some(value)
}

/// Get a nested value from a JSON value by key or index.
fn get_nested_value(value: &serde_json::Value, key: &str) -> Option<serde_json::Value> {
    match value {
        serde_json::Value::Object(obj) => obj.get(key).cloned(),
        serde_json::Value::Array(arr) => {
            // Try to parse as array index
            let idx: usize = key.parse().ok()?;
            arr.get(idx).cloned()
        }
        _ => None,
    }
}

/// Resolve all variable references in a string template.
///
/// Template syntax: `${variable.path}` or `{{variable.path}}`
///
/// # Examples
///
/// ```ignore
/// let mut context = HashMap::new();
/// context.insert("name".to_string(), json!("Alice"));
/// context.insert("count".to_string(), json!(5));
///
/// let template = "Hello, ${name}! You have {{count}} messages.";
/// let result = resolve_template(template, &context);
/// assert_eq!(result, "Hello, Alice! You have 5 messages.");
/// ```
pub fn resolve_template(
    template: &str,
    context: &HashMap<String, serde_json::Value>,
) -> String {
    let mut result = template.to_string();

    // Resolve ${...} syntax
    let re_dollar = regex::Regex::new(r"\$\{([^}]+)\}").unwrap();
    for cap in re_dollar.captures_iter(template) {
        let full_match = &cap[0];
        let var_name = cap[1].trim();
        let value = evaluate_expression(var_name, context);
        let value_str = value_to_string(&value);
        result = result.replace(full_match, &value_str);
    }

    // Resolve {{...}} syntax
    let re_mustache = regex::Regex::new(r"\{\{([^}]+)\}\}").unwrap();
    for cap in re_mustache.captures_iter(template) {
        let full_match = &cap[0];
        let var_name = cap[1].trim();
        let value = evaluate_expression(var_name, context);
        let value_str = value_to_string(&value);
        result = result.replace(full_match, &value_str);
    }

    result
}

/// Convert a JSON value to a string for template interpolation.
fn value_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        // For objects and arrays, use JSON string representation
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

/// Resolve input mappings from context.
///
/// Each mapping is a key-value pair where:
/// - Key: the target input parameter name
/// - Value: a path to resolve from context
///
/// # Example
///
/// ```ignore
/// let mut mappings = HashMap::new();
/// mappings.insert("input1".to_string(), "step1.output".to_string());
/// mappings.insert("input2".to_string(), "config.mode".to_string());
///
/// let mut context = HashMap::new();
/// context.insert("step1".to_string(), json!({"output": "data"}));
/// context.insert("config".to_string(), json!({"mode": "fast"}));
///
/// let resolved = resolve_input_mappings(&mappings, &context);
/// assert_eq!(resolved.get("input1"), Some(&json!("data")));
/// assert_eq!(resolved.get("input2"), Some(&json!("fast")));
/// ```
pub fn resolve_input_mappings(
    mappings: &HashMap<String, String>,
    context: &HashMap<String, serde_json::Value>,
) -> HashMap<String, serde_json::Value> {
    let mut resolved = HashMap::new();

    for (target_key, source_path) in mappings {
        if let Some(value) = get_value_by_path(source_path, context) {
            resolved.insert(target_key.clone(), value);
        }
    }

    resolved
}

/// Merge outputs into context with a step prefix.
///
/// For example, if step_id is "step1" and outputs contains {"result": "data"},
/// the context will have "step1.result" = "data".
pub fn merge_outputs_to_context(
    step_id: &str,
    outputs: &HashMap<String, serde_json::Value>,
    context: &mut HashMap<String, serde_json::Value>,
) {
    for (key, value) in outputs {
        let context_key = format!("{}.{}", step_id, key);
        context.insert(context_key, value.clone());
    }
}

/// Extract outputs from a context by step prefix.
///
/// For example, if context has "step1.result" = "data" and "step1.count" = 5,
/// calling with step_id "step1" returns {"result": "data", "count": 5}.
pub fn extract_step_outputs(
    step_id: &str,
    context: &HashMap<String, serde_json::Value>,
) -> HashMap<String, serde_json::Value> {
    let prefix = format!("{}.", step_id);
    let mut outputs = HashMap::new();

    for (key, value) in context {
        if let Some(output_key) = key.strip_prefix(&prefix) {
            outputs.insert(output_key.to_string(), value.clone());
        }
    }

    outputs
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_evaluate_simple() {
        let mut context = HashMap::new();
        context.insert("name".to_string(), json!("Alice"));

        assert_eq!(evaluate_expression("name", &context), json!("Alice"));
    }

    #[test]
    fn test_evaluate_nested() {
        let mut context = HashMap::new();
        context.insert("user".to_string(), json!({"name": "Bob", "age": 30}));

        assert_eq!(evaluate_expression("user.name", &context), json!("Bob"));
        assert_eq!(evaluate_expression("user.age", &context), json!(30));
    }

    #[test]
    fn test_evaluate_array_index() {
        let mut context = HashMap::new();
        context.insert("items".to_string(), json!(["a", "b", "c"]));

        assert_eq!(evaluate_expression("items.0", &context), json!("a"));
        assert_eq!(evaluate_expression("items.2", &context), json!("c"));
    }

    #[test]
    fn test_evaluate_context_prefix() {
        let mut context = HashMap::new();
        context.insert("foo".to_string(), json!("bar"));

        assert_eq!(evaluate_expression("context.foo", &context), json!("bar"));
    }

    #[test]
    fn test_evaluate_not_found() {
        let context = HashMap::new();

        // Returns the expression as a string literal
        assert_eq!(evaluate_expression("unknown", &context), json!("unknown"));
    }

    #[test]
    fn test_resolve_template_dollar() {
        let mut context = HashMap::new();
        context.insert("name".to_string(), json!("Alice"));
        context.insert("count".to_string(), json!(5));

        let template = "Hello, ${name}! You have ${count} messages.";
        let result = resolve_template(template, &context);
        assert_eq!(result, "Hello, Alice! You have 5 messages.");
    }

    #[test]
    fn test_resolve_template_mustache() {
        let mut context = HashMap::new();
        context.insert("name".to_string(), json!("Bob"));

        let template = "Welcome, {{name}}!";
        let result = resolve_template(template, &context);
        assert_eq!(result, "Welcome, Bob!");
    }

    #[test]
    fn test_resolve_input_mappings() {
        let mut mappings = HashMap::new();
        mappings.insert("input1".to_string(), "step1.output".to_string());
        mappings.insert("input2".to_string(), "config.mode".to_string());

        let mut context = HashMap::new();
        context.insert("step1".to_string(), json!({"output": "data"}));
        context.insert("config".to_string(), json!({"mode": "fast"}));

        let resolved = resolve_input_mappings(&mappings, &context);
        assert_eq!(resolved.get("input1"), Some(&json!("data")));
        assert_eq!(resolved.get("input2"), Some(&json!("fast")));
    }

    #[test]
    fn test_merge_outputs_to_context() {
        let mut context = HashMap::new();
        let mut outputs = HashMap::new();
        outputs.insert("result".to_string(), json!("success"));
        outputs.insert("count".to_string(), json!(10));

        merge_outputs_to_context("step1", &outputs, &mut context);

        assert_eq!(context.get("step1.result"), Some(&json!("success")));
        assert_eq!(context.get("step1.count"), Some(&json!(10)));
    }

    #[test]
    fn test_extract_step_outputs() {
        let mut context = HashMap::new();
        context.insert("step1.result".to_string(), json!("data"));
        context.insert("step1.count".to_string(), json!(5));
        context.insert("step2.output".to_string(), json!("other"));

        let outputs = extract_step_outputs("step1", &context);
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs.get("result"), Some(&json!("data")));
        assert_eq!(outputs.get("count"), Some(&json!(5)));
    }
}
