//! Embedded MCP - In-Process Tool Execution
//!
//! This module provides an in-process MCP tool executor that bypasses HTTP
//! for internal calls. Inspired by Goose's "platform extensions" pattern.
//!
//! Supports:
//! - Built-in tools (30+ JSON, string, array, utility operations)
//! - Runner-specific tools (screenshots, logs)
//! - Unified workflow tools (shell, API, Playwright, MCP)
//!
//! Use cases:
//! - Flow Designer tool steps
//! - Verification loops running inside the runner
//! - High-frequency tool calls without HTTP overhead
//! - Internal automation orchestration
//!
//! For tools requiring external state (workflow execution, screenshot capture),
//! use the HTTP API at port 9876 instead.

use crate::execution_core::builtin_tools::{execute_builtin_tool, BuiltinToolRegistry};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use tracing::{debug, info, warn};

/// Tool information for MCP discovery.
///
/// Follows the MCP protocol tool format with JSON Schema for inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    /// Tool name (unique identifier).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON Schema for tool inputs.
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// In-process MCP tool executor - bypasses HTTP for internal calls.
///
/// Provides access to:
/// - 30+ built-in tools (JSON, string, array, timestamp, utility operations)
/// - Runner-specific tools (screenshots, logs)
/// - Tool discovery via `get_all_tools()`
///
/// For tools requiring external state (workflow execution, screenshot capture),
/// use the HTTP API at port 9876 instead.
pub struct EmbeddedMcp {
    /// Base path for dev logs (hardcoded for now)
    dev_logs_path: std::path::PathBuf,
}

impl EmbeddedMcp {
    /// Create a new embedded MCP executor.
    pub fn new() -> Self {
        Self {
            dev_logs_path: crate::paths::get_dev_logs_dir(),
        }
    }

    /// Execute a tool directly without HTTP round-trip.
    ///
    /// Supports:
    /// - Built-in tools (30+ JSON, string, array, utility operations)
    /// - Runner-specific tools (screenshots, logs)
    ///
    /// # Arguments
    /// * `name` - Tool name to execute
    /// * `args` - JSON arguments for the tool
    ///
    /// # Returns
    /// * `Ok(Value)` - Tool execution result as JSON
    /// * `Err(String)` - Error message if tool execution fails
    pub async fn call_tool(&self, name: &str, args: Value) -> Result<Value, String> {
        debug!("EmbeddedMcp: calling tool {} with args: {:?}", name, args);

        // Check for runner-specific tools first
        match name {
            "list_screenshots" => return self.list_screenshots().await,
            "read_runner_logs" => return self.read_runner_logs(args.clone()).await,
            _ => {}
        }

        // Check if it's a built-in tool
        if BuiltinToolRegistry::is_builtin(name) {
            return self.execute_builtin(name, args).await;
        }

        // Check for tools requiring HTTP API
        if Self::requires_http_api(name) {
            warn!(
                "EmbeddedMcp: tool '{}' requires HTTP API - use port 9876",
                name
            );
            return Err(format!(
                "Tool '{}' is not available in embedded mode. Use the HTTP API at port 9876.",
                name
            ));
        }

        // Unknown tool
        warn!("EmbeddedMcp: unknown tool: {}", name);
        Err(format!("Unknown tool: {}", name))
    }

    /// Execute a built-in tool.
    async fn execute_builtin(&self, name: &str, args: Value) -> Result<Value, String> {
        // Convert JSON Value to HashMap for the builtin tool executor
        let inputs: HashMap<String, Value> = match args {
            Value::Object(map) => map.into_iter().collect(),
            Value::Null => HashMap::new(),
            _ => {
                return Err(format!(
                    "Tool '{}' expects object arguments, got: {}",
                    name, args
                ));
            }
        };

        let context = HashMap::new(); // Empty context for embedded calls
        let step_id = format!("embedded_{}", name);

        match execute_builtin_tool(&step_id, name, &inputs, &context).await {
            Some(result) => {
                if result.success {
                    info!(
                        tool = %name,
                        duration_ms = result.duration_ms,
                        "Builtin tool executed successfully"
                    );
                    // Return outputs as the result
                    Ok(json!(result.outputs))
                } else {
                    let error = result.error.unwrap_or_else(|| "Unknown error".to_string());
                    warn!(tool = %name, error = %error, "Builtin tool failed");
                    Err(error)
                }
            }
            None => {
                // This shouldn't happen since we checked is_builtin above
                Err(format!("Builtin tool '{}' not found", name))
            }
        }
    }

