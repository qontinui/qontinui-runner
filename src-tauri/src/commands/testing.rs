//! Verification Test Execution Commands
//!
//! Provides Tauri commands for executing verification tests.
//! Supports four test types:
//! - Playwright CDP (browser DOM assertions)
//! - Qontinui Vision (screenshot-based verification)
//! - Python Scripts (custom verification logic)
//! - Repository Tests (pytest, Jest, cargo test)
//!
//! Also provides CRUD operations for managing tests in the database.

use super::CommandResponse;
use crate::database::{
    CreateTestResultInput, CreateVerificationTestInput, TestResultStatus, TriggerPoint,
    VerificationTest,
};
use crate::test_executor::{
    execute_test, execute_test_suite, RepoTestConfig, TestCategory, TestDefinition,
    TestExecutionResult, TestStatus, TestSuiteSummary, TestType, VisionConfig,
};
use crate::AppState;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;
use tracing::info;

/// Request to execute a single verification test
#[derive(Debug, Serialize, Deserialize)]
pub struct ExecuteTestRequest {
    /// Test definition from the database
    pub test_definition: TestDefinition,
}

/// Response from executing a single test
#[derive(Debug, Serialize, Deserialize)]
pub struct ExecuteTestResponse {
    pub success: bool,
    pub result: TestExecutionResult,
}

/// Request to execute multiple tests
#[derive(Debug, Serialize, Deserialize)]
pub struct ExecuteTestSuiteRequest {
    /// List of test definitions
    pub tests: Vec<TestDefinition>,
    /// Run tests in parallel (default: false)
    #[serde(default)]
    pub parallel: bool,
    /// Stop on first failure (default: false)
    #[serde(default)]
    pub stop_on_failure: bool,
}

/// Response from executing a test suite
#[derive(Debug, Serialize, Deserialize)]
pub struct ExecuteTestSuiteResponse {
    pub success: bool,
    pub results: Vec<TestExecutionResult>,
    pub summary: TestSuiteSummary,
}

/// Execute a single verification test
///
/// # Arguments
/// * `test_definition` - The test definition containing type, code, and configuration
///
/// # Returns
/// The execution result including status, output, assertions, and any errors
#[tauri::command]
pub fn execute_verification_test(test_definition: TestDefinition) -> ExecuteTestResponse {
    info!(
        "Executing verification test: {} (type: {:?})",
        test_definition.name, test_definition.test_type
    );

    let result = execute_test(&test_definition);

    let success = matches!(result.status, TestStatus::Passed);

    info!(
        "Test {} completed: {:?} ({}ms)",
        test_definition.name, result.status, result.duration_ms
    );

    ExecuteTestResponse { success, result }
}

/// Execute a suite of verification tests
///
/// # Arguments
/// * `request` - Contains test definitions and execution options
///
/// # Returns
/// Results for each test and an aggregated summary
#[tauri::command]
pub fn execute_verification_test_suite(
    request: ExecuteTestSuiteRequest,
) -> ExecuteTestSuiteResponse {
    info!(
        "Executing test suite: {} tests (parallel: {})",
        request.tests.len(),
        request.parallel
    );

    let (results, summary) = if request.stop_on_failure {
        // Execute tests one by one, stopping on first failure
        let mut results = Vec::new();
        let mut summary = TestSuiteSummary::default();

        for test in &request.tests {
            let result = execute_test(test);
            let failed = !matches!(result.status, TestStatus::Passed);
            summary.add_result(&result);
            results.push(result);

            if failed {
                info!("Stopping test suite due to failure in: {}", test.name);
                break;
            }
        }

        (results, summary)
    } else {
        execute_test_suite(&request.tests, request.parallel)
    };

    let success = summary.all_passed();

    info!(
        "Test suite completed: {}/{} passed ({:.1}% pass rate, {}ms total)",
        summary.passed,
        summary.total,
        summary.pass_rate(),
        summary.duration_ms
    );

    ExecuteTestSuiteResponse {
        success,
        results,
        summary,
    }
}

