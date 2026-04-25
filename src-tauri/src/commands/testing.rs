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

use super::compartments::{BridgeCompartment, StorageCompartment};
use super::CommandResponse;
use crate::database::{CreateVerificationTestInput, TriggerPoint, VerificationTest};
use crate::executor::{is_default_bridge_running_compartment, with_default_bridge_compartment};
use crate::test_executor::{
    execute_test, execute_test_suite, RepoTestConfig, TestCategory, TestDefinition,
    TestExecutionResult, TestStatus, TestSuiteSummary, TestType, VisionConfig,
};
use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
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
        timeout_seconds: test.timeout_seconds.unwrap_or(60),
        is_critical: test.is_critical,
        config: test.config.clone(),
    }
}

/// Persist a `TestExecutionResult` into the `test_results` table (Bug 9).
async fn persist_test_result(
    pg_db: &crate::database::pg::PgDb,
    test_id: &str,
    task_run_id: Option<&str>,
    exec: &TestExecutionResult,
) -> Result<String, String> {
    let status_str = match exec.status {
        TestStatus::Pending => "pending",
        TestStatus::Running => "running",
        TestStatus::Passed => "passed",
        TestStatus::Failed => "failed",
        TestStatus::Skipped => "skipped",
        TestStatus::Error => "error",
        TestStatus::Timeout => "timeout",
    };
    pg_db
        .create_test_result(
            test_id,
            task_run_id,
            status_str,
            Some(exec.started_at.as_str()),
            Some(exec.completed_at.as_str()),
            Some(exec.duration_ms as i64),
            Some(exec.output.as_str()),
            exec.error.as_deref(),
            exec.structured_output.as_ref(),
            exec.assertions_passed,
            exec.assertions_failed,
            &exec.screenshots,
        )
        .await
}

// ========================================================================
// Database CRUD Commands
// ========================================================================

/// List all verification tests
#[tauri::command]
pub async fn list_verification_tests(
    enabled_only: Option<bool>,
    test_type: Option<String>,
    category: Option<String>,
    state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    let filter = crate::database::pg::verification_tests::VerificationTestFilter {
        enabled_only: enabled_only.unwrap_or(false),
        test_type,
        category,
    };
    match state.pg_db().list_verification_tests(filter).await {
        Ok(tests) => Ok(CommandResponse {
            success: true,
            message: Some(format!("Found {} verification tests", tests.len())),
            data: Some(serde_json::to_value(tests).unwrap_or_default()),
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to list tests: {}", e)),
            data: None,
        }),
    }
}

/// Get a single verification test by ID
#[tauri::command]
pub async fn get_verification_test(
    id: String,
    state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    match state.pg_db().get_verification_test(&id).await {
        Ok(Some(test)) => {
            return Ok(CommandResponse {
                success: true,
                message: Some("Test found".to_string()),
                data: Some(serde_json::to_value(test).unwrap_or_default()),
            })
        }
        Ok(None) => {
            return Ok(CommandResponse {
                success: false,
                message: Some(format!("Test not found: {}", id)),
                data: None,
            })
        }
        Err(e) => {
            return Ok(CommandResponse {
                success: false,
                message: Some(format!("Failed to get test: {}", e)),
                data: None,
            })
        }
    }
}

/// Create a new verification test
#[tauri::command]
pub async fn create_verification_test(
    input: CreateVerificationTestInput,
    state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    info!(
        "Creating verification test: {} (type: {:?})",
        input.name, input.test_type
    );
    match state.pg_db().create_verification_test(&input).await {
        Ok(test) => Ok(CommandResponse {
            success: true,
            message: Some(format!("Created test: {}", test.id)),
            data: Some(serde_json::to_value(test).unwrap_or_default()),
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to create test: {}", e)),
            data: None,
        }),
    }
}

/// Update an existing verification test
#[tauri::command]
pub async fn update_verification_test(
    id: String,
    input: CreateVerificationTestInput,
    state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    info!("Updating verification test: {} ({})", input.name, id);
    match state.pg_db().update_verification_test(&id, &input).await {
        Ok(test) => Ok(CommandResponse {
            success: true,
            message: Some(format!("Updated test: {}", test.id)),
            data: Some(serde_json::to_value(test).unwrap_or_default()),
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to update test: {}", e)),
            data: None,
        }),
    }
}

