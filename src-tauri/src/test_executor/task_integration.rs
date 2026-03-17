//! Task Integration for Verification Tests
//!
//! This module integrates verification test execution with the task run workflow.
//! It provides functions to:
//! - Execute tests at specific trigger points (before/after workflow, on action)
//! - Format test results for AI context
//! - Create findings for failed critical tests

use crate::database::{CheckpointDb, CreateTestResultInput, TestResultStatus, TriggerPoint};
use crate::test_executor::{
    execute_test, RepoTestConfig, TestCategory, TestDefinition, TestExecutionResult, TestStatus,
    TestType, VisionConfig,
};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

/// Result of executing tests for a trigger point
#[derive(Debug, Clone, Default)]
pub struct TriggerTestsResult {
    /// Number of tests executed
    pub total: usize,
    /// Number of tests passed
    pub passed: usize,
    /// Number of tests failed
    pub failed: usize,
    /// Whether any critical test failed
    pub critical_failure: bool,
    /// Formatted output for AI context
    pub ai_context: String,
    /// Individual test results
    pub results: Vec<TestExecutionResult>,
}

/// Execute verification tests for a specific trigger point
///
/// # Arguments
/// * `db` - Database connection
/// * `config_id` - The configuration ID to get test associations for
/// * `trigger` - The trigger point (before_workflow, after_workflow, on_action, manual)
/// * `task_run_id` - Optional task run ID to link results to
///
/// # Returns
/// Result containing test execution summary and AI-formatted context
pub fn execute_tests_for_trigger(
    db: &CheckpointDb,
    config_id: &str,
    trigger: &TriggerPoint,
    task_run_id: Option<&str>,
) -> TriggerTestsResult {
    info!(
        "Executing tests for trigger {:?} on config {}",
        trigger, config_id
    );

    // Get test associations for this config and trigger
    let associations = match db.get_associations_for_config(config_id, Some(trigger)) {
        Ok(assocs) => assocs,
        Err(e) => {
            warn!("Failed to get test associations: {}", e);
            return TriggerTestsResult::default();
        }
    };

    if associations.is_empty() {
        info!("No tests associated with trigger {:?}", trigger);
        return TriggerTestsResult::default();
    }

    info!(
        "Found {} test associations for trigger {:?}",
        associations.len(),
        trigger
    );

    let mut result = TriggerTestsResult::default();
    let mut context_lines: Vec<String> = Vec::new();

    context_lines.push(format!("=== Verification Tests ({:?}) ===", trigger));

    // Sort by execution_order
    let mut sorted_assocs = associations;
    sorted_assocs.sort_by_key(|a| a.execution_order);

    for assoc in sorted_assocs {
        if !assoc.enabled {
            continue;
        }

        // Get the test definition
        let test = match db.get_verification_test(&assoc.test_id) {
            Ok(Some(t)) => t,
            Ok(None) => {
                warn!("Test {} not found", assoc.test_id);
                continue;
            }
            Err(e) => {
                warn!("Failed to get test {}: {}", assoc.test_id, e);
                continue;
            }
        };

        if !test.enabled {
            continue;
        }

        // Convert to TestDefinition
        let test_def = db_test_to_definition(&test);

        // Create test result record
        let result_input = CreateTestResultInput {
            test_id: test.id.clone(),
            task_run_id: task_run_id.map(|s| s.to_string()),
        };

        let test_result_record = match db.create_test_result(&result_input) {
            Ok(r) => r,
            Err(e) => {
                warn!("Failed to create test result: {}", e);
                continue;
            }
        };

        // Mark as started
        let _ = db.start_test_result(&test_result_record.id);

        // Execute the test
        info!("Executing test: {} (type: {:?})", test.name, test.test_type);
        let exec_result = execute_test(&test_def);

        // Update result in database
        let db_status = executor_status_to_db(&exec_result.status);
        let _ = db.update_test_result(
            &test_result_record.id,
            &db_status,
            Some(&exec_result.output),
            exec_result.error.as_deref(),
            exec_result.structured_output.as_ref(),
            exec_result.assertions_passed,
            exec_result.assertions_failed,
            &exec_result.screenshots,
        );

        // Track results
        result.total += 1;
        let status = &exec_result.status;

        if matches!(status, TestStatus::Passed) {
            result.passed += 1;
            context_lines.push(format!(
                "PASSED: {} ({}ms)",
                test.name, exec_result.duration_ms
            ));
        } else if matches!(status, TestStatus::Skipped) {
            // Skipped tests are not failures — don't count them
            context_lines.push(format!("SKIPPED: {}", test.name));
        } else {
            result.failed += 1;
            if test.is_critical {
                result.critical_failure = true;
            }
            context_lines.push(format!(
                "FAILED: {} ({}ms) - {}",
                test.name,
                exec_result.duration_ms,
                exec_result.error.as_deref().unwrap_or("Unknown error")
            ));
        }

        // Add assertion details
        if exec_result.assertions_passed > 0 || exec_result.assertions_failed > 0 {
            context_lines.push(format!(
                "  Assertions: {}/{} passed",
                exec_result.assertions_passed,
                exec_result.assertions_passed + exec_result.assertions_failed
            ));
        }

        result.results.push(exec_result);
    }

    // Summary line
    if result.total > 0 {
        context_lines.push(format!(
            "\nExecuted {} tests: {} passed, {} failed{}",
            result.total,
            result.passed,
            result.failed,
            if result.critical_failure {
                " (CRITICAL FAILURE)"
            } else {
                ""
            }
        ));
    }

    result.ai_context = context_lines.join("\n");

    info!(
        "Test execution complete: {}/{} passed",
        result.passed, result.total
    );

    result
}

