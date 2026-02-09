//! Global Log Source HTTP API handlers
//!
//! Provides HTTP endpoints for managing global log sources, profiles,
//! and AI selection mode. Wraps the existing settings functions.

use axum::{
    http::StatusCode,
    response::Json,
    routing::{get, post, put},
    Router,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::settings::{self, GlobalLogSourceSettings, LogSourceAiSelectionMode};

// ============================================================================
// Request types
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct SetAiModeRequest {
    pub mode: String,
}

#[derive(Debug, Deserialize)]
pub struct SetDefaultProfileRequest {
    pub profile_id: Option<String>,
}

// ============================================================================
// Handlers
// ============================================================================

/// GET /log-sources/settings - Get all global log source settings
async fn get_settings(
) -> Result<Json<ApiResponse<GlobalLogSourceSettings>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(settings::get_global_log_source_settings)
        .await
        .map_err(|e| {
            error!("Failed to get log source settings: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get settings: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(result)))
}

/// PUT /log-sources/settings - Save all global log source settings
async fn save_settings(
    Json(request): Json<GlobalLogSourceSettings>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result =
        tokio::task::spawn_blocking(move || settings::save_global_log_source_settings(request))
            .await
            .map_err(|e| {
                error!("Failed to save log source settings: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Save task failed: {}", e))),
                )
            })?;

    match result {
        Ok(()) => {
            info!("Global log source settings saved via HTTP");
            Ok(Json(ApiResponse::success(())))
        }
        Err(e) => {
            error!("Failed to save settings: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to save settings: {}", e))),
            ))
        }
    }
}

/// PUT /log-sources/ai-mode - Set AI selection mode
async fn set_ai_mode(
    Json(request): Json<SetAiModeRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mode = request.mode.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut current = settings::get_global_log_source_settings();
        current.ai_selection_mode = match mode.as_str() {
            "dynamic" => LogSourceAiSelectionMode::Dynamic,
            "static" => LogSourceAiSelectionMode::Static,
            "disabled" => LogSourceAiSelectionMode::Disabled,
            _ => return Err(format!("Invalid mode: {}", mode)),
        };
        settings::save_global_log_source_settings(current)
    })
    .await
    .map_err(|e| {
        error!("Failed to set AI mode: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::BAD_REQUEST, Json(api_error(e)))),
    }
}

/// PUT /log-sources/default-profile - Set default profile
async fn set_default_profile(
    Json(request): Json<SetDefaultProfileRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(move || {
        let mut current = settings::get_global_log_source_settings();
        current.default_profile_id = request.profile_id;
        settings::save_global_log_source_settings(current)
    })
    .await
    .map_err(|e| {
        error!("Failed to set default profile: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// POST /log-sources/migrate - Migrate project sources to global
async fn migrate_from_projects(
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(|| {
        crate::commands::global_log_sources::migrate_project_sources_to_global_impl()
    })
    .await
    .map_err(|e| {
        error!("Migration task failed: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Migration task failed: {}", e))),
        )
    })?;

    match result {
        Ok(count) => Ok(Json(ApiResponse::success(serde_json::json!({
            "migrated": count,
        })))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route(
            "/log-sources/settings",
            get(get_settings).put(save_settings),
        )
        .route("/log-sources/ai-mode", put(set_ai_mode))
        .route("/log-sources/default-profile", put(set_default_profile))
        .route("/log-sources/migrate", post(migrate_from_projects))
}
