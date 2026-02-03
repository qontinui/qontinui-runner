//! Workflow execution handlers for MCP API
//!
//! Provides handlers for workflow execution, status, and configuration.

use axum::{extract::State, http::StatusCode, Json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

use super::types::{
    api_error, ApiResponse, ApiState, ExecutionResult, LoadConfigRequest, RunWorkflowRequest,
    StatusResponse,
};
use crate::commands::AppState;
use crate::executor::with_default_bridge;
use crate::settings;

/// Health check endpoint
pub async fn health() -> Json<ApiResponse<String>> {
    Json(ApiResponse::success("ok".to_string()))
}

/// Get executor status
pub async fn get_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<StatusResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        let (executor_running, executor_state) = with_default_bridge(&app_state, |bridge| {
            (bridge.is_running(), bridge.get_state().name().to_string())
        })
        .unwrap_or((false, "not_started".to_string()));

        let config_lock = app_state.current_config.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: current_config mutex was poisoned, recovering");
            poisoned.into_inner()
        });
        let (config_loaded, config_path) = if config_lock.is_some() {
            (true, settings::get_last_config_path())
        } else {
            (false, None)
        };

        let ai_analysis_running =
            crate::mcp_api::has_running_ai_tasks(&app_state.checkpoint_db);

        StatusResponse {
            executor_running,
            executor_state,
            config_loaded,
            config_path,
            ai_analysis_running,
        }
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(result)))
}

/// Load a workflow configuration file
pub async fn load_config(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<LoadConfigRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Loading config from {}", request.config_path);

    let app_state = state.app_state.clone();
    let config_path = request.config_path.clone();

    let result = tokio::task::spawn_blocking(move || {
        load_config_internal(&app_state, &config_path)
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(config_info) => {
            info!("MCP API: Config loaded successfully");
            Ok(Json(ApiResponse::success(config_info)))
        }
        Err(e) => {
            error!("MCP API: Failed to load config: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Load the last used configuration
pub async fn load_last_config(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let config_path = match settings::get_last_config_path() {
        Some(path) => path,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error("No last config path saved")),
            ));
        }
    };

    info!("MCP API: Loading last config from {}", config_path);

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        load_config_internal(&app_state, &config_path)
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(config_info) => {
            info!("MCP API: Last config loaded successfully");
            Ok(Json(ApiResponse::success(config_info)))
        }
        Err(e) => {
            error!("MCP API: Failed to load last config: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Internal function to load config (called from spawn_blocking)
pub fn load_config_internal(
    app_state: &Arc<AppState>,
    config_path: &str,
) -> Result<serde_json::Value, String> {
    // Delegate to mcp_api for now - this will be fully migrated later
    crate::mcp_api::load_config_internal(app_state, config_path)
}

/// Run a workflow by name
pub async fn run_workflow(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RunWorkflowRequest>,
) -> Result<Json<ApiResponse<ExecutionResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Running workflow '{}' (timeout: {}s, monitor: {:?})",
        request.workflow_name, request.timeout_seconds, request.monitor_index
    );

    let app_state = state.app_state.clone();
    let workflow_name = request.workflow_name.clone();
    let monitor_index = request.monitor_index;
    let timeout_duration = Duration::from_secs(request.timeout_seconds);

    let result = tokio::task::spawn_blocking(move || {
        crate::mcp_api::run_workflow_internal(
            &app_state,
            &workflow_name,
            monitor_index,
            timeout_duration,
        )
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(exec_result) => {
            if exec_result.success {
                info!("MCP API: Workflow '{}' completed successfully", workflow_name);
            } else {
                warn!(
                    "MCP API: Workflow '{}' failed: {:?}",
                    workflow_name, exec_result.error
                );
            }
            Ok(Json(ApiResponse::success(exec_result)))
        }
        Err(e) => {
            error!("MCP API: Failed to run workflow: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Stop the current workflow execution
pub async fn stop_execution(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stopping execution");

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            bridge
                .send_command("stop", None)
                .map_err(|e| format!("Failed to send stop command: {}", e))?;

            Ok(serde_json::json!({
                "message": "Stop command sent"
            }))
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => Ok(Json(ApiResponse::success(response))),
        Err(e) => {
            error!("MCP API: Failed to stop execution: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get workspace paths (internal helper)
pub fn get_workspace_paths_internal() -> Result<(PathBuf, PathBuf, PathBuf), String> {
    crate::mcp_api::get_workspace_paths_internal()
}
