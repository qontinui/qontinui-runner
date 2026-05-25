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

/// Map a process-manager error string into a canonical envelope.
///
/// The manager's lifecycle methods return `Result<_, String>`. The Phase-B2
/// `ExternallyOwned` guards embed a `[CODE]` prefix (e.g.
/// `"[ACTION_NOT_SUPPORTED] Process ... "`) so this HTTP boundary can surface a
/// structured `code` field on the canonical envelope instead of burying the
/// code in free text. Errors without a recognised `[CODE]` prefix fall back to
/// a plain `api_error` under `default_status`.
fn manager_error_to_envelope(
    err: &str,
    default_status: StatusCode,
    fallback_prefix: &str,
) -> (StatusCode, Json<ApiResponse<()>>) {
    if let Some(rest) = err.strip_prefix('[') {
        if let Some((code, message)) = rest.split_once(']') {
            let code = code.trim();
            let message = message.trim();
            if !code.is_empty() {
                let status = match code {
                    "INVALID_STATE" | "ACTION_NOT_SUPPORTED" => StatusCode::BAD_REQUEST,
                    "EXTERNAL_PORT_OWNED" => StatusCode::CONFLICT,
                    _ => default_status,
                };
                return (
                    status,
                    Json(ApiResponse::error_with_code(
                        message.to_string(),
                        code.to_string(),
                    )),
                );
            }
        }
    }
    (
        default_status,
        Json(api_error(format!("{}: {}", fallback_prefix, err))),
    )
}

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
        manager_error_to_envelope(
            &e,
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to start process",
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
        manager_error_to_envelope(&e, StatusCode::BAD_REQUEST, "Failed to stop process")
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
            StatusCode::INTERNAL_SERVER_ERROR,
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

/// Get recent log lines for a process, split into stdout / stderr ring-buffer
/// tails. Default `lines=200`, clamped to `[1, 5000]`.
///
/// Response shape: `{ stdout: [..], stderr: [..], truncated: bool }`.
/// `truncated` is `true` when either side's live ring buffer held more
/// entries than the per-stream cap (i.e. older entries were dropped from the
/// returned slice).
pub async fn get_logs(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let lines_raw = query
        .get("lines")
        .and_then(|t| t.parse::<usize>().ok())
        .unwrap_or(200);
    let lines = lines_raw.clamp(1, 5000);

    if primary_proxy::is_secondary() {
        let logs = primary_proxy::get_logs(&id, lines).await.map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                Json(api_error(format!("Primary runner proxy error: {}", e))),
            )
        })?;
        return Ok(Json(ApiResponse::success(serde_json::json!({
            "stdout": logs.stdout,
            "stderr": logs.stderr,
            "truncated": logs.truncated,
        }))));
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
    let (stdout, stderr, truncated) = manager.get_logs(&resolved, lines).await.map_err(|e| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Failed to get logs: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "stdout": stdout,
        "stderr": stderr,
        "truncated": truncated,
    }))))
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
        .route("/processes/{id}/logs", get(get_logs))
}
