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

    if let Some(pg) = &state.app_state.pg_db {
        let pg_errors = pg
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

        return Ok(Json(ApiResponse::success(errors)));
    }

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

    if let Some(pg) = &state.app_state.pg_db {
        let pg_summary = pg
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

        return Ok(Json(ApiResponse::success(summary)));
    }

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

    // Try PG first — returns a simplified list of unresolved errors as debug context.
    // Falls back to SQLite curator which builds a richer DebugContext from multiple queries.
    if let Some(pg) = &state.app_state.pg_db {
        match pg.get_error_debug_context(task_run_id.as_deref()).await {
            Ok(ctx) => {
                let formatted = serde_json::to_string_pretty(&ctx).unwrap_or_default();
                return Ok(Json(ApiResponse::success(formatted)));
            }
            Err(e) => {
                tracing::warn!("PG debug context failed, falling back to SQLite: {}", e);
            }
        }
    }

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
    if let Some(pg) = &state.app_state.pg_db {
        let result = if let Some(ref task_run_id) = request.resolved_by_task_run_id {
            pg.mark_resolved_by_task(id, task_run_id, request.resolution_notes.as_deref())
                .await
        } else {
            pg.update_error_status(id, "resolved", request.resolution_notes.as_deref())
                .await
        };
        return match result {
            Ok(()) => Json(ApiResponse::success(())),
            Err(e) => Json(api_error(format!("Failed to resolve error: {}", e))),
        };
    }

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
    if let Some(pg) = &state.app_state.pg_db {
        return match pg.update_error_status(id, "acknowledged", None).await {
            Ok(()) => Json(ApiResponse::success(())),
            Err(e) => Json(api_error(format!("Failed to acknowledge error: {}", e))),
        };
    }

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
    // NOTE: Workflow generation uses the curator and generator which are deeply SQLite-integrated.
    // No PG equivalent exists for the full error-fix pipeline. Falls through to SQLite always.
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
            "/error-monitor/errors/{id}/resolve",
            post(resolve_error_monitor_error),
        )
        .route(
            "/error-monitor/errors/{id}/acknowledge",
            post(acknowledge_error_monitor_error),
        )
        .route("/error-monitor/fix-workflow", post(generate_fix_workflow))
}
