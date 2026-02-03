//! Condition Evaluation
//!
//! Provides condition types and evaluation logic for flow control.
//! These conditions are used by both Flow Designer's Conditional steps
//! and Unified Workflow's verification criteria.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A condition for flow control.
///
/// Conditions are evaluated against a context (HashMap of values) and return
/// a boolean result. They can be composed using logical operators (And, Or, Not).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FlowCondition {
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
    /// Check if a value is greater than or equal.
    GreaterThanOrEqual { left: String, right: f64 },
    /// Check if a value is less than.
    LessThan { left: String, right: f64 },
    /// Check if a value is less than or equal.
    LessThanOrEqual { left: String, right: f64 },
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
    /// Check if a string starts with a prefix.
    StartsWith { value: String, prefix: String },
    /// Check if a string ends with a suffix.
    EndsWith { value: String, suffix: String },
    /// Check if a string matches a regex pattern.
    Matches { value: String, pattern: String },
    /// Check if an array contains an element.
    ArrayContains { array: String, element: serde_json::Value },
    /// Check if an array is empty.
    ArrayEmpty { array: String },
    /// Check if array length meets a condition.
    ArrayLength { array: String, operator: ComparisonOp, value: usize },
    /// Logical AND of conditions.
    And { conditions: Vec<FlowCondition> },
    /// Logical OR of conditions.
    Or { conditions: Vec<FlowCondition> },
    /// Logical NOT of a condition.
    Not { condition: Box<FlowCondition> },
    /// Custom expression (evaluated as JavaScript-like expression).
    Expression { expr: String },
    /// Always true (useful for default branches).
    Always,
    /// Always false (useful for disabled branches).
    Never,
}

/// Comparison operators for numeric comparisons.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonOp {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
}

impl FlowCondition {
    // ========================================================================
    // Constructor Helpers
    // ========================================================================

    /// Create an equals condition.
    pub fn equals(left: impl Into<String>, right: serde_json::Value) -> Self {
        Self::Equals {
            left: left.into(),
            right,
        }
    }