/// Create findings for failed critical tests
///
/// # Arguments
/// * `db` - Database connection
/// * `task_run_id` - Task run ID to link findings to
/// * `results` - Test execution results
/// * `test_definitions` - Map of test IDs to their definitions for is_critical check
pub fn create_findings_for_failures(
    db: &CheckpointDb,
    task_run_id: &str,
    session_num: i32,
    tests_result: &TriggerTestsResult,
    _config_id: &str,
) {
    for exec_result in &tests_result.results {
        if matches!(exec_result.status, TestStatus::Passed | TestStatus::Skipped) {
            continue;
        }

        // Get the test to check if it's critical
        let test = match db.get_verification_test(&exec_result.test_id) {
            Ok(Some(t)) => t,
            _ => continue,
        };

        if !test.is_critical {
            continue;
        }

        // Create a finding for the critical failure
        let finding_title = format!("Test failed: {}", test.name);
        let finding_description = format!(
            "Critical verification test '{}' failed.\n\nError: {}\n\nTest Type: {:?}\nDuration: {}ms\nAssertions: {} passed, {} failed",
            test.name,
            exec_result.error.as_deref().unwrap_or("Unknown error"),
            test.test_type,
            exec_result.duration_ms,
            exec_result.assertions_passed,
            exec_result.assertions_failed
        );

        // Insert finding using the findings storage
        match insert_test_failure_finding(
            db,
            task_run_id,
            session_num,
            &finding_title,
            &finding_description,
            &test.name,
        ) {
            Ok(finding_id) => {
                info!(
                    "Created finding {} for failed test {}",
                    finding_id, test.name
                );
            }
            Err(e) => {
                warn!("Failed to create finding for test {}: {}", test.name, e);
            }
        }
    }
}

