use serde_json::Value;
use std::collections::HashMap;

/// Stores outputs from completed DAG nodes and resolves `$nodeId.field` references.
#[derive(Debug, Clone, Default)]
pub struct VariableStore {
    /// Map from node_id to its output Value.
    node_outputs: HashMap<String, Value>,
}

impl VariableStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a node's output.
    pub fn set_output(&mut self, node_id: &str, output: Value) {
        self.node_outputs.insert(node_id.to_string(), output);
    }

    /// Get a node's raw output.
    pub fn get_output(&self, node_id: &str) -> Option<&Value> {
        self.node_outputs.get(node_id)
    }

    /// Resolve a `$nodeId.field.path` reference to a Value.
    /// Returns None if the node or path doesn't exist.
    pub fn resolve_reference(&self, reference: &str) -> Option<Value> {
        let reference = reference.strip_prefix('$').unwrap_or(reference);
        let mut parts = reference.splitn(2, '.');
        let node_id = parts.next()?;
        let output = self.node_outputs.get(node_id)?;

        match parts.next() {
            None => Some(output.clone()),
            Some(path) => {
                // Walk the JSON path: "field.subfield.0"
                let mut current = output;
                for segment in path.split('.') {
                    current = if let Ok(idx) = segment.parse::<usize>() {
                        current.get(idx)?
                    } else {
                        current.get(segment)?
                    };
                }
                Some(current.clone())
            }
        }
    }

    /// Resolve all inputs in a map, returning resolved values.
    pub fn resolve_inputs(&self, inputs: &HashMap<String, String>) -> HashMap<String, Value> {
        let mut resolved = HashMap::new();
        for (name, reference) in inputs {
            if let Some(val) = self.resolve_reference(reference) {
                resolved.insert(name.clone(), val);
            }
        }
        resolved
    }

    /// Evaluate a simple condition expression like "$nodeId.output == 'pass'".
    /// Supports: ==, !=, basic boolean evaluation.
    /// Returns true if the condition evaluates to true, false otherwise.
    /// Returns true for unparseable conditions (fail-open for forward compat).
    pub fn evaluate_condition(&self, condition: &str) -> bool {
        // Handle == and !=
        for op in &["!=", "=="] {
            if let Some(idx) = condition.find(op) {
                let lhs_raw = condition[..idx].trim();
                let rhs_raw = condition[idx + op.len()..].trim();

                let lhs = self.resolve_to_string(lhs_raw);
                let rhs = self.resolve_to_string(rhs_raw);

                return if *op == "==" { lhs == rhs } else { lhs != rhs };
            }
        }

        // Handle || (OR)
        if condition.contains("||") {
            return condition
                .split("||")
                .any(|part| self.evaluate_condition(part.trim()));
        }

        // Handle && (AND)
        if condition.contains("&&") {
            return condition
                .split("&&")
                .all(|part| self.evaluate_condition(part.trim()));
        }

        // Single value: resolve and check truthiness
        let val = self.resolve_to_string(condition);
        !val.is_empty() && val != "false" && val != "null" && val != "0"
    }

    /// Resolve a token to a string value. Handles $references and string literals.
    fn resolve_to_string(&self, token: &str) -> String {
        let token = token.trim().trim_matches('\'').trim_matches('"');
        if token.starts_with('$') {
            self.resolve_reference(token)
                .map(|v| match &v {
                    Value::String(s) => s.clone(),
                    Value::Null => "null".to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::Number(n) => n.to_string(),
                    other => other.to_string(),
                })
                .unwrap_or_default()
        } else {
            token.to_string()
        }
    }

    /// Perform variable substitution in a string template.
    /// Replaces `$nodeId` with resolved values inline.
    pub fn substitute_in_string(&self, template: &str) -> String {
        let mut result = template.to_string();
        // Find all $word.word patterns and replace
        // Simple approach: iterate through node outputs and replace matching patterns
        for (node_id, output) in &self.node_outputs {
            let pattern = format!("${}", node_id);
            if result.contains(&pattern) {
                let replacement = match output {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                // Replace $nodeId (no field) with the whole output
                result = result.replace(&format!("${}", node_id), &replacement);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_set_and_get_output() {
        let mut store = VariableStore::new();
        store.set_output("nodeA", json!({"status": "pass"}));
        assert!(store.get_output("nodeA").is_some());
        assert!(store.get_output("nodeB").is_none());
    }

    #[test]
    fn test_resolve_reference_no_path() {
        let mut store = VariableStore::new();
        store.set_output("nodeA", json!("hello"));
        let val = store.resolve_reference("$nodeA");
        assert_eq!(val, Some(json!("hello")));
    }

    #[test]
    fn test_resolve_reference_with_path() {
        let mut store = VariableStore::new();
        store.set_output("nodeA", json!({"result": {"score": 42}}));
        let val = store.resolve_reference("$nodeA.result.score");
        assert_eq!(val, Some(json!(42)));
    }

    #[test]
    fn test_resolve_reference_array_index() {
        let mut store = VariableStore::new();
        store.set_output("nodeA", json!({"items": ["a", "b", "c"]}));
        let val = store.resolve_reference("$nodeA.items.1");
        assert_eq!(val, Some(json!("b")));
    }

    #[test]
    fn test_evaluate_condition_equality() {
        let mut store = VariableStore::new();
        store.set_output("nodeA", json!({"status": "pass"}));
        assert!(store.evaluate_condition("$nodeA.status == 'pass'"));
        assert!(!store.evaluate_condition("$nodeA.status == 'fail'"));
    }

    #[test]
    fn test_evaluate_condition_inequality() {
        let mut store = VariableStore::new();
        store.set_output("nodeA", json!({"status": "fail"}));
        assert!(store.evaluate_condition("$nodeA.status != 'pass'"));
        assert!(!store.evaluate_condition("$nodeA.status != 'fail'"));
    }

    #[test]
    fn test_evaluate_condition_truthiness() {
        let mut store = VariableStore::new();
        store.set_output("nodeA", json!({"ready": true}));
        assert!(store.evaluate_condition("$nodeA.ready"));
    }

    #[test]
    fn test_substitute_in_string() {
        let mut store = VariableStore::new();
        store.set_output("nodeA", json!("world"));
        let result = store.substitute_in_string("Hello $nodeA!");
        assert_eq!(result, "Hello world!");
    }

    #[test]
    fn test_resolve_inputs() {
        let mut store = VariableStore::new();
        store.set_output("nodeA", json!({"score": 99}));
        let inputs: HashMap<String, String> =
            [("my_score".to_string(), "$nodeA.score".to_string())]
                .into_iter()
                .collect();
        let resolved = store.resolve_inputs(&inputs);
        assert_eq!(resolved.get("my_score"), Some(&json!(99)));
    }
}
