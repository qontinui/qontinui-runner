//! Dry Run / Simulation
//!
//! Simulates workflow execution by validating that all referenced commands,
//! URLs, and files exist without actually running steps. Useful for pre-flight
//! checks before committing to a full execution.

use serde::{Deserialize, Serialize};
use std::time::Instant;
use tracing::info;

/// Result of a dry run simulation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DryRunResult {
    /// Whether all pre-flight checks passed.
    pub all_passed: bool,
    /// Results per step.
    pub step_results: Vec<DryRunStepResult>,
    /// Total duration of the dry run.
    pub duration_ms: u64,
    /// Summary of issues found.
    pub summary: String,
}

/// Pre-flight result for a single step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DryRunStepResult {
    /// Step ID.
    pub step_id: String,
    /// Step name.
    pub step_name: String,
    /// Step type.
    pub step_type: String,
    /// Whether this step's pre-flight checks passed.
    pub passed: bool,
    /// Issues found.
    pub issues: Vec<DryRunIssue>,
}

/// A single issue discovered during dry run validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DryRunIssue {
    /// Issue severity.
    pub severity: DryRunSeverity,
    /// What was checked.
    pub check: String,
    /// What went wrong.
    pub message: String,
}

/// Severity level for dry run issues.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DryRunSeverity {
    /// Will definitely fail at runtime.
    Error,
    /// Might fail at runtime.
    Warning,
    /// Informational only.
    Info,
}

/// Run a dry-run simulation of a workflow.
pub fn simulate_workflow(workflow_json: &serde_json::Value) -> DryRunResult {
    let start = Instant::now();
    let mut step_results = Vec::new();

    // Collect all steps from all phases
    let phases = [
        "setup_steps",
        "verification_steps",
        "agentic_steps",
        "completion_steps",
    ];

    for phase in &phases {
        if let Some(steps) = workflow_json.get(phase).and_then(|v| v.as_array()) {
            for step in steps {
                step_results.push(validate_step(step));
            }
        }
    }

    // Also check steps within stages
    if let Some(stages) = workflow_json.get("stages").and_then(|v| v.as_array()) {
        for stage in stages {
            for phase in &phases {
                if let Some(steps) = stage.get(phase).and_then(|v| v.as_array()) {
                    for step in steps {
                        step_results.push(validate_step(step));
                    }
                }
            }
        }
    }

    let all_passed = step_results.iter().all(|r| r.passed);
    let error_count = step_results
        .iter()
        .flat_map(|r| r.issues.iter())
        .filter(|i| matches!(i.severity, DryRunSeverity::Error))
        .count();
    let warning_count = step_results
        .iter()
        .flat_map(|r| r.issues.iter())
        .filter(|i| matches!(i.severity, DryRunSeverity::Warning))
        .count();

    let summary = format!(
        "Dry run: {} steps checked, {} errors, {} warnings",
        step_results.len(),
        error_count,
        warning_count
    );

    info!("{}", summary);

    DryRunResult {
        all_passed,
        step_results,
        duration_ms: start.elapsed().as_millis() as u64,
        summary,
    }
}

/// Validate a single step without executing it.
fn validate_step(step: &serde_json::Value) -> DryRunStepResult {
    let step_id = step
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let step_name = step
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("unnamed")
        .to_string();
    let step_type = step
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let mut issues = Vec::new();

    match step_type.as_str() {
        "command" => validate_command_step(step, &mut issues),
        "ui_bridge" => validate_ui_bridge_step(step, &mut issues),
        "prompt" => validate_prompt_step(step, &mut issues),
        _ => {
            issues.push(DryRunIssue {
                severity: DryRunSeverity::Warning,
                check: "step_type".to_string(),
                message: format!("Unknown step type: {}", step_type),
            });
        }
    }

    let passed = !issues
        .iter()
        .any(|i| matches!(i.severity, DryRunSeverity::Error));

    DryRunStepResult {
        step_id,
        step_name,
        step_type,
        passed,
        issues,
    }
}

