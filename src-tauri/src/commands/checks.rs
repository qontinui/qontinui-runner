//! Code Quality Check Commands
//!
//! Provides Tauri commands for managing and executing code quality checks.
//! Supports linting, formatting, type checking, and custom commands.
//!
//! # Supported Tools
//! - Python: black, isort, ruff, mypy, pyright
//! - JavaScript/TypeScript: eslint, prettier, tsc, biome
//! - Rust: clippy, rustfmt, cargo check
//! - Custom: Any command-line tool

use super::CommandResponse;
use crate::check_executor::{
    detect_project_checks, execute_check, execute_check_suite, CheckDefinition,
    CheckExecutionResult, CheckSuiteSummary, CheckToolInfoSerialized, ProjectDetectionResult,
    CHECK_TOOLS, CHECK_TYPE_INFO,
};
use crate::database::{
    Check, CreateCheckGroupInput, CreateCheckInput, UpdateCheckGroupInput, UpdateCheckInput,
};
use crate::AppState;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;
use tracing::{error, info};

// ============================================================================
// Request/Response Types
// ============================================================================

/// Request to execute a single check
#[derive(Debug, Serialize, Deserialize)]
pub struct ExecuteCheckRequest {
    pub check_definition: CheckDefinition,
}

/// Response from executing a single check
#[derive(Debug, Serialize, Deserialize)]
pub struct ExecuteCheckResponse {
    pub success: bool,
    pub result: CheckExecutionResult,
}

/// Request to execute multiple checks
#[derive(Debug, Serialize, Deserialize)]
pub struct ExecuteCheckSuiteRequest {
    pub checks: Vec<CheckDefinition>,
    #[serde(default)]
    pub stop_on_failure: bool,
}

/// Response from executing a check suite
#[derive(Debug, Serialize, Deserialize)]
pub struct ExecuteCheckSuiteResponse {
    pub success: bool,
    pub results: Vec<CheckExecutionResult>,
    pub summary: CheckSuiteSummary,
}

// ============================================================================
// Execution Commands
// ============================================================================

/// Execute a single code quality check
#[tauri::command]
pub fn execute_code_check(check_definition: CheckDefinition) -> ExecuteCheckResponse {
    info!(
        "Executing check: {} (tool: {:?}, type: {:?})",
        check_definition.name, check_definition.tool, check_definition.check_type
    );

    let result = execute_check(&check_definition);
    let success = result.is_success();

    info!(
        "Check {} completed: {:?} (issues: {}, fixed: {}, {}ms)",
        check_definition.name,
        result.status,
        result.issues_found,
        result.issues_fixed,
        result.duration_ms
    );

    ExecuteCheckResponse { success, result }
}

/// Execute multiple code quality checks
#[tauri::command]
pub fn execute_code_check_suite(request: ExecuteCheckSuiteRequest) -> ExecuteCheckSuiteResponse {
    info!(
        "Executing check suite: {} checks (stop_on_failure: {})",
        request.checks.len(),
        request.stop_on_failure
    );

    let (results, summary) = execute_check_suite(&request.checks, request.stop_on_failure);
    let success = summary.all_passed();

    info!(
        "Check suite completed: {}/{} passed ({:.1}% pass rate, {}ms total)",
        summary.passed + summary.fixed,
        summary.total,
        summary.pass_rate(),
        summary.duration_ms
    );

    ExecuteCheckSuiteResponse {
        success,
        results,
        summary,
    }
}

/// Execute a check by its database ID
#[tauri::command]
pub async fn execute_check_by_id(
    check_id: String,
    task_run_id: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Executing check by ID: {}", check_id);

    let db = &state.pg_db;

    // Get the check from database
    let check = match db.get_check(&check_id).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Ok(CommandResponse {
                success: false,
                message: Some(format!("Check not found: {}", check_id)),
                data: None,
            });
        }
        Err(e) => {
            return Ok(CommandResponse {
                success: false,
                message: Some(format!("Failed to get check: {}", e)),
                data: None,
            });
        }
    };

    // Convert to CheckDefinition
    let check_def = check_to_definition(&check);

    // Execute the check
    let result = execute_check(&check_def);

    // Store result in database if task_run_id provided
    if let Some(ref run_id) = task_run_id {
        let status_str = serde_json::to_string(&result.status)
            .unwrap_or("\"pending\"".to_string())
            .trim_matches('"')
            .to_string();
        let structured = result
            .structured_output
            .as_ref()
            .map(|o| serde_json::to_string(o).unwrap_or_default());

        if let Err(e) = state
            .pg_db
            .save_check_result(
                &result.check_id,
                &status_str,
                Some(result.started_at.as_str()),
                Some(result.completed_at.as_str()),
                Some(result.duration_ms as i64),
                Some(result.output.as_str()),
                result.error.as_deref(),
                result.issues_found as i32,
                result.issues_fixed as i32,
                result.files_checked as i32,
                structured.as_deref(),
                Some(run_id.as_str()),
            )
            .await
        {
            error!("Failed to store check result in PG: {}", e);
        }
    }

    Ok(CommandResponse {
        success: result.is_success(),
        message: Some(format!(
            "Check {} completed: {:?}",
            check.name, result.status
        )),
        data: Some(serde_json::to_value(result).unwrap_or_default()),
    })
}

