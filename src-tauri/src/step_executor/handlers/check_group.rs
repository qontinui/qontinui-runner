//! Check Group Step Handler
//!
//! Handles check_group steps that execute multiple code quality checks as a group.

use async_trait::async_trait;
use serde_json::json;
use std::time::Instant;
use tracing::info;

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::check_executor::{execute_check, CheckDefinition, CheckTool, CheckType};
use crate::step_executor::executor::{
    CheckIssueDetail, ExecutionStepConfig, IndividualCheckResult,
};

/// Handler for check_group steps.
pub struct CheckGroupHandler;

#[async_trait]
impl StepHandler for CheckGroupHandler {
    fn step_type(&self) -> &'static str {
        "check_group"
    }

    fn display_name(&self) -> &'static str {
        "Check Group"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let step_name = step.name.as_deref().unwrap_or("Check Group");
        let group_id = match &step.check_group_id {
            Some(id) => id.clone(),
            None => {
                return StepHandlerResult::failure(
                    "No check group ID specified for check_group step",
                );
            }
        };

        info!(
            "execute_check_group_step: step_name={:?}, group_id={:?}",
            step_name, group_id
        );

        // Get the checkpoint_db from app_state
        let db = &context.app_state.checkpoint_db;

        // Get the group
        let group = match db.get_check_group(&group_id) {
            Ok(Some(g)) => g,
            Ok(None) => {
                return StepHandlerResult::failure(format!("Check group not found: {}", group_id));
            }
            Err(e) => {
                return StepHandlerResult::failure(format!("Failed to get check group: {}", e));
            }
        };

        if !group.enabled {
            info!("Check group '{}' is disabled, skipping", group.name);
            return StepHandlerResult::success_with_data(json!({
                "skipped": true,
                "reason": format!("Check group '{}' is disabled", group.name),
            }));
        }

        // Get checks in the group
        let checks = match db.get_checks_in_group(&group_id) {
            Ok(c) => c,
            Err(e) => {
                return StepHandlerResult::failure(format!("Failed to get checks in group: {}", e));
            }
        };

        if checks.is_empty() {
            return StepHandlerResult::success_with_data(json!({
                "message": format!("No checks in group '{}'", group.name),
            }));
        }

        info!(
            "Executing check group '{}' with {} checks (stop_on_failure: {})",
            group.name,
            checks.len(),
            group.stop_on_failure
        );

        // Execute each check
        let start_time = Instant::now();
        let mut passed = 0;
        let mut failed = 0;
        let mut skipped = 0;
        let mut results_output = Vec::new();
        let mut check_results: Vec<IndividualCheckResult> = Vec::new();

        for check in &checks {
            if !check.enabled {
                results_output.push(format!("  [SKIPPED] {} (disabled)", check.name));
                check_results.push(IndividualCheckResult {
                    name: check.name.clone(),
                    status: "skipped".to_string(),
                    duration_ms: 0,
                    issues_found: 0,
                    issues_fixed: 0,
                    files_checked: 0,
                    error_message: Some("Check is disabled".to_string()),
                    output: None,
                    issues: Vec::new(),
                });
                skipped += 1;
                continue;
            }

            let check_def = CheckDefinition {
                id: check.id.clone(),
                name: check.name.clone(),
                check_type: serde_json::from_str(&format!("\"{}\"", check.check_type))
                    .unwrap_or(CheckType::Lint),
                tool: serde_json::from_str(&format!("\"{}\"", check.tool))
                    .unwrap_or(CheckTool::Custom),
                command: check.command.clone(),
                working_directory: check.working_directory.clone(),
                config_path: check.config_path.clone(),
                auto_fix: check.auto_fix,
                fail_on_warning: check.fail_on_warning,
                timeout_seconds: check.timeout_seconds,
                is_critical: check.is_critical,
            };

            let result = execute_check(&check_def);
            let is_success = result.is_success();

            // Extract issues from structured output (limit to 50)
            let issues: Vec<CheckIssueDetail> = result
                .structured_output
                .as_ref()
                .map(|so| {
                    so.issues
                        .iter()
                        .take(50)
                        .map(|issue| CheckIssueDetail {
                            file: issue.file.clone(),
                            line: issue.line,
                            column: issue.column,
                            code: issue.code.clone(),
                            message: issue.message.clone(),
                            severity: format!("{:?}", issue.severity).to_lowercase(),
                            fixable: issue.fixable,
                        })
                        .collect()
                })
                .unwrap_or_default();

            // Build individual check result
            let check_result = IndividualCheckResult {
                name: check.name.clone(),
                status: if is_success { "passed" } else { "failed" }.to_string(),
                duration_ms: result.duration_ms,
                issues_found: result.issues_found,
                issues_fixed: result.issues_fixed,
                files_checked: result.files_checked,
                error_message: result.error.clone(),
                output: if result.output.len() > 2000 {
                    Some(format!("{}... (truncated)", &result.output[..2000]))
                } else if !result.output.is_empty() {
                    Some(result.output.clone())
                } else {
                    None
                },
                issues,
            };
            check_results.push(check_result);

            if is_success {
                passed += 1;
                results_output.push(format!(
                    "  [PASSED] {} ({}ms, {} issues found, {} fixed)",
                    check.name, result.duration_ms, result.issues_found, result.issues_fixed
                ));
            } else {
                failed += 1;
                results_output.push(format!(
                    "  [FAILED] {} ({}ms): {}",
                    check.name,
                    result.duration_ms,
                    result.error.as_deref().unwrap_or(&result.output)
                ));

                if group.stop_on_failure {
                    results_output.push("  Stopping due to stop_on_failure setting".to_string());
                    break;
                }
            }
        }

        let duration_ms = start_time.elapsed().as_millis();
        let total = passed + failed;
        let success = failed == 0;

        let summary = format!(
            "Check group '{}': {}/{} passed ({}ms total)\n{}",
            group.name,
            passed,
            total,
            duration_ms,
            results_output.join("\n")
        );

        info!(
            "Check group '{}' completed: {}/{} passed, {} skipped ({}ms)",
            group.name, passed, total, skipped, duration_ms
        );

        let output_data = json!({
            "summary": summary,
            "passed": passed,
            "failed": failed,
            "skipped": skipped,
            "total": total,
            "duration_ms": duration_ms,
            "check_results": check_results,
        });

        if success {
            StepHandlerResult::success_with_data(output_data)
        } else {
            StepHandlerResult::failure_with_data(
                format!(
                    "Check group '{}' failed: {}/{} passed",
                    group.name, passed, total
                ),
                output_data,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_group_handler_step_type() {
        let handler = CheckGroupHandler;
        assert_eq!(handler.step_type(), "check_group");
        assert_eq!(handler.display_name(), "Check Group");
    }
}
