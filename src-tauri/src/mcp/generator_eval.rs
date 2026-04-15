//! Generator Evaluation HTTP API handlers
//!
//! Provides endpoints for the Generator Evaluation page:
//! - Dashboard metrics and trends
//! - Pipeline artifact inspection
//! - Edit analysis from feedback data
//!
//! Benchmarks, Example Library, Training Data, and Insights tabs were removed
//! as vestigial from the SQLite-era implementation (see plans/generator-eval-design.md).

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use std::sync::Arc;

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

// ============================================================================
// Request Types
// ============================================================================

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

// ============================================================================
// Dashboard Endpoints
// ============================================================================

async fn dashboard_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let metrics = state
        .app_state
        .pg_db
        .get_generator_dashboard_metrics()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(&e))))?;
    Ok(Json(ApiResponse::success(metrics)))
}

async fn trends_handler(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<TrendsParams>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let rows = state
        .app_state
        .pg_db
        .get_generator_trends(params.days as i32)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(&e))))?;
    Ok(Json(ApiResponse::success(serde_json::Value::Array(rows))))
}

// ============================================================================
// Pipeline Inspector Endpoints
// ============================================================================

async fn list_artifacts_handler(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let rows = state
        .app_state
        .pg_db
        .list_generation_artifacts(params.limit as i64, params.offset as i64)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(&e))))?;
    Ok(Json(ApiResponse::success(serde_json::Value::Array(rows))))
}

async fn get_artifact_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state
        .app_state
        .pg_db
        .get_generation_artifact_by_id(&id)
        .await
    {
        Ok(Some(row)) => Ok(Json(ApiResponse::success(row))),
        Ok(None) => Err((StatusCode::NOT_FOUND, Json(api_error("Artifact not found")))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(&e)))),
    }
}

async fn get_artifact_by_workflow_handler(
    State(state): State<Arc<ApiState>>,
    Path(workflow_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state
        .app_state
        .pg_db
        .get_generation_artifact_by_workflow(&workflow_id)
        .await
    {
        Ok(Some(row)) => Ok(Json(ApiResponse::success(row))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error("No artifact found for this workflow")),
        )),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(&e)))),
    }
}

// ============================================================================
// Edit Analysis Endpoints
// ============================================================================

async fn edit_analysis_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let analysis = state
        .app_state
        .pg_db
        .get_edit_analysis()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(&e))))?;
    Ok(Json(ApiResponse::success(analysis)))
}

// ============================================================================
// Feedback Endpoint
// ============================================================================

async fn save_feedback_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<FeedbackRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let id = state
        .app_state
        .pg_db
        .record_generation_feedback(
            &body.workflow_id,
            None,
            &body.feedback_type,
            body.edited_field.as_deref(),
            body.old_value.as_deref(),
            body.new_value.as_deref(),
            None,
            body.rating,
            None,
            None,
            body.workflow_name.as_deref(),
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(&e))))?;
    Ok(Json(ApiResponse::success(serde_json::json!({ "id": id }))))
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
        .route(
            "/generator-eval/artifacts/by-workflow/{workflow_id}",
            get(get_artifact_by_workflow_handler),
        )
        .route("/generator-eval/artifacts/{id}", get(get_artifact_handler))
        // Edit Analysis + Feedback
        .route("/generator-eval/edit-analysis", get(edit_analysis_handler))
        .route("/generator-eval/feedback", post(save_feedback_handler))
}