fn validate_command_step(step: &serde_json::Value, issues: &mut Vec<DryRunIssue>) {
    // Check that command field exists and is non-empty
    match step.get("command").and_then(|v| v.as_str()) {
        Some(cmd) if cmd.is_empty() => {
            issues.push(DryRunIssue {
                severity: DryRunSeverity::Error,
                check: "command_empty".to_string(),
                message: "Command step has empty command".to_string(),
            });
        }
        Some(cmd) => {
            // Check for common issues
            // 1. Verify the base command exists (first word)
            let base_cmd = cmd.split_whitespace().next().unwrap_or("");
            // Check if it's a file path reference
            if base_cmd.starts_with("./")
                || base_cmd.starts_with('/')
                || base_cmd.starts_with("..")
            {
                issues.push(DryRunIssue {
                    severity: DryRunSeverity::Info,
                    check: "command_path".to_string(),
                    message: format!(
                        "Command references path '{}' — verify it exists at runtime",
                        base_cmd
                    ),
                });
            }

            // 2. Check for potential injection issues
            if cmd.contains("rm -rf /") || cmd.contains("rm -rf ~") {
                issues.push(DryRunIssue {
                    severity: DryRunSeverity::Error,
                    check: "dangerous_command".to_string(),
                    message: "Command contains potentially dangerous recursive delete".to_string(),
                });
            }

            // 3. Check working_directory if specified
            if let Some(wd) = step.get("working_directory").and_then(|v| v.as_str()) {
                if wd.is_empty() {
                    issues.push(DryRunIssue {
                        severity: DryRunSeverity::Warning,
                        check: "working_directory_empty".to_string(),
                        message: "Working directory is set but empty".to_string(),
                    });
                }
            }
        }
        None => {
            // Check if it has check_type instead (some command steps use check_type)
            if step.get("check_type").is_none() {
                issues.push(DryRunIssue {
                    severity: DryRunSeverity::Error,
                    check: "command_missing".to_string(),
                    message: "Command step has no command or check_type".to_string(),
                });
            }
        }
    }

    // Check URL fields if present
    validate_url_field(step, "url", issues);
    validate_url_field(step, "health_check_url", issues);

    // Check expected_status is valid HTTP status
    if let Some(status) = step.get("expected_status").and_then(|v| v.as_u64()) {
        if status < 100 || status > 599 {
            issues.push(DryRunIssue {
                severity: DryRunSeverity::Error,
                check: "expected_status_range".to_string(),
                message: format!(
                    "Expected HTTP status {} is outside valid range 100-599",
                    status
                ),
            });
        }
    }
}

fn validate_ui_bridge_step(step: &serde_json::Value, issues: &mut Vec<DryRunIssue>) {
    // UI Bridge steps need either an action or an assertion
    let has_action = step.get("action").is_some();
    let has_assertions = step
        .get("assertions")
        .and_then(|v| v.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    let has_snapshot =
        step.get("snapshot").is_some() || step.get("snapshot_assert").is_some();

    if !has_action && !has_assertions && !has_snapshot {
        issues.push(DryRunIssue {
            severity: DryRunSeverity::Warning,
            check: "ui_bridge_no_operation".to_string(),
            message: "UI Bridge step has no action, assertions, or snapshot — it won't do anything useful".to_string(),
        });
    }

    // Check target_url if present
    validate_url_field(step, "target_url", issues);
}

fn validate_prompt_step(step: &serde_json::Value, issues: &mut Vec<DryRunIssue>) {
    // Prompt steps need a non-empty prompt field
    match step.get("prompt").and_then(|v| v.as_str()) {
        Some(prompt) if prompt.len() < 10 => {
            issues.push(DryRunIssue {
                severity: DryRunSeverity::Warning,
                check: "prompt_too_short".to_string(),
                message: format!(
                    "Prompt is very short ({} chars) — may not provide enough guidance",
                    prompt.len()
                ),
            });
        }
        Some(_) => {} // OK
        None => {
            issues.push(DryRunIssue {
                severity: DryRunSeverity::Error,
                check: "prompt_missing".to_string(),
                message: "Prompt step has no prompt field".to_string(),
            });
        }
    }
}

fn validate_url_field(step: &serde_json::Value, field: &str, issues: &mut Vec<DryRunIssue>) {
    if let Some(url) = step.get(field).and_then(|v| v.as_str()) {
        if url.is_empty() {
            issues.push(DryRunIssue {
                severity: DryRunSeverity::Warning,
                check: format!("{}_empty", field),
                message: format!("'{}' field is set but empty", field),
            });
        } else if !url.starts_with("http://")
            && !url.starts_with("https://")
            && !url.starts_with("{{")
        {
            issues.push(DryRunIssue {
                severity: DryRunSeverity::Warning,
                check: format!("{}_scheme", field),
                message: format!(
                    "'{}' value '{}' doesn't start with http:// or https://",
                    field, url
                ),
            });
        }
    }
}