/// Get information about available test types and their requirements
#[tauri::command]
pub fn get_test_type_info() -> CommandResponse {
    let info = serde_json::json!({
        "test_types": [
            {
                "type": "playwright_cdp",
                "name": "Playwright CDP",
                "description": "Browser DOM assertions via Chrome DevTools Protocol. Connects to an existing browser running with --remote-debugging-port.",
                "requirements": ["Browser running with CDP enabled (--remote-debugging-port=9222)", "@playwright/test installed in qontinui-runner"],
                "fields": {
                    "playwright_code": "TypeScript code using Playwright's expect() assertions",
                    "config.cdp_port": "CDP port (default: 9222)"
                }
            },
            {
                "type": "qontinui_vision",
                "name": "Qontinui Vision",
                "description": "Screenshot-based ML verification using qontinui's vision module. Runs visual assertions on captured screenshots.",
                "requirements": ["Python bridge running", "qontinui library installed"],
                "fields": {
                    "vision_config.assertions": "List of vision assertions (element_within_bounds, text_present, visual_regression)"
                }
            },
            {
                "type": "python_script",
                "name": "Python Script",
                "description": "Custom verification logic in Python. Scripts output structured JSON with assertions and metrics.",
                "requirements": ["Python installed"],
                "fields": {
                    "python_code": "Python code using assertion(), log(), metric() helpers",
                    "config.env_vars": "Environment variables to pass to the script"
                }
            },
            {
                "type": "repository_test",
                "name": "Repository Test",
                "description": "Run tests from the user's project (pytest, Jest, cargo test). Parses structured output for AI debugging.",
                "requirements": ["Test framework installed in project"],
                "fields": {
                    "repo_test_config.command": "Test command to execute (e.g., 'pytest tests/ -v')",
                    "repo_test_config.working_directory": "Working directory (supports ${PROJECT_ROOT})",
                    "repo_test_config.parse_format": "Output format: pytest_json, jest_json, cargo_test, go_test, generic"
                }
            }
        ]
    });

    CommandResponse {
        success: true,
        message: Some("Test type information".to_string()),
        data: Some(info),
    }
}

/// Validate a test definition without executing it
#[tauri::command]
pub fn validate_test_definition(test_definition: TestDefinition) -> CommandResponse {
    let mut errors: Vec<String> = Vec::new();

    // Validate based on test type
    match test_definition.test_type {
        TestType::PlaywrightCdp => {
            if test_definition.playwright_code.is_none()
                || test_definition
                    .playwright_code
                    .as_ref()
                    .map(|c| c.trim().is_empty())
                    .unwrap_or(true)
            {
                errors.push("Playwright CDP tests require playwright_code".to_string());
            }
        }
        TestType::QontinuiVision => {
            if test_definition.vision_config.is_none() {
                errors.push("Qontinui Vision tests require vision_config".to_string());
            } else if let Some(ref config) = test_definition.vision_config {
                if config.assertions.is_empty() {
                    errors.push("vision_config must have at least one assertion".to_string());
                }
            }
        }
        TestType::PythonScript => {
            if test_definition.python_code.is_none()
                || test_definition
                    .python_code
                    .as_ref()
                    .map(|c| c.trim().is_empty())
                    .unwrap_or(true)
            {
                errors.push("Python script tests require python_code".to_string());
            }
        }
        TestType::RepositoryTest => {
            if test_definition.repo_test_config.is_none() {
                errors.push("Repository tests require repo_test_config".to_string());
            } else if let Some(ref config) = test_definition.repo_test_config {
                if config.command.trim().is_empty() {
                    errors.push("repo_test_config.command cannot be empty".to_string());
                }
            }
        }
    }

    // Common validation
    if test_definition.name.trim().is_empty() {
        errors.push("Test name cannot be empty".to_string());
    }

    if test_definition.timeout_seconds == 0 {
        errors.push("timeout_seconds must be greater than 0".to_string());
    }

    if errors.is_empty() {
        CommandResponse {
            success: true,
            message: Some("Test definition is valid".to_string()),
            data: None,
        }
    } else {
        CommandResponse {
            success: false,
            message: Some(format!("Validation errors: {}", errors.join(", "))),
            data: Some(serde_json::json!({ "errors": errors })),
        }
    }
}