// ============================================================================
// CRUD Commands
// ============================================================================

/// List all checks
#[tauri::command]
pub async fn list_checks(
    enabled_only: Option<bool>,
    check_type: Option<String>,
    tool: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    let db = &state.pg_db;

    match db.list_checks().await {
        Ok(checks) => {
            // Apply filters in memory since PG list_checks has no filter params
            let enabled_filter = enabled_only.unwrap_or(false);
            let checks: Vec<_> = checks
                .into_iter()
                .filter(|c| {
                    if enabled_filter && !c.enabled {
                        return false;
                    }
                    if let Some(ref ct) = check_type {
                        if c.check_type != *ct {
                            return false;
                        }
                    }
                    if let Some(ref t) = tool {
                        if c.tool != *t {
                            return false;
                        }
                    }
                    true
                })
                .collect();
            Ok(CommandResponse {
                success: true,
                message: Some(format!("Found {} checks", checks.len())),
                data: Some(serde_json::to_value(checks).unwrap_or_default()),
            })
        }
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to list checks: {}", e)),
            data: None,
        }),
    }
}

/// Get a single check by ID
#[tauri::command]
pub async fn get_check(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    let db = &state.pg_db;

    match db.get_check(&id).await {
        Ok(Some(check)) => Ok(CommandResponse {
            success: true,
            message: None,
            data: Some(serde_json::to_value(check).unwrap_or_default()),
        }),
        Ok(None) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Check not found: {}", id)),
            data: None,
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to get check: {}", e)),
            data: None,
        }),
    }
}

/// Create a new check
#[tauri::command]
pub async fn create_check(
    input: CreateCheckInput,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    let db = &state.pg_db;

    info!(
        "Creating check: {} (type: {})",
        input.name, input.check_type
    );

    match db.create_check(&input).await {
        Ok(check) => {
            info!("Created check: {} ({})", check.name, check.id);
            Ok(CommandResponse {
                success: true,
                message: Some(format!("Check created: {}", check.name)),
                data: Some(serde_json::to_value(check).unwrap_or_default()),
            })
        }
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to create check: {}", e)),
            data: None,
        }),
    }
}

/// Update an existing check
#[tauri::command]
pub async fn update_check(
    id: String,
    input: UpdateCheckInput,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    let db = &state.pg_db;

    info!("Updating check: {}", id);

    match db.update_check(&id, &input).await {
        Ok(check) => {
            info!("Updated check: {} ({})", check.name, check.id);
            Ok(CommandResponse {
                success: true,
                message: Some(format!("Check updated: {}", check.name)),
                data: Some(serde_json::to_value(check).unwrap_or_default()),
            })
        }
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to update check: {}", e)),
            data: None,
        }),
    }
}

/// Delete a check
#[tauri::command]
pub async fn delete_check(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    let db = &state.pg_db;

    info!("Deleting check: {}", id);

    match db.delete_check(&id).await {
        Ok(true) => {
            info!("Deleted check: {}", id);
            Ok(CommandResponse {
                success: true,
                message: Some("Check deleted".to_string()),
                data: None,
            })
        }
        Ok(false) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Check not found: {}", id)),
            data: None,
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to delete check: {}", e)),
            data: None,
        }),
    }
}

// ============================================================================
// Project Detection Commands
// ============================================================================

/// Detect project type and suggest relevant checks
#[tauri::command]
pub fn detect_project_check_suggestions(
    working_directory: String,
) -> Result<ProjectDetectionResult, String> {
    info!("Detecting project checks for: {}", working_directory);

    let result = detect_project_checks(&working_directory);

    info!(
        "Detected {} languages, {} tools, {} suggested checks",
        result.detected_languages.len(),
        result.detected_tools.len(),
        result.suggested_checks.len()
    );

    Ok(result)
}

/// Get information about available check tools
#[tauri::command]
pub fn get_check_tool_info() -> CommandResponse {
    let tools: Vec<CheckToolInfoSerialized> = CHECK_TOOLS.iter().map(|t| t.into()).collect();
    let check_types: Vec<serde_json::Value> = CHECK_TYPE_INFO
        .iter()
        .map(|t| {
            serde_json::json!({
                "check_type": t.check_type,
                "name": t.name,
                "description": t.description,
                "icon": t.icon,
                "color": t.color,
            })
        })
        .collect();

    CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "tools": tools,
            "check_types": check_types,
        })),
    }
}

