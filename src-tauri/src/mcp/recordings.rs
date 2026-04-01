//! Recording and playback handlers for MCP API
//!
//! Provides HTTP handlers for browser interaction recording,
//! action management, status control, and script export.
//!
//! NOTE: checkpoint_db removed — recordings not yet migrated to PG.
//! All handlers return empty/no-op results.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::recording::{
    AddActionInput, CreateRecordingInput, RecordedAction, Recording,
    RecordingStatus,
};

// ============================================================================
// Recording & Playback HTTP API Handlers
// ============================================================================

/// List all recordings
pub async fn list_recordings_handler(
    State(_state): State<Arc<ApiState>>,
    Query(_params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<Recording>>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — recordings not yet migrated to PG
    Ok(Json(ApiResponse::success(vec![])))
}

/// Create a new recording
pub async fn create_recording_handler(
    State(_state): State<Arc<ApiState>>,
    Json(_input): Json<CreateRecordingInput>,
) -> Result<Json<ApiResponse<Recording>>, (StatusCode, Json<ApiResponse<()>>)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(api_error("Recordings: SQLite removed, not yet migrated to PG".to_string())),
    ))
}

/// Get a specific recording
pub async fn get_recording_handler(
    State(_state): State<Arc<ApiState>>,
    Path(_id): Path<String>,
) -> Result<Json<ApiResponse<Recording>>, (StatusCode, Json<ApiResponse<()>>)> {
    Err((
        StatusCode::NOT_FOUND,
        Json(api_error("Recording not found (SQLite removed)".to_string())),
    ))
}

/// Delete a recording
pub async fn delete_recording_handler(
    State(_state): State<Arc<ApiState>>,
    Path(_id): Path<String>,
) -> Json<ApiResponse<()>> {
    // checkpoint_db removed — no-op
    Json(ApiResponse::success(()))
}

/// Get actions for a recording
pub async fn get_recording_actions_handler(
    State(_state): State<Arc<ApiState>>,
    Path(_id): Path<String>,
) -> Result<Json<ApiResponse<Vec<RecordedAction>>>, (StatusCode, Json<ApiResponse<()>>)> {
    Ok(Json(ApiResponse::success(vec![])))
}

/// Add an action to a recording
pub async fn add_recording_action_handler(
    State(_state): State<Arc<ApiState>>,
    Path(_id): Path<String>,
    Json(_input): Json<AddActionInput>,
) -> Result<Json<ApiResponse<RecordedAction>>, (StatusCode, Json<ApiResponse<()>>)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(api_error("Recordings: SQLite removed, not yet migrated to PG".to_string())),
    ))
}

/// Update recording status
#[derive(Debug, Deserialize)]
pub struct UpdateRecordingStatusInput {
    status: RecordingStatus,
}

pub async fn update_recording_status_handler(
    State(_state): State<Arc<ApiState>>,
    Path(_id): Path<String>,
    Json(_input): Json<UpdateRecordingStatusInput>,
) -> Result<Json<ApiResponse<Recording>>, (StatusCode, Json<ApiResponse<()>>)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(api_error("Recordings: SQLite removed, not yet migrated to PG".to_string())),
    ))
}

/// Export a recording to script
pub async fn export_recording_handler(
    State(_state): State<Arc<ApiState>>,
    Path((_id, _format)): Path<(String, String)>,
    Query(_params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(api_error("Recordings: SQLite removed, not yet migrated to PG".to_string())),
    ))
}

/// Get exports for a recording
pub async fn get_recording_exports_handler(
    State(_state): State<Arc<ApiState>>,
    Path(_id): Path<String>,
) -> Result<
    Json<ApiResponse<Vec<crate::recording::RecordingExport>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    Ok(Json(ApiResponse::success(vec![])))
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