// ========================================================================
// Database Type Conversion
// ========================================================================

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
fn db_test_to_definition(test: &VerificationTest) -> TestDefinition {
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
        timeout_seconds: test.timeout_seconds,
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

// ========================================================================
// Database CRUD Commands
// ========================================================================

/// List all verification tests
#[tauri::command]
pub fn list_verification_tests(
    enabled_only: Option<bool>,
    test_type: Option<String>,
    category: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> CommandResponse {
    let db = &state.checkpoint_db;

    // Parse test type if provided
    let test_type_enum: Option<crate::database::TestType> =
        test_type.as_ref().and_then(|t| t.parse().ok());

    match db.list_verification_tests(
        enabled_only.unwrap_or(false),
        test_type_enum.as_ref(),
        category.as_deref(),
    ) {
        Ok(tests) => CommandResponse {
            success: true,
            message: Some(format!("Found {} verification tests", tests.len())),
            data: Some(serde_json::to_value(tests).unwrap_or_default()),
        },
        Err(e) => CommandResponse {
            success: false,
            message: Some(format!("Failed to list tests: {}", e)),
            data: None,
        },
    }
}

/// Get a single verification test by ID
#[tauri::command]
pub fn get_verification_test(id: String, state: State<'_, Arc<AppState>>) -> CommandResponse {
    let db = &state.checkpoint_db;

    match db.get_verification_test(&id) {
        Ok(Some(test)) => CommandResponse {
            success: true,
            message: Some("Test found".to_string()),
            data: Some(serde_json::to_value(test).unwrap_or_default()),
        },
        Ok(None) => CommandResponse {
            success: false,
            message: Some(format!("Test not found: {}", id)),
            data: None,
        },
        Err(e) => CommandResponse {
            success: false,
            message: Some(format!("Failed to get test: {}", e)),
            data: None,
        },
    }
}

/// Create a new verification test
#[tauri::command]
pub fn create_verification_test(
    input: CreateVerificationTestInput,
    state: State<'_, Arc<AppState>>,
) -> CommandResponse {
    let db = &state.checkpoint_db;

    info!(
        "Creating verification test: {} (type: {:?})",
        input.name, input.test_type
    );

    match db.create_verification_test(&input) {
        Ok(test) => CommandResponse {
            success: true,
            message: Some(format!("Created test: {}", test.name)),
            data: Some(serde_json::to_value(test).unwrap_or_default()),
        },
        Err(e) => CommandResponse {
            success: false,
            message: Some(format!("Failed to create test: {}", e)),
            data: None,
        },
    }
}

/// Update an existing verification test
#[tauri::command]
pub fn update_verification_test(
    id: String,
    input: CreateVerificationTestInput,
    state: State<'_, Arc<AppState>>,
) -> CommandResponse {
    let db = &state.checkpoint_db;

    info!("Updating verification test: {} ({})", input.name, id);

    match db.update_verification_test(&id, &input) {
        Ok(test) => CommandResponse {
            success: true,
            message: Some(format!("Updated test: {}", test.name)),
            data: Some(serde_json::to_value(test).unwrap_or_default()),
        },
        Err(e) => CommandResponse {
            success: false,
            message: Some(format!("Failed to update test: {}", e)),
            data: None,
        },
    }
}

/// Delete a verification test
#[tauri::command]
pub fn delete_verification_test(id: String, state: State<'_, Arc<AppState>>) -> CommandResponse {
    let db = &state.checkpoint_db;

    info!("Deleting verification test: {}", id);

    match db.delete_verification_test(&id) {
        Ok(true) => CommandResponse {
            success: true,
            message: Some("Test deleted".to_string()),
            data: None,
        },
        Ok(false) => CommandResponse {
            success: false,
            message: Some(format!("Test not found: {}", id)),
            data: None,
        },
        Err(e) => CommandResponse {
            success: false,
            message: Some(format!("Failed to delete test: {}", e)),
            data: None,
        },
    }
}

// ========================================================================
// Test Execution with Database Integration
// ========================================================================

/// Execute a test by ID (from database)
#[tauri::command]
pub fn execute_test_by_id(
    test_id: String,
    task_run_id: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> CommandResponse {
    let db = &state.checkpoint_db;

    // Get the test from database
    let test = match db.get_verification_test(&test_id) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return CommandResponse {
                success: false,
                message: Some(format!("Test not found: {}", test_id)),
                data: None,
            };
        }
        Err(e) => {
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to get test: {}", e)),
                data: None,
            };
        }
    };

    // Create test result record
    let result_input = CreateTestResultInput {
        test_id: test_id.clone(),
        task_run_id: task_run_id.clone(),
    };

    let test_result = match db.create_test_result(&result_input) {
        Ok(r) => r,
        Err(e) => {
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to create test result: {}", e)),
                data: None,
            };
        }
    };

    // Mark as started
    if let Err(e) = db.start_test_result(&test_result.id) {
        info!("Failed to mark test as started: {}", e);
    }

    info!(
        "Executing test from database: {} (type: {:?}, result_id: {})",
        test.name, test.test_type, test_result.id
    );

    // Convert to executor type and execute
    let test_def = db_test_to_definition(&test);
    let exec_result = execute_test(&test_def);

    // Update result in database
    let db_status = executor_status_to_db(&exec_result.status);
    let structured = exec_result.structured_output.as_ref();

    if let Err(e) = db.update_test_result(
        &test_result.id,
        &db_status,
        Some(&exec_result.output),
        exec_result.error.as_deref(),
        structured,
        exec_result.assertions_passed,
        exec_result.assertions_failed,
        &exec_result.screenshots,
    ) {
        info!("Failed to update test result: {}", e);
    }

    let success = matches!(exec_result.status, TestStatus::Passed);

    info!(
        "Test {} completed: {:?} ({}ms)",
        test.name, exec_result.status, exec_result.duration_ms
    );

    CommandResponse {
        success,
        message: Some(format!(
            "Test {}: {:?}",
            if success { "passed" } else { "failed" },
            exec_result.status
        )),
        data: Some(serde_json::json!({
            "result_id": test_result.id,
            "execution_result": exec_result,
        })),
    }
}