// ============================================================================
// Check Results Commands
// ============================================================================

/// Get check results for a specific check
#[tauri::command]
pub async fn get_check_results(
    check_id: String,
    limit: Option<u32>,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    let limit = limit.unwrap_or(10);

    match state.pg_db.get_check_results(&check_id, limit).await {
        Ok(results) => Ok(CommandResponse {
            success: true,
            message: Some(format!("Found {} results", results.len())),
            data: Some(serde_json::to_value(results).unwrap_or_default()),
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to get check results: {}", e)),
            data: None,
        }),
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Convert a database Check to a CheckDefinition for execution
fn check_to_definition(check: &Check) -> CheckDefinition {
    use crate::check_executor::{CheckTool, CheckType};

    CheckDefinition {
        id: check.id.clone(),
        name: check.name.clone(),
        check_type: serde_json::from_str(&format!("\"{}\"", check.check_type))
            .unwrap_or(CheckType::Lint),
        tool: serde_json::from_str(&format!("\"{}\"", check.tool)).unwrap_or(CheckTool::Custom),
        command: check.command.clone(),
        working_directory: check.working_directory.clone(),
        config_path: check.config_path.clone(),
        auto_fix: check.auto_fix,
        fail_on_warning: check.fail_on_warning,
        timeout_seconds: check.timeout_seconds,
        is_critical: check.is_critical,
    }
}

// ============================================================================
// Check Group Commands
// ============================================================================

/// List all check groups
#[tauri::command]
pub async fn list_check_groups(
    enabled_only: Option<bool>,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    let db = &state.pg_db;

    match db.list_check_groups(enabled_only.unwrap_or(false)).await {
        Ok(groups) => Ok(CommandResponse {
            success: true,
            message: Some(format!("Found {} check groups", groups.len())),
            data: Some(serde_json::to_value(groups).unwrap_or_default()),
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to list check groups: {}", e)),
            data: None,
        }),
    }
}

/// Get a single check group by ID
#[tauri::command]
pub async fn get_check_group(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    let db = &state.pg_db;

    match db.get_check_group(&id).await {
        Ok(Some(group)) => Ok(CommandResponse {
            success: true,
            message: None,
            data: Some(serde_json::to_value(group).unwrap_or_default()),
        }),
        Ok(None) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Check group not found: {}", id)),
            data: None,
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to get check group: {}", e)),
            data: None,
        }),
    }
}

/// Create a new check group
#[tauri::command]
pub async fn create_check_group(
    input: CreateCheckGroupInput,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    let db = &state.pg_db;

    info!("Creating check group: {}", input.name);

    match db.create_check_group(&input).await {
        Ok(group) => {
            info!("Created check group: {} ({})", group.name, group.id);
            Ok(CommandResponse {
                success: true,
                message: Some(format!("Check group created: {}", group.name)),
                data: Some(serde_json::to_value(group).unwrap_or_default()),
            })
        }
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to create check group: {}", e)),
            data: None,
        }),
    }
}

/// Update an existing check group
#[tauri::command]
pub async fn update_check_group(
    id: String,
    input: UpdateCheckGroupInput,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    let db = &state.pg_db;

    info!("Updating check group: {}", id);

    match db.update_check_group(&id, &input).await {
        Ok(group) => {
            info!("Updated check group: {} ({})", group.name, group.id);
            Ok(CommandResponse {
                success: true,
                message: Some(format!("Check group updated: {}", group.name)),
                data: Some(serde_json::to_value(group).unwrap_or_default()),
            })
        }
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to update check group: {}", e)),
            data: None,
        }),
    }
}

/// Delete a check group
#[tauri::command]
pub async fn delete_check_group(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    let db = &state.pg_db;

    info!("Deleting check group: {}", id);

    match db.delete_check_group(&id).await {
        Ok(true) => {
            info!("Deleted check group: {}", id);
            Ok(CommandResponse {
                success: true,
                message: Some("Check group deleted".to_string()),
                data: None,
            })
        }
        Ok(false) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Check group not found: {}", id)),
            data: None,
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to delete check group: {}", e)),
            data: None,
        }),
    }
}

/// Get checks in a group
#[tauri::command]
pub async fn get_checks_in_group(
    group_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    let db = &state.pg_db;

    match db.get_checks_in_group(&group_id).await {
        Ok(checks) => Ok(CommandResponse {
            success: true,
            message: Some(format!("Found {} checks in group", checks.len())),
            data: Some(serde_json::to_value(checks).unwrap_or_default()),
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to get checks in group: {}", e)),
            data: None,
        }),
    }
}

