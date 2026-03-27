//! Settings HTTP API handlers
//!
//! Provides HTTP endpoints for reading and writing all settings categories.
//! Wraps the existing settings functions from `crate::settings` and
//! keychain operations from `crate::config_facade`.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::Emitter;
use tracing::{error, info};

use crate::commands::backup::{ComprehensiveExport, ComprehensiveImportResult};
use crate::config_facade;
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::settings;

// ============================================================================
// Request / Response types
// ============================================================================

/// General (top-level) settings that don't belong to a specific sub-struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralSettingsPayload {
    pub auto_load_last_config: bool,
    pub include_summary_step_by_default: bool,
    pub preflight_check_enabled: bool,
    pub session_auto_fix_on_failure: bool,
}

/// Agentic settings payload (compression, retry, routing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticSettingsPayload {
    pub compression: crate::orchestrator::CompressionConfig,
    pub retry: crate::orchestrator::RetryConfig,
    pub routing: crate::ai_router::RoutingConfig,
}

/// Request to save an API key for a given provider.
#[derive(Debug, Deserialize)]
pub struct SaveApiKeyRequest {
    pub provider: String,
    pub api_key: String,
}

/// Request for storage cleanup.
#[derive(Debug, Deserialize)]
pub struct StorageCleanupRequest {
    pub storage_type: String,
    pub older_than_days: u32,
}

/// App mode payload (simple / advanced).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppModePayload {
    pub mode: String,
}

/// Device info response.
#[derive(Debug, Serialize)]
pub struct DeviceInfoResponse {
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub platform: String,
}

// ============================================================================
// General Settings
// ============================================================================

/// GET /settings/general
async fn get_general_settings(
) -> Result<Json<ApiResponse<GeneralSettingsPayload>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(|| {
        let s = settings::load_settings();
        GeneralSettingsPayload {
            auto_load_last_config: s.auto_load_last_config,
            include_summary_step_by_default: s.include_summary_step_by_default,
            preflight_check_enabled: s.preflight_check_enabled,
            session_auto_fix_on_failure: s.session_auto_fix_on_failure,
        }
    })
    .await
    .map_err(|e| {
        error!("Failed to get general settings: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(result)))
}

/// PUT /settings/general
async fn save_general_settings(
    Json(payload): Json<GeneralSettingsPayload>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(move || {
        let mut s = settings::load_settings();
        s.auto_load_last_config = payload.auto_load_last_config;
        s.include_summary_step_by_default = payload.include_summary_step_by_default;
        s.preflight_check_enabled = payload.preflight_check_enabled;
        s.session_auto_fix_on_failure = payload.session_auto_fix_on_failure;
        settings::save_settings(&s)
    })
    .await
    .map_err(|e| {
        error!("Failed to save general settings: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(()) => {
            info!("General settings saved via HTTP");
            Ok(Json(ApiResponse::success(())))
        }
        Err(e) => {
            error!("Failed to save general settings: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to save settings: {}", e))),
            ))
        }
    }
}

// ============================================================================
// App Mode
// ============================================================================

/// GET /settings/app-mode
async fn get_app_mode(
) -> Result<Json<ApiResponse<AppModePayload>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(|| {
        let s = settings::load_settings();
        AppModePayload { mode: s.app_mode }
    })
    .await
    .map_err(|e| {
        error!("Failed to get app mode: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(result)))
}

/// PUT /settings/app-mode
async fn save_app_mode(
    State(api_state): State<Arc<ApiState>>,
    Json(payload): Json<AppModePayload>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mode = payload.mode.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut s = settings::load_settings();
        s.app_mode = payload.mode;
        settings::save_settings(&s)
    })
    .await
    .map_err(|e| {
        error!("Failed to save app mode: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(()) => {
            info!("App mode saved via HTTP: {}", mode);
            // Emit Tauri event so runner frontend updates immediately
            let _ = api_state.app_handle.emit("app-mode-changed", &mode);
            Ok(Json(ApiResponse::success(())))
        }
        Err(e) => {
            error!("Failed to save app mode: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to save app mode: {}", e))),
            ))
        }
    }
}

// ============================================================================
// AI Settings
// ============================================================================

