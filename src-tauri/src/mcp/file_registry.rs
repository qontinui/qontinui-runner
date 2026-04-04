//! File Registry MCP API
//!
//! HTTP endpoints for the advisory file registry. Sessions (workflows and
//! AI terminal sessions) use these to register files they're working on,
//! check for conflicts, and release registrations.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::executor::file_registry::{FileConflict, FileLockInfo, FileRegistryInfo};
use crate::mcp::types::ApiState;

// =============================================================================
// Request / Response Types
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct RegisterFilesRequest {
    /// Files being actively developed.
    pub file_paths: Vec<String>,
    /// Task run ID of the registering session.
    pub task_run_id: String,
    /// Human-readable session/workflow name.
    pub holder_name: String,
}

#[derive(Debug, Serialize)]
pub struct RegisterFilesResponse {
    /// Number of files registered.
    pub registered: usize,
    /// Files that are also being worked on by other sessions.
    pub conflicts: Vec<FileConflict>,
}

#[derive(Debug, Deserialize)]
pub struct UnregisterFilesRequest {
    /// Files to unregister.
    pub file_paths: Vec<String>,
    /// Task run ID of the session releasing the files.
    pub task_run_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ReleaseAllRequest {
    /// Task run ID of the session releasing all files.
    pub task_run_id: String,
}

#[derive(Debug, Deserialize)]
pub struct CheckConflictsRequest {
    /// Task run ID of the querying session (excluded from conflicts).
    pub task_run_id: String,
    /// Optional: only check these specific files. If empty, checks all.
    #[serde(default)]
    pub file_paths: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct CheckConflictsResponse {
    /// Files under active development by other sessions.
    pub conflicts: Vec<FileConflict>,
}

// =============================================================================
// Handlers
// =============================================================================

/// POST /file-registry/register
///
/// Register files as under active development. Returns any conflicts with
/// other sessions. Registration always succeeds (advisory, not exclusive).
async fn register_files(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<RegisterFilesRequest>,
) -> Result<Json<RegisterFilesResponse>, (StatusCode, String)> {
    if req.file_paths.is_empty() {
        return Ok(Json(RegisterFilesResponse {
            registered: 0,
            conflicts: vec![],
        }));
    }

    let conflicts = state
        .app_state
        .file_registry_manager
        .register(&req.file_paths, &req.task_run_id, &req.holder_name)
        .await;

    Ok(Json(RegisterFilesResponse {
        registered: req.file_paths.len(),
        conflicts,
    }))
}

/// POST /file-registry/unregister
///
/// Unregister specific files for a session.
async fn unregister_files(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<UnregisterFilesRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .app_state
        .file_registry_manager
        .unregister(&req.file_paths, &req.task_run_id)
        .await;

    Ok(Json(serde_json::json!({ "success": true })))
}

/// POST /file-registry/release-all
///
/// Release all file registrations for a session. Called when a session ends.
async fn release_all(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<ReleaseAllRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .app_state
        .file_registry_manager
        .release_all(&req.task_run_id)
        .await;

    Ok(Json(serde_json::json!({ "success": true })))
}

/// POST /file-registry/check-conflicts
///
/// Check which files are under active development by other sessions.
/// If `file_paths` is provided, only checks those files. Otherwise checks all.
async fn check_conflicts(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CheckConflictsRequest>,
) -> Result<Json<CheckConflictsResponse>, (StatusCode, String)> {
    let conflicts = if req.file_paths.is_empty() {
        state
            .app_state
            .file_registry_manager
            .check_conflicts(&req.task_run_id)
            .await
    } else {
        state
            .app_state
            .file_registry_manager
            .check_conflicts_for_files(&req.file_paths, &req.task_run_id)
            .await
    };

    Ok(Json(CheckConflictsResponse { conflicts }))
}

/// GET /file-registry/info
///
/// Get a snapshot of all current file registrations across all sessions.
async fn get_info(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Vec<FileRegistryInfo>>, (StatusCode, String)> {
    let info = state.app_state.file_registry_manager.info().await;
    Ok(Json(info))
}

/// GET /file-locks/info
///
/// Get a snapshot of all currently held exclusive file locks.
async fn get_lock_info(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Vec<FileLockInfo>>, (StatusCode, String)> {
    let info = state.app_state.file_lock_manager.info().await;
    Ok(Json(info))
}

// =============================================================================
// Routes
// =============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/file-registry/register", post(register_files))
        .route("/file-registry/unregister", post(unregister_files))
        .route("/file-registry/release-all", post(release_all))
        .route("/file-registry/check-conflicts", post(check_conflicts))
        .route("/file-registry/info", get(get_info))
        .route("/file-locks/info", get(get_lock_info))
}
