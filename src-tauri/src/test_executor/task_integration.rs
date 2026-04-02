//! Task Integration for Verification Tests
//!
//! This module integrates verification test execution with the task run workflow.
//! It provides functions to:
//! - Execute tests at specific trigger points (before/after workflow, on action)
//! - Format test results for AI context
//! - Create findings for failed critical tests

use crate::database::{CreateTestResultInput, TestResultStatus, TriggerPoint};
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
    _config_id: &str,
    _trigger: &TriggerPoint,
    _task_run_id: Option<&str>,
) -> TriggerTestsResult {
    // SQLite removed — test execution now handled via PG-backed paths.
    // Return empty result to avoid panicking in callers.
    TriggerTestsResult::default()
}

/// Create findings for failed critical tests
///
/// # Arguments
/// * `db` - Database connection
/// * `task_run_id` - Task run ID to link findings to
/// * `results` - Test execution results
/// * `test_definitions` - Map of test IDs to their definitions for is_critical check
pub fn create_findings_for_failures(
    task_run_id: &str,
    session_num: i32,
    tests_result: &TriggerTestsResult,
    _config_id: &str,
) {
    // SQLite removed - no-op
}

/// Insert a test failure finding into the database via PG.
/// Uses block_in_place to call the async PG method from sync context.
fn insert_test_failure_finding(
    task_run_id: &str,
    session_num: i32,
    title: &str,
    description: &str,
    test_name: &str,
) -> Result<String, String> {
    Err("SQLite removed".to_string())
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
