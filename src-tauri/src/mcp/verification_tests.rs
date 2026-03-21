//! Verification test handlers for MCP API
//!
//! Provides HTTP handlers for verification test CRUD, execution,
//! and test result management.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{info, warn};

use crate::commands::testing::{
    execute_verification_test, execute_verification_test_suite, ExecuteTestResponse,
    ExecuteTestSuiteRequest, ExecuteTestSuiteResponse,
};
use crate::database::{CreateVerificationTestInput, TestResultStatus};
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::test_executor::{RepoTestConfig, TestCategory, TestDefinition, TestType, VisionConfig};

// ============================================================================
// Types
// ============================================================================

/// Request for listing tests with filters
#[derive(Debug, Deserialize)]
pub struct ListTestsQuery {
    enabled_only: Option<bool>,
    test_type: Option<String>,
    category: Option<String>,
}

/// Request for listing test results with filters
#[derive(Debug, Deserialize)]
pub struct ListTestResultsQuery {
    test_id: Option<String>,
    task_run_id: Option<String>,
    status: Option<String>,
    limit: Option<u32>,
}

/// Request for executing a test by ID
#[derive(Debug, Deserialize)]
pub struct ExecuteTestByIdRequest {
    task_run_id: Option<String>,
}

// ============================================================================
// Handlers
// ============================================================================

/// List all verification tests
pub async fn list_tests(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListTestsQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::VerificationTest>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let db = &state.app_state.checkpoint_db;

    let test_type_enum: Option<crate::database::TestType> =
        query.test_type.as_ref().and_then(|t| t.parse().ok());

    match db.list_verification_tests(
        query.enabled_only.unwrap_or(false),
        test_type_enum.as_ref(),
        query.category.as_deref(),
    ) {
        Ok(tests) => Ok(Json(ApiResponse::success(tests))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to list tests: {}", e))),
        )),
    }
}

/// Get a verification test by ID
pub async fn get_test(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<crate::database::VerificationTest>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let db = &state.app_state.checkpoint_db;

    match db.get_verification_test(&id) {
        Ok(Some(test)) => Ok(Json(ApiResponse::success(test))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Test not found: {}", id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get test: {}", e))),
        )),
    }
}

/// Create a new verification test
pub async fn create_test(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<CreateVerificationTestInput>,
) -> Result<Json<ApiResponse<crate::database::VerificationTest>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let db = &state.app_state.checkpoint_db;

    info!(
        "Creating verification test: {} (type: {:?})",
        input.name, input.test_type
    );

    match db.create_verification_test(&input) {
        Ok(test) => Ok(Json(ApiResponse::success(test))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to create test: {}", e))),
        )),
    }
}

/// Update a verification test
pub async fn update_test(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(input): Json<CreateVerificationTestInput>,
) -> Result<Json<ApiResponse<crate::database::VerificationTest>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let db = &state.app_state.checkpoint_db;

    info!("Updating verification test: {} ({})", input.name, id);

    match db.update_verification_test(&id, &input) {
        Ok(test) => Ok(Json(ApiResponse::success(test))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to update test: {}", e))),
        )),
    }
}

/// Delete a verification test
pub async fn delete_test(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = &state.app_state.checkpoint_db;

    info!("Deleting verification test: {}", id);

    match db.delete_verification_test(&id) {
        Ok(true) => Ok(Json(ApiResponse::success(()))),
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Test not found: {}", id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to delete test: {}", e))),
        )),
    }
}

/// Execute a verification test by ID
pub async fn execute_test_by_id(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<ExecuteTestByIdRequest>,
) -> Result<Json<ApiResponse<ExecuteTestResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = &state.app_state.checkpoint_db;

    // Get the test from database
    let test = match db.get_verification_test(&id) {
        Ok(Some(test)) => test,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Test not found: {}", id))),
            ))
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get test: {}", e))),
            ))
        }
    };

    // Convert to TestDefinition
    let test_def = db_test_to_definition(&test);

    info!(
        "Executing verification test: {} (type: {:?})",
        test.name, test.test_type
    );

    // Execute the test
    let response = execute_verification_test(test_def);

    // Store the result if task_run_id is provided
    if let Some(task_run_id) = request.task_run_id {
        let result_input = crate::database::CreateTestResultInput {
            test_id: id.clone(),
            task_run_id: Some(task_run_id),
        };

        match db.create_test_result(&result_input) {
            Ok(test_result) => {
                // Convert status
                let db_status = match response.result.status {
                    crate::test_executor::TestStatus::Passed => TestResultStatus::Passed,
                    crate::test_executor::TestStatus::Failed => TestResultStatus::Failed,
                    crate::test_executor::TestStatus::Error => TestResultStatus::Error,
                    crate::test_executor::TestStatus::Timeout => TestResultStatus::Timeout,
                    crate::test_executor::TestStatus::Skipped => TestResultStatus::Skipped,
                    _ => TestResultStatus::Error,
                };

                // Update with execution results
                if let Err(e) = db.update_test_result(
                    &test_result.id,
                    &db_status,
                    Some(&response.result.output),
                    response.result.error.as_deref(),
                    response.result.structured_output.as_ref(),
                    response.result.assertions_passed,
                    response.result.assertions_failed,
                    &response.result.screenshots,
                ) {
                    warn!("Failed to update test result: {}", e);
                }
            }
            Err(e) => {
                warn!("Failed to create test result: {}", e);
            }
        }
    }

    Ok(Json(ApiResponse::success(response)))
}

