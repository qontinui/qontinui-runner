//! Process management and output HTTP API endpoints.
//!
//! Provides REST endpoints for managing spawned child processes:
//! listing, starting, stopping, restarting, and reading output.
//! Also exposes process status and output for AI to query during workflow execution.
//!
//! When running as a secondary instance, these endpoints proxy to the primary runner.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use std::sync::Arc;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::process_capture::primary_proxy;
use crate::process_capture::types::{OutputLine, ProcessStatus};

/// List all managed processes with their status.
pub async fn list_processes(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<ProcessStatus>>>, (StatusCode, Json<ApiResponse<()>>)> {
    if primary_proxy::is_secondary() {
        let statuses = primary_proxy::get_all_status().await.map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                Json(api_error(format!("Primary runner proxy error: {}", e))),
            )
        })?;
        return Ok(Json(ApiResponse::success(statuses)));
    }

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
    if primary_proxy::is_secondary() {
        let statuses = primary_proxy::get_all_status().await.map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                Json(api_error(format!("Primary runner proxy error: {}", e))),
            )
        })?;
        return Ok(Json(ApiResponse::success(statuses)));
    }

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
    if primary_proxy::is_secondary() {
        primary_proxy::start_process(&id).await.map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                Json(api_error(format!("Primary runner proxy error: {}", e))),
            )
        })?;
        return Ok(Json(ApiResponse::success("started".to_string())));
    }

    let manager = state.app_state.process_capture_manager.lock().await;
    let manager = manager.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error("Process capture manager not initialized")),
        )
    })?;

    let resolved = manager
        .resolve_process_id(&id)
        .await
        .unwrap_or_else(|| id.clone());
    manager.start_process(&resolved).await.map_err(|e| {
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
    if primary_proxy::is_secondary() {
        primary_proxy::stop_process(&id).await.map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                Json(api_error(format!("Primary runner proxy error: {}", e))),
            )
        })?;
        return Ok(Json(ApiResponse::success("stopped".to_string())));
    }

    let manager = state.app_state.process_capture_manager.lock().await;
    let manager = manager.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error("Process capture manager not initialized")),
        )
    })?;

    let resolved = manager
        .resolve_process_id(&id)
        .await
        .unwrap_or_else(|| id.clone());
    manager.stop_process(&resolved).await.map_err(|e| {
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
    if primary_proxy::is_secondary() {
        primary_proxy::restart_process(&id).await.map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                Json(api_error(format!("Primary runner proxy error: {}", e))),
            )
        })?;
        return Ok(Json(ApiResponse::success("restarted".to_string())));
    }

    let manager = state.app_state.process_capture_manager.lock().await;
    let manager = manager.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error("Process capture manager not initialized")),
        )
    })?;

    let resolved = manager
        .resolve_process_id(&id)
        .await
        .unwrap_or_else(|| id.clone());
    manager.restart_process(&resolved).await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Failed to restart process: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success("restarted".to_string())))
}

/// Rebuild and restart a process (run build command, then start).
pub async fn rebuild_and_restart_process(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    if primary_proxy::is_secondary() {
        primary_proxy::rebuild_and_restart_process(&id)
            .await
            .map_err(|e| {
                (
                    StatusCode::BAD_GATEWAY,
                    Json(api_error(format!("Primary runner proxy error: {}", e))),
                )
            })?;
        return Ok(Json(ApiResponse::success(
            "rebuild-and-restarted".to_string(),
        )));
    }

    let manager = state.app_state.process_capture_manager.lock().await;
    let manager = manager.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error("Process capture manager not initialized")),
        )
    })?;

    let resolved = manager
        .resolve_process_id(&id)
        .await
        .unwrap_or_else(|| id.clone());
    manager
        .rebuild_and_restart_process(&resolved)
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(api_error(format!(
                    "Failed to rebuild and restart process: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(
        "rebuild-and-restarted".to_string(),
    )))
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

    if primary_proxy::is_secondary() {
        let output = primary_proxy::get_output(&id, tail).await.map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                Json(api_error(format!("Primary runner proxy error: {}", e))),
            )
        })?;
        return Ok(Json(ApiResponse::success(output)));
    }

    let manager = state.app_state.process_capture_manager.lock().await;
    let manager = manager.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error("Process capture manager not initialized")),
        )
    })?;

    let resolved = manager
        .resolve_process_id(&id)
        .await
        .unwrap_or_else(|| id.clone());
    let output = manager.get_output(&resolved, tail).await.map_err(|e| {
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
        .route(
            "/processes/{id}/rebuild-and-restart",
            post(rebuild_and_restart_process),
        )
        .route("/processes/{id}/output", get(get_output))
}
