//! Python Script Test Runner
//!
//! Executes user-defined Python verification scripts that output structured JSON results.
//! Scripts can perform custom verification logic like log analysis, API checks, etc.

use super::types::{AssertionResult, TestDefinition, TestExecutionResult, TestStatus, TestType};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::process::Command;
use tracing::{info, warn};
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

/// Generate a wrapper script that provides helpers and enforces JSON output
fn generate_python_wrapper(user_code: &str, env_vars: &HashMap<String, String>) -> String {
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
    _result["output"] += f"\nException: {{e}}\n"
    _result["output"] += traceback.format_exc()

# Output JSON result
print(json.dumps(_result))
"#,
        env_dict = env_dict,
        user_code = indent_code(user_code, 4)
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

    // Create temp directory for scripts
    let temp_dir = std::env::temp_dir().join("qontinui-verification");
    if let Err(e) = fs::create_dir_all(&temp_dir) {
        return TestExecutionResult::error(
            test_def.id.clone(),
            test_def.name.clone(),
            format!("Failed to create temp directory: {}", e),
        );
    }

    // Generate wrapped script
    let wrapped_script = generate_python_wrapper(&python_code, &env_vars);

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
            match serde_json::from_str::<PythonScriptOutput>(&stdout) {
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
        let wrapper = generate_python_wrapper(code, &env);
        assert!(wrapper.contains("def assertion"));
        assert!(wrapper.contains("json.dumps(_result)"));
        assert!(wrapper.contains("assertion('test', True)"));
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
}
