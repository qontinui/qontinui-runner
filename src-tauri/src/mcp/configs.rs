//! Configuration storage handlers for MCP API
//!
//! Provides handlers for managing stored configurations.

use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info};

use super::types::{api_error, ApiResponse, ApiState};
use crate::config_storage::{ConfigMetadata, StoredConfig};

/// Request body for importing a config
#[derive(Debug, Deserialize)]
pub struct ImportConfigRequest {
    /// Path to the config file to import
    pub path: String,
    /// Name to give the imported config
    pub name: String,
}

/// Response for import config
#[derive(Debug, Serialize)]
pub struct ImportConfigResponse {
    pub id: String,
}

/// Request body for updating config metadata
#[derive(Debug, Deserialize)]
pub struct UpdateConfigRequest {
    /// New name (optional)
    pub name: Option<String>,
    /// New description (optional)
    pub description: Option<String>,
}

/// Request body for exporting a config
#[derive(Debug, Deserialize)]
pub struct ExportConfigRequest {
    /// Path to export the config to
    pub path: String,
}

/// List all stored configurations
pub async fn list_configs(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<ConfigMetadata>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = state.config_storage.lock().await;
    let configs = storage.list();
    Ok(Json(ApiResponse::success(configs)))
}

/// Import a configuration from a file
pub async fn import_config(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ImportConfigRequest>,
) -> Result<Json<ApiResponse<ImportConfigResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mut storage = state.config_storage.lock().await;
    match storage.import_from_file(&request.path, &request.name) {
        Ok(id) => Ok(Json(ApiResponse::success(ImportConfigResponse { id }))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )),
    }
}

/// Get a stored configuration by ID
pub async fn get_stored_config(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<StoredConfig>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = state.config_storage.lock().await;
    match storage.get(&id) {
        Ok(config) => Ok(Json(ApiResponse::success(config))),
        Err(crate::config_storage::ConfigStorageError::NotFound(_)) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Config not found: {}", id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )),
    }
}

/// Update config metadata
pub async fn update_stored_config(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<UpdateConfigRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mut storage = state.config_storage.lock().await;
    match storage.update_metadata(&id, request.name, request.description) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(crate::config_storage::ConfigStorageError::NotFound(_)) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Config not found: {}", id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )),
    }
}

/// Delete a stored configuration
pub async fn delete_stored_config(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mut storage = state.config_storage.lock().await;
    match storage.delete(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(crate::config_storage::ConfigStorageError::NotFound(_)) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Config not found: {}", id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )),
    }
}

/// Export a configuration to a file
pub async fn export_config(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<ExportConfigRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = state.config_storage.lock().await;
    match storage.export_to_file(&id, &request.path) {
        Ok(()) => {
            info!("Config {} exported to {}", id, request.path);
            Ok(Json(ApiResponse::success(())))
        }
        Err(crate::config_storage::ConfigStorageError::NotFound(_)) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Config not found: {}", id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )),
    }
}
