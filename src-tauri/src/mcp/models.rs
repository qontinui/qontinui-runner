//! Model management handlers for MCP API
//!
//! Provides HTTP handlers for ML model management:
//! list, download, delete, status, and disk usage.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info};

use crate::executor::with_default_bridge;
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::timeout_config::Timeouts;

// ============================================================================
// Types
// ============================================================================

/// Request to download a model
#[derive(Debug, Deserialize)]
pub struct ModelDownloadRequest {
    /// Model identifier (e.g., "sam3", "clip_vit_b32")
    pub model_id: String,
    /// Force re-download even if already available
    #[serde(default)]
    pub force: bool,
}

/// Request to delete a model
#[derive(Debug, Deserialize)]
pub struct ModelDeleteRequest {
    /// Model identifier
    pub model_id: String,
}

/// Request to get model status
#[derive(Debug, Deserialize)]
pub struct ModelStatusRequest {
    /// Model identifier
    pub model_id: String,
}

// ============================================================================
// Handlers
// ============================================================================

/// List all available models with their download status
pub async fn list_models(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: List models");

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("models_list", None, timeout_duration)
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
        Ok(response) => {
            if response.success {
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true,
                        "models": []
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "List models failed".to_string());
                error!("MCP API: List models failed: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to list models: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Download a model (returns when download completes)
pub async fn download_model(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ModelDownloadRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Download model {} (force: {})",
        request.model_id, request.force
    );

    let app_state = state.app_state.clone();
    let params = serde_json::json!({
        "model_id": request.model_id,
        "force": request.force,
    });

    // Model downloads can take a long time, use 10 minute timeout
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Use configurable timeout for python commands (default: disabled)
            // Falls back to 1 hour to prevent infinite IPC hangs
            let timeout_duration =
                Timeouts::python_command().unwrap_or_else(|| std::time::Duration::from_secs(3600));
            bridge.send_command_and_wait("models_download", Some(params), timeout_duration)
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
        Ok(response) => {
            if response.success {
                info!("MCP API: Model download completed");
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Model download failed".to_string());
                error!("MCP API: Model download failed: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to download model: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Delete a downloaded model
pub async fn delete_model(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ModelDeleteRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Delete model {}", request.model_id);

    let app_state = state.app_state.clone();
    let params = serde_json::json!({
        "model_id": request.model_id,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("models_delete", Some(params), timeout_duration)
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
        Ok(response) => {
            if response.success {
                info!("MCP API: Model deleted");
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Model delete failed".to_string());
                error!("MCP API: Model delete failed: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to delete model: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get status of a specific model
pub async fn get_model_status(
    State(state): State<Arc<ApiState>>,
    Path(model_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Get model status for {}", model_id);

    let app_state = state.app_state.clone();
    let params = serde_json::json!({
        "model_id": model_id,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("models_status", Some(params), timeout_duration)
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
        Ok(response) => {
            if response.success {
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true,
                        "available": false
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Get model status failed".to_string());
                error!("MCP API: Get model status failed: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to get model status: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get disk usage for all downloaded models
pub async fn get_models_disk_usage(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Get models disk usage");

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("models_disk_usage", None, timeout_duration)
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
        Ok(response) => {
            if response.success {
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true,
                        "total_bytes": 0,
                        "models": {}
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Get disk usage failed".to_string());
                error!("MCP API: Get disk usage failed: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to get disk usage: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/models", get(list_models))
        .route("/models/download", post(download_model))
        .route("/models/delete", post(delete_model))
        .route("/models/disk-usage", get(get_models_disk_usage))
        .route("/models/{model_id}", get(get_model_status))
}