/// Delete a verification test
#[tauri::command]
pub async fn delete_verification_test(
    id: String,
    state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    info!("Deleting verification test: {}", id);
    match state.pg_db().delete_verification_test(&id).await {
        Ok(true) => Ok(CommandResponse {
            success: true,
            message: Some(format!("Deleted test: {}", id)),
            data: None,
        }),
        Ok(false) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Test not found: {}", id)),
            data: None,
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to delete test: {}", e)),
            data: None,
        }),
    }
}

// ========================================================================
// Test Execution with Database Integration
// ========================================================================

/// Execute a test by ID (from database)
#[tauri::command]
pub async fn execute_test_by_id(
    test_id: String,
    task_run_id: Option<String>,
    state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    let db = state.pg_db();

    // Get the test from database
    let test = match db.get_verification_test(&test_id).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return Ok(CommandResponse {
                success: false,
                message: Some(format!("Test not found: {}", test_id)),
                data: None,
            });
        }
        Err(e) => {
            return Ok(CommandResponse {
                success: false,
                message: Some(format!("Failed to get test: {}", e)),
                data: None,
            });
        }
    };

    info!(
        "Executing test from database: {} (type: {:?})",
        test.name, test.test_type
    );

    // Convert to executor type and execute
    let test_def = db_test_to_definition(&test);
    let exec_result = execute_test(&test_def);

    // Persist the test result so it shows up in history.
    if let Err(e) = persist_test_result(db, &test.id, task_run_id.as_deref(), &exec_result).await {
        info!("Failed to persist test result for {}: {}", test.id, e);
    }

    let success = matches!(exec_result.status, TestStatus::Passed);

    info!(
        "Test {} completed: {:?} ({}ms)",
        test.name, exec_result.status, exec_result.duration_ms
    );

    Ok(CommandResponse {
        success,
        message: Some(format!(
            "Test {}: {:?}",
            if success { "passed" } else { "failed" },
            exec_result.status
        )),
        data: Some(serde_json::json!({
            "execution_result": exec_result,
        })),
    })
}