    /// List available screenshots from the dev-logs directory.
    async fn list_screenshots(&self) -> Result<Value, String> {
        use std::fs;

        let screenshots_dir = self.dev_logs_path.join("screenshots");

        if !screenshots_dir.exists() {
            return Ok(json!({
                "screenshots": []
            }));
        }

        let mut screenshots = Vec::new();
        if let Ok(entries) = fs::read_dir(&screenshots_dir) {
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.extension().map(|e| e == "png").unwrap_or(false) {
                    let filename = path.file_name().unwrap_or_default().to_string_lossy();
                    let metadata = fs::metadata(&path).ok();

                    screenshots.push(json!({
                        "filename": filename,
                        "path": path.to_string_lossy(),
                        "size": metadata.as_ref().map(|m| m.len()),
                        "modified": metadata.as_ref().and_then(|m| m.modified().ok())
                            .map(|t| format!("{:?}", t)),
                    }));
                }
            }
        }

        // Sort by modification time (newest first)
        screenshots.sort_by(|a, b| {
            let a_time = a.get("modified").and_then(|v| v.as_str());
            let b_time = b.get("modified").and_then(|v| v.as_str());
            b_time.cmp(&a_time)
        });

        Ok(json!({
            "screenshots": screenshots
        }))
    }

    /// Read runner logs from the dev-logs directory.
    async fn read_runner_logs(&self, args: Value) -> Result<Value, String> {
        use std::fs::File;
        use std::io::{BufRead, BufReader};

        let log_type = args
            .get("log_type")
            .and_then(|v| v.as_str())
            .unwrap_or("all");
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;

        let mut result: std::collections::HashMap<String, Vec<Value>> =
            std::collections::HashMap::new();

        let log_files = match log_type {
            "general" => vec![("general", "runner-general.jsonl")],
            "actions" => vec![("actions", "runner-actions.jsonl")],
            "image-recognition" => vec![("image_recognition", "runner-image-recognition.jsonl")],
            "playwright" => vec![("playwright", "runner-playwright.jsonl")],
            "all" => vec![
                ("general", "runner-general.jsonl"),
                ("actions", "runner-actions.jsonl"),
                ("image_recognition", "runner-image-recognition.jsonl"),
                ("playwright", "runner-playwright.jsonl"),
            ],
            _ => {
                return Err(format!("Unknown log type: {}", log_type));
            }
        };

        for (key, filename) in log_files {
            let path = self.dev_logs_path.join(filename);
            if path.exists() {
                let file = File::open(&path).map_err(|e| format!("Failed to open log: {}", e))?;
                let reader = BufReader::new(file);

                let lines: Vec<String> = reader.lines().filter_map(Result::ok).collect();
                let entries: Vec<Value> = lines
                    .iter()
                    .rev()
                    .take(limit)
                    .filter_map(|line| serde_json::from_str(line).ok())
                    .collect();

                result.insert(key.to_string(), entries);
            }
        }

        Ok(json!({
            "logs": result,
            "log_type": log_type,
            "limit": limit,
        }))
    }
}

impl Default for EmbeddedMcp {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tool Discovery
// ============================================================================

impl EmbeddedMcp {
    /// Get all available tools with their schemas.
    ///
    /// Returns tool information for:
    /// - Built-in tools (30+ operations)
    /// - Runner-specific tools (screenshots, logs)
    ///
    /// This is useful for MCP discovery and UI tool pickers.
    pub fn get_all_tools() -> Vec<ToolInfo> {
        let mut tools = Vec::new();

        // Add built-in tools
        tools.extend(Self::get_builtin_tool_infos());

        // Add runner-specific tools
        tools.extend(Self::get_runner_tool_infos());

        tools
    }

