//! Embedded MCP - In-Process Tool Execution
//!
//! This module provides an in-process MCP tool executor that bypasses HTTP
//! for internal calls. Inspired by Goose's "platform extensions" pattern.
//!
//! **NOTE:** Currently a minimal implementation. For full functionality,
//! use the HTTP API at port 9876 instead.
//!
//! Use cases:
//! - Verification loops running inside the runner (future)
//! - High-frequency tool calls without HTTP overhead (future)
//! - Internal automation orchestration (future)
//!
//! For external clients (Claude Code, Claude Desktop), use the HTTP API instead.

use serde_json::{json, Value};
use tracing::{debug, warn};

/// In-process MCP tool executor - bypasses HTTP for internal calls.
///
/// **Note:** This is currently a minimal implementation that provides
/// basic tool support. For full functionality (workflow execution,
/// screenshot capture, etc.), use the HTTP API at port 9876.
///
/// The embedded MCP is designed to be used from within the runner
/// for self-contained operations that don't require external communication.
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
    /// **Note:** Only a limited subset of tools is available in embedded mode.
    /// For full tool access, use the HTTP API at port 9876.
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

        match name {
            // Tools available in embedded mode
            "list_screenshots" => self.list_screenshots().await,
            "read_runner_logs" => self.read_runner_logs(args).await,

            // Tools requiring HTTP API
            "get_executor_status"
            | "list_monitors"
            | "get_loaded_config"
            | "run_workflow"
            | "stop_execution"
            | "capture_screenshot"
            | "list_tests"
            | "execute_test"
            | "pattern_find" => {
                warn!(
                    "EmbeddedMcp: tool '{}' requires HTTP API - use port 9876",
                    name
                );
                Err(format!(
                    "Tool '{}' is not available in embedded mode. Use the HTTP API at port 9876.",
                    name
                ))
            }

            // Unknown tool
            _ => {
                warn!("EmbeddedMcp: unknown tool: {}", name);
                Err(format!("Unknown tool: {}", name))
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

/// Helper trait for checking if EmbeddedMcp can handle a tool.
impl EmbeddedMcp {
    /// Check if a tool is supported by the embedded MCP.
    ///
    /// Returns true only for tools that are fully functional in embedded mode.
    pub fn supports_tool(name: &str) -> bool {
        matches!(name, "list_screenshots" | "read_runner_logs")
    }

    /// Check if a tool is known but requires HTTP API.
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
        )
    }

    /// Get a list of all supported tools.
    pub fn supported_tools() -> Vec<&'static str> {
        vec!["list_screenshots", "read_runner_logs"]
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
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supports_tool() {
        assert!(EmbeddedMcp::supports_tool("list_screenshots"));
        assert!(EmbeddedMcp::supports_tool("read_runner_logs"));
        assert!(!EmbeddedMcp::supports_tool("run_workflow"));
        assert!(!EmbeddedMcp::supports_tool("unknown_tool"));
    }

    #[test]
    fn test_requires_http_api() {
        assert!(EmbeddedMcp::requires_http_api("run_workflow"));
        assert!(EmbeddedMcp::requires_http_api("capture_screenshot"));
        assert!(!EmbeddedMcp::requires_http_api("list_screenshots"));
        assert!(!EmbeddedMcp::requires_http_api("unknown_tool"));
    }

    #[test]
    fn test_supported_tools_list() {
        let tools = EmbeddedMcp::supported_tools();
        assert!(tools.contains(&"list_screenshots"));
        assert!(tools.contains(&"read_runner_logs"));
        assert_eq!(tools.len(), 2);
    }

    #[test]
    fn test_http_required_tools_list() {
        let tools = EmbeddedMcp::http_required_tools();
        assert!(tools.contains(&"run_workflow"));
        assert!(tools.contains(&"capture_screenshot"));
        assert!(tools.len() >= 9);
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
}
