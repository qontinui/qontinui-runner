//! Built-in Tool Implementations
//!
//! Provides simple, in-process tools that don't require external services.
//! These tools are shared between Flow Designer and Unified Workflow systems.
//!
//! For more powerful tools (shell commands, API requests, Playwright tests, MCP calls),
//! see the `unified_tools` module which exposes Unified Workflow step types as Flow tools.

use super::results::StepExecutionResult;
use super::unified_tools::UnifiedToolRegistry;
use serde_json::json;
use std::collections::HashMap;
use tracing::{debug, error, info, warn};

/// Registry of built-in tools.
pub struct BuiltinToolRegistry;

impl BuiltinToolRegistry {
    /// Get list of all available built-in tool IDs (simple in-process tools only).
    pub fn available_tools() -> Vec<&'static str> {
        vec![
            "json_parse",
            "json_stringify",
            "get_context",
            "merge_context",
            "string_concat",
            "string_split",
            "string_replace",
            "string_trim",
            "string_uppercase",
            "string_lowercase",
            "array_length",
            "array_map",
            "array_filter",
            "array_find",
            "array_join",
            "array_push",
            "array_slice",
            "timestamp",
            "format_date",
            "log",
            "sleep",
            "http_request",
            "file_exists",
            "env_get",
            "uuid",
            "random_number",
            "hash_sha256",
            "base64_encode",
            "base64_decode",
        ]
    }

    /// Get list of ALL available tools (built-in + unified workflow tools).
    ///
    /// This is useful for UI tool pickers that want to show all available options.
    pub fn all_available_tools() -> Vec<&'static str> {
        let mut tools = Self::available_tools();
        tools.extend(UnifiedToolRegistry::available_tools());
        tools
    }

    /// Check if a tool ID is a built-in tool.
    pub fn is_builtin(tool_id: &str) -> bool {
        Self::available_tools().contains(&tool_id)
    }

    /// Check if a tool ID is any available tool (built-in or unified).
    pub fn is_available(tool_id: &str) -> bool {
        Self::is_builtin(tool_id) || UnifiedToolRegistry::is_unified_tool(tool_id)
    }
}

/// Execute a built-in tool.
///
/// Returns `Some(StepExecutionResult)` if the tool was executed,
/// or `None` if the tool ID is not a built-in tool.
pub async fn execute_builtin_tool(
    step_id: &str,
    tool_id: &str,
    inputs: &HashMap<String, serde_json::Value>,
    context: &HashMap<String, serde_json::Value>,
) -> Option<StepExecutionResult> {
    match tool_id {
        // JSON manipulation tools
        "json_parse" => Some(execute_json_parse(step_id, inputs)),
        "json_stringify" => Some(execute_json_stringify(step_id, inputs)),

        // Context tools
        "get_context" => Some(execute_get_context(step_id, inputs, context)),
        "merge_context" => Some(execute_merge_context(step_id, context)),

        // String tools
        "string_concat" => Some(execute_string_concat(step_id, inputs)),
        "string_split" => Some(execute_string_split(step_id, inputs)),
        "string_replace" => Some(execute_string_replace(step_id, inputs)),
        "string_trim" => Some(execute_string_trim(step_id, inputs)),
        "string_uppercase" => Some(execute_string_uppercase(step_id, inputs)),
        "string_lowercase" => Some(execute_string_lowercase(step_id, inputs)),

        // Array tools
        "array_length" => Some(execute_array_length(step_id, inputs)),
        "array_map" => Some(execute_array_map(step_id, inputs)),
        "array_filter" => Some(execute_array_filter(step_id, inputs)),
        "array_find" => Some(execute_array_find(step_id, inputs)),
        "array_join" => Some(execute_array_join(step_id, inputs)),
        "array_push" => Some(execute_array_push(step_id, inputs)),
        "array_slice" => Some(execute_array_slice(step_id, inputs)),

        // Timestamp tools
        "timestamp" => Some(execute_timestamp(step_id)),
        "format_date" => Some(execute_format_date(step_id, inputs)),

        // Logging
        "log" => Some(execute_log(step_id, inputs)),

        // Delay
        "sleep" => Some(execute_sleep(step_id, inputs).await),

        // Utility tools
        "uuid" => Some(execute_uuid(step_id)),
        "random_number" => Some(execute_random_number(step_id, inputs)),
        "hash_sha256" => Some(execute_hash_sha256(step_id, inputs)),
        "base64_encode" => Some(execute_base64_encode(step_id, inputs)),
        "base64_decode" => Some(execute_base64_decode(step_id, inputs)),
        "env_get" => Some(execute_env_get(step_id, inputs)),

        _ => None, // Not a built-in tool
    }
}

