//! Process management and output HTTP API endpoints.
//!
//! Provides REST endpoints for managing spawned child processes:
//! listing, starting, stopping, restarting, and reading output.
//! Also exposes process status and output for AI to query during workflow execution.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use std::sync::Arc;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::process_capture::types::{OutputLine, ProcessStatus};

/// List all managed processes with their status.
pub async fn list_processes(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<ProcessStatus>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let manager = state.app_state.process_capture_manager.lock().await;
    let manager = manager.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error("Process capture manager not initialized")),
        )
    })?;

    let statuses = manager.get_all_status().await;
    Ok(Json(ApiResponse::success(statuses)))
}

/// Get status of all managed processes (alias for list_processes).
/// Available at GET /processes/status for AI workflow queries.
pub async fn get_process_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<ProcessStatus>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let manager = state.app_state.process_capture_manager.lock().await;
    let manager = manager.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error("Process capture manager not initialized")),
        )
    })?;

    let statuses = manager.get_all_status().await;
    Ok(Json(ApiResponse::success(statuses)))
}

/// Start a process by ID.
pub async fn start_process(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    let manager = state.app_state.process_capture_manager.lock().await;
    let manager = manager.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error("Process capture manager not initialized")),
        )
    })?;

    manager.start_process(&id).await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Failed to start process: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success("started".to_string())))
}

/// Stop a process by ID.
pub async fn stop_process(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    let manager = state.app_state.process_capture_manager.lock().await;
    let manager = manager.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error("Process capture manager not initialized")),
        )
    })?;

    manager.stop_process(&id).await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Failed to stop process: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success("stopped".to_string())))
}

/// Restart a process by ID.
pub async fn restart_process(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    let manager = state.app_state.process_capture_manager.lock().await;
    let manager = manager.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error("Process capture manager not initialized")),
        )
    })?;

    manager.restart_process(&id).await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Failed to restart process: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success("restarted".to_string())))
}

/// Get recent output from a process.
pub async fn get_output(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<OutputLine>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let tail = query
        .get("tail")
        .and_then(|t| t.parse::<usize>().ok())
        .unwrap_or(100);

    let manager = state.app_state.process_capture_manager.lock().await;
    let manager = manager.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error("Process capture manager not initialized")),
        )
    })?;

    let output = manager.get_output(&id, tail).await.map_err(|e| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Failed to get output: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(output)))
}

/// Build the axum routes for process management.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/processes", get(list_processes))
        .route("/processes/status", get(get_process_status))
        .route("/processes/{id}/start", post(start_process))
        .route("/processes/{id}/stop", post(stop_process))
        .route("/processes/{id}/restart", post(restart_process))
        .route("/processes/{id}/output", get(get_output))
}
