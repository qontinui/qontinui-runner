//! Python Script Test Runner
//!
//! Executes user-defined Python verification scripts that output structured JSON results.
//! Scripts can perform custom verification logic like log analysis, API checks, etc.
//!
//! ## API Request Integration
//!
//! Tests can include an `api_request_config` in their config to execute an HTTP request
//! before running the Python script. The API response is injected as `_api_response`
//! variable in the script, providing access to:
//! - `_api_response["status_code"]` - HTTP status code
//! - `_api_response["response_body"]` - Response body (JSON or text)
//! - `_api_response["response_headers"]` - Response headers dict
//! - `_api_response["response_time_ms"]` - Response time in milliseconds
//! - `_api_response["extractions"]` - Extracted variables from JSON paths
//! - `_api_response["assertions"]` - Pre-evaluated assertion results

use super::types::{AssertionResult, TestDefinition, TestExecutionResult, TestStatus, TestType};
use crate::api_request::executor::ApiRequestExecutor;
use crate::api_request::types::ApiRequestConfig;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::process::Command;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Expected output format from Python verification scripts
#[derive(Debug, Deserialize)]
struct PythonScriptOutput {
    /// Overall status: "passed", "failed", "error"
    status: String,
    /// Individual assertion results
    #[serde(default)]
    assertions: Vec<PythonAssertion>,
    /// Freeform output text
    #[serde(default)]
    output: String,
    /// Error message if status is "error"
    #[serde(default)]
    error: Option<String>,
    /// Custom metrics collected during verification
    #[serde(default)]
    metrics: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct PythonAssertion {
    name: String,
    passed: bool,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    expected: Option<serde_json::Value>,
    #[serde(default)]
    actual: Option<serde_json::Value>,
}

/// Extract the last JSON object from a string containing multiple JSON outputs.
/// This handles the case where user code prints JSON before the wrapper.
/// The wrapper always prints last, so we want that JSON.
fn extract_last_json(input: &str) -> Option<String> {
    // Split by common line endings and find the last valid JSON
    let lines: Vec<&str> = input.lines().collect();

    // Work backwards through lines to find the last complete JSON
    for i in (0..lines.len()).rev() {
        let line = lines[i].trim();
        if line.starts_with('{') && line.ends_with('}') {
            // This looks like a complete JSON object on a single line
            // Try to parse it to make sure it's valid
            if serde_json::from_str::<serde_json::Value>(line).is_ok() {
                return Some(line.to_string());
            }
        }
    }

    // If no single-line JSON found, try to find the last JSON object
    // by looking for the last '{' and matching '}'
    if let Some(last_open) = input.rfind('{') {
        let potential_json = &input[last_open..];
        // Find where this JSON object ends
        let mut depth = 0;
        let mut in_string = false;
        let mut escape_next = false;

        for (byte_idx, c) in potential_json.char_indices() {
            if escape_next {
                escape_next = false;
                continue;
            }
            if c == '\\' && in_string {
                escape_next = true;
                continue;
            }
            if c == '"' {
                in_string = !in_string;
                continue;
            }
            if !in_string {
                if c == '{' {
                    depth += 1;
                } else if c == '}' {
                    depth -= 1;
                    if depth == 0 {
                        // byte_idx is the start of '}', add char length to include it
                        let end_byte = byte_idx + c.len_utf8();
                        let json_str = &potential_json[..end_byte];
                        if serde_json::from_str::<serde_json::Value>(json_str).is_ok() {
                            return Some(json_str.to_string());
                        }
                        break;
                    }
                }
            }
        }
    }

    None
}

/// Generate a wrapper script that provides helpers and enforces JSON output
fn generate_python_wrapper(
    user_code: &str,
    env_vars: &HashMap<String, String>,
    api_response_json: Option<&str>,
) -> String {
    // Convert env vars to Python dict literal
    let env_dict: String = env_vars
        .iter()
        .map(|(k, v)| {
            format!(
                "    \"{}\": \"{}\",",
                k,
                v.replace('\\', "\\\\").replace('"', "\\\"")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    // API response injection
    let api_response_code = match api_response_json {
        Some(json_str) => {
            format!(
                r#"
# API Response from pre-request (captured from browser extension or configured API request)
# Available fields:
#   _api_response["status_code"] - HTTP status code (int)
#   _api_response["response_body"] - Response body (str, may be JSON)
#   _api_response["response_headers"] - Headers dict
#   _api_response["response_time_ms"] - Response time in ms (int)
#   _api_response["extractions"] - Extracted variables list
#   _api_response["assertions"] - Pre-evaluated assertion results
_api_response = json.loads('''{}''')

# Convenience: parse response body as JSON if possible
try:
    if _api_response.get("response_body"):
        _api_response["json"] = json.loads(_api_response["response_body"])
except (json.JSONDecodeError, TypeError):
    _api_response["json"] = None
"#,
                json_str.replace("'''", r"\'\'\'")
            )
        }
        None => String::from(
            "\n# No API request configured - _api_response is not available\n_api_response = None\n",
        ),
    };

    let indented_user_code = indent_code(user_code, 4);

    format!(
        r#"#!/usr/bin/env python3
"""
Qontinui Verification Script Wrapper
This wrapper ensures the user's script outputs proper JSON.
"""

import json
import os
import sys
import traceback
from pathlib import Path

# Inject environment variables from test config
_env_vars = {{
{env_dict}
}}
for k, v in _env_vars.items():
    os.environ[k] = v
{api_response_code}
# Result structure
_result = {{
    "status": "error",
    "assertions": [],
    "output": "",
    "error": None,
    "metrics": {{}}
}}

# Helper functions available to user scripts
def assertion(name: str, passed: bool, message: str = None, expected=None, actual=None):
    """Record an assertion result."""
    _result["assertions"].append({{
        "name": name,
        "passed": passed,
        "message": message,
        "expected": expected,
        "actual": actual
    }})

def log(msg: str):
    """Add a log message to output."""
    _result["output"] += msg + "\n"

def metric(name: str, value):
    """Record a custom metric."""
    _result["metrics"][name] = value

def set_status(status: str):
    """Set overall status: 'passed', 'failed', or 'error'."""
    _result["status"] = status

def fail(message: str):
    """Mark the test as failed with a message."""
    _result["status"] = "failed"
    _result["error"] = message

def error(message: str):
    """Mark the test as error with a message."""
    _result["status"] = "error"
    _result["error"] = message

# Run user's verification code
try:
    # User code starts here
{user_code}
    # User code ends here

    # Auto-determine status from assertions if not explicitly set
    if _result["status"] == "error" and _result["assertions"]:
        all_passed = all(a["passed"] for a in _result["assertions"])
        _result["status"] = "passed" if all_passed else "failed"
    elif _result["status"] == "error":
        # No assertions and no explicit status - assume passed
        _result["status"] = "passed"

except Exception as e:
    _result["status"] = "error"
    _result["error"] = str(e)
    _result["output"] += f"\nException: {{{{e}}}}\n"
    _result["output"] += traceback.format_exc()

# Output JSON result
print(json.dumps(_result))
"#,
        env_dict = env_dict,
        api_response_code = api_response_code,
        user_code = indented_user_code
    )
}

/// Indent code by a number of spaces
fn indent_code(code: &str, spaces: usize) -> String {
    let indent = " ".repeat(spaces);
    code.lines()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                format!("{}{}", indent, line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Execute an API request and return the result as JSON string
fn execute_api_request(config: &ApiRequestConfig) -> Result<String, String> {
    // Use tokio runtime to execute async code
    let runtime = match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle,
        Err(_) => {
            // Create a new runtime if not in async context
            return Err("No async runtime available for API request".to_string());
        }
    };

    let executor = ApiRequestExecutor::new();
    let config = config.clone();

    // Block on the async execution
    let result = std::thread::spawn(move || {
        runtime.block_on(async move { executor.execute(&config, None).await })
    })
    .join()
    .map_err(|_| "API request thread panicked".to_string())?
    .map_err(|e| format!("API request failed: {}", e))?;

    // Serialize the result to JSON
    serde_json::to_string(&result).map_err(|e| format!("Failed to serialize API response: {}", e))
}

/// Execute a Python verification script
pub fn execute_python_script(test_def: &TestDefinition) -> TestExecutionResult {
    let start_time = std::time::Instant::now();

    // Validate test type
    if test_def.test_type != TestType::PythonScript {
        return TestExecutionResult::error(
            test_def.id.clone(),
            test_def.name.clone(),
            "Invalid test type: expected PythonScript".to_string(),
        );
    }

    // Get the Python code
    let python_code = match &test_def.python_code {
        Some(code) if !code.trim().is_empty() => code.clone(),
        _ => {
            return TestExecutionResult::error(
                test_def.id.clone(),
                test_def.name.clone(),
                "No Python code provided".to_string(),
            );
        }
    };

    // Get environment variables from config
    let env_vars: HashMap<String, String> = test_def
        .config
        .get("env_vars")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    // Check for API request configuration
    let api_response_json: Option<String> = test_def
        .config
        .get("api_request_config")
        .and_then(|v| serde_json::from_value::<ApiRequestConfig>(v.clone()).ok())
        .and_then(|api_config| {
            info!(
                "Executing API request before Python script: {} {}",
                api_config.method, api_config.url
            );
            match execute_api_request(&api_config) {
                Ok(json) => {
                    info!("API request successful, injecting response into Python script");
                    Some(json)
                }
                Err(e) => {
                    error!("API request failed: {}", e);
                    // Return None to indicate no API response available
                    // The Python script can check if _api_response is None
                    None
                }
            }
        });

    // Create temp directory for scripts
    let temp_dir = std::env::temp_dir().join("qontinui-verification");
    if let Err(e) = fs::create_dir_all(&temp_dir) {
        return TestExecutionResult::error(
            test_def.id.clone(),
            test_def.name.clone(),
            format!("Failed to create temp directory: {}", e),
        );
    }

    // Generate wrapped script with optional API response
    let wrapped_script =
        generate_python_wrapper(&python_code, &env_vars, api_response_json.as_deref());

    // Write to temp file
    let script_filename = format!("verify-{}.py", Uuid::new_v4());
    let script_path = temp_dir.join(&script_filename);
    if let Err(e) = fs::write(&script_path, &wrapped_script) {
        return TestExecutionResult::error(
            test_def.id.clone(),
            test_def.name.clone(),
            format!("Failed to write script: {}", e),
        );
    }

    info!(
        "Executing Python verification script: {} (timeout: {}s)",
        test_def.name, test_def.timeout_seconds
    );

    // Find Python executable
    let python_cmd = if cfg!(target_os = "windows") {
        "python"
    } else {
        "python3"
    };

    // Execute the script
    let output = Command::new(python_cmd)
        .arg(&script_path)
        .envs(&env_vars)
        .output();

    // Clean up the script file
    let _ = fs::remove_file(&script_path);

    let duration_ms = start_time.elapsed().as_millis() as u64;

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            if !stderr.is_empty() {
                warn!("Python stderr: {}", stderr);
            }

            // Try to parse JSON output
            // First try to parse the whole stdout as JSON
            // If that fails (e.g., multiple JSON objects), try to extract the LAST JSON object
            // The wrapper always outputs the last JSON, so that's the one we want
            let json_to_parse = match serde_json::from_str::<PythonScriptOutput>(&stdout) {
                Ok(_) => stdout.to_string(),
                Err(_) => {
                    // Try to extract the last JSON object from stdout
                    // This handles the case where user code also prints JSON before the wrapper
                    extract_last_json(&stdout).unwrap_or_else(|| stdout.to_string())
                }
            };

            match serde_json::from_str::<PythonScriptOutput>(&json_to_parse) {
                Ok(script_output) => {
                    let status = match script_output.status.as_str() {
                        "passed" => TestStatus::Passed,
                        "failed" => TestStatus::Failed,
                        _ => TestStatus::Error,
                    };

                    let assertions: Vec<AssertionResult> = script_output
                        .assertions
                        .into_iter()
                        .map(|a| AssertionResult {
                            name: a.name,
                            passed: a.passed,
                            message: a.message,
                            expected: a.expected,
                            actual: a.actual,
                            duration_ms: None,
                            metadata: Default::default(),
                        })
                        .collect();

                    let assertions_passed = assertions.iter().filter(|a| a.passed).count() as u32;
                    let assertions_failed = assertions.iter().filter(|a| !a.passed).count() as u32;

                    let mut result = match status {
                        TestStatus::Passed => TestExecutionResult::passed(
                            test_def.id.clone(),
                            test_def.name.clone(),
                            duration_ms,
                            script_output.output,
                        ),
                        TestStatus::Failed => TestExecutionResult::failed(
                            test_def.id.clone(),
                            test_def.name.clone(),
                            duration_ms,
                            script_output.output,
                            script_output
                                .error
                                .unwrap_or_else(|| "Test failed".to_string()),
                        ),
                        _ => TestExecutionResult::error(
                            test_def.id.clone(),
                            test_def.name.clone(),
                            script_output
                                .error
                                .unwrap_or_else(|| "Unknown error".to_string()),
                        ),
                    };

                    result.assertions = assertions;
                    result.assertions_passed = assertions_passed;
                    result.assertions_failed = assertions_failed;

                    // Store metrics in structured output
                    if !script_output.metrics.is_empty() {
                        result.structured_output =
                            Some(serde_json::json!({ "metrics": script_output.metrics }));
                    }

                    info!(
                        "Python script complete: status={:?}, assertions_passed={}, assertions_failed={}",
                        status, assertions_passed, assertions_failed
                    );

                    result
                }
                Err(e) => {
                    // Failed to parse JSON - script output was invalid
                    warn!("Failed to parse Python output as JSON: {}", e);
                    TestExecutionResult::error(
                        test_def.id.clone(),
                        test_def.name.clone(),
                        format!(
                            "Invalid script output (not JSON): {}\nStdout: {}\nStderr: {}",
                            e, stdout, stderr
                        ),
                    )
                }
            }
        }
        Err(e) => TestExecutionResult::error(
            test_def.id.clone(),
            test_def.name.clone(),
            format!("Failed to execute Python: {}", e),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_indent_code() {
        let code = "x = 1\ny = 2";
        let indented = indent_code(code, 4);
        assert!(indented.starts_with("    x = 1"));
        assert!(indented.contains("\n    y = 2"));
    }

    #[test]
    fn test_generate_wrapper() {
        let code = "assertion('test', True)";
        let env = HashMap::new();
        let wrapper = generate_python_wrapper(code, &env, None);
        assert!(wrapper.contains("def assertion"));
        assert!(wrapper.contains("json.dumps(_result)"));
        assert!(wrapper.contains("assertion('test', True)"));
        assert!(wrapper.contains("_api_response = None"));
    }

    #[test]
    fn test_generate_wrapper_with_api_response() {
        let code = "log(_api_response['status_code'])";
        let env = HashMap::new();
        let api_response = r#"{"status_code": 200, "response_body": "{\"data\": 42}"}"#;
        let wrapper = generate_python_wrapper(code, &env, Some(api_response));
        assert!(wrapper.contains("_api_response = json.loads"));
        assert!(wrapper.contains("status_code"));
        assert!(wrapper.contains("_api_response[\"json\"]"));
    }

    #[test]
    fn test_parse_script_output() {
        let json = r#"{
            "status": "passed",
            "assertions": [
                {"name": "test1", "passed": true, "message": "OK"}
            ],
            "output": "Test output",
            "metrics": {"count": 42}
        }"#;

        let output: PythonScriptOutput = serde_json::from_str(json).unwrap();
        assert_eq!(output.status, "passed");
        assert_eq!(output.assertions.len(), 1);
        assert!(output.assertions[0].passed);
    }

    #[test]
    fn test_extract_last_json_single_line() {
        // Test with two JSON objects on separate lines (like the actual error case)
        let input = r#"{"status": "error", "assertions": [], "output": "Log file not found", "metrics": {}}
{"status": "passed", "assertions": [], "output": "", "error": null, "metrics": {}}"#;

        let result = extract_last_json(input);
        assert!(result.is_some());

        let json: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(json["status"], "passed");
        assert_eq!(json["error"], serde_json::Value::Null);
    }

    #[test]
    fn test_extract_last_json_with_carriage_return() {
        // Test with CRLF line endings (Windows)
        let input = "{\"status\": \"error\", \"output\": \"first\"}\r\n{\"status\": \"passed\", \"output\": \"second\"}";

        let result = extract_last_json(input);
        assert!(result.is_some());

        let json: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(json["status"], "passed");
    }

    #[test]
    fn test_extract_last_json_single_object() {
        // Test with a single JSON object (should still work)
        let input = r#"{"status": "passed", "assertions": [], "metrics": {}}"#;

        let result = extract_last_json(input);
        assert!(result.is_some());

        let json: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(json["status"], "passed");
    }

    #[test]
    fn test_extract_last_json_with_nested_objects() {
        // Test with nested JSON to ensure brace matching works
        let input = r#"{"outer": {"inner": {}}}
{"status": "passed", "data": {"nested": {"deep": 1}}}"#;

        let result = extract_last_json(input);
        assert!(result.is_some());

        let json: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(json["status"], "passed");
    }

    #[test]
    fn test_extract_last_json_with_strings_containing_braces() {
        // Test with strings containing braces (should not confuse the parser)
        let input = r#"{"text": "{ not json }"}
{"status": "passed", "msg": "has { and } in string"}"#;

        let result = extract_last_json(input);
        assert!(result.is_some());

        let json: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(json["status"], "passed");
    }
}
