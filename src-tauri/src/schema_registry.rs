//! Schema Registry
//!
//! Central registry for JSON Schemas derived from Rust types via `schemars`.
//! Provides schema generation, caching, and prompt instruction generation.
//!
//! # Usage
//!
//! ```rust
//! use crate::schema_registry;
//!
//! // Get the JSON Schema for a type as a serde_json::Value
//! let schema = schema_registry::schema_as_json::<WorkerOutput>();
//!
//! // Generate human-readable prompt instructions from the schema
//! let instructions = schema_registry::prompt_instructions_for::<WorkerOutput>();
//! ```

use schemars::schema_for;
use schemars::JsonSchema;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;

use once_cell::sync::Lazy;

// ============================================================================
// Schema cache
// ============================================================================

/// Global schema cache keyed by type name.
static SCHEMA_CACHE: Lazy<Mutex<HashMap<String, Value>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Get the JSON Schema for type `T` as a `serde_json::Value`.
///
/// Schemas are cached after first generation. Thread-safe.
pub fn schema_as_json<T: JsonSchema>() -> Value {
    let type_name = T::schema_name();

    let mut cache = SCHEMA_CACHE.lock().unwrap();
    if let Some(cached) = cache.get(type_name.as_ref()) {
        return cached.clone();
    }

    let schema = schema_for!(T);
    let value = serde_json::to_value(&schema).unwrap_or_else(|_| Value::Object(Default::default()));
    cache.insert(type_name.into_owned(), value.clone());
    value
}

/// Look up a cached schema by type name.
///
/// Returns `None` if the schema hasn't been generated yet via `schema_as_json<T>()`.
/// This is used by middleware that only has the schema name (not the generic type).
pub fn get_cached_schema(type_name: &str) -> Option<Value> {
    let cache = SCHEMA_CACHE.lock().unwrap();
    cache.get(type_name).cloned()
}

/// Generate the JSON Schema for type `T` as a pretty-printed JSON string.
pub fn schema_as_json_string<T: JsonSchema>() -> String {
    serde_json::to_string_pretty(&schema_as_json::<T>()).unwrap_or_default()
}

// ============================================================================
// Prompt instruction generation
// ============================================================================

/// Generate human-readable output format instructions from a JSON Schema.
///
/// Walks the schema tree and produces markdown documentation including:
/// - Field names with types and descriptions
/// - Required vs optional markers
/// - Enum values
/// - Default values
///
/// Designed to be included in AI system prompts to instruct agents on
/// the expected output format.
pub fn prompt_instructions_for<T: JsonSchema>() -> String {
    let schema = schema_as_json::<T>();
    let type_name = T::schema_name();
    let mut output = String::new();

    output.push_str(&format!("## Output Format: {}\n\n", type_name));
    output.push_str("Respond with a JSON object matching this schema:\n\n");
    output.push_str("```json\n");

    // Generate example skeleton
    let skeleton = generate_skeleton(&schema, &schema, 0);
    output.push_str(&skeleton);
    output.push_str("\n```\n\n");

    // Generate field documentation
    output.push_str("### Field Reference\n\n");
    generate_field_docs(&schema, &schema, "", &mut output);

    output
}

// ============================================================================
// Schema skeleton generation
// ============================================================================

/// Generate a JSON skeleton from a schema for prompt examples.
fn generate_skeleton(schema: &Value, root: &Value, depth: usize) -> String {
    let indent = "  ".repeat(depth);

    // Handle $ref
    if let Some(ref_path) = schema.get("$ref").and_then(|v| v.as_str()) {
        if let Some(resolved) = resolve_ref(root, ref_path) {
            return generate_skeleton(resolved, root, depth);
        }
    }

    // Handle oneOf/anyOf (tagged enums)
    if let Some(one_of) = schema.get("oneOf").and_then(|v| v.as_array()) {
        if let Some(first) = one_of.first() {
            return generate_skeleton(first, root, depth);
        }
    }

    let schema_type = schema
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("object");

    match schema_type {
        "object" => {
            let mut result = String::from("{\n");
            if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
                let required: Vec<&str> = schema
                    .get("required")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();

                for (i, (key, prop_schema)) in props.iter().enumerate() {
                    let is_required = required.contains(&key.as_str());
                    let child = generate_skeleton(prop_schema, root, depth + 1);
                    let child_indent = "  ".repeat(depth + 1);
                    let comment = if !is_required { " // optional" } else { "" };
                    result.push_str(&format!("{}\"{}\": {}{}", child_indent, key, child, comment));
                    if i < props.len() - 1 {
                        result.push(',');
                    }
                    result.push('\n');
                }
            }
            result.push_str(&format!("{}}}", indent));
            result
        }
        "array" => {
            if let Some(items) = schema.get("items") {
                let child = generate_skeleton(items, root, depth + 1);
                format!("[{}]", child)
            } else {
                "[]".to_string()
            }
        }
        "string" => {
            if let Some(enum_values) = schema.get("enum").and_then(|v| v.as_array()) {
                let first = enum_values
                    .first()
                    .and_then(|v| v.as_str())
                    .unwrap_or("value");
                format!("\"{}\"", first)
            } else {
                "\"...\"".to_string()
            }
        }
        "integer" | "number" => {
            if let Some(default) = schema.get("default") {
                default.to_string()
            } else {
                "0".to_string()
            }
        }
        "boolean" => "false".to_string(),
        "null" => "null".to_string(),
        _ => "null".to_string(),
    }
}

