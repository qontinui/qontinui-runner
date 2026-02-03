//! Unified Workflow Tools for Flow Designer
//!
//! Exposes Unified Workflow step types as Flow Designer tools.
//! This allows Flow Designer to use the same execution capabilities:
//! - Playwright script execution
//! - Shell command execution
//! - HTTP API requests
//! - MCP tool calls
//!
//! These tools bridge the two orchestration systems, enabling
//! Flow Designer to leverage the full power of the Unified Workflow executor.

use super::results::StepExecutionResult;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{timeout, Duration};
use tracing::{error, info, warn};

// ============================================================================
// Tool Registry
// ============================================================================

/// Registry of Unified Workflow tools available in Flow Designer.
pub struct UnifiedToolRegistry;

impl UnifiedToolRegistry {
    /// Get list of all available unified workflow tool IDs.
    pub fn available_tools() -> Vec<&'static str> {
        vec![
            "shell_command",
            "api_request",
            "playwright_test",
            "mcp_call",
        ]
    }

    /// Check if a tool ID is a unified workflow tool.
    pub fn is_unified_tool(tool_id: &str) -> bool {
        Self::available_tools().contains(&tool_id)
    }

    /// Get tool schema for documentation/UI.
    pub fn get_tool_schema(tool_id: &str) -> Option<ToolSchema> {
        match tool_id {
            "shell_command" => Some(ToolSchema {
                name: "shell_command".to_string(),
                description: "Execute a shell command".to_string(),
                inputs: vec![
                    InputSchema {
                        name: "command".to_string(),
                        description: "The shell command to execute".to_string(),
                        required: true,
                        default: None,
                    },
                    InputSchema {
                        name: "working_directory".to_string(),
                        description: "Working directory for command execution".to_string(),
                        required: false,
                        default: None,
                    },
                    InputSchema {
                        name: "timeout_secs".to_string(),
                        description: "Timeout in seconds (default: 120)".to_string(),
                        required: false,
                        default: Some(json!(120)),
                    },
                    InputSchema {
                        name: "fail_on_error".to_string(),
                        description: "Whether to fail the flow on non-zero exit (default: true)"
                            .to_string(),
                        required: false,
                        default: Some(json!(true)),
                    },
                ],
            }),
            "api_request" => Some(ToolSchema {
                name: "api_request".to_string(),
                description: "Execute an HTTP API request".to_string(),
                inputs: vec![
                    InputSchema {
                        name: "method".to_string(),
                        description: "HTTP method (GET, POST, PUT, PATCH, DELETE)".to_string(),
                        required: true,
                        default: None,
                    },
                    InputSchema {
                        name: "url".to_string(),
                        description: "Request URL".to_string(),
                        required: true,
                        default: None,
                    },
                    InputSchema {
                        name: "headers".to_string(),
                        description: "Request headers as JSON object".to_string(),
                        required: false,
                        default: None,
                    },
                    InputSchema {
                        name: "body".to_string(),
                        description: "Request body".to_string(),
                        required: false,
                        default: None,
                    },
                    InputSchema {
                        name: "content_type".to_string(),
                        description: "Content-Type header value".to_string(),
                        required: false,
                        default: None,
                    },
                    InputSchema {
                        name: "timeout_secs".to_string(),
                        description: "Timeout in seconds (default: 30)".to_string(),
                        required: false,
                        default: Some(json!(30)),
                    },
                ],
            }),
            "playwright_test" => Some(ToolSchema {
                name: "playwright_test".to_string(),
                description: "Execute a Playwright test script".to_string(),
                inputs: vec![
                    InputSchema {
                        name: "script_path".to_string(),
                        description: "Path to the Playwright test script".to_string(),
                        required: false,
                        default: None,
                    },
                    InputSchema {
                        name: "script_content".to_string(),
                        description: "Inline Playwright script content".to_string(),
                        required: false,
                        default: None,
                    },
                    InputSchema {
                        name: "target_url".to_string(),
                        description: "Target URL to test".to_string(),
                        required: false,
                        default: None,
                    },
                    InputSchema {
                        name: "project".to_string(),
                        description: "Playwright project to use (default: chromium)".to_string(),
                        required: false,
                        default: Some(json!("chromium")),
                    },
                    InputSchema {
                        name: "timeout_secs".to_string(),
                        description: "Timeout in seconds (default: 120)".to_string(),
                        required: false,
                        default: Some(json!(120)),
                    },
                ],
            }),
            "mcp_call" => Some(ToolSchema {
                name: "mcp_call".to_string(),
                description: "Call an MCP server tool".to_string(),
                inputs: vec![
                    InputSchema {
                        name: "server_id".to_string(),
                        description: "MCP server ID".to_string(),
                        required: true,
                        default: None,
                    },
                    InputSchema {
                        name: "tool_name".to_string(),
                        description: "Tool name to call".to_string(),
                        required: true,
                        default: None,
                    },
                    InputSchema {
                        name: "arguments".to_string(),
                        description: "Tool arguments as JSON object".to_string(),
                        required: false,
                        default: None,
                    },
                    InputSchema {
                        name: "timeout_secs".to_string(),
                        description: "Timeout in seconds (default: 60)".to_string(),
                        required: false,
                        default: Some(json!(60)),
                    },
                ],
            }),
            _ => None,
        }
    }
}