/// Execute multiple tests by IDs
#[tauri::command]
pub async fn execute_tests_by_ids(
    test_ids: Vec<String>,
    task_run_id: Option<String>,
    stop_on_failure: Option<bool>,
    state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    let db = state.pg_db();
    let stop_on_failure = stop_on_failure.unwrap_or(false);

    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut total_passed = 0u32;
    let mut total_failed = 0u32;

    for test_id in &test_ids {
        // Get test from database
        let test = match db.get_verification_test(test_id).await {
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

        // Execute test
        let test_def = db_test_to_definition(&test);
        let exec_result = execute_test(&test_def);

        // Persist the test result so it shows up in history.
        if let Err(e) =
            persist_test_result(db, &test.id, task_run_id.as_deref(), &exec_result).await
        {
            info!("Failed to persist test result for {}: {}", test.id, e);
        }

        let passed = matches!(exec_result.status, TestStatus::Passed);
        if passed {
            total_passed += 1;
        } else {
            total_failed += 1;
        }

        results.push(serde_json::json!({
            "test_id": test_id,
            "test_name": test.name,
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

    Ok(CommandResponse {
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
    })
}

// ========================================================================
// Test Result Commands
// ========================================================================

/// Get test results for a specific test
#[tauri::command]
pub async fn get_test_results(
    test_id: String,
    limit: Option<u32>,
    state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    match state
        .pg_db()
        .list_test_results(Some(&test_id), None, None, limit)
        .await
    {
        Ok(results) => Ok(CommandResponse {
            success: true,
            message: Some(format!("Found {} results", results.len())),
            data: Some(serde_json::to_value(results).unwrap_or_default()),
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to list results: {}", e)),
            data: None,
        }),
    }
}

/// Get test results for a task run
#[tauri::command]
pub async fn get_task_run_test_results(
    task_run_id: String,
    state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    match state
        .pg_db()
        .list_test_results(None, Some(&task_run_id), None, None)
        .await
    {
        Ok(results) => Ok(CommandResponse {
            success: true,
            message: Some(format!("Found {} results", results.len())),
            data: Some(serde_json::to_value(results).unwrap_or_default()),
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to list results: {}", e)),
            data: None,
        }),
    }
}

// ========================================================================
// Test Association Commands
// ========================================================================

/// Create a test association
#[tauri::command]
pub async fn create_test_association(
    test_id: String,
    config_id: Option<String>,
    workflow_name: Option<String>,
    trigger_point: String,
    action_id: Option<String>,
    execution_order: Option<i32>,
    state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    let _db = state.pg_db();
    let _test_id = test_id;
    let _config_id = config_id;
    let _workflow_name = workflow_name;
    let _trigger_point = trigger_point;
    let _action_id = action_id;
    let _execution_order = execution_order;

    // create_test_association not yet implemented on PgDb — stub
    Ok(CommandResponse {
        success: false,
        message: Some("create_test_association not yet implemented on PgDb".to_string()),
        data: None,
    })
}

/// Get test associations for a config
#[tauri::command]
pub async fn get_config_test_associations(
    config_id: String,
    trigger_point: Option<String>,
    state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    let _db = state.pg_db();
    let _config_id = config_id;
    let _trigger_point = trigger_point;

    // get_associations_for_config not yet implemented on PgDb — return empty
    Ok(CommandResponse {
        success: true,
        message: Some("Found 0 associations".to_string()),
        data: Some(serde_json::json!([])),
    })
}

/// Delete a test association
#[tauri::command]
pub async fn delete_test_association(
    id: String,
    state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    let _db = state.pg_db();
    let _id = id;

    // delete_test_association not yet implemented on PgDb — stub
    Ok(CommandResponse {
        success: false,
        message: Some("delete_test_association not yet implemented on PgDb".to_string()),
        data: None,
    })
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
pub async fn export_tests_to_file(
    test_ids: Vec<String>,
    file_path: String,
    state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    let db = state.pg_db();

    info!("Exporting {} tests to {}", test_ids.len(), file_path);

    // Get tests from database
    let mut exported_tests: Vec<ExportedTest> = Vec::new();

    for test_id in &test_ids {
        match db.get_verification_test(test_id).await {
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
                    timeout_seconds: test.timeout_seconds.unwrap_or(60),
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
        return Ok(CommandResponse {
            success: false,
            message: Some("No tests found to export".to_string()),
            data: None,
        });
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
                Ok(CommandResponse {
                    success: true,
                    message: Some(format!("Exported {} tests", export.tests.len())),
                    data: Some(serde_json::json!({
                        "count": export.tests.len(),
                        "file_path": file_path,
                    })),
                })
            }
            Err(e) => Ok(CommandResponse {
                success: false,
                message: Some(format!("Failed to write JSON: {}", e)),
                data: None,
            }),
        },
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to create file: {}", e)),
            data: None,
        }),
    }
}

/// Export all tests to a JSON file
#[tauri::command]
pub async fn export_all_tests_to_file(
    file_path: String,
    state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    let _db = state.pg_db();

    info!("Exporting all tests to {}", file_path);

    // list_verification_tests not yet implemented on PgDb — return empty
    Ok(CommandResponse {
        success: false,
        message: Some(
            "export_all_tests_to_file: list_verification_tests not yet implemented on PgDb"
                .to_string(),
        ),
        data: None,
    })
}

/// Import tests from a JSON file
#[tauri::command]
pub async fn import_tests_from_file(
    file_path: String,
    update_existing: bool,
    state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    let _db = state.pg_db();
    let _update_existing = update_existing;

    info!(
        "Importing tests from {} (update_existing: {})",
        file_path, update_existing
    );

    // import_tests_from_file: create/update_verification_test not yet on PgDb — stub
    Ok(CommandResponse {
        success: false,
        message: Some(
            "import_tests_from_file: create/update_verification_test not yet implemented on PgDb"
                .to_string(),
        ),
        data: None,
    })
}

// ========================================================================
// Page Analysis Commands for AI-Powered Test Generation
// ========================================================================

/// Response from page analysis
#[derive(Debug, Serialize, Deserialize)]
pub struct PageAnalysisResponse {
    pub success: bool,
    pub analysis: Option<serde_json::Value>,
    pub error: Option<String>,
}

/// Analyze a page via Playwright CDP for AI test generation.
///
/// Connects to an existing browser via Chrome DevTools Protocol,
/// captures a screenshot, and extracts DOM elements with bounding boxes,
/// text content, and CSS selectors.
///
/// # Arguments
/// * `cdp_port` - CDP port number (default: 9222)
/// * `state` - Application state containing the Python bridge
#[tauri::command]
pub fn analyze_page_playwright(
    cdp_port: Option<u16>,
    bridge: State<'_, BridgeCompartment>,
) -> CommandResponse {
    let port = cdp_port.unwrap_or(9222);
    info!("Analyzing page via Playwright CDP on port {}", port);

    if !is_default_bridge_running_compartment(&bridge) {
        return CommandResponse {
            success: false,
            message: Some(
                "Python executor not running. Please start the executor first.".to_string(),
            ),
            data: None,
        };
    }

    let params = serde_json::json!({
        "cdp_port": port,
    });

    // Use send_command_and_wait for synchronous response
    let result = with_default_bridge_compartment(&bridge, |bridge| {
        bridge.send_command_and_wait(
            "analyze_page_playwright",
            Some(params),
            std::time::Duration::from_secs(60),
        )
    });

    match result {
        Ok(Ok(response)) => {
            if response.success {
                CommandResponse {
                    success: true,
                    message: Some("Page analysis completed".to_string()),
                    data: response.data,
                }
            } else {
                CommandResponse {
                    success: false,
                    message: Some(
                        response
                            .error
                            .unwrap_or_else(|| "Analysis failed".to_string()),
                    ),
                    data: None,
                }
            }
        }
        Ok(Err(e)) => CommandResponse {
            success: false,
            message: Some(format!("Command failed: {}", e)),
            data: None,
        },
        Err(e) => CommandResponse {
            success: false,
            message: Some(format!("Bridge error: {}", e)),
            data: None,
        },
    }
}

/// Analyze a page by running a Playwright script for AI test generation.
///
/// Executes the provided Playwright script to navigate to a page,
/// then captures DOM elements with their bounding boxes, text content,
/// and CSS selectors.
///
/// # Arguments
/// * `script` - Playwright TypeScript/JavaScript code to navigate to the page
/// * `state` - Application state containing the Python bridge
#[tauri::command]
pub fn analyze_page_playwright_script(
    script: String,
    bridge: State<'_, BridgeCompartment>,
) -> CommandResponse {
    info!(
        "Analyzing page via Playwright script ({} chars)",
        script.len()
    );

    if !is_default_bridge_running_compartment(&bridge) {
        return CommandResponse {
            success: false,
            message: Some(
                "Python executor not running. Please start the executor first.".to_string(),
            ),
            data: None,
        };
    }

    let params = serde_json::json!({
        "script": script,
    });

    // Use longer timeout for script execution (may include navigation, waiting, etc.)
    let result = with_default_bridge_compartment(&bridge, |bridge| {
        bridge.send_command_and_wait(
            "analyze_page_playwright_script",
            Some(params),
            std::time::Duration::from_secs(120),
        )
    });

    match result {
        Ok(Ok(response)) => {
            if response.success {
                CommandResponse {
                    success: true,
                    message: Some("Page analysis completed".to_string()),
                    data: response.data,
                }
            } else {
                CommandResponse {
                    success: false,
                    message: Some(
                        response
                            .error
                            .unwrap_or_else(|| "Analysis failed".to_string()),
                    ),
                    data: None,
                }
            }
        }
        Ok(Err(e)) => CommandResponse {
            success: false,
            message: Some(format!("Command failed: {}", e)),
            data: None,
        },
        Err(e) => CommandResponse {
            success: false,
            message: Some(format!("Bridge error: {}", e)),
            data: None,
        },
    }
}

/// Analyze a page via Qontinui Vision for AI test generation.
///
/// Captures a screenshot from the specified monitor and runs OCR,
/// edge detection, and SAM3 segmentation to detect UI elements.
///
/// # Arguments
/// * `monitor_index` - Monitor index to capture (default: 0)
/// * `state` - Application state containing the Python bridge
#[tauri::command]
pub fn analyze_page_vision(
    monitor_index: Option<i32>,
    bridge: State<'_, BridgeCompartment>,
) -> CommandResponse {
    let monitor = monitor_index.unwrap_or(0);
    info!("Analyzing page via Vision on monitor {}", monitor);

    if !is_default_bridge_running_compartment(&bridge) {
        return CommandResponse {
            success: false,
            message: Some(
                "Python executor not running. Please start the executor first.".to_string(),
            ),
            data: None,
        };
    }

    let params = serde_json::json!({
        "monitor_index": monitor,
    });

    // Use send_command_and_wait for synchronous response
    let result = with_default_bridge_compartment(&bridge, |bridge| {
        bridge.send_command_and_wait(
            "analyze_page_vision",
            Some(params),
            std::time::Duration::from_secs(120), // Vision analysis can take longer
        )
    });

    match result {
        Ok(Ok(response)) => {
            if response.success {
                CommandResponse {
                    success: true,
                    message: Some("Page analysis completed".to_string()),
                    data: response.data,
                }
            } else {
                CommandResponse {
                    success: false,
                    message: Some(
                        response
                            .error
                            .unwrap_or_else(|| "Analysis failed".to_string()),
                    ),
                    data: None,
                }
            }
        }
        Ok(Err(e)) => CommandResponse {
            success: false,
            message: Some(format!("Command failed: {}", e)),
            data: None,
        },
        Err(e) => CommandResponse {
            success: false,
            message: Some(format!("Bridge error: {}", e)),
            data: None,
        },
    }
}

// ========================================================================
// AI Test Generation
// ========================================================================

/// Reference document for AI test generation context
#[derive(Debug, Serialize, Deserialize)]
pub struct ReferenceDocument {
    /// Unique ID for this document
    pub id: String,
    /// Display name for the document
    pub name: String,
    /// Document type (markdown, text, image, json)
    #[serde(rename = "type")]
    pub doc_type: String,
    /// Document content (text for markdown/text/json, base64 for images)
    pub content: String,
    /// Original file name if uploaded
    pub filename: Option<String>,
}

/// Input for AI test generation
#[derive(Debug, Serialize, Deserialize)]
pub struct GenerateTestInput {
    /// User's natural language description of the test
    pub user_prompt: String,
    /// Test type to generate
    pub test_type: String,
    /// Optional page analysis context (single page)
    pub page_analysis: Option<serde_json::Value>,
    /// Optional multi-request analysis context (multiple API responses)
    pub multi_request_analysis: Option<serde_json::Value>,
    /// Optional collected analyses (multiple analyses of different types)
    pub collected_analyses: Option<serde_json::Value>,
    /// Optional reference documents (specs, expected data, images)
    pub reference_documents: Option<Vec<ReferenceDocument>>,
    /// Optional workflow run context (data from a previous workflow execution)
    pub workflow_run_context: Option<serde_json::Value>,
}

/// Generate test code using AI.
///
/// Uses the configured AI provider (Claude CLI, Claude API, etc.) to generate
/// test code from a natural language description and optional page analysis context.
///
/// This command is async to avoid blocking the main thread during AI generation,
/// which can take a long time and would otherwise cause "Not Responding" on Windows.
///
/// # Arguments
/// * `input` - Generation parameters including prompt and test type
/// * `state` - Application state containing Python bridge and settings
#[tauri::command]
pub async fn generate_test_with_ai(
    input: GenerateTestInput,
    bridge: State<'_, BridgeCompartment>,
) -> Result<CommandResponse, String> {
    use crate::settings;

    info!(
        "Generating {} test with AI: {}",
        input.test_type,
        input.user_prompt.chars().take(50).collect::<String>()
    );

    // Get AI settings
    let ai_settings = settings::get_ai_settings();

    // Map provider to Python format
    let ai_provider = match ai_settings.provider {
        settings::AiProvider::ClaudeCli => "claude_cli",
        settings::AiProvider::ClaudeApi => "claude_api",
        settings::AiProvider::GeminiCli => "gemini_cli",
        settings::AiProvider::GeminiApi => "gemini_api",
    };

    // Build provider-specific settings
    let provider_settings = match ai_settings.provider {
        settings::AiProvider::ClaudeCli => {
            let execution_mode = match ai_settings.claude_cli.execution_mode {
                settings::CliExecutionMode::Auto => "auto",
                settings::CliExecutionMode::WindowsNative => "native",
                settings::CliExecutionMode::Wsl => "wsl",
                settings::CliExecutionMode::Native => "native",
            };
            serde_json::json!({
                "execution_mode": execution_mode,
                "timeout_seconds": ai_settings.claude_cli.timeout_seconds,
            })
        }
        settings::AiProvider::ClaudeApi => {
            // Get API key from keychain
            let api_key = super::ai_settings::get_provider_api_key("claude_api")
                .unwrap_or(None)
                .unwrap_or_default();
            serde_json::json!({
                "api_key": api_key,
                "model": ai_settings.claude_api.model,
                "max_tokens": ai_settings.claude_api.max_tokens,
            })
        }
        settings::AiProvider::GeminiCli | settings::AiProvider::GeminiApi => {
            serde_json::json!({})
        }
    };

    // Clone state for use in spawn_blocking
    let bridge_compartment = bridge.inner().clone();

    // Build params before moving into spawn_blocking
    let params = serde_json::json!({
        "user_prompt": input.user_prompt,
        "test_type": input.test_type,
        "page_analysis": input.page_analysis,
        "multi_request_analysis": input.multi_request_analysis,
        "collected_analyses": input.collected_analyses,
        "reference_documents": input.reference_documents,
        "workflow_run_context": input.workflow_run_context,
        "ai_provider": ai_provider,
        "ai_settings": provider_settings,
    });

    // SQLite: complex function — Python bridge IPC for AI test generation (sync bridge call)
    let result = tokio::task::spawn_blocking(move || {
        if !is_default_bridge_running_compartment(&bridge_compartment) {
            return CommandResponse {
                success: false,
                message: Some(
                    "Python executor not running. Please start the executor first.".to_string(),
                ),
                data: None,
            };
        }

        // Use longer timeout for AI generation (can take time)
        let bridge_result = with_default_bridge_compartment(&bridge_compartment, |bridge| {
            bridge.send_command_and_wait(
                "generate_test_with_ai",
                Some(params),
                std::time::Duration::from_secs(180),
            )
        });

        match bridge_result {
            Ok(Ok(response)) => {
                if response.success {
                    CommandResponse {
                        success: true,
                        message: Some("Test generated successfully".to_string()),
                        data: response.data,
                    }
                } else {
                    CommandResponse {
                        success: false,
                        message: Some(
                            response
                                .error
                                .unwrap_or_else(|| "Generation failed".to_string()),
                        ),
                        data: None,
                    }
                }
            }
            Ok(Err(e)) => CommandResponse {
                success: false,
                message: Some(format!("Command failed: {}", e)),
                data: None,
            },
            Err(e) => CommandResponse {
                success: false,
                message: Some(format!("Bridge error: {}", e)),
                data: None,
            },
        }
    })
    .await;

    // Handle spawn_blocking result
    match result {
        Ok(response) => Ok(response),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Task execution error: {}", e)),
            data: None,
        }),
    }
}

/// Input for generating test metadata from code
#[derive(Debug, Serialize, Deserialize)]
pub struct GenerateMetadataInput {
    /// The test code to analyze
    pub test_code: String,
    /// The type of test (playwright_cdp, python_script, etc.)
    pub test_type: String,
}

/// Generated metadata from test code analysis
#[derive(Debug, Serialize, Deserialize)]
pub struct GeneratedMetadata {
    /// Suggested test name
    pub name: Option<String>,
    /// Suggested description
    pub description: Option<String>,
    /// Suggested category
    pub category: Option<String>,
    /// Suggested tags
    pub tags: Vec<String>,
}

/// Generate test metadata by analyzing test code.
///
/// Uses local pattern matching to extract meaningful metadata from test code.
/// This is fast and doesn't require the Python executor or AI provider.
///
/// # Arguments
/// * `test_code` - The test code to analyze
/// * `test_type` - The type of test (playwright_cdp, python_script, etc.)
#[tauri::command]
pub fn generate_test_metadata(input: GenerateMetadataInput) -> CommandResponse {
    info!(
        "Generating metadata for {} test ({} chars)",
        input.test_type,
        input.test_code.len()
    );

    let metadata = analyze_test_code(&input.test_code, &input.test_type);

    CommandResponse {
        success: true,
        message: Some("Metadata generated from code analysis".to_string()),
        data: Some(serde_json::to_value(metadata).unwrap_or_default()),
    }
}

/// Analyze test code and extract metadata
fn analyze_test_code(code: &str, test_type: &str) -> GeneratedMetadata {
    let mut name = None;
    let mut description = None;
    let mut category = None;
    let mut tags = Vec::new();

    match test_type {
        "playwright_cdp" => {
            // Extract test name from test('name', ...) or test.describe('name', ...)
            if let Some(cap) = regex_find(code, r#"test\s*\(\s*['"]([^'"]+)['"]"#) {
                name = Some(cap);
            } else if let Some(cap) = regex_find(code, r#"test\.describe\s*\(\s*['"]([^'"]+)['"]"#)
            {
                name = Some(cap);
            }

            // Detect category from patterns
            if code.contains("toBeVisible") || code.contains("screenshot") {
                category = Some("visual".to_string());
                tags.push("ui".to_string());
            }
            if code.contains("locator")
                || code.contains("getByRole")
                || code.contains("querySelector")
            {
                category = category.or(Some("dom".to_string()));
                tags.push("dom".to_string());
            }
            if code.contains("request") || code.contains("response") || code.contains("fetch") {
                tags.push("network".to_string());
            }
            if code.contains("waitFor") || code.contains("expect") {
                tags.push("assertions".to_string());
            }

            // Build description from assertions
            let assertions: Vec<&str> = code
                .lines()
                .filter(|l| l.contains("expect(") || l.contains("toHave") || l.contains("toBe"))
                .take(3)
                .collect();
            if !assertions.is_empty() {
                description = Some(format!(
                    "Verifies: {}",
                    assertions
                        .iter()
                        .map(|a| a.trim())
                        .collect::<Vec<_>>()
                        .join("; ")
                ));
            }
        }
        "python_script" => {
            // Extract function name from def test_xxx() or def xxx()
            if let Some(cap) = regex_find(code, r#"def\s+(test_\w+|check_\w+|\w+_test)\s*\("#) {
                let func_name = cap
                    .replace("test_", "")
                    .replace("_test", "")
                    .replace("check_", "")
                    .replace('_', " ");
                name = Some(capitalize_words(&func_name));
            }

            // Detect category from imports and patterns
            if code.contains("requests") || code.contains("httpx") || code.contains("aiohttp") {
                category = Some("network".to_string());
                tags.push("api".to_string());
            }
            if code.contains("selenium") || code.contains("playwright") {
                category = Some("integration".to_string());
                tags.push("browser".to_string());
            }
            if code.contains("pytest") || code.contains("unittest") {
                tags.push("pytest".to_string());
            }
            if code.contains("assert ") || code.contains("assertEqual") {
                tags.push("assertions".to_string());
            }
            if code.contains("json") || code.contains("dict") || code.contains("response.") {
                tags.push("data".to_string());
            }

            // Build description from docstring
            if let Some(docstring) = extract_docstring(code) {
                description = Some(docstring);
            } else {
                // Build from assertions
                let assertions: Vec<&str> = code
                    .lines()
                    .filter(|l| l.trim().starts_with("assert "))
                    .take(3)
                    .collect();
                if !assertions.is_empty() {
                    description = Some(format!(
                        "Verifies: {}",
                        assertions
                            .iter()
                            .map(|a| a.trim().trim_start_matches("assert "))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ));
                }
            }
        }
        "qontinui_vision" | "repository_test" => {
            // JSON-based configs - try to parse and extract
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(code) {
                if let Some(n) = json.get("name").and_then(|v| v.as_str()) {
                    name = Some(n.to_string());
                }
                if let Some(d) = json.get("description").and_then(|v| v.as_str()) {
                    description = Some(d.to_string());
                }
                if let Some(cmd) = json.get("command").and_then(|v| v.as_str()) {
                    if cmd.contains("pytest") {
                        tags.push("pytest".to_string());
                        category = Some("unit".to_string());
                    } else if cmd.contains("jest") || cmd.contains("vitest") {
                        tags.push("jest".to_string());
                        category = Some("unit".to_string());
                    } else if cmd.contains("cargo test") {
                        tags.push("cargo".to_string());
                        category = Some("unit".to_string());
                    }
                }
            }
            if test_type == "qontinui_vision" {
                category = Some("visual".to_string());
                tags.push("vision".to_string());
            }
        }
        _ => {}
    }

    // Default category if not detected
    if category.is_none() {
        category = Some("custom".to_string());
    }

    // Dedupe tags
    tags.sort();
    tags.dedup();

    GeneratedMetadata {
        name,
        description,
        category,
        tags,
    }
}

/// Simple regex find that returns the first capture group
fn regex_find(text: &str, pattern: &str) -> Option<String> {
    use regex::Regex;
    if let Ok(re) = Regex::new(pattern) {
        if let Some(caps) = re.captures(text) {
            if let Some(m) = caps.get(1) {
                return Some(m.as_str().to_string());
            }
        }
    }
    None
}

/// Extract docstring from Python code
fn extract_docstring(code: &str) -> Option<String> {
    // Look for triple-quoted strings after def
    let patterns = [
        r#"def\s+\w+\s*\([^)]*\)\s*:\s*"""([^"]+)""""#,
        r#"def\s+\w+\s*\([^)]*\)\s*:\s*'''([^']+)'''"#,
    ];
    for pattern in patterns {
        if let Some(docstring) = regex_find(code, pattern) {
            let trimmed = docstring.trim().lines().next().unwrap_or("").to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

/// Capitalize first letter of each word
fn capitalize_words(s: &str) -> String {
    s.split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ========================================================================
// Workflow Run Context for AI Test Generation
// ========================================================================

/// Summary of a task run for UI selection
#[derive(Debug, Serialize, Deserialize)]
pub struct TaskRunSummary {
    pub id: String,
    pub task_name: String,
    pub workflow_name: Option<String>,
    pub status: String,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub duration_ms: Option<i64>,
    pub goal_achieved: Option<bool>,
}

/// Full workflow run context for AI test generation
#[derive(Debug, Serialize, Deserialize)]
pub struct WorkflowRunContext {
    pub task_run: serde_json::Value,
    pub automation: Option<serde_json::Value>,
    pub screenshots: Vec<serde_json::Value>,
    pub api_requests: Vec<serde_json::Value>,
    pub playwright_results: Vec<serde_json::Value>,
    pub events: Vec<serde_json::Value>,
    pub knowledge: Vec<serde_json::Value>,
    pub test_results: Vec<serde_json::Value>,
}

/// List recent task runs for workflow context selection.
///
/// Returns a list of recent task runs that can be selected for AI test generation context.
#[tauri::command]
pub async fn list_recent_task_runs(
    limit: Option<i32>,
    state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    info!("[list_recent_task_runs] Called with limit: {:?}", limit);
    let limit = limit.unwrap_or(20);

    let pg_runs = state.pg_db().list_recent_task_runs_pg(limit).await?;

    let runs: Vec<TaskRunSummary> = pg_runs
        .into_iter()
        .map(|row| TaskRunSummary {
            id: row["id"].as_str().unwrap_or("").to_string(),
            task_name: row["task_name"].as_str().unwrap_or("").to_string(),
            workflow_name: row["workflow_name"].as_str().map(|s| s.to_string()),
            status: row["status"].as_str().unwrap_or("").to_string(),
            created_at: row["created_at"].as_str().unwrap_or("").to_string(),
            completed_at: row["completed_at"].as_str().map(|s| s.to_string()),
            goal_achieved: row["goal_achieved"].as_bool(),
            duration_ms: row["duration_ms"].as_i64(),
        })
        .collect();

    info!("[list_recent_task_runs] Found {} task runs", runs.len());

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} recent task runs", runs.len())),
        data: Some(serde_json::to_value(&runs).unwrap_or_default()),
    })
}

/// Get full workflow run context for AI test generation.
///
/// Returns comprehensive context about a workflow run including automation results,
/// screenshots, API requests, events, and AI findings.
#[tauri::command]
pub async fn get_workflow_run_context(
    task_run_id: String,
    state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    match state.pg_db().get_workflow_run_context_pg(&task_run_id).await {
        Ok(Some(context)) => Ok(CommandResponse {
            success: true,
            message: None,
            data: Some(context),
        }),
        Ok(None) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Task run not found: {}", task_run_id)),
            data: None,
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to get workflow run context: {}", e)),
            data: None,
        }),
    }
}

// LEGACY get_workflow_run_context_inner removed — was checkpoint_db SQLite-only dead code.

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_testing")
        .invoke_handler(tauri::generate_handler![
            execute_verification_test,
            execute_verification_test_suite,
            get_test_type_info,
            validate_test_definition,
            list_verification_tests,
            get_verification_test,
            create_verification_test,
            update_verification_test,
            delete_verification_test,
            execute_test_by_id,
            execute_tests_by_ids,
            get_test_results,
            get_task_run_test_results,
            create_test_association,
            get_config_test_associations,
            delete_test_association,
            export_tests_to_file,
            export_all_tests_to_file,
            import_tests_from_file,
            analyze_page_playwright,
            analyze_page_playwright_script,
            analyze_page_vision,
            generate_test_with_ai,
            generate_test_metadata,
            list_recent_task_runs,
            get_workflow_run_context,
        ])
        .build()
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
