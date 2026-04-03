//! Type Coercion for LLM Output
//!
//! Best-effort coercion of common LLM type mismatches before schema validation.
//! This reduces unnecessary retry round-trips for minor issues like
//! `"85"` instead of `85` or `"true"` instead of `true`.

use serde_json::Value;
use tracing::debug;

// ============================================================================
// Coercion Policy
// ============================================================================

/// Controls how aggressively type coercion is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CoercionPolicy {
    /// No coercion — values must match the schema exactly.
    Strict,
    /// Apply safe, unambiguous coercions (string→number, string→bool).
    #[default]
    Lenient,
    /// Disabled — same as Strict but signals explicit opt-out.
    Off,
}

// ============================================================================
// Coercion Record
// ============================================================================

/// Record of a coercion that was applied.
#[derive(Debug, Clone)]
pub struct CoercionRecord {
    /// JSON Pointer path to the coerced value.
    pub path: String,
    /// Original value before coercion.
    pub from: Value,
    /// Coerced value.
    pub to: Value,
    /// Type of coercion applied.
    pub coercion_type: CoercionType,
}

/// Types of coercion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoercionType {
    /// String "123" → number 123
    StringToNumber,
    /// String "true"/"false" → boolean
    StringToBoolean,
    /// Number 1/0 → boolean true/false
    NumberToBoolean,
    /// Single value → single-element array
    ValueToArray,
    /// Number 0.85 → string (for enum coercion)
    NumberToString,
}

// ============================================================================
// Coercion Engine
// ============================================================================

/// Apply coercions to a JSON value based on a JSON Schema.
///
/// Walks the schema and value in parallel, applying type coercions where
/// the value type doesn't match the schema type but can be safely converted.
///
/// Returns the (possibly modified) value and a list of coercions applied.
pub fn apply_coercions(
    value: &Value,
    schema: &Value,
    policy: CoercionPolicy,
) -> (Value, Vec<CoercionRecord>) {
    if matches!(policy, CoercionPolicy::Strict | CoercionPolicy::Off) {
        return (value.clone(), Vec::new());
    }

    let mut records = Vec::new();
    let coerced = coerce_value(value, schema, schema, "", &mut records);
    if !records.is_empty() {
        debug!("Applied {} type coercions", records.len());
    }
    (coerced, records)
}

/// Recursively coerce a value against its schema.
fn coerce_value(
    value: &Value,
    schema: &Value,
    root: &Value,
    path: &str,
    records: &mut Vec<CoercionRecord>,
) -> Value {
    // Resolve $ref
    if let Some(ref_path) = schema.get("$ref").and_then(|v| v.as_str()) {
        if let Some(resolved) = resolve_ref(root, ref_path) {
            return coerce_value(value, resolved, root, path, records);
        }
    }

    let expected_type = schema.get("type").and_then(|v| v.as_str());

    match expected_type {
        Some("object") => coerce_object(value, schema, root, path, records),
        Some("array") => coerce_array(value, schema, root, path, records),
        Some("string") => coerce_to_string(value, schema, path, records),
        Some("integer") | Some("number") => {
            coerce_to_number(value, expected_type.unwrap(), path, records)
        }
        Some("boolean") => coerce_to_boolean(value, path, records),
        _ => value.clone(),
    }
}

/// Coerce object properties.
fn coerce_object(
    value: &Value,
    schema: &Value,
    root: &Value,
    path: &str,
    records: &mut Vec<CoercionRecord>,
) -> Value {
    let obj = match value.as_object() {
        Some(obj) => obj,
        None => return value.clone(),
    };

    let props = match schema.get("properties").and_then(|v| v.as_object()) {
        Some(props) => props,
        None => return value.clone(),
    };

    let mut result = obj.clone();
    for (key, prop_schema) in props {
        if let Some(val) = result.get(key) {
            let child_path = format!("{}/{}", path, key);
            let coerced = coerce_value(val, prop_schema, root, &child_path, records);
            result.insert(key.clone(), coerced);
        }
    }

    Value::Object(result)
}

/// Coerce array items.
fn coerce_array(
    value: &Value,
    schema: &Value,
    root: &Value,
    path: &str,
    records: &mut Vec<CoercionRecord>,
) -> Value {
    // If schema expects array but got a single value, wrap it
    if !value.is_array() && !value.is_null() {
        records.push(CoercionRecord {
            path: path.to_string(),
            from: value.clone(),
            to: Value::Array(vec![value.clone()]),
            coercion_type: CoercionType::ValueToArray,
        });
        return Value::Array(vec![value.clone()]);
    }

    let arr = match value.as_array() {
        Some(arr) => arr,
        None => return value.clone(),
    };

    let items_schema = match schema.get("items") {
        Some(s) => s,
        None => return value.clone(),
    };

    let coerced: Vec<Value> = arr
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let child_path = format!("{}/{}", path, i);
            coerce_value(item, items_schema, root, &child_path, records)
        })
        .collect();

    Value::Array(coerced)
}