/// Resolve a `$ref` pointer to its target schema.
fn resolve_ref<'a>(root: &'a Value, ref_path: &str) -> Option<&'a Value> {
    // Handle "#/$defs/TypeName" format (schemars 1.x)
    let path = ref_path.strip_prefix("#/")?;
    let mut current = root;
    for segment in path.split('/') {
        current = current.get(segment)?;
    }
    Some(current)
}

// ============================================================================
// Field documentation generation
// ============================================================================

/// Generate markdown field documentation from a schema.
fn generate_field_docs(schema: &Value, root: &Value, prefix: &str, output: &mut String) {
    // Handle $ref
    if let Some(ref_path) = schema.get("$ref").and_then(|v| v.as_str()) {
        if let Some(resolved) = resolve_ref(root, ref_path) {
            generate_field_docs(resolved, root, prefix, output);
            return;
        }
    }

    let required: Vec<&str> = schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
        for (key, prop_schema) in props {
            let full_path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{}.{}", prefix, key)
            };

            let is_required = required.contains(&key.as_str());
            let req_marker = if is_required { " **(required)**" } else { "" };

            // Resolve refs for type info
            let resolved = if let Some(ref_path) = prop_schema.get("$ref").and_then(|v| v.as_str())
            {
                resolve_ref(root, ref_path).unwrap_or(prop_schema)
            } else {
                prop_schema
            };

            let type_str = describe_type(resolved);
            let description = resolved
                .get("description")
                .and_then(|v| v.as_str())
                .or_else(|| prop_schema.get("description").and_then(|v| v.as_str()))
                .unwrap_or("");

            output.push_str(&format!(
                "- **`{}`** ({}){}: {}\n",
                full_path, type_str, req_marker, description
            ));

            // Recurse into nested objects
            if resolved.get("type").and_then(|v| v.as_str()) == Some("object")
                && resolved.get("properties").is_some()
            {
                generate_field_docs(resolved, root, &full_path, output);
            }
        }
    }
}

/// Describe a schema type in human-readable form.
fn describe_type(schema: &Value) -> String {
    if let Some(enum_values) = schema.get("enum").and_then(|v| v.as_array()) {
        let values: Vec<&str> = enum_values.iter().filter_map(|v| v.as_str()).collect();
        if !values.is_empty() {
            return format!("enum: {}", values.join(" | "));
        }
    }

    if let Some(one_of) = schema.get("oneOf").and_then(|v| v.as_array()) {
        let variants: Vec<String> = one_of
            .iter()
            .filter_map(|v| {
                v.get("properties")
                    .and_then(|p| p.as_object())
                    .and_then(|p| p.get("type"))
                    .and_then(|t| t.get("const"))
                    .and_then(|c| c.as_str())
                    .map(String::from)
            })
            .collect();
        if !variants.is_empty() {
            return format!("tagged union: {}", variants.join(" | "));
        }
    }

    let type_str = schema
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("any");

    match type_str {
        "array" => {
            if let Some(items) = schema.get("items") {
                let item_type = describe_type(items);
                format!("array of {}", item_type)
            } else {
                "array".to_string()
            }
        }
        other => other.to_string(),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    struct TestOutput {
        /// Summary of work performed.
        pub summary: String,
        /// Confidence level.
        #[serde(default)]
        pub confidence: TestConfidence,
        /// Optional notes.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub notes: Option<String>,
    }

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
    #[serde(rename_all = "lowercase")]
    enum TestConfidence {
        High,
        #[default]
        Medium,
        Low,
    }

    #[test]
    fn test_schema_generation() {
        let schema = schema_as_json::<TestOutput>();
        assert!(schema.get("properties").is_some());
        let props = schema.get("properties").unwrap();
        assert!(props.get("summary").is_some());
        assert!(props.get("confidence").is_some());
    }

    #[test]
    fn test_schema_caching() {
        let schema1 = schema_as_json::<TestOutput>();
        let schema2 = schema_as_json::<TestOutput>();
        assert_eq!(schema1, schema2);
    }

    #[test]
    fn test_prompt_instructions() {
        let instructions = prompt_instructions_for::<TestOutput>();
        assert!(instructions.contains("TestOutput"));
        assert!(instructions.contains("summary"));
        assert!(instructions.contains("confidence"));
    }

    #[test]
    fn test_schema_json_string() {
        let json_str = schema_as_json_string::<TestOutput>();
        assert!(json_str.contains("\"properties\""));
        assert!(json_str.contains("\"summary\""));
    }
}