/// Execute multiple tests by IDs
#[tauri::command]
pub fn execute_tests_by_ids(
    test_ids: Vec<String>,
    task_run_id: Option<String>,
    stop_on_failure: Option<bool>,
    state: State<'_, Arc<AppState>>,
) -> CommandResponse {
    let db = &state.checkpoint_db;
    let stop_on_failure = stop_on_failure.unwrap_or(false);

    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut total_passed = 0u32;
    let mut total_failed = 0u32;

    for test_id in &test_ids {
        // Get test from database
        let test = match db.get_verification_test(test_id) {
            Ok(Some(t)) => t,
            Ok(None) => {
                info!("Test not found: {}", test_id);
                continue;
            }
            Err(e) => {
                info!("Failed to get test {}: {}", test_id, e);
                continue;
            }
        };

        // Create result record
        let result_input = CreateTestResultInput {
            test_id: test_id.clone(),
            task_run_id: task_run_id.clone(),
        };

        let test_result = match db.create_test_result(&result_input) {
            Ok(r) => r,
            Err(e) => {
                info!("Failed to create result for {}: {}", test_id, e);
                continue;
            }
        };

        let _ = db.start_test_result(&test_result.id);

        // Execute test
        let test_def = db_test_to_definition(&test);
        let exec_result = execute_test(&test_def);

        // Update database
        let db_status = executor_status_to_db(&exec_result.status);
        let _ = db.update_test_result(
            &test_result.id,
            &db_status,
            Some(&exec_result.output),
            exec_result.error.as_deref(),
            exec_result.structured_output.as_ref(),
            exec_result.assertions_passed,
            exec_result.assertions_failed,
            &exec_result.screenshots,
        );

        let passed = matches!(exec_result.status, TestStatus::Passed);
        if passed {
            total_passed += 1;
        } else {
            total_failed += 1;
        }

        results.push(serde_json::json!({
            "test_id": test_id,
            "test_name": test.name,
            "result_id": test_result.id,
            "status": exec_result.status,
            "duration_ms": exec_result.duration_ms,
            "passed": passed,
        }));

        if stop_on_failure && !passed {
            info!("Stopping test suite due to failure in: {}", test.name);
            break;
        }
    }

    let all_passed = total_failed == 0;

    CommandResponse {
        success: all_passed,
        message: Some(format!(
            "Executed {} tests: {} passed, {} failed",
            results.len(),
            total_passed,
            total_failed
        )),
        data: Some(serde_json::json!({
            "results": results,
            "summary": {
                "total": results.len(),
                "passed": total_passed,
                "failed": total_failed,
            }
        })),
    }
}

