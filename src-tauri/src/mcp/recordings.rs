//! Recording and playback handlers for MCP API
//!
//! Provides HTTP handlers for browser interaction recording,
//! action management, status control, and script export.
//!
//! Storage is PostgreSQL via `PgDb` methods in `database/pg/recordings.rs`.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use chrono::Utc;
use serde::Deserialize;
use std::sync::Arc;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::recording::{
    script_generator::ScriptGenerator, AddActionInput, CreateRecordingInput, ExportFormat,
    ExportOptions, RecordedAction, Recording, RecordingExport, RecordingStatus,
};

type HandlerError = (StatusCode, Json<ApiResponse<()>>);

fn internal_err(msg: impl Into<String>) -> HandlerError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(api_error(msg.into())),
    )
}

fn not_found_err(msg: impl Into<String>) -> HandlerError {
    (StatusCode::NOT_FOUND, Json(api_error(msg.into())))
}

fn bad_request_err(msg: impl Into<String>) -> HandlerError {
    (StatusCode::BAD_REQUEST, Json(api_error(msg.into())))
}

// ============================================================================
// Recording & Playback HTTP API Handlers
// ============================================================================

/// List all recordings
pub async fn list_recordings_handler(
    State(state): State<Arc<ApiState>>,
    Query(_params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<Recording>>>, HandlerError> {
    let recordings = state
        .app_state
        .pg_db
        .list_recordings()
        .await
        .map_err(|e| internal_err(format!("Failed to list recordings: {}", e)))?;
    Ok(Json(ApiResponse::success(recordings)))
}

/// Create a new recording
pub async fn create_recording_handler(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<CreateRecordingInput>,
) -> Result<Json<ApiResponse<Recording>>, HandlerError> {
    let browser_info_json = input
        .browser_info
        .as_ref()
        .map(|bi| serde_json::to_string(bi).unwrap_or_else(|_| "null".to_string()));

    let recording = state
        .app_state
        .pg_db
        .create_recording(
            &input.name,
            input.description.as_deref(),
            &input.base_url,
            browser_info_json.as_deref(),
            input.tab_id,
            &input.tags,
        )
        .await
        .map_err(|e| internal_err(format!("Failed to create recording: {}", e)))?;

    Ok(Json(ApiResponse::success(recording)))
}

/// Get a specific recording
pub async fn get_recording_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Recording>>, HandlerError> {
    match state.app_state.pg_db.get_recording(&id).await {
        Ok(Some(r)) => Ok(Json(ApiResponse::success(r))),
        Ok(None) => Err(not_found_err(format!("Recording not found: {}", id))),
        Err(e) => Err(internal_err(format!("Failed to get recording: {}", e))),
    }
}

/// Delete a recording
pub async fn delete_recording_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<()>> {
    match state.app_state.pg_db.delete_recording(&id).await {
        Ok(_) => Json(ApiResponse::success(())),
        Err(e) => Json(ApiResponse::error(format!(
            "Failed to delete recording: {}",
            e
        ))),
    }
}

/// Get actions for a recording
pub async fn get_recording_actions_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Vec<RecordedAction>>>, HandlerError> {
    let actions = state
        .app_state
        .pg_db
        .get_recorded_actions(&id)
        .await
        .map_err(|e| internal_err(format!("Failed to get recording actions: {}", e)))?;
    Ok(Json(ApiResponse::success(actions)))
}

/// Add an action to a recording
pub async fn add_recording_action_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(input): Json<AddActionInput>,
) -> Result<Json<ApiResponse<RecordedAction>>, HandlerError> {
    // Round-trip Option<serde_json::Value> into the typed ActionData enum.
    let action_data = match input.action_data {
        Some(v) if !v.is_null() => Some(
            serde_json::from_value(v)
                .map_err(|e| bad_request_err(format!("Invalid action_data: {}", e)))?,
        ),
        _ => None,
    };

    let now = Utc::now().to_rfc3339();
    let staged = RecordedAction {
        id: String::new(), // filled by add_recorded_action
        recording_id: id.clone(),
        sequence_number: 0, // assigned inside add_recorded_action
        action_type: input.action_type,
        url: input.url,
        page_title: input.page_title,
        target: input.target,
        action_data,
        screenshot_path: None,
        timestamp: input.timestamp,
        duration_ms: input.duration_ms,
        created_at: now,
    };

    let new_id = state
        .app_state
        .pg_db
        .add_recorded_action(&id, &staged)
        .await
        .map_err(|e| internal_err(format!("Failed to add recording action: {}", e)))?;

    // Re-fetch so the response mirrors what's persisted (including the
    // assigned sequence_number and created_at).
    let actions = state
        .app_state
        .pg_db
        .get_recorded_actions(&id)
        .await
        .map_err(|e| internal_err(format!("Failed to reload recording actions: {}", e)))?;
    let created = actions
        .into_iter()
        .find(|a| a.id == new_id)
        .ok_or_else(|| internal_err("Recorded action not found after insert"))?;

    Ok(Json(ApiResponse::success(created)))
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
) -> Result<Json<ApiResponse<Recording>>, HandlerError> {
    let status_str = input.status.to_string();
    let updated = state
        .app_state
        .pg_db
        .update_recording_status(&id, &status_str)
        .await
        .map_err(|e| internal_err(format!("Failed to update recording status: {}", e)))?;
    if !updated {
        return Err(not_found_err(format!("Recording not found: {}", id)));
    }

    let recording = state
        .app_state
        .pg_db
        .get_recording(&id)
        .await
        .map_err(|e| internal_err(format!("Failed to reload recording: {}", e)))?
        .ok_or_else(|| not_found_err(format!("Recording not found: {}", id)))?;
    Ok(Json(ApiResponse::success(recording)))
}

/// Export a recording to script
pub async fn export_recording_handler(
    State(state): State<Arc<ApiState>>,
    Path((id, format)): Path<(String, String)>,
    Query(_params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, HandlerError> {
    let export_format: ExportFormat = format
        .parse()
        .map_err(|e: String| bad_request_err(format!("Invalid export format: {}", e)))?;

    let recording = state
        .app_state
        .pg_db
        .get_recording(&id)
        .await
        .map_err(|e| internal_err(format!("Failed to get recording: {}", e)))?
        .ok_or_else(|| not_found_err(format!("Recording not found: {}", id)))?;

    let actions = state
        .app_state
        .pg_db
        .get_recorded_actions(&id)
        .await
        .map_err(|e| internal_err(format!("Failed to get recording actions: {}", e)))?;

    let options = ExportOptions::default();
    let script_content = ScriptGenerator::generate(&recording, &actions, export_format, &options)
        .map_err(|e| internal_err(format!("Script generation failed: {}", e)))?;
    let file_name = ScriptGenerator::default_file_name(&recording, export_format);

    let export = state
        .app_state
        .pg_db
        .create_recording_export(
            &id,
            &export_format.to_string(),
            &file_name,
            &script_content,
            Some(&options),
        )
        .await
        .map_err(|e| internal_err(format!("Failed to persist export: {}", e)))?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "export": export,
        "script_content": script_content,
        "file_name": file_name,
    }))))
}

/// Get exports for a recording
pub async fn get_recording_exports_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Vec<RecordingExport>>>, HandlerError> {
    let exports = state
        .app_state
        .pg_db
        .get_recording_exports(&id)
        .await
        .map_err(|e| internal_err(format!("Failed to get recording exports: {}", e)))?;
    Ok(Json(ApiResponse::success(exports)))
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
            "/recordings/{id}",
            get(get_recording_handler).delete(delete_recording_handler),
        )
        .route(
            "/recordings/{id}/actions",
            get(get_recording_actions_handler).post(add_recording_action_handler),
        )
        .route(
            "/recordings/{id}/status",
            put(update_recording_status_handler),
        )
        .route(
            "/recordings/{id}/export/{format}",
            get(export_recording_handler),
        )
        .route(
            "/recordings/{id}/exports",
            get(get_recording_exports_handler),
        )
}