    /// Get tool info for a specific tool.
    pub fn get_tool_info(name: &str) -> Option<ToolInfo> {
        Self::get_all_tools().into_iter().find(|t| t.name == name)
    }

    /// Get built-in tool information with schemas.
    fn get_builtin_tool_infos() -> Vec<ToolInfo> {
        vec![
            // JSON tools
            ToolInfo {
                name: "json_parse".to_string(),
                description: "Parse a JSON string into a value".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "input": {
                            "type": "string",
                            "description": "The JSON string to parse"
                        }
                    },
                    "required": ["input"]
                }),
            },
            ToolInfo {
                name: "json_stringify".to_string(),
                description: "Convert a value to a JSON string".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "input": {
                            "description": "The value to stringify"
                        },
                        "pretty": {
                            "type": "boolean",
                            "description": "Whether to format with indentation",
                            "default": false
                        }
                    },
                    "required": ["input"]
                }),
            },
            // Context tools
            ToolInfo {
                name: "get_context".to_string(),
                description: "Get a value from the execution context".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "key": {
                            "type": "string",
                            "description": "The context key to retrieve"
                        }
                    },
                    "required": ["key"]
                }),
            },
            ToolInfo {
                name: "merge_context".to_string(),
                description: "Merge all context values into a single object".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            // String tools
            ToolInfo {
                name: "string_concat".to_string(),
                description: "Concatenate strings with an optional separator".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "parts": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Array of strings to concatenate"
                        },
                        "separator": {
                            "type": "string",
                            "description": "Separator between parts",
                            "default": ""
                        }
                    },
                    "required": ["parts"]
                }),
            },
            ToolInfo {
                name: "string_split".to_string(),
                description: "Split a string by a separator".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "input": {
                            "type": "string",
                            "description": "The string to split"
                        },
                        "separator": {
                            "type": "string",
                            "description": "The separator to split on"
                        }
                    },
                    "required": ["input", "separator"]
                }),
            },
            ToolInfo {
                name: "string_replace".to_string(),
                description: "Replace occurrences of a pattern in a string".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "input": {
                            "type": "string",
                            "description": "The input string"
                        },
                        "pattern": {
                            "type": "string",
                            "description": "The pattern to find"
                        },
                        "replacement": {
                            "type": "string",
                            "description": "The replacement string",
                            "default": ""
                        },
                        "replace_all": {
                            "type": "boolean",
                            "description": "Replace all occurrences (default: true)",
                            "default": true
                        }
                    },
                    "required": ["input", "pattern"]
                }),
            },
            ToolInfo {
                name: "string_trim".to_string(),
                description: "Trim whitespace from a string".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "input": {
                            "type": "string",
                            "description": "The string to trim"
                        }
                    },
                    "required": ["input"]
                }),
            },
            ToolInfo {
                name: "string_uppercase".to_string(),
                description: "Convert a string to uppercase".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "input": {
                            "type": "string",
                            "description": "The string to convert"
                        }
                    },
                    "required": ["input"]
                }),
            },
            ToolInfo {
                name: "string_lowercase".to_string(),
                description: "Convert a string to lowercase".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "input": {
                            "type": "string",
                            "description": "The string to convert"
                        }
                    },
                    "required": ["input"]
                }),
            },
            // Array tools
            ToolInfo {
                name: "array_length".to_string(),
                description: "Get the length of an array".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "array": {
                            "type": "array",
                            "description": "The array to measure"
                        }
                    },
                    "required": ["array"]
                }),
            },
            ToolInfo {
                name: "array_map".to_string(),
                description: "Extract a field from each object in an array".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "array": {
                            "type": "array",
                            "description": "Array of objects"
                        },
                        "field": {
                            "type": "string",
                            "description": "Field name to extract"
                        }
                    },
                    "required": ["array", "field"]
                }),
            },
            ToolInfo {
                name: "array_filter".to_string(),
                description: "Filter array items where a field equals a value".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "array": {
                            "type": "array",
                            "description": "Array to filter"
                        },
                        "field": {
                            "type": "string",
                            "description": "Field name to check"
                        },
                        "value": {
                            "description": "Value to match"
                        }
                    },
                    "required": ["array", "field", "value"]
                }),
            },
            ToolInfo {
                name: "array_find".to_string(),
                description: "Find the first item where a field equals a value".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "array": {
                            "type": "array",
                            "description": "Array to search"
                        },
                        "field": {
                            "type": "string",
                            "description": "Field name to check"
                        },
                        "value": {
                            "description": "Value to match"
                        }
                    },
                    "required": ["array", "field", "value"]
                }),
            },
            ToolInfo {
                name: "array_join".to_string(),
                description: "Join array elements into a string".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "array": {
                            "type": "array",
                            "description": "Array to join"
                        },
                        "separator": {
                            "type": "string",
                            "description": "Separator between elements",
                            "default": ","
                        }
                    },
                    "required": ["array"]
                }),
            },
            ToolInfo {
                name: "array_push".to_string(),
                description: "Add an item to an array".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "array": {
                            "type": "array",
                            "description": "The array"
                        },
                        "item": {
                            "description": "Item to add"
                        }
                    },
                    "required": ["array", "item"]
                }),
            },
            ToolInfo {
                name: "array_slice".to_string(),
                description: "Get a slice of an array".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "array": {
                            "type": "array",
                            "description": "The array"
                        },
                        "start": {
                            "type": "integer",
                            "description": "Start index (0-based)",
                            "default": 0
                        },
                        "end": {
                            "type": "integer",
                            "description": "End index (exclusive, defaults to array length)"
                        }
                    },
                    "required": ["array"]
                }),
            },
            // Timestamp tools
            ToolInfo {
                name: "timestamp".to_string(),
                description: "Get the current timestamp in various formats".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            ToolInfo {
                name: "format_date".to_string(),
                description: "Format a timestamp using a format string".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "timestamp": {
                            "type": "integer",
                            "description": "Unix timestamp (seconds). Defaults to current time."
                        },
                        "format": {
                            "type": "string",
                            "description": "strftime format string",
                            "default": "%Y-%m-%d %H:%M:%S"
                        }
                    }
                }),
            },
            // Utility tools
            ToolInfo {
                name: "log".to_string(),
                description: "Log a message (useful for debugging flows)".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "message": {
                            "type": "string",
                            "description": "Message to log"
                        },
                        "level": {
                            "type": "string",
                            "enum": ["debug", "info", "warn", "error"],
                            "description": "Log level",
                            "default": "info"
                        }
                    },
                    "required": ["message"]
                }),
            },
            ToolInfo {
                name: "sleep".to_string(),
                description: "Pause execution for a specified duration".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "ms": {
                            "type": "integer",
                            "description": "Milliseconds to sleep",
                            "default": 1000
                        }
                    }
                }),
            },
            ToolInfo {
                name: "uuid".to_string(),
                description: "Generate a random UUID".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            ToolInfo {
                name: "random_number".to_string(),
                description: "Generate a random number in a range".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "min": {
                            "type": "number",
                            "description": "Minimum value",
                            "default": 0
                        },
                        "max": {
                            "type": "number",
                            "description": "Maximum value",
                            "default": 1
                        }
                    }
                }),
            },
            ToolInfo {
                name: "hash_sha256".to_string(),
                description: "Compute SHA-256 hash of a string".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "input": {
                            "type": "string",
                            "description": "String to hash"
                        }
                    },
                    "required": ["input"]
                }),
            },
            ToolInfo {
                name: "base64_encode".to_string(),
                description: "Encode a string to base64".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "input": {
                            "type": "string",
                            "description": "String to encode"
                        }
                    },
                    "required": ["input"]
                }),
            },
            ToolInfo {
                name: "base64_decode".to_string(),
                description: "Decode a base64 string".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "input": {
                            "type": "string",
                            "description": "Base64 string to decode"
                        }
                    },
                    "required": ["input"]
                }),
            },
            ToolInfo {
                name: "env_get".to_string(),
                description: "Get an environment variable".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "key": {
                            "type": "string",
                            "description": "Environment variable name"
                        },
                        "default": {
                            "type": "string",
                            "description": "Default value if not set"
                        }
                    },
                    "required": ["key"]
                }),
            },
        ]
    }

    /// Get runner-specific tool information.
    fn get_runner_tool_infos() -> Vec<ToolInfo> {
        vec![
            ToolInfo {
                name: "list_screenshots".to_string(),
                description: "List screenshots from the dev-logs directory".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            ToolInfo {
                name: "read_runner_logs".to_string(),
                description: "Read runner logs from the dev-logs directory".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "log_type": {
                            "type": "string",
                            "enum": ["all", "general", "actions", "image-recognition", "playwright"],
                            "description": "Type of logs to read",
                            "default": "all"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of entries to return",
                            "default": 100
                        }
                    }
                }),
            },
            ToolInfo {
                name: "read_execution_spans".to_string(),
                description: "Query execution spans from the database for performance analysis. Requires HTTP API.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "execution_id": {
                            "type": "string",
                            "description": "Filter by execution/task ID"
                        },
                        "name_pattern": {
                            "type": "string",
                            "description": "Filter span names using SQL LIKE pattern (e.g., 'workflow.%')"
                        },
                        "min_duration_ms": {
                            "type": "integer",
                            "description": "Filter spans with duration >= this value"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of spans to return",
                            "default": 100
                        }
                    }
                }),
            },
        ]
    }
}