// ========================================================================
// Test Result Commands
// ========================================================================

/// Get test results for a specific test
#[tauri::command]
pub fn get_test_results(
    test_id: String,
    limit: Option<u32>,
    state: State<'_, Arc<AppState>>,
) -> CommandResponse {
    let db = &state.checkpoint_db;

    match db.get_results_for_test(&test_id, limit) {
        Ok(results) => CommandResponse {
            success: true,
            message: Some(format!("Found {} results", results.len())),
            data: Some(serde_json::to_value(results).unwrap_or_default()),
        },
        Err(e) => CommandResponse {
            success: false,
            message: Some(format!("Failed to get results: {}", e)),
            data: None,
        },
    }
}

/// Get test results for a task run
#[tauri::command]
pub fn get_task_run_test_results(
    task_run_id: String,
    state: State<'_, Arc<AppState>>,
) -> CommandResponse {
    let db = &state.checkpoint_db;

    match db.get_results_for_task_run(&task_run_id) {
        Ok(results) => CommandResponse {
            success: true,
            message: Some(format!("Found {} results", results.len())),
            data: Some(serde_json::to_value(results).unwrap_or_default()),
        },
        Err(e) => CommandResponse {
            success: false,
            message: Some(format!("Failed to get results: {}", e)),
            data: None,
        },
    }
}

// ========================================================================
// Test Association Commands
// ========================================================================

/// Create a test association
#[tauri::command]
pub fn create_test_association(
    test_id: String,
    config_id: Option<String>,
    workflow_name: Option<String>,
    trigger_point: String,
    action_id: Option<String>,
    execution_order: Option<i32>,
    state: State<'_, Arc<AppState>>,
) -> CommandResponse {
    let db = &state.checkpoint_db;

    let trigger: TriggerPoint = match trigger_point.parse() {
        Ok(t) => t,
        Err(e) => {
            return CommandResponse {
                success: false,
                message: Some(format!("Invalid trigger point: {}", e)),
                data: None,
            };
        }
    };

    match db.create_test_association(
        &test_id,
        config_id.as_deref(),
        workflow_name.as_deref(),
        &trigger,
        action_id.as_deref(),
        execution_order.unwrap_or(0),
    ) {
        Ok(assoc) => CommandResponse {
            success: true,
            message: Some("Association created".to_string()),
            data: Some(serde_json::to_value(assoc).unwrap_or_default()),
        },
        Err(e) => CommandResponse {
            success: false,
            message: Some(format!("Failed to create association: {}", e)),
            data: None,
        },
    }
}