/// Set checks in a group (replaces existing)
#[tauri::command]
pub async fn set_checks_in_group(
    group_id: String,
    check_ids: Vec<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    let db = &state.pg_db;

    info!("Setting {} checks in group: {}", check_ids.len(), group_id);

    match db.set_checks_in_group(&group_id, &check_ids).await {
        Ok(()) => {
            info!("Updated checks in group: {}", group_id);
            Ok(CommandResponse {
                success: true,
                message: Some(format!("Set {} checks in group", check_ids.len())),
                data: None,
            })
        }
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to set checks in group: {}", e)),
            data: None,
        }),
    }
}

/// Execute all checks in a group
#[tauri::command]
pub async fn execute_check_group(
    group_id: String,
    task_run_id: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    let db = &state.pg_db;

    info!("Executing check group: {}", group_id);

    // Get the group
    let group = match db.get_check_group(&group_id).await {
        Ok(Some(g)) => g,
        Ok(None) => {
            return Ok(CommandResponse {
                success: false,
                message: Some(format!("Check group not found: {}", group_id)),
                data: None,
            });
        }
        Err(e) => {
            return Ok(CommandResponse {
                success: false,
                message: Some(format!("Failed to get check group: {}", e)),
                data: None,
            });
        }
    };

    // Get checks in the group
    let checks = match db.get_checks_in_group(&group_id).await {
        Ok(c) => c,
        Err(e) => {
            return Ok(CommandResponse {
                success: false,
                message: Some(format!("Failed to get checks in group: {}", e)),
                data: None,
            });
        }
    };

    if checks.is_empty() {
        return Ok(CommandResponse {
            success: true,
            message: Some("No checks in group".to_string()),
            data: Some(serde_json::json!({
                "group": group,
                "results": [],
                "summary": {
                    "total": 0,
                    "passed": 0,
                    "failed": 0,
                    "fixed": 0,
                    "skipped": 0,
                    "duration_ms": 0
                }
            })),
        });
    }

    // Convert checks to definitions
    let check_defs: Vec<CheckDefinition> = checks.iter().map(check_to_definition).collect();

    // Execute the checks
    let (results, summary) = execute_check_suite(&check_defs, group.stop_on_failure);

    // Store results if task_run_id provided
    if let Some(ref run_id) = task_run_id {
        for result in &results {
            let status_str = serde_json::to_string(&result.status)
                .unwrap_or("\"pending\"".to_string())
                .trim_matches('"')
                .to_string();
            let structured = result
                .structured_output
                .as_ref()
                .map(|o| serde_json::to_string(o).unwrap_or_default());

            if let Err(e) = state
                .pg_db
                .save_check_result(
                    &result.check_id,
                    &status_str,
                    Some(result.started_at.as_str()),
                    Some(result.completed_at.as_str()),
                    Some(result.duration_ms as i64),
                    Some(result.output.as_str()),
                    result.error.as_deref(),
                    result.issues_found as i32,
                    result.issues_fixed as i32,
                    result.files_checked as i32,
                    structured.as_deref(),
                    Some(run_id.as_str()),
                )
                .await
            {
                error!("Failed to store check result in PG: {}", e);
            }
        }
    }

    let success = summary.all_passed();

    info!(
        "Check group {} completed: {}/{} passed ({:.1}% pass rate, {}ms total)",
        group.name,
        summary.passed + summary.fixed,
        summary.total,
        summary.pass_rate(),
        summary.duration_ms
    );

    Ok(CommandResponse {
        success,
        message: Some(format!(
            "Check group {} completed: {}/{} passed",
            group.name,
            summary.passed + summary.fixed,
            summary.total
        )),
        data: Some(serde_json::json!({
            "group": group,
            "results": results,
            "summary": summary
        })),
    })
}

/// Repair check-group associations based on naming convention.
///
/// Checks are named with format "{group_name} - {tool_name}" (e.g., "multistate - Ruff Linting").
/// This command finds checks that match groups by this pattern and ensures they are linked.
#[tauri::command]
pub async fn repair_check_group_associations(
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    let db = &state.pg_db;

    info!("Repairing check-group associations based on naming convention");

    match db.repair_check_group_associations().await {
        Ok(count) => {
            let message = if count > 0 {
                format!("Repaired {} check-group associations", count)
            } else {
                "All check-group associations are already correct".to_string()
            };
            info!("{}", message);
            Ok(CommandResponse {
                success: true,
                message: Some(message),
                data: Some(serde_json::json!({ "associations_created": count })),
            })
        }
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to repair associations: {}", e)),
            data: None,
        }),
    }
}