/// Schema for a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub inputs: Vec<InputSchema>,
}

/// Schema for a tool input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputSchema {
    pub name: String,
    pub description: String,
    pub required: bool,
    pub default: Option<Value>,
}

// ============================================================================
// Tool Execution
// ============================================================================

/// Execute a unified workflow tool.
///
/// Returns `Some(StepExecutionResult)` if the tool was executed,
/// or `None` if the tool ID is not a unified workflow tool.
pub async fn execute_unified_tool(
    step_id: &str,
    tool_id: &str,
    inputs: &HashMap<String, Value>,
    _context: &HashMap<String, Value>,
) -> Option<StepExecutionResult> {
    match tool_id {
        "shell_command" => Some(execute_shell_command(step_id, inputs).await),
        "api_request" => Some(execute_api_request(step_id, inputs).await),
        "playwright_test" => Some(execute_playwright_test(step_id, inputs).await),
        "mcp_call" => Some(execute_mcp_call(step_id, inputs).await),
        _ => None,
    }
}

// ============================================================================
// Shell Command Execution
// ============================================================================

/// Execute a shell command.
async fn execute_shell_command(
    step_id: &str,
    inputs: &HashMap<String, Value>,
) -> StepExecutionResult {
    let command = match inputs.get("command").and_then(|v| v.as_str()) {
        Some(cmd) => cmd.to_string(),
        None => return StepExecutionResult::failure(step_id, "Missing required input 'command'"),
    };

    let working_directory = inputs
        .get("working_directory")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let timeout_secs = inputs
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(120);

    let fail_on_error = inputs
        .get("fail_on_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    info!(
        step_id = %step_id,
        command = %command,
        timeout_secs = timeout_secs,
        "Executing shell command via unified tool"
    );

    // Detect if command uses PowerShell syntax
    let is_powershell = command.contains("Get-")
        || command.contains("Set-")
        || command.contains("New-")
        || command.contains("Remove-")
        || command.contains("Invoke-")
        || command.contains("ForEach-Object")
        || command.contains("Where-Object")
        || command.contains("Select-Object")
        || command.contains("$_")
        || command.contains("$env:")
        || command.contains("-ErrorAction")
        || command.contains("| %")
        || command.contains("| ?");

    // Build the command
    let mut cmd = if cfg!(target_os = "windows") {
        if is_powershell {
            let mut c = Command::new("powershell");
            c.args(["-NoProfile", "-NonInteractive", "-Command", &command]);
            c
        } else {
            let mut c = Command::new("cmd");
            c.args(["/C", &command]);
            c
        }
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", &command]);
        c
    };

    // Set working directory if specified
    if let Some(ref wd) = working_directory {
        cmd.current_dir(wd);
    }

    // Capture stdout and stderr
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Execute with timeout
    let start = std::time::Instant::now();
    let timeout_duration = Duration::from_secs(timeout_secs);

    let output_result = timeout(timeout_duration, cmd.output()).await;
    let duration_ms = start.elapsed().as_millis() as u64;

    match output_result {
        Ok(Ok(output)) => {
            let exit_code = output.status.code();
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let success = output.status.success();

            info!(
                step_id = %step_id,
                success = success,
                exit_code = ?exit_code,
                duration_ms = duration_ms,
                "Shell command completed"
            );

            if success {
                StepExecutionResult::success(step_id, None)
                    .with_output("stdout", json!(stdout))
                    .with_output("stderr", json!(stderr))
                    .with_output("exit_code", json!(exit_code.unwrap_or(0)))
                    .with_duration(duration_ms)
            } else if fail_on_error {
                let error_msg = if !stderr.is_empty() {
                    format!(
                        "Command failed (exit code {:?}): {}",
                        exit_code,
                        stderr.trim()
                    )
                } else {
                    format!("Command failed with exit code {:?}", exit_code)
                };
                StepExecutionResult::failure(step_id, error_msg)
                    .with_output("stdout", json!(stdout))
                    .with_output("stderr", json!(stderr))
                    .with_output("exit_code", json!(exit_code.unwrap_or(-1)))
                    .with_duration(duration_ms)
            } else {
                // Return success but include error info
                StepExecutionResult::success(step_id, None)
                    .with_output("stdout", json!(stdout))
                    .with_output("stderr", json!(stderr))
                    .with_output("exit_code", json!(exit_code.unwrap_or(-1)))
                    .with_output("warning", json!("Command failed but fail_on_error=false"))
                    .with_duration(duration_ms)
            }
        }
        Ok(Err(e)) => {
            error!(step_id = %step_id, error = %e, "Failed to execute shell command");
            StepExecutionResult::failure(step_id, format!("Failed to execute command: {}", e))
                .with_duration(duration_ms)
        }
        Err(_) => {
            warn!(step_id = %step_id, timeout_secs = timeout_secs, "Shell command timed out");
            StepExecutionResult::failure(
                step_id,
                format!("Command timed out after {} seconds", timeout_secs),
            )
            .with_duration(duration_ms)
        }
    }
}