/// GET /settings/ai
async fn get_ai_settings(
) -> Result<Json<ApiResponse<settings::AiSettings>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(settings::get_ai_settings)
        .await
        .map_err(|e| {
            error!("Failed to get AI settings: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Task failed: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(result)))
}

/// PUT /settings/ai
async fn save_ai_settings(
    Json(payload): Json<settings::AiSettings>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(move || settings::save_ai_settings(payload))
        .await
        .map_err(|e| {
            error!("Failed to save AI settings: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Task failed: {}", e))),
            )
        })?;

    match result {
        Ok(()) => {
            info!("AI settings saved via HTTP");
            Ok(Json(ApiResponse::success(())))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// POST /settings/ai/test-connection - stub
async fn test_ai_connection(
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Testing AI connection requires spawning a CLI process; not yet implemented via HTTP.
    Ok(Json(ApiResponse::success(serde_json::json!({
        "success": false,
        "message": "AI connection testing is available in the desktop app"
    }))))
}

/// POST /settings/ai/api-key
async fn save_ai_api_key(
    Json(req): Json<SaveApiKeyRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(move || {
        config_facade::ai_keychain()
            .store(&req.provider, &req.api_key)
            .map_err(|e| format!("Failed to store API key: {}", e))
    })
    .await
    .map_err(|e| {
        error!("Failed to save AI API key: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(()) => {
            info!("AI API key saved via HTTP");
            Ok(Json(ApiResponse::success(())))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// DELETE /settings/ai/api-key/:provider
async fn delete_ai_api_key(
    Path(provider): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(move || {
        config_facade::ai_keychain()
            .delete(&provider)
            .map_err(|e| format!("Failed to delete API key: {}", e))
    })
    .await
    .map_err(|e| {
        error!("Failed to delete AI API key: {}", e);
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

/// GET /settings/ai/has-key/:provider
async fn has_ai_api_key(
    Path(provider): Path<String>,
) -> Result<Json<ApiResponse<bool>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(move || {
        config_facade::ai_keychain()
            .exists(&provider)
            .map_err(|e| format!("Failed to check API key: {}", e))
    })
    .await
    .map_err(|e| {
        error!("Failed to check AI API key: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(exists) => Ok(Json(ApiResponse::success(exists))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

// ============================================================================
// Agentic Settings
// ============================================================================

/// GET /settings/agentic
async fn get_agentic_settings(
) -> Result<Json<ApiResponse<AgenticSettingsPayload>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(|| {
        let ai = settings::get_ai_settings();
        AgenticSettingsPayload {
            compression: ai.compression,
            retry: ai.retry,
            routing: ai.routing,
        }
    })
    .await
    .map_err(|e| {
        error!("Failed to get agentic settings: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(result)))
}

/// PUT /settings/agentic
async fn save_agentic_settings(
    Json(payload): Json<AgenticSettingsPayload>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(move || {
        let mut ai = settings::get_ai_settings();
        ai.compression = payload.compression;
        ai.retry = payload.retry;
        ai.routing = payload.routing;
        settings::save_ai_settings(ai)
    })
    .await
    .map_err(|e| {
        error!("Failed to save agentic settings: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(()) => {
            info!("Agentic settings saved via HTTP");
            Ok(Json(ApiResponse::success(())))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

// ============================================================================
// Debug Settings
// ============================================================================

/// GET /settings/debug
async fn get_debug_settings(
) -> Result<Json<ApiResponse<settings::DebugSettings>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(settings::get_debug_settings)
        .await
        .map_err(|e| {
            error!("Failed to get debug settings: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Task failed: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(result)))
}

/// PUT /settings/debug
async fn save_debug_settings(
    Json(payload): Json<settings::DebugSettings>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(move || settings::save_debug_settings(payload))
        .await
        .map_err(|e| {
            error!("Failed to save debug settings: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Task failed: {}", e))),
            )
        })?;

    match result {
        Ok(()) => {
            info!("Debug settings saved via HTTP");
            Ok(Json(ApiResponse::success(())))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

// ============================================================================
// Playwright Settings
// ============================================================================

/// GET /settings/playwright
async fn get_playwright_settings(
) -> Result<Json<ApiResponse<settings::PlaywrightSettings>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(settings::get_playwright_settings)
        .await
        .map_err(|e| {
            error!("Failed to get Playwright settings: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Task failed: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(result)))
}

/// PUT /settings/playwright
async fn save_playwright_settings(
    Json(payload): Json<settings::PlaywrightSettings>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(move || settings::save_playwright_settings(payload))
        .await
        .map_err(|e| {
            error!("Failed to save Playwright settings: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Task failed: {}", e))),
            )
        })?;

    match result {
        Ok(()) => {
            info!("Playwright settings saved via HTTP");
            Ok(Json(ApiResponse::success(())))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// GET /settings/playwright/has-password
async fn has_playwright_password(
) -> Result<Json<ApiResponse<bool>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(|| {
        config_facade::playwright_keychain()
            .exists(config_facade::keychain_keys::PLAYWRIGHT_PASSWORD)
            .map_err(|e| format!("Failed to check password: {}", e))
    })
    .await
    .map_err(|e| {
        error!("Failed to check Playwright password: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(exists) => Ok(Json(ApiResponse::success(exists))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// DELETE /settings/playwright/password
async fn delete_playwright_password(
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(|| {
        config_facade::playwright_keychain()
            .delete(config_facade::keychain_keys::PLAYWRIGHT_PASSWORD)
            .map_err(|e| format!("Failed to delete password: {}", e))
    })
    .await
    .map_err(|e| {
        error!("Failed to delete Playwright password: {}", e);
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

// ============================================================================
// Self-Healing Settings
// ============================================================================

/// GET /settings/self-healing
async fn get_self_healing_settings(
) -> Result<Json<ApiResponse<settings::SelfHealingSettings>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(settings::get_self_healing_settings)
        .await
        .map_err(|e| {
            error!("Failed to get self-healing settings: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Task failed: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(result)))
}

/// PUT /settings/self-healing
async fn save_self_healing_settings(
    Json(payload): Json<settings::SelfHealingSettings>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(move || settings::save_self_healing_settings(payload))
        .await
        .map_err(|e| {
            error!("Failed to save self-healing settings: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Task failed: {}", e))),
            )
        })?;

    match result {
        Ok(()) => {
            info!("Self-healing settings saved via HTTP");
            Ok(Json(ApiResponse::success(())))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// POST /settings/self-healing/api-key
async fn save_self_healing_api_key(
    Json(req): Json<SaveApiKeyRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(move || {
        config_facade::self_healing_keychain()
            .store(&req.provider, &req.api_key)
            .map_err(|e| format!("Failed to store API key: {}", e))
    })
    .await
    .map_err(|e| {
        error!("Failed to save self-healing API key: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(()) => {
            info!("Self-healing API key saved via HTTP");
            Ok(Json(ApiResponse::success(())))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// DELETE /settings/self-healing/api-key/:provider
async fn delete_self_healing_api_key(
    Path(provider): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(move || {
        config_facade::self_healing_keychain()
            .delete(&provider)
            .map_err(|e| format!("Failed to delete API key: {}", e))
    })
    .await
    .map_err(|e| {
        error!("Failed to delete self-healing API key: {}", e);
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

/// GET /settings/self-healing/has-key/:provider
async fn has_self_healing_api_key(
    Path(provider): Path<String>,
) -> Result<Json<ApiResponse<bool>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(move || {
        config_facade::self_healing_keychain()
            .exists(&provider)
            .map_err(|e| format!("Failed to check API key: {}", e))
    })
    .await
    .map_err(|e| {
        error!("Failed to check self-healing API key: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(exists) => Ok(Json(ApiResponse::success(exists))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

// ============================================================================
// Mobile Settings
// ============================================================================

/// GET /settings/mobile
async fn get_mobile_settings(
) -> Result<Json<ApiResponse<settings::MobileSettings>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(settings::get_mobile_settings)
        .await
        .map_err(|e| {
            error!("Failed to get mobile settings: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Task failed: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(result)))
}

/// PUT /settings/mobile
async fn save_mobile_settings(
    Json(payload): Json<settings::MobileSettings>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(move || settings::save_mobile_settings(payload))
        .await
        .map_err(|e| {
            error!("Failed to save mobile settings: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Task failed: {}", e))),
            )
        })?;

    match result {
        Ok(()) => {
            info!("Mobile settings saved via HTTP");
            Ok(Json(ApiResponse::success(())))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

// ============================================================================
// Storage
// ============================================================================

/// GET /settings/storage
async fn get_storage_info(
    State(api_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = api_state.app_state.clone();
    let result = tokio::task::spawn_blocking(move || {
        let storage =
            crate::safe_lock::safe_lock_or_recover(&app_state.local_storage, "local_storage");
        let usage = storage
            .get_storage_usage()
            .map_err(|e| format!("Failed to get storage usage: {}", e))?;
        let config = &storage.config;
        Ok::<_, String>(serde_json::json!({
            "screenshot_path": config.screenshot_path.to_string_lossy().to_string(),
            "video_path": config.video_path.to_string_lossy().to_string(),
            "max_screenshot_mb": config.max_screenshot_mb,
            "max_video_mb": config.max_video_mb,
            "auto_cleanup": config.auto_cleanup,
            "usage": serde_json::to_value(&usage).unwrap_or_default(),
        }))
    })
    .await
    .map_err(|e| {
        error!("Failed to get storage info: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// POST /settings/storage/cleanup
async fn cleanup_storage(
    State(api_state): State<Arc<ApiState>>,
    Json(req): Json<StorageCleanupRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = api_state.app_state.clone();
    let result = tokio::task::spawn_blocking(move || {
        let storage =
            crate::safe_lock::safe_lock_or_recover(&app_state.local_storage, "local_storage");
        storage
            .delete_old_sessions(&req.storage_type, req.older_than_days)
            .map_err(|e| format!("Failed to cleanup storage: {}", e))
    })
    .await
    .map_err(|e| {
        error!("Failed to cleanup storage: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(()) => {
            info!("Storage cleanup completed via HTTP");
            Ok(Json(ApiResponse::success(())))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// POST /settings/storage/clear-all
async fn clear_all_storage(
    State(api_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = api_state.app_state.clone();
    let result = tokio::task::spawn_blocking(move || {
        let storage =
            crate::safe_lock::safe_lock_or_recover(&app_state.local_storage, "local_storage");
        storage
            .clear_all_storage()
            .map_err(|e| format!("Failed to clear all storage: {}", e))
    })
    .await
    .map_err(|e| {
        error!("Failed to clear all storage: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(()) => {
            info!("All storage cleared via HTTP");
            Ok(Json(ApiResponse::success(())))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

// ============================================================================
// Device Info
// ============================================================================

/// GET /settings/device-info
async fn get_device_info(
) -> Result<Json<ApiResponse<DeviceInfoResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = tokio::task::spawn_blocking(|| {
        let device_id = crate::runtime_env::get_runner_id();
        let device_name = hostname::get().ok().and_then(|h| h.into_string().ok());
        let platform = std::env::consts::OS.to_string();
        DeviceInfoResponse {
            device_id,
            device_name,
            platform,
        }
    })
    .await
    .map_err(|e| {
        error!("Failed to get device info: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(result)))
}

// ============================================================================
// Backup / Export / Import
// ============================================================================

/// GET /settings/backup/summary
async fn get_backup_summary(
    State(api_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = api_state.app_state.pg_db.get_export_summary().await;

    match result {
        Ok(summary) => Ok(Json(ApiResponse::success(summary))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// POST /settings/backup/export
async fn export_all_data(
    State(api_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<ComprehensiveExport>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = api_state.app_state.checkpoint_db.clone();
    let result = tokio::task::spawn_blocking(move || {
        let summary = db.get_export_summary()?;

        let flows = db.export_all_flows()?;
        let flow_executions = db.export_all_flow_executions()?;
        let checkpoints = db.export_all_orchestrator_checkpoints()?;
        let learning_outcomes = db.get_learning_outcomes(None)?;
        let learning_patterns = db.get_learning_patterns()?;
        let settings_data = db.export_all_settings()?;
        let prompts = db.export_all_prompts()?;
        let unified_workflows = db.export_all_unified_workflows()?;
        let verification_tests = db.export_all_verification_tests()?;
        let task_hooks = db.export_all_task_hooks()?;
        let scheduled_tasks = db.export_all_scheduled_tasks()?;
        let saved_api_requests = db.export_all_saved_api_requests()?;
        let configs = db.export_all_configs()?;

        Ok::<_, String>(ComprehensiveExport {
            manifest: crate::commands::backup::ExportManifest {
                version: "2.0.0".to_string(),
                created_at: chrono::Utc::now().to_rfc3339(),
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            summary,
            flows,
            flow_executions,
            checkpoints,
            learning_outcomes,
            learning_patterns,
            settings: settings_data,
            prompts,
            unified_workflows,
            verification_tests,
            task_hooks,
            scheduled_tasks,
            saved_api_requests,
            configs,
        })
    })
    .await
    .map_err(|e| {
        error!("Failed to export data: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(export) => {
            info!("Data exported via HTTP");
            Ok(Json(ApiResponse::success(export)))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// POST /settings/backup/import
async fn import_all_data(
    State(api_state): State<Arc<ApiState>>,
    Json(data): Json<ComprehensiveExport>,
) -> Result<Json<ApiResponse<ComprehensiveImportResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = api_state.app_state.checkpoint_db.clone();
    let result = tokio::task::spawn_blocking(move || {
        let conflict_mode = "skip";

        let mut results: std::collections::HashMap<String, crate::database::ImportResult> =
            std::collections::HashMap::new();
        let mut total_imported = 0;
        let mut total_skipped = 0;
        let mut total_errors = 0;

        // Helper macro-like closure to reduce repetition
        let mut do_import =
            |name: &str,
             data_vec: &[serde_json::Value],
             import_fn: &dyn Fn(
                &[serde_json::Value],
                &str,
            ) -> Result<crate::database::ImportResult, String>| {
                if !data_vec.is_empty() {
                    match import_fn(data_vec, conflict_mode) {
                        Ok(r) => {
                            total_imported += r.imported;
                            total_skipped += r.skipped;
                            total_errors += r.errors.len();
                            results.insert(name.to_string(), r);
                        }
                        Err(e) => {
                            results.insert(
                                name.to_string(),
                                crate::database::ImportResult {
                                    imported: 0,
                                    skipped: 0,
                                    errors: vec![e],
                                },
                            );
                        }
                    }
                }
            };

        do_import("flows", &data.flows, &|d, m| db.import_flows(d, m));
        do_import("prompts", &data.prompts, &|d, m| db.import_prompts(d, m));
        do_import("settings", &data.settings, &|d, m| db.import_settings(d, m));
        do_import("unified_workflows", &data.unified_workflows, &|d, m| {
            db.import_unified_workflows(d, m)
        });
        do_import("learning_outcomes", &data.learning_outcomes, &|d, m| {
            db.import_learning_outcomes(d, m)
        });
        do_import("learning_patterns", &data.learning_patterns, &|d, m| {
            db.import_learning_patterns(d, m)
        });

        let success = total_errors == 0;
        Ok::<_, String>(ComprehensiveImportResult {
            success,
            results,
            total_imported,
            total_skipped,
            total_errors,
        })
    })
    .await
    .map_err(|e| {
        error!("Failed to import data: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(import_result) => {
            info!(
                "Data imported via HTTP: {} imported, {} skipped, {} errors",
                import_result.total_imported,
                import_result.total_skipped,
                import_result.total_errors
            );
            Ok(Json(ApiResponse::success(import_result)))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        // General settings
        .route(
            "/settings/general",
            get(get_general_settings).put(save_general_settings),
        )
        // App mode
        .route("/settings/app-mode", get(get_app_mode).put(save_app_mode))
        // AI settings
        .route("/settings/ai", get(get_ai_settings).put(save_ai_settings))
        .route("/settings/ai/test-connection", post(test_ai_connection))
        .route("/settings/ai/api-key", post(save_ai_api_key))
        .route("/settings/ai/api-key/{provider}", delete(delete_ai_api_key))
        .route("/settings/ai/has-key/{provider}", get(has_ai_api_key))
        // Agentic settings
        .route(
            "/settings/agentic",
            get(get_agentic_settings).put(save_agentic_settings),
        )
        // Debug settings
        .route(
            "/settings/debug",
            get(get_debug_settings).put(save_debug_settings),
        )
        // Playwright settings
        .route(
            "/settings/playwright",
            get(get_playwright_settings).put(save_playwright_settings),
        )
        .route(
            "/settings/playwright/has-password",
            get(has_playwright_password),
        )
        .route(
            "/settings/playwright/password",
            delete(delete_playwright_password),
        )
        // Self-healing settings
        .route(
            "/settings/self-healing",
            get(get_self_healing_settings).put(save_self_healing_settings),
        )
        .route(
            "/settings/self-healing/api-key",
            post(save_self_healing_api_key),
        )
        .route(
            "/settings/self-healing/api-key/{provider}",
            delete(delete_self_healing_api_key),
        )
        .route(
            "/settings/self-healing/has-key/{provider}",
            get(has_self_healing_api_key),
        )
        // Mobile settings
        .route(
            "/settings/mobile",
            get(get_mobile_settings).put(save_mobile_settings),
        )
        // Storage
        .route("/settings/storage", get(get_storage_info))
        .route("/settings/storage/cleanup", post(cleanup_storage))
        .route("/settings/storage/clear-all", post(clear_all_storage))
        // Device info
        .route("/settings/device-info", get(get_device_info))
        // Backup
        .route("/settings/backup/summary", get(get_backup_summary))
        .route("/settings/backup/export", post(export_all_data))
        .route("/settings/backup/import", post(import_all_data))
}