// ============================================================================
// JSON Tools
// ============================================================================

fn execute_json_parse(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let input = match inputs.get("input").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'input'"),
    };

    match serde_json::from_str::<serde_json::Value>(input) {
        Ok(parsed) => StepExecutionResult::success(step_id, None).with_output("parsed", parsed),
        Err(e) => StepExecutionResult::failure(step_id, format!("JSON parse error: {}", e)),
    }
}

fn execute_json_stringify(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let input = match inputs.get("input") {
        Some(v) => v,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'input'"),
    };

    let pretty = inputs
        .get("pretty")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let output = if pretty {
        serde_json::to_string_pretty(input)
    } else {
        serde_json::to_string(input)
    };

    match output {
        Ok(s) => StepExecutionResult::success(step_id, None).with_output("output", json!(s)),
        Err(e) => StepExecutionResult::failure(step_id, format!("JSON stringify error: {}", e)),
    }
}

// ============================================================================
// Context Tools
// ============================================================================

fn execute_get_context(
    step_id: &str,
    inputs: &HashMap<String, serde_json::Value>,
    context: &HashMap<String, serde_json::Value>,
) -> StepExecutionResult {
    let key = match inputs.get("key").and_then(|v| v.as_str()) {
        Some(k) => k,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'key'"),
    };

    let value = context.get(key).cloned().unwrap_or(serde_json::Value::Null);
    StepExecutionResult::success(step_id, None).with_output("value", value)
}

fn execute_merge_context(
    step_id: &str,
    context: &HashMap<String, serde_json::Value>,
) -> StepExecutionResult {
    let mut merged = serde_json::Map::new();
    for (key, value) in context {
        merged.insert(key.clone(), value.clone());
    }
    StepExecutionResult::success(step_id, None)
        .with_output("context", serde_json::Value::Object(merged))
}

// ============================================================================
// String Tools
// ============================================================================

fn execute_string_concat(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let parts = match inputs.get("parts").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'parts' (array)"),
    };

    let separator = inputs
        .get("separator")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let result: Vec<String> = parts
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    StepExecutionResult::success(step_id, None).with_output("output", json!(result.join(separator)))
}

fn execute_string_split(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let input = match inputs.get("input").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'input'"),
    };

    let separator = match inputs.get("separator").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'separator'"),
    };

    let parts: Vec<&str> = input.split(separator).collect();
    StepExecutionResult::success(step_id, None).with_output("parts", json!(parts))
}

fn execute_string_replace(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let input = match inputs.get("input").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'input'"),
    };

    let pattern = match inputs.get("pattern").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'pattern'"),
    };

    let replacement = inputs
        .get("replacement")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let replace_all = inputs
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let output = if replace_all {
        input.replace(pattern, replacement)
    } else {
        input.replacen(pattern, replacement, 1)
    };

    StepExecutionResult::success(step_id, None).with_output("output", json!(output))
}

fn execute_string_trim(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let input = match inputs.get("input").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'input'"),
    };

    StepExecutionResult::success(step_id, None).with_output("output", json!(input.trim()))
}

fn execute_string_uppercase(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let input = match inputs.get("input").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'input'"),
    };

    StepExecutionResult::success(step_id, None).with_output("output", json!(input.to_uppercase()))
}

fn execute_string_lowercase(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let input = match inputs.get("input").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'input'"),
    };

    StepExecutionResult::success(step_id, None).with_output("output", json!(input.to_lowercase()))
}

// ============================================================================
// Array Tools
// ============================================================================

fn execute_array_length(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let array = match inputs.get("array").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'array'"),
    };

    StepExecutionResult::success(step_id, None).with_output("length", json!(array.len()))
}

fn execute_array_map(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let array = match inputs.get("array").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'array'"),
    };

    let field = match inputs.get("field").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'field'"),
    };

    let mapped: Vec<serde_json::Value> = array
        .iter()
        .filter_map(|item| item.get(field).cloned())
        .collect();

    StepExecutionResult::success(step_id, None).with_output("result", json!(mapped))
}

