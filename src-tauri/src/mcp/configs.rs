//! Config storage handlers for MCP API
//!
//! Provides HTTP handlers for configuration CRUD, import/export,
//! and config parsing.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::config::ConfigLoader;
use crate::config_storage::{ConfigMetadata, StoredConfig};
use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Config Storage HTTP API Handlers
// ============================================================================

/// List all stored configurations (metadata only)
pub async fn list_configs(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<ConfigMetadata>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = state.config_storage.lock().await;
    let configs = storage.list();
    Ok(Json(ApiResponse::success(configs)))
}

/// Request to parse a config file
#[derive(Debug, Deserialize)]
pub struct ParseConfigRequest {
    /// Path to the config file to parse
    path: String,
}

/// State info for the parse config response
#[derive(Debug, Serialize)]
pub struct ConfigStateInfo {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    is_initial: bool,
    is_final: bool,
}

/// Transition info for the parse config response
#[derive(Debug, Serialize)]
pub struct ConfigTransitionInfo {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    from_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    to_state: Option<String>,
}

/// Response for parsing a config file
#[derive(Debug, Serialize)]
pub struct ParseConfigResponse {
    states: Vec<ConfigStateInfo>,
    transitions: Vec<ConfigTransitionInfo>,
}

/// Parse a configuration file and return states/transitions without importing
pub async fn parse_config_file(
    Json(request): Json<ParseConfigRequest>,
) -> Result<Json<ApiResponse<ParseConfigResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    match ConfigLoader::load_from_file(&request.path) {
        Ok(config) => {
            let states: Vec<ConfigStateInfo> = config
                .states
                .iter()
                .filter_map(|s| {
                    let id = s.get("id")?.as_str()?.to_string();
                    let name = s.get("name")?.as_str()?.to_string();
                    let description = s
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let is_initial = s
                        .get("isInitial")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let is_final = s.get("isFinal").and_then(|v| v.as_bool()).unwrap_or(false);
                    Some(ConfigStateInfo {
                        id,
                        name,
                        description,
                        is_initial,
                        is_final,
                    })
                })
                .collect();

            let transitions: Vec<ConfigTransitionInfo> = config
                .transitions
                .iter()
                .filter_map(|t| {
                    let id = t.get("id")?.as_str()?.to_string();
                    let from_state = t
                        .get("fromState")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let to_state = t
                        .get("toState")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let name = t
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            // Generate name from from_state -> to_state if no name
                            match (&from_state, &to_state) {
                                (Some(from), Some(to)) => format!("{} → {}", from, to),
                                (Some(from), None) => format!("{} → ?", from),
                                (None, Some(to)) => format!("? → {}", to),
                                (None, None) => id.clone(),
                            }
                        });
                    Some(ConfigTransitionInfo {
                        id,
                        name,
                        from_state,
                        to_state,
                    })
                })
                .collect();

            Ok(Json(ApiResponse::success(ParseConfigResponse {
                states,
                transitions,
            })))
        }
        Err(e) => Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Failed to parse config: {}", e))),
        )),
    }
}

/// Request body for importing a config
#[derive(Debug, Deserialize)]
pub struct ImportConfigRequest {
    /// Path to the config file to import
    path: String,
    /// Name to give the imported config
    name: String,
}

/// Response for import config
#[derive(Debug, Serialize)]
pub struct ImportConfigResponse {
    id: String,
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

/// Request body for updating config metadata
#[derive(Debug, Deserialize)]
pub struct UpdateConfigRequest {
    /// New name (optional)
    name: Option<String>,
    /// New description (optional)
    description: Option<String>,
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

/// Request body for exporting a config
#[derive(Debug, Deserialize)]
pub struct ExportConfigRequest {
    /// Path to export the config to
    path: String,
}

/// Export a configuration to a file
pub async fn export_config(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<ExportConfigRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = state.config_storage.lock().await;
    match storage.export_to_file(&id, &request.path) {
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

// ============================================================================
// End Config Storage HTTP API Handlers
// ============================================================================

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/configs", get(list_configs).post(import_config))
        .route("/configs/parse", post(parse_config_file))
        .route(
            "/configs/{id}",
            get(get_stored_config)
                .put(update_stored_config)
                .delete(delete_stored_config),
        )
        .route("/configs/{id}/export", post(export_config))
}