/// Coerce a value to string if the schema expects a string.
fn coerce_to_string(
    value: &Value,
    schema: &Value,
    path: &str,
    records: &mut Vec<CoercionRecord>,
) -> Value {
    // Check if the schema has enum values
    let has_enum = schema.get("enum").is_some();

    match value {
        Value::Number(n) if !has_enum => {
            let coerced = Value::String(n.to_string());
            records.push(CoercionRecord {
                path: path.to_string(),
                from: value.clone(),
                to: coerced.clone(),
                coercion_type: CoercionType::NumberToString,
            });
            coerced
        }
        _ => value.clone(),
    }
}

/// Coerce a value to number if the schema expects integer/number.
fn coerce_to_number(
    value: &Value,
    expected: &str,
    path: &str,
    records: &mut Vec<CoercionRecord>,
) -> Value {
    match value {
        Value::String(s) => {
            let parsed = if expected == "integer" {
                s.parse::<i64>().ok().map(|n| serde_json::json!(n))
            } else {
                s.parse::<f64>().ok().map(|n| serde_json::json!(n))
            };

            if let Some(coerced) = parsed {
                records.push(CoercionRecord {
                    path: path.to_string(),
                    from: value.clone(),
                    to: coerced.clone(),
                    coercion_type: CoercionType::StringToNumber,
                });
                coerced
            } else {
                value.clone()
            }
        }
        _ => value.clone(),
    }
}

/// Coerce a value to boolean if the schema expects boolean.
fn coerce_to_boolean(value: &Value, path: &str, records: &mut Vec<CoercionRecord>) -> Value {
    let coerced = match value {
        Value::String(s) => match s.to_lowercase().as_str() {
            "true" | "yes" | "1" => Some(Value::Bool(true)),
            "false" | "no" | "0" => Some(Value::Bool(false)),
            _ => None,
        },
        Value::Number(n) => {
            if n.as_i64() == Some(0) {
                Some(Value::Bool(false))
            } else if n.as_i64() == Some(1) {
                Some(Value::Bool(true))
            } else {
                None
            }
        }
        _ => None,
    };

    if let Some(coerced_val) = coerced {
        records.push(CoercionRecord {
            path: path.to_string(),
            from: value.clone(),
            to: coerced_val.clone(),
            coercion_type: if value.is_string() {
                CoercionType::StringToBoolean
            } else {
                CoercionType::NumberToBoolean
            },
        });
        coerced_val
    } else {
        value.clone()
    }
}

/// Resolve a `$ref` pointer.
fn resolve_ref<'a>(root: &'a Value, ref_path: &str) -> Option<&'a Value> {
    let path = ref_path.strip_prefix("#/")?;
    let mut current = root;
    for segment in path.split('/') {
        current = current.get(segment)?;
    }
    Some(current)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn simple_schema() -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "count": {"type": "integer"},
                "active": {"type": "boolean"},
                "score": {"type": "number"},
                "tags": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["name", "count"]
        })
    }

    #[test]
    fn test_no_coercion_when_strict() {
        let schema = simple_schema();
        let value = serde_json::json!({"name": "test", "count": "5"});
        let (result, records) = apply_coercions(&value, &schema, CoercionPolicy::Strict);
        assert_eq!(result, value);
        assert!(records.is_empty());
    }

    #[test]
    fn test_string_to_integer() {
        let schema = simple_schema();
        let value = serde_json::json!({"name": "test", "count": "5"});
        let (result, records) = apply_coercions(&value, &schema, CoercionPolicy::Lenient);
        assert_eq!(result["count"], 5);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].coercion_type, CoercionType::StringToNumber);
    }

    #[test]
    fn test_string_to_boolean() {
        let schema = simple_schema();
        let value = serde_json::json!({"name": "test", "count": 5, "active": "true"});
        let (result, records) = apply_coercions(&value, &schema, CoercionPolicy::Lenient);
        assert_eq!(result["active"], true);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].coercion_type, CoercionType::StringToBoolean);
    }

    #[test]
    fn test_number_to_boolean() {
        let schema = simple_schema();
        let value = serde_json::json!({"name": "test", "count": 5, "active": 1});
        let (result, records) = apply_coercions(&value, &schema, CoercionPolicy::Lenient);
        assert_eq!(result["active"], true);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].coercion_type, CoercionType::NumberToBoolean);
    }

    #[test]
    fn test_single_value_to_array() {
        let schema = simple_schema();
        let value = serde_json::json!({"name": "test", "count": 5, "tags": "single"});
        let (result, records) = apply_coercions(&value, &schema, CoercionPolicy::Lenient);
        assert_eq!(result["tags"], serde_json::json!(["single"]));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].coercion_type, CoercionType::ValueToArray);
    }

    #[test]
    fn test_no_coercion_when_already_correct() {
        let schema = simple_schema();
        let value = serde_json::json!({"name": "test", "count": 5, "active": true});
        let (result, records) = apply_coercions(&value, &schema, CoercionPolicy::Lenient);
        assert_eq!(result, value);
        assert!(records.is_empty());
    }

    #[test]
    fn test_string_to_float() {
        let schema = simple_schema();
        let value = serde_json::json!({"name": "test", "count": 5, "score": "0.85"});
        let (result, records) = apply_coercions(&value, &schema, CoercionPolicy::Lenient);
        assert_eq!(result["score"], 0.85);
        assert_eq!(records.len(), 1);
    }
}
