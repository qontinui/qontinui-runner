//! Error monitor handlers for MCP API
//!
//! Provides HTTP handlers for application log error detection:
//! get errors, summary, debug context, resolve/acknowledge errors,
//! and generate fix workflows.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Types
// ============================================================================

/// Request body for resolving an error
#[derive(Debug, Deserialize)]
pub struct ResolveErrorRequest {
    resolution_notes: Option<String>,
    resolved_by_task_run_id: Option<String>,
}

/// Request body for generating fix workflow
#[derive(Debug, Deserialize)]
pub struct GenerateFixWorkflowRequest {
    task_run_id: Option<String>,
    max_iterations: Option<u32>,
}

// ============================================================================
// Handlers
// ============================================================================

/// Get errors from the error monitor
pub async fn get_error_monitor_errors(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<
    Json<ApiResponse<Vec<crate::error_monitor::StoredErrorEvent>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let task_run_id = query.get("task_run_id").cloned();
    let limit = query
        .get("limit")
        .and_then(|l| l.parse::<usize>().ok())
        .unwrap_or(100);

    let pg_errors = state.app_state.pg_db
        .get_unresolved_errors(task_run_id.as_deref(), limit)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get errors: {}", e))),
            )
        })?;

    // Deserialize JSON values into typed StoredErrorEvent structs
    let errors: Vec<crate::error_monitor::StoredErrorEvent> = pg_errors
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();

    Ok(Json(ApiResponse::success(errors)))
}

/// Get error summary from the error monitor
pub async fn get_error_monitor_summary(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<
    Json<ApiResponse<crate::error_monitor::ErrorSummary>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let task_run_id = query.get("task_run_id").cloned();

    let pg_summary = state.app_state.pg_db
        .get_error_summary(task_run_id.as_deref())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get summary: {}", e))),
            )
        })?;

    let summary: crate::error_monitor::ErrorSummary =
        serde_json::from_value(pg_summary).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to deserialize PG summary: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(summary)))
}

/// Get curated debug context for AI
pub async fn get_error_debug_context(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    let task_run_id = query.get("task_run_id").cloned();

    let ctx = state.app_state.pg_db
        .get_error_debug_context(task_run_id.as_deref())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get debug context: {}", e))),
            )
        })?;

    let formatted = serde_json::to_string_pretty(&ctx).unwrap_or_default();
    Ok(Json(ApiResponse::success(formatted)))
}

/// Resolve an error (mark as fixed)
pub async fn resolve_error_monitor_error(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<i64>,
    Json(request): Json<ResolveErrorRequest>,
) -> Json<ApiResponse<()>> {
    let result = if let Some(ref task_run_id) = request.resolved_by_task_run_id {
        state.app_state.pg_db.mark_resolved_by_task(id, task_run_id, request.resolution_notes.as_deref())
            .await
    } else {
        state.app_state.pg_db.update_error_status(id, "resolved", request.resolution_notes.as_deref())
            .await
    };
    match result {
        Ok(()) => Json(ApiResponse::success(())),
        Err(e) => Json(api_error(format!("Failed to resolve error: {}", e))),
    }
}

/// Acknowledge an error (mark as seen)
pub async fn acknowledge_error_monitor_error(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<i64>,
) -> Json<ApiResponse<()>> {
    match state.app_state.pg_db.update_error_status(id, "acknowledged", None).await {
        Ok(()) => Json(ApiResponse::success(())),
        Err(e) => Json(api_error(format!("Failed to acknowledge error: {}", e))),
    }
}

/// Generate a workflow to fix detected errors
pub async fn generate_fix_workflow(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<GenerateFixWorkflowRequest>,
) -> Result<
    Json<ApiResponse<crate::error_monitor::GeneratedWorkflow>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let task_run_id = request.task_run_id.unwrap_or_default();
    let max_iterations = request.max_iterations.unwrap_or(10);

    let result = state.app_state.pg_db
        .generate_error_fix_workflow(&task_run_id, max_iterations)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    // Convert to the expected type via serde
    let workflow: crate::error_monitor::GeneratedWorkflow = serde_json::from_value(result)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(format!("Deserialization: {}", e)))))?;
    Ok(Json(ApiResponse::success(workflow)))
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/error-monitor/errors", get(get_error_monitor_errors))
        .route("/error-monitor/summary", get(get_error_monitor_summary))
        .route("/error-monitor/debug-context", get(get_error_debug_context))
        .route(
            "/error-monitor/errors/{id}/resolve",
            post(resolve_error_monitor_error),
        )
        .route(
            "/error-monitor/errors/{id}/acknowledge",
            post(acknowledge_error_monitor_error),
        )
        .route("/error-monitor/fix-workflow", post(generate_fix_workflow))
}