fn execute_array_filter(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let array = match inputs.get("array").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'array'"),
    };

    let field = match inputs.get("field").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'field'"),
    };

    let value = match inputs.get("value") {
        Some(v) => v,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'value'"),
    };

    let filtered: Vec<serde_json::Value> = array
        .iter()
        .filter(|item| item.get(field) == Some(value))
        .cloned()
        .collect();

    StepExecutionResult::success(step_id, None).with_output("result", json!(filtered))
}

fn execute_array_find(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let array = match inputs.get("array").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'array'"),
    };

    let field = match inputs.get("field").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'field'"),
    };

    let value = match inputs.get("value") {
        Some(v) => v,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'value'"),
    };

    let found = array.iter().find(|item| item.get(field) == Some(value));

    StepExecutionResult::success(step_id, None)
        .with_output("result", found.cloned().unwrap_or(serde_json::Value::Null))
        .with_output("found", json!(found.is_some()))
}

fn execute_array_join(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let array = match inputs.get("array").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'array'"),
    };

    let separator = inputs
        .get("separator")
        .and_then(|v| v.as_str())
        .unwrap_or(",");

    let strings: Vec<String> = array
        .iter()
        .map(|v| match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect();

    StepExecutionResult::success(step_id, None).with_output("output", json!(strings.join(separator)))
}

fn execute_array_push(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let array = match inputs.get("array").and_then(|v| v.as_array()) {
        Some(arr) => arr.clone(),
        None => return StepExecutionResult::failure(step_id, "Missing required input 'array'"),
    };

    let item = match inputs.get("item") {
        Some(v) => v.clone(),
        None => return StepExecutionResult::failure(step_id, "Missing required input 'item'"),
    };

    let mut result = array;
    result.push(item);

    StepExecutionResult::success(step_id, None).with_output("result", json!(result))
}

fn execute_array_slice(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let array = match inputs.get("array").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'array'"),
    };

    let start = inputs
        .get("start")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    let end = inputs
        .get("end")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(array.len());

    let end = end.min(array.len());
    let start = start.min(end);

    let sliced: Vec<serde_json::Value> = array[start..end].to_vec();

    StepExecutionResult::success(step_id, None).with_output("result", json!(sliced))
}

// ============================================================================
// Timestamp Tools
// ============================================================================

fn execute_timestamp(step_id: &str) -> StepExecutionResult {
    let now = chrono::Utc::now();
    StepExecutionResult::success(step_id, None)
        .with_output("iso", json!(now.to_rfc3339()))
        .with_output("unix", json!(now.timestamp()))
        .with_output("unix_millis", json!(now.timestamp_millis()))
}

fn execute_format_date(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let format = inputs
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("%Y-%m-%d %H:%M:%S");

    let timestamp = inputs.get("timestamp").and_then(|v| v.as_i64());

    let datetime = if let Some(ts) = timestamp {
        chrono::DateTime::from_timestamp(ts, 0)
            .unwrap_or_else(chrono::Utc::now)
    } else {
        chrono::Utc::now()
    };

    let formatted = datetime.format(format).to_string();
    StepExecutionResult::success(step_id, None).with_output("output", json!(formatted))
}

// ============================================================================
// Logging Tool
// ============================================================================

fn execute_log(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let message = inputs
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("(no message)");

    let level = inputs
        .get("level")
        .and_then(|v| v.as_str())
        .unwrap_or("info");

    match level {
        "debug" => debug!(tool = "log", step_id = %step_id, "{}", message),
        "info" => info!(tool = "log", step_id = %step_id, "{}", message),
        "warn" => warn!(tool = "log", step_id = %step_id, "{}", message),
        "error" => error!(tool = "log", step_id = %step_id, "{}", message),
        _ => info!(tool = "log", step_id = %step_id, "{}", message),
    }

    StepExecutionResult::success(step_id, None).with_output("logged", json!(true))
}

// ============================================================================
// Sleep Tool
// ============================================================================

async fn execute_sleep(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let ms = inputs.get("ms").and_then(|v| v.as_u64()).unwrap_or(1000);
    tokio::time::sleep(tokio::time::Duration::from_millis(ms)).await;
    StepExecutionResult::success(step_id, None)
        .with_output("slept_ms", json!(ms))
        .with_duration(ms)
}

// ============================================================================
// Utility Tools
// ============================================================================