// ============================================================================
// API Request Execution
// ============================================================================

/// Execute an HTTP API request.
async fn execute_api_request(
    step_id: &str,
    inputs: &HashMap<String, Value>,
) -> StepExecutionResult {
    let method = match inputs.get("method").and_then(|v| v.as_str()) {
        Some(m) => m.to_uppercase(),
        None => return StepExecutionResult::failure(step_id, "Missing required input 'method'"),
    };

    let url = match inputs.get("url").and_then(|v| v.as_str()) {
        Some(u) => u.to_string(),
        None => return StepExecutionResult::failure(step_id, "Missing required input 'url'"),
    };

    let timeout_secs = inputs
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(30);

    info!(
        step_id = %step_id,
        method = %method,
        url = %url,
        timeout_secs = timeout_secs,
        "Executing API request via unified tool"
    );

    // Build the HTTP client with timeout
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return StepExecutionResult::failure(
                step_id,
                format!("Failed to create HTTP client: {}", e),
            )
        }
    };

    // Build the request
    let mut request = match method.as_str() {
        "GET" => client.get(&url),
        "POST" => client.post(&url),
        "PUT" => client.put(&url),
        "PATCH" => client.patch(&url),
        "DELETE" => client.delete(&url),
        "HEAD" => client.head(&url),
        _ => {
            return StepExecutionResult::failure(
                step_id,
                format!("Unsupported HTTP method: {}", method),
            )
        }
    };

    // Add headers
    if let Some(headers) = inputs.get("headers") {
        if let Some(obj) = headers.as_object() {
            for (key, value) in obj {
                if let Some(v) = value.as_str() {
                    request = request.header(key.as_str(), v);
                }
            }
        }
    }

    // Add content type
    if let Some(content_type) = inputs.get("content_type").and_then(|v| v.as_str()) {
        request = request.header("Content-Type", content_type);
    }

    // Add body
    if let Some(body) = inputs.get("body").and_then(|v| v.as_str()) {
        request = request.body(body.to_string());
    }

    // Execute the request
    let start = std::time::Instant::now();
    match request.send().await {
        Ok(response) => {
            let duration_ms = start.elapsed().as_millis() as u64;
            let status = response.status();
            let status_code = status.as_u16();

            // Get response headers
            let response_headers: HashMap<String, String> = response
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();

            match response.text().await {
                Ok(body) => {
                    info!(
                        step_id = %step_id,
                        status_code = status_code,
                        duration_ms = duration_ms,
                        "API request completed"
                    );

                    // Try to parse body as JSON
                    let body_json = serde_json::from_str::<Value>(&body).ok();

                    if status.is_success() {
                        let mut result = StepExecutionResult::success(step_id, None)
                            .with_output("status_code", json!(status_code))
                            .with_output("headers", json!(response_headers))
                            .with_duration(duration_ms);

                        if let Some(json_body) = body_json {
                            result = result.with_output("body", json_body);
                        } else {
                            result = result.with_output("body", json!(body));
                        }
                        result
                    } else {
                        StepExecutionResult::failure(
                            step_id,
                            format!(
                                "HTTP {}: {}",
                                status_code,
                                body.chars().take(500).collect::<String>()
                            ),
                        )
                        .with_output("status_code", json!(status_code))
                        .with_output("body", json!(body))
                        .with_output("headers", json!(response_headers))
                        .with_duration(duration_ms)
                    }
                }
                Err(e) => StepExecutionResult::failure(
                    step_id,
                    format!("Failed to read response body: {}", e),
                )
                .with_output("status_code", json!(status_code))
                .with_duration(start.elapsed().as_millis() as u64),
            }
        }
        Err(e) => {
            error!(step_id = %step_id, error = %e, "API request failed");
            StepExecutionResult::failure(step_id, format!("API request failed: {}", e))
                .with_duration(start.elapsed().as_millis() as u64)
        }
    }
}

