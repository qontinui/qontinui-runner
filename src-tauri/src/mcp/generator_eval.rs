//! Generator Evaluation HTTP API handlers
//!
//! Provides endpoints for the Generator Evaluation page:
//! - Dashboard metrics and trends
//! - Pipeline artifact inspection
//! - Edit analysis from feedback data
//! - Benchmark management and execution
//! - Example library (ground truth) management
//!
//! NOTE: All spawn_blocking calls in this module wrap complex SQLite operations
//! (pipeline artifacts, benchmarks, edit analysis, example workflows) that have
//! no PG equivalents yet. They use `checkpoint_db` directly.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post, put},
    Router,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::database::BenchmarkUpdate;
use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Query Parameters
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

fn default_limit() -> u32 {
    20
}

#[derive(Debug, Deserialize)]
pub struct TrendsParams {
    #[serde(default = "default_days")]
    pub days: u32,
}

fn default_days() -> u32 {
    30
}

#[derive(Debug, Deserialize)]
pub struct BenchmarkResultsParams {
    #[serde(default = "default_results_limit")]
    pub limit: u32,
}

fn default_results_limit() -> u32 {
    20
}

// ============================================================================
// Request Types
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct CreateBenchmarkRequest {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    pub expected_structure: crate::workflow_generation::benchmark::ExpectedStructure,
}