/// Execute multiple tests as a suite
pub async fn execute_test_suite_handler(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<ExecuteTestSuiteRequest>,
) -> Result<Json<ApiResponse<ExecuteTestSuiteResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "Executing test suite: {} tests (parallel: {})",
        request.tests.len(),
        request.parallel
    );

    let response = execute_verification_test_suite(request);
    Ok(Json(ApiResponse::success(response)))
}

/// List test results with optional filtering
pub async fn list_test_results(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListTestResultsQuery>,
) -> Result<Json<ApiResponse<Vec<crate::database::TestResult>>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let db = &state.app_state.checkpoint_db;

    // Parse status filter
    let status_enum: Option<TestResultStatus> =
        query
            .status
            .as_ref()
            .and_then(|s| match s.to_lowercase().as_str() {
                "pending" => Some(TestResultStatus::Pending),
                "running" => Some(TestResultStatus::Running),
                "passed" => Some(TestResultStatus::Passed),
                "failed" => Some(TestResultStatus::Failed),
                "skipped" => Some(TestResultStatus::Skipped),
                "error" => Some(TestResultStatus::Error),
                "timeout" => Some(TestResultStatus::Timeout),
                _ => None,
            });

    let limit = query.limit.unwrap_or(100);

    // Query based on provided filters
    let results = if let Some(test_id) = &query.test_id {
        db.get_results_for_test(test_id, Some(limit))
    } else if let Some(task_run_id) = &query.task_run_id {
        db.get_results_for_task_run(task_run_id)
    } else {
        db.list_test_results(status_enum.as_ref(), limit)
    };

    match results {
        Ok(results) => Ok(Json(ApiResponse::success(results))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to list test results: {}", e))),
        )),
    }
}

/// Get a specific test result by ID
pub async fn get_test_result(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<crate::database::TestResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = &state.app_state.checkpoint_db;

    match db.get_test_result(&id) {
        Ok(Some(result)) => Ok(Json(ApiResponse::success(result))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Test result not found: {}", id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get test result: {}", e))),
        )),
    }
}

/// Get test history summary (aggregated stats)
pub async fn get_test_history(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListTestResultsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = &state.app_state.checkpoint_db;
    let limit = query.limit.unwrap_or(1000);

    // Get all results (or filtered by test_id if provided)
    let results = if let Some(test_id) = &query.test_id {
        db.get_results_for_test(test_id, Some(limit))
    } else {
        db.list_test_results(None, limit)
    };

    match results {
        Ok(results) => {
            // Aggregate stats
            let mut total = 0;
            let mut passed = 0;
            let mut failed = 0;
            let mut error = 0;
            let mut timeout = 0;
            let mut skipped = 0;
            let mut total_duration_ms: i64 = 0;

            for result in &results {
                total += 1;
                total_duration_ms += result.duration_ms.unwrap_or(0);
                match result.status {
                    TestResultStatus::Passed => passed += 1,
                    TestResultStatus::Failed => failed += 1,
                    TestResultStatus::Error => error += 1,
                    TestResultStatus::Timeout => timeout += 1,
                    TestResultStatus::Skipped => skipped += 1,
                    _ => {}
                }
            }

            let pass_rate = if total > 0 {
                (passed as f64 / total as f64) * 100.0
            } else {
                0.0
            };

            let summary = serde_json::json!({
                "total_runs": total,
                "passed": passed,
                "failed": failed,
                "error": error,
                "timeout": timeout,
                "skipped": skipped,
                "pass_rate": format!("{:.1}%", pass_rate),
                "total_duration_ms": total_duration_ms,
                "average_duration_ms": if total > 0 { total_duration_ms / total as i64 } else { 0 },
                "recent_results": results.iter().take(20).collect::<Vec<_>>(),
            });

            Ok(Json(ApiResponse::success(summary)))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get test history: {}", e))),
        )),
    }
}

// ============================================================================
// Internal Helpers
// ============================================================================

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

    // Convert test type
    let test_type = match test.test_type {
        crate::database::TestType::PlaywrightCdp => TestType::PlaywrightCdp,
        crate::database::TestType::QontinuiVision => TestType::QontinuiVision,
        crate::database::TestType::PythonScript => TestType::PythonScript,
        crate::database::TestType::RepositoryTest => TestType::RepositoryTest,
    };

    TestDefinition {
        id: test.id.clone(),
        name: test.name.clone(),
        test_type,
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

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/tests", get(list_tests).post(create_test))
        .route("/tests/execute-suite", post(execute_test_suite_handler))
        .route("/tests/history", get(get_test_history))
        .route(
            "/tests/{id}",
            get(get_test).put(update_test).delete(delete_test),
        )
        .route("/tests/{id}/execute", post(execute_test_by_id))
        .route("/test-results", get(list_test_results))
        .route("/test-results/{id}", get(get_test_result))
}