// ============================================================================
// Playwright Test Execution
// ============================================================================

/// Execute a Playwright test.
async fn execute_playwright_test(
    step_id: &str,
    inputs: &HashMap<String, Value>,
) -> StepExecutionResult {
    let script_path = inputs.get("script_path").and_then(|v| v.as_str());
    let script_content = inputs.get("script_content").and_then(|v| v.as_str());
    let target_url = inputs.get("target_url").and_then(|v| v.as_str());
    let project = inputs
        .get("project")
        .and_then(|v| v.as_str())
        .unwrap_or("chromium");
    let timeout_secs = inputs
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(120);

    // Either script_path or script_content is required
    if script_path.is_none() && script_content.is_none() {
        return StepExecutionResult::failure(
            step_id,
            "Either 'script_path' or 'script_content' is required",
        );
    }

    info!(
        step_id = %step_id,
        script_path = ?script_path,
        has_content = script_content.is_some(),
        project = %project,
        timeout_secs = timeout_secs,
        "Executing Playwright test via unified tool"
    );

    // If script_content is provided, write it to a temp file
    let temp_script_path: Option<std::path::PathBuf> = if let Some(content) = script_content {
        let temp_dir = std::env::temp_dir();
        let script_file = temp_dir.join(format!("playwright_test_{}.spec.ts", step_id));

        // Wrap content if it doesn't have test structure
        let full_content = if content.contains("test(") || content.contains("test.describe") {
            content.to_string()
        } else {
            // Generate a simple test wrapper
            let url = target_url.unwrap_or("about:blank");
            format!(
                r#"import {{ test, expect }} from '@playwright/test';

test('Flow Designer Test', async ({{ page }}) => {{
    await page.goto('{}');
    {}
}});
"#,
                url, content
            )
        };

        if let Err(e) = std::fs::write(&script_file, full_content) {
            return StepExecutionResult::failure(
                step_id,
                format!("Failed to write temp script: {}", e),
            );
        }
        Some(script_file)
    } else {
        None
    };

    let test_path = temp_script_path
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .or_else(|| script_path.map(|s| s.to_string()))
        .unwrap();

    // Build the Playwright command
    let mut cmd = Command::new("npx");
    cmd.args([
        "playwright",
        "test",
        &test_path,
        "--project",
        project,
        "--reporter",
        "json",
    ]);

    // Set environment variable to skip web server
    cmd.env("SKIP_WEB_SERVER", "1");

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Execute with timeout
    let start = std::time::Instant::now();
    let timeout_duration = Duration::from_secs(timeout_secs);

    let output_result = timeout(timeout_duration, cmd.output()).await;
    let duration_ms = start.elapsed().as_millis() as u64;

    // Clean up temp file
    if let Some(temp_path) = temp_script_path {
        let _ = std::fs::remove_file(temp_path);
    }

    match output_result {
        Ok(Ok(output)) => {
            let exit_code = output.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let success = output.status.success();

            info!(
                step_id = %step_id,
                success = success,
                exit_code = exit_code,
                duration_ms = duration_ms,
                "Playwright test completed"
            );

            // Try to parse JSON output
            let test_results = serde_json::from_str::<Value>(&stdout).ok();

            if success {
                let mut result = StepExecutionResult::success(step_id, None)
                    .with_output("exit_code", json!(exit_code))
                    .with_output("stderr", json!(stderr))
                    .with_duration(duration_ms);

                if let Some(results) = test_results {
                    result = result.with_output("results", results);
                } else {
                    result = result.with_output("stdout", json!(stdout));
                }
                result
            } else {
                let mut result = StepExecutionResult::failure(
                    step_id,
                    format!("Playwright test failed with exit code {}", exit_code),
                )
                .with_output("exit_code", json!(exit_code))
                .with_output("stderr", json!(stderr))
                .with_duration(duration_ms);

                if let Some(results) = test_results {
                    result = result.with_output("results", results);
                } else {
                    result = result.with_output("stdout", json!(stdout));
                }
                result
            }
        }
        Ok(Err(e)) => {
            error!(step_id = %step_id, error = %e, "Failed to execute Playwright test");
            StepExecutionResult::failure(step_id, format!("Failed to execute Playwright: {}", e))
                .with_duration(duration_ms)
        }
        Err(_) => {
            warn!(step_id = %step_id, timeout_secs = timeout_secs, "Playwright test timed out");
            StepExecutionResult::failure(
                step_id,
                format!("Playwright test timed out after {} seconds", timeout_secs),
            )
            .with_duration(duration_ms)
        }
    }
}

