//! Recording and playback handlers for MCP API
//!
//! Provides HTTP handlers for browser interaction recording,
//! action management, status control, and script export.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::recording::{
    AddActionInput, CreateRecordingInput, ExportFormat, ExportOptions, RecordedAction, Recording,
    RecordingStatus, RecordingStorage, ScriptGenerator,
};

// ============================================================================
// Recording & Playback HTTP API Handlers
// ============================================================================

/// List all recordings
pub async fn list_recordings_handler(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<Recording>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = RecordingStorage::new(state.app_state.checkpoint_db.clone());

    let status_filter = params
        .get("status")
        .and_then(|s| s.parse::<RecordingStatus>().ok());
    let limit = params.get("limit").and_then(|l| l.parse::<i32>().ok());

    match storage.list_recordings(status_filter, limit) {
        Ok(recordings) => Ok(Json(ApiResponse::success(recordings))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to list recordings: {}", e))),
        )),
    }
}

/// Create a new recording
pub async fn create_recording_handler(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<CreateRecordingInput>,
) -> Result<Json<ApiResponse<Recording>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = RecordingStorage::new(state.app_state.checkpoint_db.clone());

    match storage.create_recording(input) {
        Ok(recording) => {
            info!("Created recording: {} ({})", recording.name, recording.id);
            Ok(Json(ApiResponse::success(recording)))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to create recording: {}", e))),
        )),
    }
}

/// Get a specific recording
pub async fn get_recording_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Recording>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = RecordingStorage::new(state.app_state.checkpoint_db.clone());

    match storage.get_recording(&id) {
        Ok(Some(recording)) => Ok(Json(ApiResponse::success(recording))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Recording not found: {}", id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get recording: {}", e))),
        )),
    }
}

/// Delete a recording
pub async fn delete_recording_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<()>> {
    let storage = RecordingStorage::new(state.app_state.checkpoint_db.clone());

    match storage.delete_recording(&id) {
        Ok(()) => {
            info!("Deleted recording: {}", id);
            Json(ApiResponse::success(()))
        }
        Err(e) => Json(api_error(format!("Failed to delete recording: {}", e))),
    }
}

/// Get actions for a recording
pub async fn get_recording_actions_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Vec<RecordedAction>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = RecordingStorage::new(state.app_state.checkpoint_db.clone());

    match storage.get_recording_actions(&id) {
        Ok(actions) => Ok(Json(ApiResponse::success(actions))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get recording actions: {}", e))),
        )),
    }
}

/// Add an action to a recording
pub async fn add_recording_action_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(input): Json<AddActionInput>,
) -> Result<Json<ApiResponse<RecordedAction>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = RecordingStorage::new(state.app_state.checkpoint_db.clone());

    // Verify recording exists and is in recording status
    match storage.get_recording(&id) {
        Ok(Some(recording)) => {
            if recording.status != RecordingStatus::Recording {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error(format!(
                        "Cannot add actions to recording with status: {}",
                        recording.status
                    ))),
                ));
            }
        }
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Recording not found: {}", id))),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get recording: {}", e))),
            ));
        }
    }

    match storage.add_action(&id, input) {
        Ok(action) => {
            debug!("Added action to recording {}: {:?}", id, action.action_type);
            Ok(Json(ApiResponse::success(action)))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to add action: {}", e))),
        )),
    }
}

/// Update recording status
#[derive(Debug, Deserialize)]
pub struct UpdateRecordingStatusInput {
    status: RecordingStatus,
}

pub async fn update_recording_status_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(input): Json<UpdateRecordingStatusInput>,
) -> Result<Json<ApiResponse<Recording>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = RecordingStorage::new(state.app_state.checkpoint_db.clone());

    match storage.update_recording_status(&id, input.status) {
        Ok(recording) => {
            info!("Updated recording {} status to: {}", id, input.status);
            Ok(Json(ApiResponse::success(recording)))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "Failed to update recording status: {}",
                e
            ))),
        )),
    }
}

/// Export a recording to script
pub async fn export_recording_handler(
    State(state): State<Arc<ApiState>>,
    Path((id, format)): Path<(String, String)>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = RecordingStorage::new(state.app_state.checkpoint_db.clone());

    // Parse export format
    let export_format: ExportFormat = format.parse().map_err(|e: String| {
        (
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Invalid export format: {}", e))),
        )
    })?;

    // Get recording
    let recording = storage
        .get_recording(&id)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get recording: {}", e))),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Recording not found: {}", id))),
            )
        })?;

    // Get actions
    let actions = storage.get_recording_actions(&id).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get actions: {}", e))),
        )
    })?;

    // Build export options from query params
    let options = ExportOptions {
        wait_strategy: params
            .get("wait_strategy")
            .cloned()
            .unwrap_or_else(|| "networkidle".to_string()),
        fixed_wait_ms: params
            .get("fixed_wait_ms")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1000),
        selector_priority: params
            .get("selector_priority")
            .map(|s| s.split(',').map(|p| p.trim().to_string()).collect())
            .unwrap_or_else(|| vec!["ui_id".to_string(), "css".to_string(), "xpath".to_string()]),
        include_visibility_assertions: params
            .get("include_visibility_assertions")
            .map(|s| s == "true")
            .unwrap_or(false),
        include_timing_assertions: params
            .get("include_timing_assertions")
            .map(|s| s == "true")
            .unwrap_or(false),
        test_name: params.get("test_name").cloned(),
        test_description: params.get("test_description").cloned(),
    };

    // Generate script
    let script_content = ScriptGenerator::generate(&recording, &actions, export_format, &options)
        .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to generate script: {}", e))),
        )
    })?;

    let file_name = ScriptGenerator::default_file_name(&recording, export_format);

    // Save export record
    let export = storage
        .save_export(
            &id,
            export_format,
            &script_content,
            &file_name,
            Some(&options),
        )
        .map_err(|e| {
            warn!("Failed to save export record: {}", e);
            // Continue even if save fails
        })
        .ok();

    info!("Exported recording {} to {} format", id, export_format);

    Ok(Json(ApiResponse::success(serde_json::json!({
        "recording_id": id,
        "format": export_format.to_string(),
        "file_name": file_name,
        "script_content": script_content,
        "export_id": export.map(|e| e.id),
    }))))
}

/// Get exports for a recording
pub async fn get_recording_exports_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<
    Json<ApiResponse<Vec<crate::recording::RecordingExport>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let storage = RecordingStorage::new(state.app_state.checkpoint_db.clone());

    match storage.get_recording_exports(&id) {
        Ok(exports) => Ok(Json(ApiResponse::success(exports))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get recording exports: {}", e))),
        )),
    }
}

// ============================================================================
// End Recording & Playback HTTP API Handlers
// ============================================================================

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, put};
    axum::Router::new()
        .route(
            "/recordings",
            get(list_recordings_handler).post(create_recording_handler),
        )
        .route(
            "/recordings/:id",
            get(get_recording_handler).delete(delete_recording_handler),
        )
        .route(
            "/recordings/:id/actions",
            get(get_recording_actions_handler).post(add_recording_action_handler),
        )
        .route(
            "/recordings/:id/status",
            put(update_recording_status_handler),
        )
        .route(
            "/recordings/:id/export/:format",
            get(export_recording_handler),
        )
        .route(
            "/recordings/:id/exports",
            get(get_recording_exports_handler),
        )
}