/// Get test associations for a config
#[tauri::command]
pub fn get_config_test_associations(
    config_id: String,
    trigger_point: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> CommandResponse {
    let db = &state.checkpoint_db;

    let trigger: Option<TriggerPoint> = trigger_point.as_ref().and_then(|t| t.parse().ok());

    match db.get_associations_for_config(&config_id, trigger.as_ref()) {
        Ok(assocs) => CommandResponse {
            success: true,
            message: Some(format!("Found {} associations", assocs.len())),
            data: Some(serde_json::to_value(assocs).unwrap_or_default()),
        },
        Err(e) => CommandResponse {
            success: false,
            message: Some(format!("Failed to get associations: {}", e)),
            data: None,
        },
    }
}

/// Delete a test association
#[tauri::command]
pub fn delete_test_association(id: String, state: State<'_, Arc<AppState>>) -> CommandResponse {
    let db = &state.checkpoint_db;

    match db.delete_test_association(&id) {
        Ok(true) => CommandResponse {
            success: true,
            message: Some("Association deleted".to_string()),
            data: None,
        },
        Ok(false) => CommandResponse {
            success: false,
            message: Some(format!("Association not found: {}", id)),
            data: None,
        },
        Err(e) => CommandResponse {
            success: false,
            message: Some(format!("Failed to delete association: {}", e)),
            data: None,
        },
    }
}

// ========================================================================
// Import/Export Commands
// ========================================================================

/// Export format for verification tests
#[derive(Debug, Serialize, Deserialize)]
pub struct TestExportFormat {
    pub version: String,
    pub exported_at: String,
    pub tests: Vec<ExportedTest>,
}

/// Exported test (simplified format for JSON files)
#[derive(Debug, Serialize, Deserialize)]
pub struct ExportedTest {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub test_type: String,
    pub category: Option<String>,
    pub playwright_code: Option<String>,
    pub vision_config: Option<serde_json::Value>,
    pub python_code: Option<String>,
    pub repo_test_config: Option<serde_json::Value>,
    pub success_criteria: Option<String>,
    pub config: serde_json::Value,
    pub timeout_seconds: u32,
    pub is_critical: bool,
    pub enabled: bool,
    pub tags: Vec<String>,
}

/// Import result for a single test
#[derive(Debug, Serialize, Deserialize)]
pub struct ImportTestResult {
    pub name: String,
    pub status: String, // "created", "updated", "skipped", "error"
    pub message: Option<String>,
    pub new_id: Option<String>,
}

/// Import summary
#[derive(Debug, Serialize, Deserialize)]
pub struct ImportSummary {
    pub total: usize,
    pub created: usize,
    pub updated: usize,
    pub skipped: usize,
    pub errors: usize,
    pub results: Vec<ImportTestResult>,
}

/// Export tests to a JSON file
#[tauri::command]
pub fn export_tests_to_file(
    test_ids: Vec<String>,
    file_path: String,
    state: State<'_, Arc<AppState>>,
) -> CommandResponse {
    let db = &state.checkpoint_db;

    info!("Exporting {} tests to {}", test_ids.len(), file_path);

    // Get tests from database
    let mut exported_tests: Vec<ExportedTest> = Vec::new();

    for test_id in &test_ids {
        match db.get_verification_test(test_id) {
            Ok(Some(test)) => {
                exported_tests.push(ExportedTest {
                    id: test.id,
                    name: test.name,
                    description: test.description,
                    test_type: format!("{:?}", test.test_type).to_lowercase(),
                    category: test.category,
                    playwright_code: test.playwright_code,
                    vision_config: test.vision_config,
                    python_code: test.python_code,
                    repo_test_config: test.repo_test_config,
                    success_criteria: test.success_criteria,
                    config: test.config,
                    timeout_seconds: test.timeout_seconds,
                    is_critical: test.is_critical,
                    enabled: test.enabled,
                    tags: test.tags,
                });
            }
            Ok(None) => {
                info!("Test not found: {}", test_id);
            }
            Err(e) => {
                info!("Error getting test {}: {}", test_id, e);
            }
        }
    }

    if exported_tests.is_empty() {
        return CommandResponse {
            success: false,
            message: Some("No tests found to export".to_string()),
            data: None,
        };
    }

    // Create export format
    let export = TestExportFormat {
        version: "1.0".to_string(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        tests: exported_tests,
    };

    // Write to file
    match std::fs::File::create(&file_path) {
        Ok(file) => match serde_json::to_writer_pretty(file, &export) {
            Ok(_) => {
                info!("Exported {} tests to {}", export.tests.len(), file_path);
                CommandResponse {
                    success: true,
                    message: Some(format!("Exported {} tests", export.tests.len())),
                    data: Some(serde_json::json!({
                        "count": export.tests.len(),
                        "file_path": file_path,
                    })),
                }
            }
            Err(e) => CommandResponse {
                success: false,
                message: Some(format!("Failed to write JSON: {}", e)),
                data: None,
            },
        },
        Err(e) => CommandResponse {
            success: false,
            message: Some(format!("Failed to create file: {}", e)),
            data: None,
        },
    }
}

/// Export all tests to a JSON file
#[tauri::command]
pub fn export_all_tests_to_file(
    file_path: String,
    state: State<'_, Arc<AppState>>,
) -> CommandResponse {
    let db = &state.checkpoint_db;

    info!("Exporting all tests to {}", file_path);

    // Get all tests
    match db.list_verification_tests(false, None, None) {
        Ok(tests) => {
            let test_ids: Vec<String> = tests.iter().map(|t| t.id.clone()).collect();
            export_tests_to_file(test_ids, file_path, state)
        }
        Err(e) => CommandResponse {
            success: false,
            message: Some(format!("Failed to list tests: {}", e)),
            data: None,
        },
    }
}

/// Import tests from a JSON file
#[tauri::command]
pub fn import_tests_from_file(
    file_path: String,
    update_existing: bool,
    state: State<'_, Arc<AppState>>,
) -> CommandResponse {
    let db = &state.checkpoint_db;

    info!(
        "Importing tests from {} (update_existing: {})",
        file_path, update_existing
    );

    // Read file
    let file_content = match std::fs::read_to_string(&file_path) {
        Ok(content) => content,
        Err(e) => {
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to read file: {}", e)),
                data: None,
            };
        }
    };

    // Parse JSON
    let export: TestExportFormat = match serde_json::from_str(&file_content) {
        Ok(export) => export,
        Err(e) => {
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to parse JSON: {}", e)),
                data: None,
            };
        }
    };

    info!(
        "Parsed {} tests from file (version: {})",
        export.tests.len(),
        export.version
    );

    // Import each test
    let mut summary = ImportSummary {
        total: export.tests.len(),
        created: 0,
        updated: 0,
        skipped: 0,
        errors: 0,
        results: Vec::new(),
    };

    for exported_test in export.tests {
        let result = import_single_test(db, &exported_test, update_existing);

        match result.status.as_str() {
            "created" => summary.created += 1,
            "updated" => summary.updated += 1,
            "skipped" => summary.skipped += 1,
            _ => summary.errors += 1,
        }

        summary.results.push(result);
    }

    let success = summary.errors == 0;

    CommandResponse {
        success,
        message: Some(format!(
            "Imported {} tests: {} created, {} updated, {} skipped, {} errors",
            summary.total, summary.created, summary.updated, summary.skipped, summary.errors
        )),
        data: Some(serde_json::to_value(summary).unwrap_or_default()),
    }
}