// ============================================================================
// Tool Support Checking
// ============================================================================

/// Helper methods for checking if EmbeddedMcp can handle a tool.
impl EmbeddedMcp {
    /// Check if a tool is supported by the embedded MCP.
    ///
    /// Returns true for:
    /// - Built-in tools (30+ operations)
    /// - Runner-specific tools (screenshots, logs)
    pub fn supports_tool(name: &str) -> bool {
        // Runner-specific tools
        if matches!(name, "list_screenshots" | "read_runner_logs") {
            return true;
        }
        // Built-in tools
        BuiltinToolRegistry::is_builtin(name)
    }

    /// Check if a tool is known but requires HTTP API.
    ///
    /// These tools need access to runner state that isn't available
    /// in the embedded context.
    pub fn requires_http_api(name: &str) -> bool {
        matches!(
            name,
            "get_executor_status"
                | "list_monitors"
                | "get_loaded_config"
                | "run_workflow"
                | "stop_execution"
                | "capture_screenshot"
                | "list_tests"
                | "execute_test"
                | "pattern_find"
                | "read_execution_spans"
        )
    }

    /// Get a list of all supported tool names.
    ///
    /// Includes both built-in tools and runner-specific tools.
    pub fn supported_tools() -> Vec<&'static str> {
        let mut tools = BuiltinToolRegistry::available_tools();
        tools.push("list_screenshots");
        tools.push("read_runner_logs");
        tools
    }

    /// Get a list of tools that require HTTP API.
    pub fn http_required_tools() -> Vec<&'static str> {
        vec![
            "get_executor_status",
            "list_monitors",
            "get_loaded_config",
            "run_workflow",
            "stop_execution",
            "capture_screenshot",
            "list_tests",
            "execute_test",
            "pattern_find",
            "read_execution_spans",
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supports_tool() {
        // Runner-specific tools
        assert!(EmbeddedMcp::supports_tool("list_screenshots"));
        assert!(EmbeddedMcp::supports_tool("read_runner_logs"));
        // Built-in tools
        assert!(EmbeddedMcp::supports_tool("json_parse"));
        assert!(EmbeddedMcp::supports_tool("string_concat"));
        assert!(EmbeddedMcp::supports_tool("array_length"));
        assert!(EmbeddedMcp::supports_tool("timestamp"));
        assert!(EmbeddedMcp::supports_tool("uuid"));
        // HTTP API required tools
        assert!(!EmbeddedMcp::supports_tool("run_workflow"));
        // Unknown tools
        assert!(!EmbeddedMcp::supports_tool("unknown_tool"));
    }

    #[test]
    fn test_requires_http_api() {
        assert!(EmbeddedMcp::requires_http_api("run_workflow"));
        assert!(EmbeddedMcp::requires_http_api("capture_screenshot"));
        assert!(!EmbeddedMcp::requires_http_api("list_screenshots"));
        assert!(!EmbeddedMcp::requires_http_api("json_parse"));
        assert!(!EmbeddedMcp::requires_http_api("unknown_tool"));
    }

    #[test]
    fn test_supported_tools_list() {
        let tools = EmbeddedMcp::supported_tools();
        // Should include runner-specific tools
        assert!(tools.contains(&"list_screenshots"));
        assert!(tools.contains(&"read_runner_logs"));
        // Should include built-in tools
        assert!(tools.contains(&"json_parse"));
        assert!(tools.contains(&"string_concat"));
        assert!(tools.contains(&"array_length"));
        assert!(tools.contains(&"timestamp"));
        // Should have 30+ tools (28 builtin + 2 runner)
        assert!(tools.len() >= 30);
    }

    #[test]
    fn test_http_required_tools_list() {
        let tools = EmbeddedMcp::http_required_tools();
        assert!(tools.contains(&"run_workflow"));
        assert!(tools.contains(&"capture_screenshot"));
        assert!(tools.len() >= 9);
    }

    #[test]
    fn test_get_all_tools() {
        let tools = EmbeddedMcp::get_all_tools();
        // Should have builtin + runner tools
        assert!(
            tools.len() >= 20,
            "Expected at least 20 tools, got {}",
            tools.len()
        );

        // Check that each tool has required fields
        for tool in &tools {
            assert!(!tool.name.is_empty());
            assert!(!tool.description.is_empty());
            assert!(tool.input_schema.is_object());
        }

        // Check specific tools exist
        let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(tool_names.contains(&"json_parse"));
        assert!(tool_names.contains(&"string_concat"));
        assert!(tool_names.contains(&"array_length"));
        assert!(tool_names.contains(&"timestamp"));
        assert!(tool_names.contains(&"list_screenshots"));
    }

    #[test]
    fn test_get_tool_info() {
        let tool = EmbeddedMcp::get_tool_info("json_parse");
        assert!(tool.is_some());
        let tool = tool.unwrap();
        assert_eq!(tool.name, "json_parse");
        assert!(tool.description.contains("JSON"));

        let unknown = EmbeddedMcp::get_tool_info("unknown_tool");
        assert!(unknown.is_none());
    }

    #[tokio::test]
    async fn test_call_tool_http_required() {
        let mcp = EmbeddedMcp::new();
        let result = mcp
            .call_tool("run_workflow", json!({"workflow_name": "test"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("HTTP API"));
    }

    #[tokio::test]
    async fn test_call_builtin_tool_json_parse() {
        let mcp = EmbeddedMcp::new();
        let result = mcp
            .call_tool("json_parse", json!({"input": r#"{"key": "value"}"#}))
            .await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.get("parsed").is_some());
        assert_eq!(output["parsed"]["key"], "value");
    }

    #[tokio::test]
    async fn test_call_builtin_tool_string_concat() {
        let mcp = EmbeddedMcp::new();
        let result = mcp
            .call_tool(
                "string_concat",
                json!({"parts": ["a", "b", "c"], "separator": "-"}),
            )
            .await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output["output"], "a-b-c");
    }

    #[tokio::test]
    async fn test_call_builtin_tool_array_length() {
        let mcp = EmbeddedMcp::new();
        let result = mcp
            .call_tool("array_length", json!({"array": [1, 2, 3, 4, 5]}))
            .await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output["length"], 5);
    }

    #[tokio::test]
    async fn test_call_builtin_tool_timestamp() {
        let mcp = EmbeddedMcp::new();
        let result = mcp.call_tool("timestamp", json!({})).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.get("iso").is_some());
        assert!(output.get("unix").is_some());
        assert!(output.get("unix_millis").is_some());
    }

    #[tokio::test]
    async fn test_call_builtin_tool_uuid() {
        let mcp = EmbeddedMcp::new();
        let result = mcp.call_tool("uuid", json!({})).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        let uuid = output["uuid"].as_str().unwrap();
        assert_eq!(uuid.len(), 36); // UUID format: 8-4-4-4-12
    }

    #[tokio::test]
    async fn test_call_builtin_tool_missing_input() {
        let mcp = EmbeddedMcp::new();
        let result = mcp.call_tool("json_parse", json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("input"));
    }
}
