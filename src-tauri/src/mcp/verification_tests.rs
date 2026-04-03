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
    // checkpoint_db removed — verification tests not yet migrated to PG
    let _ = query;
    Ok(Json(ApiResponse::success(vec![])))
}

/// Get a verification test by ID
pub async fn get_test(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<crate::database::VerificationTest>>, (StatusCode, Json<ApiResponse<()>>)>
{
    // checkpoint_db removed — use PG for get
    match state.app_state.pg_db.get_verification_test(&id).await {
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
    State(_state): State<Arc<ApiState>>,
    Json(_input): Json<CreateVerificationTestInput>,
) -> Result<Json<ApiResponse<crate::database::VerificationTest>>, (StatusCode, Json<ApiResponse<()>>)>
{
    // checkpoint_db removed — create not yet migrated to PG
    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(api_error(
            "Verification tests: SQLite removed, not yet migrated to PG".to_string(),
        )),
    ))
}

/// Update a verification test
pub async fn update_test(
    State(_state): State<Arc<ApiState>>,
    Path(_id): Path<String>,
    Json(_input): Json<CreateVerificationTestInput>,
) -> Result<Json<ApiResponse<crate::database::VerificationTest>>, (StatusCode, Json<ApiResponse<()>>)>
{
    // checkpoint_db removed — update not yet migrated to PG
    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(api_error(
            "Verification tests: SQLite removed, not yet migrated to PG".to_string(),
        )),
    ))
}

/// Delete a verification test
pub async fn delete_test(
    State(_state): State<Arc<ApiState>>,
    Path(_id): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — delete not yet migrated to PG
    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(api_error(
            "Verification tests: SQLite removed, not yet migrated to PG".to_string(),
        )),
    ))
}

/// Execute a verification test by ID
pub async fn execute_test_by_id(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<ExecuteTestByIdRequest>,
) -> Result<Json<ApiResponse<ExecuteTestResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Get the test from PG
    let test = match state.app_state.pg_db.get_verification_test(&id).await {
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

    // checkpoint_db removed — test result storage not yet migrated to PG
    let _ = request.task_run_id;

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
    // checkpoint_db removed — test results not yet migrated to PG
    let _ = query;
    Ok(Json(ApiResponse::success(vec![])))
}

/// Get a specific test result by ID
pub async fn get_test_result(
    State(_state): State<Arc<ApiState>>,
    Path(_id): Path<String>,
) -> Result<Json<ApiResponse<crate::database::TestResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — test results not yet migrated to PG
    Err((
        StatusCode::NOT_FOUND,
        Json(api_error(
            "Test result not found (SQLite removed)".to_string(),
        )),
    ))
}

/// Get test history summary (aggregated stats)
pub async fn get_test_history(
    State(_state): State<Arc<ApiState>>,
    Query(_query): Query<ListTestResultsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — test history not yet migrated to PG
    Ok(Json(ApiResponse::success(serde_json::json!({
        "total_runs": 0,
        "passed": 0,
        "failed": 0,
        "error": 0,
        "timeout": 0,
        "skipped": 0,
        "pass_rate": "0.0%",
        "total_duration_ms": 0,
        "average_duration_ms": 0,
        "recent_results": [],
    }))))
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