/// Import a single test from export format
fn import_single_test(
    db: &crate::database::CheckpointDb,
    exported: &ExportedTest,
    update_existing: bool,
) -> ImportTestResult {
    // Parse test type
    let test_type: crate::database::TestType = match exported.test_type.as_str() {
        "playwrightcdp" | "playwright_cdp" => crate::database::TestType::PlaywrightCdp,
        "qontinuivision" | "qontinui_vision" => crate::database::TestType::QontinuiVision,
        "pythonscript" | "python_script" => crate::database::TestType::PythonScript,
        "repositorytest" | "repository_test" => crate::database::TestType::RepositoryTest,
        _ => {
            return ImportTestResult {
                name: exported.name.clone(),
                status: "error".to_string(),
                message: Some(format!("Unknown test type: {}", exported.test_type)),
                new_id: None,
            };
        }
    };

    // Check if test with same name exists
    let existing = db
        .list_verification_tests(false, None, None)
        .ok()
        .and_then(|tests| tests.into_iter().find(|t| t.name == exported.name));

    if let Some(existing_test) = existing {
        if update_existing {
            // Update existing test
            let input = CreateVerificationTestInput {
                name: exported.name.clone(),
                description: exported.description.clone(),
                test_type: test_type.clone(),
                category: exported.category.clone(),
                playwright_code: exported.playwright_code.clone(),
                vision_config: exported.vision_config.clone(),
                python_code: exported.python_code.clone(),
                repo_test_config: exported.repo_test_config.clone(),
                success_criteria: exported.success_criteria.clone(),
                config: exported.config.clone(),
                timeout_seconds: exported.timeout_seconds,
                is_critical: exported.is_critical,
                enabled: exported.enabled,
                ai_generated: false,
                ai_generation_prompt: None,
                tags: exported.tags.clone(),
                source_file: None,
            };

            match db.update_verification_test(&existing_test.id, &input) {
                Ok(_) => ImportTestResult {
                    name: exported.name.clone(),
                    status: "updated".to_string(),
                    message: None,
                    new_id: Some(existing_test.id),
                },
                Err(e) => ImportTestResult {
                    name: exported.name.clone(),
                    status: "error".to_string(),
                    message: Some(format!("Failed to update: {}", e)),
                    new_id: None,
                },
            }
        } else {
            ImportTestResult {
                name: exported.name.clone(),
                status: "skipped".to_string(),
                message: Some("Test with same name already exists".to_string()),
                new_id: None,
            }
        }
    } else {
        // Create new test
        let input = CreateVerificationTestInput {
            name: exported.name.clone(),
            description: exported.description.clone(),
            test_type,
            category: exported.category.clone(),
            playwright_code: exported.playwright_code.clone(),
            vision_config: exported.vision_config.clone(),
            python_code: exported.python_code.clone(),
            repo_test_config: exported.repo_test_config.clone(),
            success_criteria: exported.success_criteria.clone(),
            config: exported.config.clone(),
            timeout_seconds: exported.timeout_seconds,
            is_critical: exported.is_critical,
            enabled: exported.enabled,
            ai_generated: false,
            ai_generation_prompt: None,
            tags: exported.tags.clone(),
            source_file: None,
        };

        match db.create_verification_test(&input) {
            Ok(test) => ImportTestResult {
                name: exported.name.clone(),
                status: "created".to_string(),
                message: None,
                new_id: Some(test.id),
            },
            Err(e) => ImportTestResult {
                name: exported.name.clone(),
                status: "error".to_string(),
                message: Some(format!("Failed to create: {}", e)),
                new_id: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_definition(test_type: TestType) -> TestDefinition {
        TestDefinition {
            id: "test-1".to_string(),
            name: "Test".to_string(),
            test_type,
            category: TestCategory::Unit,
            playwright_code: None,
            vision_config: None,
            python_code: None,
            repo_test_config: None,
            timeout_seconds: 60,
            is_critical: false,
            config: serde_json::json!({}),
        }
    }

    #[test]
    fn test_validate_playwright_missing_code() {
        let test_def = create_test_definition(TestType::PlaywrightCdp);
        let result = validate_test_definition(test_def);
        assert!(!result.success);
        assert!(result.message.unwrap().contains("playwright_code"));
    }

    #[test]
    fn test_validate_python_missing_code() {
        let test_def = create_test_definition(TestType::PythonScript);
        let result = validate_test_definition(test_def);
        assert!(!result.success);
        assert!(result.message.unwrap().contains("python_code"));
    }

    #[test]
    fn test_validate_repo_missing_config() {
        let test_def = create_test_definition(TestType::RepositoryTest);
        let result = validate_test_definition(test_def);
        assert!(!result.success);
        assert!(result.message.unwrap().contains("repo_test_config"));
    }
}