    /// Create a not-equals condition.
    pub fn not_equals(left: impl Into<String>, right: serde_json::Value) -> Self {
        Self::NotEquals {
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

    /// Create an is_null condition.
    pub fn is_null(value: impl Into<String>) -> Self {
        Self::IsNull {
            value: value.into(),
        }
    }

    /// Create an is_not_null condition.
    pub fn is_not_null(value: impl Into<String>) -> Self {
        Self::IsNotNull {
            value: value.into(),
        }
    }

    /// Create a contains condition.
    pub fn contains(value: impl Into<String>, substring: impl Into<String>) -> Self {
        Self::Contains {
            value: value.into(),
            substring: substring.into(),
        }
    }

    /// Create a greater_than condition.
    pub fn greater_than(left: impl Into<String>, right: f64) -> Self {
        Self::GreaterThan {
            left: left.into(),
            right,
        }
    }

    /// Create a less_than condition.
    pub fn less_than(left: impl Into<String>, right: f64) -> Self {
        Self::LessThan {
            left: left.into(),
            right,
        }
    }

    /// Create an AND condition.
    pub fn and(conditions: Vec<FlowCondition>) -> Self {
        Self::And { conditions }
    }

    /// Create an OR condition.
    pub fn or(conditions: Vec<FlowCondition>) -> Self {
        Self::Or { conditions }
    }

    /// Create a NOT condition.
    pub fn not(condition: FlowCondition) -> Self {
        Self::Not {
            condition: Box::new(condition),
        }
    }

    // ========================================================================
    // Evaluation
    // ========================================================================

    /// Evaluate condition against context.
    ///
    /// The context is a HashMap where keys are variable names and values
    /// are JSON values. Dot notation (e.g., "user.name") is supported for
    /// accessing nested values.
    pub fn evaluate(&self, context: &HashMap<String, serde_json::Value>) -> bool {
        match self {
            Self::Equals { left, right } => {
                get_value_from_context(left, context).as_ref() == Some(right)
            }
            Self::NotEquals { left, right } => {
                get_value_from_context(left, context).as_ref() != Some(right)
            }
            Self::GreaterThan { left, right } => {
                get_value_from_context(left, context)
                    .and_then(|v| v.as_f64())
                    .map(|v| v > *right)
                    .unwrap_or(false)
            }
            Self::GreaterThanOrEqual { left, right } => {
                get_value_from_context(left, context)
                    .and_then(|v| v.as_f64())
                    .map(|v| v >= *right)
                    .unwrap_or(false)
            }
            Self::LessThan { left, right } => {
                get_value_from_context(left, context)
                    .and_then(|v| v.as_f64())
                    .map(|v| v < *right)
                    .unwrap_or(false)
            }
            Self::LessThanOrEqual { left, right } => {
                get_value_from_context(left, context)
                    .and_then(|v| v.as_f64())
                    .map(|v| v <= *right)
                    .unwrap_or(false)
            }
            Self::IsTrue { value } => {
                get_value_from_context(value, context)
                    .map(|v| is_truthy(&v))
                    .unwrap_or(false)
            }
            Self::IsFalse { value } => {
                get_value_from_context(value, context)
                    .map(|v| !is_truthy(&v))
                    .unwrap_or(true)
            }
            Self::IsNull { value } => {
                get_value_from_context(value, context)
                    .map(|v| v.is_null())
                    .unwrap_or(true)
            }
            Self::IsNotNull { value } => {
                get_value_from_context(value, context)
                    .map(|v| !v.is_null())
                    .unwrap_or(false)
            }
            Self::Contains { value, substring } => {
                get_value_from_context(value, context)
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .map(|s| s.contains(substring))
                    .unwrap_or(false)
            }
            Self::StartsWith { value, prefix } => {
                get_value_from_context(value, context)
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .map(|s| s.starts_with(prefix))
                    .unwrap_or(false)
            }
            Self::EndsWith { value, suffix } => {
                get_value_from_context(value, context)
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .map(|s| s.ends_with(suffix))
                    .unwrap_or(false)
            }
            Self::Matches { value, pattern } => {
                let val = get_value_from_context(value, context)
                    .and_then(|v| v.as_str().map(|s| s.to_string()));

                if let Some(s) = val {
                    regex::Regex::new(pattern)
                        .map(|re| re.is_match(&s))
                        .unwrap_or(false)
                } else {
                    false
                }
            }
            Self::ArrayContains { array, element } => {
                get_value_from_context(array, context)
                    .and_then(|v| v.as_array().cloned())
                    .map(|arr| arr.contains(element))
                    .unwrap_or(false)
            }
            Self::ArrayEmpty { array } => {
                get_value_from_context(array, context)
                    .and_then(|v| v.as_array().cloned())
                    .map(|arr| arr.is_empty())
                    .unwrap_or(true)
            }
            Self::ArrayLength { array, operator, value } => {
                get_value_from_context(array, context)
                    .and_then(|v| v.as_array().cloned())
                    .map(|arr| {
                        let len = arr.len();
                        match operator {
                            ComparisonOp::Eq => len == *value,
                            ComparisonOp::Ne => len != *value,
                            ComparisonOp::Gt => len > *value,
                            ComparisonOp::Gte => len >= *value,
                            ComparisonOp::Lt => len < *value,
                            ComparisonOp::Lte => len <= *value,
                        }
                    })
                    .unwrap_or(false)
            }
            Self::And { conditions } => conditions.iter().all(|c| c.evaluate(context)),
            Self::Or { conditions } => conditions.iter().any(|c| c.evaluate(context)),
            Self::Not { condition } => !condition.evaluate(context),
            Self::Expression { expr } => evaluate_expression(expr, context),
            Self::Always => true,
            Self::Never => false,
        }
    }

    /// Get a human-readable description of this condition.
    pub fn description(&self) -> String {
        match self {
            Self::Equals { left, right } => format!("{} == {}", left, right),
            Self::NotEquals { left, right } => format!("{} != {}", left, right),
            Self::GreaterThan { left, right } => format!("{} > {}", left, right),
            Self::GreaterThanOrEqual { left, right } => format!("{} >= {}", left, right),
            Self::LessThan { left, right } => format!("{} < {}", left, right),
            Self::LessThanOrEqual { left, right } => format!("{} <= {}", left, right),
            Self::IsTrue { value } => format!("{} is true", value),
            Self::IsFalse { value } => format!("{} is false", value),
            Self::IsNull { value } => format!("{} is null", value),
            Self::IsNotNull { value } => format!("{} is not null", value),
            Self::Contains { value, substring } => format!("{} contains '{}'", value, substring),
            Self::StartsWith { value, prefix } => format!("{} starts with '{}'", value, prefix),
            Self::EndsWith { value, suffix } => format!("{} ends with '{}'", value, suffix),
            Self::Matches { value, pattern } => format!("{} matches /{}/", value, pattern),
            Self::ArrayContains { array, element } => format!("{} contains {}", array, element),
            Self::ArrayEmpty { array } => format!("{} is empty", array),
            Self::ArrayLength { array, operator, value } => {
                let op = match operator {
                    ComparisonOp::Eq => "==",
                    ComparisonOp::Ne => "!=",
                    ComparisonOp::Gt => ">",
                    ComparisonOp::Gte => ">=",
                    ComparisonOp::Lt => "<",
                    ComparisonOp::Lte => "<=",
                };
                format!("length({}) {} {}", array, op, value)
            }
            Self::And { conditions } => {
                let descs: Vec<String> = conditions.iter().map(|c| c.description()).collect();
                format!("({})", descs.join(" AND "))
            }
            Self::Or { conditions } => {
                let descs: Vec<String> = conditions.iter().map(|c| c.description()).collect();
                format!("({})", descs.join(" OR "))
            }
            Self::Not { condition } => format!("NOT ({})", condition.description()),
            Self::Expression { expr } => format!("expr: {}", expr),
            Self::Always => "always".to_string(),
            Self::Never => "never".to_string(),
        }
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Get a value from context, supporting dot notation for nested access.
pub fn get_value_from_context(
    key: &str,
    context: &HashMap<String, serde_json::Value>,
) -> Option<serde_json::Value> {
    // First try direct lookup
    if let Some(value) = context.get(key) {
        return Some(value.clone());
    }

    // Try dot notation access
    if key.contains('.') {
        let parts: Vec<&str> = key.split('.').collect();
        if let Some(first) = parts.first() {
            if let Some(mut value) = context.get(*first).cloned() {
                for part in parts.iter().skip(1) {
                    if let Some(obj) = value.as_object() {
                        if let Some(v) = obj.get(*part) {
                            value = v.clone();
                        } else {
                            return None;
                        }
                    } else if let Some(arr) = value.as_array() {
                        // Support array index access like "items.0"
                        if let Ok(idx) = part.parse::<usize>() {
                            if let Some(v) = arr.get(idx) {
                                value = v.clone();
                            } else {
                                return None;
                            }
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    }
                }
                return Some(value);
            }
        }
    }

    None
}

/// Check if a JSON value is truthy.
///
/// Truthy values: true, non-zero numbers, non-empty strings, non-empty arrays, non-null objects
/// Falsy values: false, 0, null, empty string, empty array
fn is_truthy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Array(arr) => !arr.is_empty(),
        serde_json::Value::Object(_) => true,
    }
}

/// Evaluate a simple expression.
///
/// Supports basic patterns:
/// - Variable references: `foo`, `foo.bar`
/// - Comparisons: `foo == "bar"`, `count > 5`
/// - Boolean literals: `true`, `false`
fn evaluate_expression(expr: &str, context: &HashMap<String, serde_json::Value>) -> bool {
    let expr = expr.trim();

    // Boolean literals
    if expr == "true" {
        return true;
    }
    if expr == "false" {
        return false;
    }

    // Simple equality: foo == "bar" or foo == 123
    if let Some((left, right)) = expr.split_once("==") {
        let left = left.trim();
        let right = right.trim();
        let left_val = get_value_from_context(left, context);
        let right_val = parse_literal(right);
        return left_val == right_val;
    }

    // Simple inequality: foo != "bar"
    if let Some((left, right)) = expr.split_once("!=") {
        let left = left.trim();
        let right = right.trim();
        let left_val = get_value_from_context(left, context);
        let right_val = parse_literal(right);
        return left_val != right_val;
    }

    // Greater than
    if let Some((left, right)) = expr.split_once('>') {
        if !left.ends_with('=') {
            let left = left.trim();
            let right = right.trim();
            let left_val = get_value_from_context(left, context)
                .and_then(|v| v.as_f64());
            let right_val = right.parse::<f64>().ok();
            if let (Some(l), Some(r)) = (left_val, right_val) {
                return l > r;
            }
        }
    }

    // Less than
    if let Some((left, right)) = expr.split_once('<') {
        if !left.ends_with('=') {
            let left = left.trim();
            let right = right.trim();
            let left_val = get_value_from_context(left, context)
                .and_then(|v| v.as_f64());
            let right_val = right.parse::<f64>().ok();
            if let (Some(l), Some(r)) = (left_val, right_val) {
                return l < r;
            }
        }
    }

    // Simple variable reference (truthy check)
    if let Some(val) = get_value_from_context(expr, context) {
        return is_truthy(&val);
    }

    // Default to true for unrecognized expressions (backwards compat)
    true
}

/// Parse a literal value from a string.
fn parse_literal(s: &str) -> Option<serde_json::Value> {
    let s = s.trim();

    // String literal
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        return Some(serde_json::Value::String(s[1..s.len() - 1].to_string()));
    }

    // Boolean
    if s == "true" {
        return Some(serde_json::Value::Bool(true));
    }
    if s == "false" {
        return Some(serde_json::Value::Bool(false));
    }

    // Null
    if s == "null" {
        return Some(serde_json::Value::Null);
    }

    // Number
    if let Ok(n) = s.parse::<i64>() {
        return Some(serde_json::json!(n));
    }
    if let Ok(n) = s.parse::<f64>() {
        return Some(serde_json::json!(n));
    }

    // Try JSON parse
    serde_json::from_str(s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_equals() {
        let mut context = HashMap::new();
        context.insert("name".to_string(), json!("Alice"));

        assert!(FlowCondition::equals("name", json!("Alice")).evaluate(&context));
        assert!(!FlowCondition::equals("name", json!("Bob")).evaluate(&context));
    }

    #[test]
    fn test_comparison() {
        let mut context = HashMap::new();
        context.insert("count".to_string(), json!(10));

        assert!(FlowCondition::greater_than("count", 5.0).evaluate(&context));
        assert!(!FlowCondition::greater_than("count", 15.0).evaluate(&context));
        assert!(FlowCondition::less_than("count", 15.0).evaluate(&context));
    }

    #[test]
    fn test_nested_access() {
        let mut context = HashMap::new();
        context.insert("user".to_string(), json!({"name": "Alice", "age": 30}));

        let result = get_value_from_context("user.name", &context);
        assert_eq!(result, Some(json!("Alice")));

        let result = get_value_from_context("user.age", &context);
        assert_eq!(result, Some(json!(30)));
    }

    #[test]
    fn test_array_index_access() {
        let mut context = HashMap::new();
        context.insert("items".to_string(), json!(["a", "b", "c"]));

        let result = get_value_from_context("items.0", &context);
        assert_eq!(result, Some(json!("a")));

        let result = get_value_from_context("items.2", &context);
        assert_eq!(result, Some(json!("c")));
    }

    #[test]
    fn test_logical_operators() {
        let mut context = HashMap::new();
        context.insert("a".to_string(), json!(true));
        context.insert("b".to_string(), json!(false));

        let and_cond = FlowCondition::and(vec![
            FlowCondition::is_true("a"),
            FlowCondition::is_true("b"),
        ]);
        assert!(!and_cond.evaluate(&context));

        let or_cond = FlowCondition::or(vec![
            FlowCondition::is_true("a"),
            FlowCondition::is_true("b"),
        ]);
        assert!(or_cond.evaluate(&context));

        let not_cond = FlowCondition::not(FlowCondition::is_true("b"));
        assert!(not_cond.evaluate(&context));
    }

    #[test]
    fn test_is_truthy() {
        assert!(is_truthy(&json!(true)));
        assert!(!is_truthy(&json!(false)));
        assert!(is_truthy(&json!(1)));
        assert!(!is_truthy(&json!(0)));
        assert!(is_truthy(&json!("hello")));
        assert!(!is_truthy(&json!("")));
        assert!(is_truthy(&json!([1, 2, 3])));
        assert!(!is_truthy(&json!([])));
        assert!(!is_truthy(&serde_json::Value::Null));
    }

    #[test]
    fn test_array_conditions() {
        let mut context = HashMap::new();
        context.insert("items".to_string(), json!(["a", "b", "c"]));
        context.insert("empty".to_string(), json!([]));

        assert!(FlowCondition::ArrayContains {
            array: "items".to_string(),
            element: json!("b"),
        }
        .evaluate(&context));

        assert!(!FlowCondition::ArrayContains {
            array: "items".to_string(),
            element: json!("d"),
        }
        .evaluate(&context));

        assert!(FlowCondition::ArrayEmpty {
            array: "empty".to_string(),
        }
        .evaluate(&context));

        assert!(FlowCondition::ArrayLength {
            array: "items".to_string(),
            operator: ComparisonOp::Eq,
            value: 3,
        }
        .evaluate(&context));
    }

    #[test]
    fn test_expression_evaluation() {
        let mut context = HashMap::new();
        context.insert("x".to_string(), json!(10));
        context.insert("name".to_string(), json!("test"));

        assert!(evaluate_expression("x > 5", &context));
        assert!(!evaluate_expression("x > 15", &context));
        assert!(evaluate_expression("name == \"test\"", &context));
        assert!(evaluate_expression("true", &context));
        assert!(!evaluate_expression("false", &context));
    }
}