// ============================================================================
// MCP Call Execution
// ============================================================================

/// Execute an MCP tool call.
async fn execute_mcp_call(step_id: &str, inputs: &HashMap<String, Value>) -> StepExecutionResult {
    let server_id = match inputs.get("server_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return StepExecutionResult::failure(step_id, "Missing required input 'server_id'"),
    };

    let tool_name = match inputs.get("tool_name").and_then(|v| v.as_str()) {
        Some(name) => name.to_string(),
        None => return StepExecutionResult::failure(step_id, "Missing required input 'tool_name'"),
    };

    let arguments = inputs.get("arguments").cloned().unwrap_or(json!({}));

    let timeout_secs = inputs
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(60);

    info!(
        step_id = %step_id,
        server_id = %server_id,
        tool_name = %tool_name,
        timeout_secs = timeout_secs,
        "Executing MCP call via unified tool"
    );

    // Build the request to the runner's MCP API
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return StepExecutionResult::failure(
                step_id,
                format!("Failed to create HTTP client: {}", e),
            )
        }
    };

    // Call the runner's MCP tool execution endpoint
    let url = format!(
        "http://localhost:9876/api/mcp/servers/{}/tools/{}/execute",
        server_id, tool_name
    );

    let start = std::time::Instant::now();
    match client.post(&url).json(&arguments).send().await {
        Ok(response) => {
            let duration_ms = start.elapsed().as_millis() as u64;
            let status = response.status();
            let status_code = status.as_u16();

            match response.json::<Value>().await {
                Ok(result) => {
                    info!(
                        step_id = %step_id,
                        status_code = status_code,
                        duration_ms = duration_ms,
                        "MCP call completed"
                    );

                    if status.is_success() {
                        // Extract outputs from result
                        let mut step_result = StepExecutionResult::success(step_id, None)
                            .with_output("tool_name", json!(tool_name))
                            .with_output("server_id", json!(server_id))
                            .with_duration(duration_ms);

                        // Add result fields as outputs
                        if let Some(obj) = result.as_object() {
                            for (key, value) in obj {
                                step_result = step_result.with_output(key.clone(), value.clone());
                            }
                        } else {
                            step_result = step_result.with_output("result", result);
                        }
                        step_result
                    } else {
                        let error_msg = result
                            .get("error")
                            .and_then(|e| e.as_str())
                            .unwrap_or("MCP call failed");
                        StepExecutionResult::failure(step_id, error_msg)
                            .with_output("status_code", json!(status_code))
                            .with_output("response", result)
                            .with_duration(duration_ms)
                    }
                }
                Err(e) => StepExecutionResult::failure(
                    step_id,
                    format!("Failed to parse MCP response: {}", e),
                )
                .with_output("status_code", json!(status_code))
                .with_duration(duration_ms),
            }
        }
        Err(e) => {
            let duration_ms = start.elapsed().as_millis() as u64;
            error!(step_id = %step_id, error = %e, "MCP call failed");
            StepExecutionResult::failure(step_id, format!("MCP call failed: {}", e))
                .with_duration(duration_ms)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry() {
        assert!(UnifiedToolRegistry::is_unified_tool("shell_command"));
        assert!(UnifiedToolRegistry::is_unified_tool("api_request"));
        assert!(UnifiedToolRegistry::is_unified_tool("playwright_test"));
        assert!(UnifiedToolRegistry::is_unified_tool("mcp_call"));
        assert!(!UnifiedToolRegistry::is_unified_tool("unknown_tool"));
    }

    #[test]
    fn test_tool_schemas() {
        let schema = UnifiedToolRegistry::get_tool_schema("shell_command");
        assert!(schema.is_some());
        let schema = schema.unwrap();
        assert_eq!(schema.name, "shell_command");
        assert!(schema
            .inputs
            .iter()
            .any(|i| i.name == "command" && i.required));

        let schema = UnifiedToolRegistry::get_tool_schema("api_request");
        assert!(schema.is_some());
        let schema = schema.unwrap();
        assert!(schema
            .inputs
            .iter()
            .any(|i| i.name == "method" && i.required));
        assert!(schema.inputs.iter().any(|i| i.name == "url" && i.required));
    }

    #[tokio::test]
    async fn test_unknown_tool() {
        let result =
            execute_unified_tool("step1", "unknown_tool", &HashMap::new(), &HashMap::new()).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_shell_command_missing_input() {
        let result =
            execute_unified_tool("step1", "shell_command", &HashMap::new(), &HashMap::new()).await;
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("command"));
    }

    #[tokio::test]
    async fn test_api_request_missing_inputs() {
        let result =
            execute_unified_tool("step1", "api_request", &HashMap::new(), &HashMap::new()).await;
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("method"));
    }
}