fn execute_uuid(step_id: &str) -> StepExecutionResult {
    let id = uuid::Uuid::new_v4().to_string();
    StepExecutionResult::success(step_id, None).with_output("uuid", json!(id))
}

fn execute_random_number(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let min = inputs.get("min").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let max = inputs.get("max").and_then(|v| v.as_f64()).unwrap_or(1.0);

    use rand::Rng;
    let mut rng = rand::thread_rng();
    let value = rng.gen_range(min..max);

    StepExecutionResult::success(step_id, None).with_output("value", json!(value))
}

fn execute_hash_sha256(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let input = match inputs.get("input").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'input'"),
    };

    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    let hex = format!("{:x}", result);

    StepExecutionResult::success(step_id, None).with_output("hash", json!(hex))
}

fn execute_base64_encode(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let input = match inputs.get("input").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'input'"),
    };

    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(input.as_bytes());

    StepExecutionResult::success(step_id, None).with_output("output", json!(encoded))
}

fn execute_base64_decode(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let input = match inputs.get("input").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'input'"),
    };

    use base64::Engine;
    match base64::engine::general_purpose::STANDARD.decode(input) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(s) => StepExecutionResult::success(step_id, None).with_output("output", json!(s)),
            Err(e) => StepExecutionResult::failure(step_id, format!("UTF-8 decode error: {}", e)),
        },
        Err(e) => StepExecutionResult::failure(step_id, format!("Base64 decode error: {}", e)),
    }
}

fn execute_env_get(step_id: &str, inputs: &HashMap<String, serde_json::Value>) -> StepExecutionResult {
    let key = match inputs.get("key").and_then(|v| v.as_str()) {
        Some(k) => k,
        None => return StepExecutionResult::failure(step_id, "Missing required input 'key'"),
    };

    let default = inputs
        .get("default")
        .and_then(|v| v.as_str())
        .map(String::from);

    let value = std::env::var(key).ok().or(default);

    StepExecutionResult::success(step_id, None).with_output(
        "value",
        value.map(|v| json!(v)).unwrap_or(serde_json::Value::Null),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_json_parse() {
        let mut inputs = HashMap::new();
        inputs.insert("input".to_string(), json!(r#"{"key": "value"}"#));

        let result = execute_builtin_tool("step1", "json_parse", &inputs, &HashMap::new()).await;
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(result.success);
        assert_eq!(result.outputs.get("parsed"), Some(&json!({"key": "value"})));
    }

    #[tokio::test]
    async fn test_string_concat() {
        let mut inputs = HashMap::new();
        inputs.insert("parts".to_string(), json!(["a", "b", "c"]));
        inputs.insert("separator".to_string(), json!("-"));

        let result = execute_builtin_tool("step1", "string_concat", &inputs, &HashMap::new()).await;
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(result.success);
        assert_eq!(result.outputs.get("output"), Some(&json!("a-b-c")));
    }

    #[tokio::test]
    async fn test_array_filter() {
        let mut inputs = HashMap::new();
        inputs.insert(
            "array".to_string(),
            json!([
                {"name": "a", "active": true},
                {"name": "b", "active": false},
                {"name": "c", "active": true}
            ]),
        );
        inputs.insert("field".to_string(), json!("active"));
        inputs.insert("value".to_string(), json!(true));

        let result = execute_builtin_tool("step1", "array_filter", &inputs, &HashMap::new()).await;
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(result.success);
        let filtered = result.outputs.get("result").unwrap().as_array().unwrap();
        assert_eq!(filtered.len(), 2);
    }

    #[tokio::test]
    async fn test_timestamp() {
        let result = execute_builtin_tool("step1", "timestamp", &HashMap::new(), &HashMap::new()).await;
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(result.success);
        assert!(result.outputs.contains_key("iso"));
        assert!(result.outputs.contains_key("unix"));
        assert!(result.outputs.contains_key("unix_millis"));
    }

    #[tokio::test]
    async fn test_uuid() {
        let result = execute_builtin_tool("step1", "uuid", &HashMap::new(), &HashMap::new()).await;
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(result.success);
        let uuid = result.outputs.get("uuid").unwrap().as_str().unwrap();
        assert_eq!(uuid.len(), 36); // UUID format: 8-4-4-4-12
    }

    #[tokio::test]
    async fn test_not_builtin() {
        let result = execute_builtin_tool("step1", "unknown_tool", &HashMap::new(), &HashMap::new()).await;
        assert!(result.is_none());
    }
}