#[derive(Debug, Deserialize)]
pub struct UpdateExampleStatusRequest {
    pub example_status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FeedbackRequest {
    pub workflow_id: String,
    #[serde(default)]
    pub workflow_name: Option<String>,
    pub feedback_type: String,
    #[serde(default)]
    pub edited_field: Option<String>,
    #[serde(default)]
    pub old_value: Option<String>,
    #[serde(default)]
    pub new_value: Option<String>,
    #[serde(default)]
    pub rating: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct CleanupRequest {
    #[serde(default = "default_cleanup_days")]
    pub older_than_days: u32,
}

fn default_cleanup_days() -> u32 {
    30
}

#[derive(Debug, Deserialize)]
pub struct ImportBenchmarksRequest {
    pub benchmarks: Vec<ImportBenchmarkEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ImportBenchmarkEntry {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    pub expected_structure: crate::workflow_generation::benchmark::ExpectedStructure,
    #[serde(default)]
    pub enabled: Option<bool>,
}

// ============================================================================
// Dashboard Endpoints
// ============================================================================

async fn dashboard_handler(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — generator eval not yet migrated to PG
    Ok(Json(ApiResponse::success(serde_json::json!({}))))
}

async fn trends_handler(
    State(_state): State<Arc<ApiState>>,
    Query(_params): Query<TrendsParams>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — generator eval not yet migrated to PG
    Ok(Json(ApiResponse::success(serde_json::json!([]))))
}

// ============================================================================
// Pipeline Inspector Endpoints
// ============================================================================

async fn list_artifacts_handler(
    State(_state): State<Arc<ApiState>>,
    Query(_params): Query<PaginationParams>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — generator eval not yet migrated to PG
    Ok(Json(ApiResponse::success(serde_json::json!([]))))
}

async fn get_artifact_handler(
    State(_state): State<Arc<ApiState>>,
    Path(_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — generator eval not yet migrated to PG
    Err((StatusCode::NOT_FOUND, Json(api_error("Artifact not found (SQLite removed)"))))
}

async fn get_artifact_by_workflow_handler(
    State(_state): State<Arc<ApiState>>,
    Path(_workflow_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — generator eval not yet migrated to PG
    Err((
        StatusCode::NOT_FOUND,
        Json(api_error("No artifact found for this workflow (SQLite removed)")),
    ))
}

// ============================================================================
// Edit Analysis Endpoints
// ============================================================================

async fn edit_analysis_handler(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — generator eval not yet migrated to PG
    Ok(Json(ApiResponse::success(serde_json::json!({}))))
}

// ============================================================================
// Benchmark Endpoints
// ============================================================================

async fn list_benchmarks_handler(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — generator eval not yet migrated to PG
    Ok(Json(ApiResponse::success(serde_json::json!([]))))
}

async fn create_benchmark_handler(
    State(_state): State<Arc<ApiState>>,
    Json(_body): Json<CreateBenchmarkRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — generator eval not yet migrated to PG
    Err((StatusCode::NOT_IMPLEMENTED, Json(api_error("Benchmarks: SQLite removed, not yet migrated to PG"))))
}

async fn get_benchmark_handler(
    State(_state): State<Arc<ApiState>>,
    Path(_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — generator eval not yet migrated to PG
    Err((StatusCode::NOT_FOUND, Json(api_error("Benchmark not found (SQLite removed)"))))
}

async fn update_benchmark_handler(
    State(_state): State<Arc<ApiState>>,
    Path(_id): Path<String>,
    Json(_body): Json<BenchmarkUpdate>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — generator eval not yet migrated to PG
    Err((StatusCode::NOT_IMPLEMENTED, Json(api_error("Benchmarks: SQLite removed, not yet migrated to PG"))))
}

async fn delete_benchmark_handler(
    State(_state): State<Arc<ApiState>>,
    Path(_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — generator eval not yet migrated to PG
    Err((StatusCode::NOT_IMPLEMENTED, Json(api_error("Benchmarks: SQLite removed, not yet migrated to PG"))))
}

async fn run_benchmark_handler(
    State(_state): State<Arc<ApiState>>,
    Path(_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — benchmarks not yet migrated to PG
    Err((StatusCode::NOT_IMPLEMENTED, Json(api_error("Benchmarks: SQLite removed, not yet migrated to PG"))))
}

async fn list_benchmark_results_handler(
    State(_state): State<Arc<ApiState>>,
    Path(_id): Path<String>,
    Query(_params): Query<BenchmarkResultsParams>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — generator eval not yet migrated to PG
    Ok(Json(ApiResponse::success(serde_json::json!([]))))
}

// ============================================================================
// Feedback Endpoint
// ============================================================================

async fn save_feedback_handler(
    State(_state): State<Arc<ApiState>>,
    Json(_body): Json<FeedbackRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — generator feedback not yet migrated to PG
    Err((StatusCode::NOT_IMPLEMENTED, Json(api_error("Generator feedback: SQLite removed, not yet migrated to PG"))))
}

// ============================================================================
// Artifact Cleanup Endpoint
// ============================================================================

async fn cleanup_artifacts_handler(
    State(_state): State<Arc<ApiState>>,
    Json(_body): Json<CleanupRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — artifact cleanup not yet migrated to PG
    Ok(Json(ApiResponse::success(serde_json::json!({
        "deleted_count": 0,
        "older_than_days": 0
    }))))
}

// ============================================================================
// Benchmark Import/Export Endpoints
// ============================================================================

async fn export_benchmarks_handler(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — generator eval not yet migrated to PG
    Ok(Json(ApiResponse::success(serde_json::json!([]))))
}

async fn import_benchmarks_handler(
    State(_state): State<Arc<ApiState>>,
    Json(_body): Json<ImportBenchmarksRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — generator eval not yet migrated to PG
    Err((StatusCode::NOT_IMPLEMENTED, Json(api_error("Benchmarks: SQLite removed, not yet migrated to PG"))))
}

// ============================================================================
// Example Library Endpoints
// ============================================================================

async fn list_examples_handler(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — generator eval not yet migrated to PG
    Ok(Json(ApiResponse::success(serde_json::json!([]))))
}

async fn update_example_status_handler(
    State(_state): State<Arc<ApiState>>,
    Path(_workflow_id): Path<String>,
    Json(_body): Json<UpdateExampleStatusRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — generator eval not yet migrated to PG
    Err((StatusCode::NOT_IMPLEMENTED, Json(api_error("Example status: SQLite removed, not yet migrated to PG"))))
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        // Dashboard
        .route("/generator-eval/dashboard", get(dashboard_handler))
        .route("/generator-eval/trends", get(trends_handler))
        // Pipeline Inspector
        .route("/generator-eval/artifacts", get(list_artifacts_handler))
        // Artifact Cleanup (static path before {id} param)
        .route(
            "/generator-eval/artifacts/cleanup",
            delete(cleanup_artifacts_handler),
        )
        .route(
            "/generator-eval/artifacts/by-workflow/{workflow_id}",
            get(get_artifact_by_workflow_handler),
        )
        .route("/generator-eval/artifacts/{id}", get(get_artifact_handler))
        // Edit Analysis + Feedback
        .route("/generator-eval/edit-analysis", get(edit_analysis_handler))
        .route("/generator-eval/feedback", post(save_feedback_handler))
        // Benchmarks
        .route(
            "/generator-eval/benchmarks",
            get(list_benchmarks_handler).post(create_benchmark_handler),
        )
        // Benchmark Import/Export (static paths before {id} param)
        .route(
            "/generator-eval/benchmarks/export",
            get(export_benchmarks_handler),
        )
        .route(
            "/generator-eval/benchmarks/import",
            post(import_benchmarks_handler),
        )
        .route(
            "/generator-eval/benchmarks/{id}",
            get(get_benchmark_handler)
                .put(update_benchmark_handler)
                .delete(delete_benchmark_handler),
        )
        .route(
            "/generator-eval/benchmarks/{id}/run",
            post(run_benchmark_handler),
        )
        .route(
            "/generator-eval/benchmarks/{id}/results",
            get(list_benchmark_results_handler),
        )
        // Example Library
        .route("/generator-eval/examples", get(list_examples_handler))
        .route(
            "/generator-eval/examples/{workflow_id}",
            put(update_example_status_handler),
        )
        // Training Data Export
        .route("/generator-eval/training-data", get(training_data_handler))
        // Prompt Analysis Insights
        .route("/generator-eval/insights", get(prompt_insights_handler))
}

// ============================================================================
// Training Data Export
// ============================================================================

#[derive(Deserialize)]
struct TrainingDataQuery {
    agent: String,
    #[serde(default = "default_min_score")]
    min_score: f64,
    #[serde(default = "default_training_limit")]
    limit: u32,
}

fn default_min_score() -> f64 {
    0.0
}
fn default_training_limit() -> u32 {
    1000
}

async fn training_data_handler(
    State(_state): State<Arc<ApiState>>,
    Query(_params): Query<TrainingDataQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::workflow_generation::training_data::TrainingExample>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    // checkpoint_db removed — training data not yet migrated to PG
    Ok(Json(ApiResponse::success(vec![])))
}

// ============================================================================
// Prompt Analysis Insights
// ============================================================================

async fn prompt_insights_handler(
    State(_state): State<Arc<ApiState>>,
) -> Result<
    Json<ApiResponse<Vec<crate::workflow_generation::prompt_analysis::PromptInsight>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    // checkpoint_db removed — prompt insights not yet migrated to PG
    Ok(Json(ApiResponse::success(vec![])))
}
