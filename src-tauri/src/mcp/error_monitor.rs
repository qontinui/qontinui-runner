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

    let conn = state.app_state.checkpoint_db.connection().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Database error: {}", e))),
        )
    })?;

    let errors = crate::error_monitor::ErrorEventStorage::get_unresolved(
        &conn,
        task_run_id.as_deref(),
        limit,
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get errors: {}", e))),
        )
    })?;

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

    let conn = state.app_state.checkpoint_db.connection().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Database error: {}", e))),
        )
    })?;

    let summary =
        crate::error_monitor::ErrorEventStorage::get_summary(&conn, task_run_id.as_deref())
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Failed to get summary: {}", e))),
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

    let conn = state.app_state.checkpoint_db.connection().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Database error: {}", e))),
        )
    })?;

    let curator = crate::error_monitor::DebugContextCurator::new();
    let context = curator
        .build_context(&conn, task_run_id.as_deref())
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to build debug context: {}", e))),
            )
        })?;

    let formatted = curator.format_for_ai(&context);
    Ok(Json(ApiResponse::success(formatted)))
}

/// Resolve an error (mark as fixed)
pub async fn resolve_error_monitor_error(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<i64>,
    Json(request): Json<ResolveErrorRequest>,
) -> Json<ApiResponse<()>> {
    match state.app_state.checkpoint_db.connection() {
        Ok(conn) => {
            let result = if let Some(ref task_run_id) = request.resolved_by_task_run_id {
                crate::error_monitor::ErrorEventStorage::mark_resolved_by_task(
                    &conn,
                    id,
                    task_run_id,
                    request.resolution_notes.as_deref(),
                )
            } else {
                crate::error_monitor::ErrorEventStorage::update_status(
                    &conn,
                    id,
                    crate::error_monitor::ErrorStatus::Resolved,
                    request.resolution_notes.as_deref(),
                )
            };
            match result {
                Ok(()) => Json(ApiResponse::success(())),
                Err(e) => Json(api_error(format!("Failed to resolve error: {}", e))),
            }
        }
        Err(e) => Json(api_error(format!("Database error: {}", e))),
    }
}

/// Acknowledge an error (mark as seen)
pub async fn acknowledge_error_monitor_error(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<i64>,
) -> Json<ApiResponse<()>> {
    match state.app_state.checkpoint_db.connection() {
        Ok(conn) => {
            let result = crate::error_monitor::ErrorEventStorage::update_status(
                &conn,
                id,
                crate::error_monitor::ErrorStatus::Acknowledged,
                None,
            );
            match result {
                Ok(()) => Json(ApiResponse::success(())),
                Err(e) => Json(api_error(format!("Failed to acknowledge error: {}", e))),
            }
        }
        Err(e) => Json(api_error(format!("Database error: {}", e))),
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
    let conn = state.app_state.checkpoint_db.connection().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Database error: {}", e))),
        )
    })?;

    let config = crate::error_monitor::ErrorFixWorkflowConfig {
        task_run_id: request.task_run_id,
        max_iterations: request.max_iterations.unwrap_or(10),
        ..Default::default()
    };
    let generator = crate::error_monitor::ErrorFixWorkflowGenerator::with_config(config);
    let workflow = generator.generate(&conn).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to generate workflow: {}", e))),
        )
    })?;

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
            "/error-monitor/errors/:id/resolve",
            post(resolve_error_monitor_error),
        )
        .route(
            "/error-monitor/errors/:id/acknowledge",
            post(acknowledge_error_monitor_error),
        )
        .route("/error-monitor/fix-workflow", post(generate_fix_workflow))
}