/// Insert a test failure finding into the database
fn insert_test_failure_finding(
    db: &CheckpointDb,
    task_run_id: &str,
    session_num: i32,
    title: &str,
    description: &str,
    test_name: &str,
) -> Result<String, String> {
    let conn = db.connection().map_err(|e| format!("DB error: {}", e))?;

    let finding_id = uuid::Uuid::new_v4().to_string();
    let signature = format!("test_failure:{}:{}", task_run_id, test_name);
    let mut hasher = Sha256::new();
    hasher.update(signature.as_bytes());
    let signature_hash = format!("{:x}", hasher.finalize());
    let now = chrono::Utc::now().to_rfc3339();

    // Check for existing finding with same signature
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM task_run_findings WHERE task_run_id = ? AND signature_hash = ? AND status NOT IN ('resolved', 'wont_fix')",
            rusqlite::params![task_run_id, signature_hash],
            |row| row.get(0),
        )
        .ok();

    if let Some(id) = existing {
        return Ok(id);
    }

    // Insert new finding
    conn.execute(
        "INSERT INTO task_run_findings (
            id, task_run_id, category, severity, signature_hash,
            title, description, status, action_type, detected_in_session, detected_at
        ) VALUES (?, ?, 'test_failure', 'high', ?, ?, ?, 'detected', 'auto_fix', ?, ?)",
        rusqlite::params![
            finding_id,
            task_run_id,
            signature_hash,
            title,
            description,
            session_num,
            now
        ],
    )
    .map_err(|e| format!("Insert error: {}", e))?;

    Ok(finding_id)
}

/// Format test results as AI context string
pub fn format_results_for_ai(results: &TriggerTestsResult) -> String {
    if results.total == 0 {
        return String::new();
    }

    results.ai_context.clone()
}

/// Convert database TestType to executor TestType
fn db_test_type_to_executor(db_type: &crate::database::TestType) -> TestType {
    match db_type {
        crate::database::TestType::PlaywrightCdp => TestType::PlaywrightCdp,
        crate::database::TestType::QontinuiVision => TestType::QontinuiVision,
        crate::database::TestType::PythonScript => TestType::PythonScript,
        crate::database::TestType::RepositoryTest => TestType::RepositoryTest,
    }
}

/// Convert database VerificationTest to executor TestDefinition
fn db_test_to_definition(test: &crate::database::VerificationTest) -> TestDefinition {
    // Parse category from string
    let category = test
        .category
        .as_ref()
        .map(|c| match c.as_str() {
            "visual" => TestCategory::Visual,
            "dom" => TestCategory::Dom,
            "network" => TestCategory::Network,
            "data" => TestCategory::Data,
            "log" => TestCategory::Log,
            "layout" => TestCategory::Layout,
            "unit" => TestCategory::Unit,
            "integration" => TestCategory::Integration,
            _ => TestCategory::Custom,
        })
        .unwrap_or(TestCategory::Custom);

    // Parse vision config if present
    let vision_config = test
        .vision_config
        .as_ref()
        .and_then(|v| serde_json::from_value::<VisionConfig>(v.clone()).ok());

    // Parse repo test config if present
    let repo_test_config = test
        .repo_test_config
        .as_ref()
        .and_then(|v| serde_json::from_value::<RepoTestConfig>(v.clone()).ok());

    TestDefinition {
        id: test.id.clone(),
        name: test.name.clone(),
        test_type: db_test_type_to_executor(&test.test_type),
        category,
        playwright_code: test.playwright_code.clone(),
        vision_config,
        python_code: test.python_code.clone(),
        repo_test_config,
        timeout_seconds: test.timeout_seconds.unwrap_or(60),
        is_critical: test.is_critical,
        config: test.config.clone(),
    }
}

/// Convert executor TestStatus to database TestResultStatus
fn executor_status_to_db(status: &TestStatus) -> TestResultStatus {
    match status {
        TestStatus::Pending => TestResultStatus::Pending,
        TestStatus::Running => TestResultStatus::Running,
        TestStatus::Passed => TestResultStatus::Passed,
        TestStatus::Failed => TestResultStatus::Failed,
        TestStatus::Skipped => TestResultStatus::Skipped,
        TestStatus::Error => TestResultStatus::Error,
        TestStatus::Timeout => TestResultStatus::Timeout,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_result() {
        let result = TriggerTestsResult::default();
        assert_eq!(result.total, 0);
        assert_eq!(result.passed, 0);
        assert_eq!(result.failed, 0);
        assert!(!result.critical_failure);
    }
}
