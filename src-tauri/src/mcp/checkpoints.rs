//! Checkpoint handlers for MCP API
//!
//! Provides HTTP handlers for checkpoint CRUD, status, and history.
//! NOTE: SQLite checkpoint_db removed — these endpoints return empty/no-op results
//! until migrated to PG.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Deserialize;
use std::sync::Arc;

use crate::database::{CheckpointData, SessionEvent};
use crate::mcp::types::ApiState;

/// List all active (non-completed) checkpoints.
pub async fn list_checkpoints(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<Vec<CheckpointData>>, (StatusCode, String)> {
    // checkpoint_db removed — not yet migrated to PG
    Ok(Json(vec![]))
}

/// Get a checkpoint by workflow name.
pub async fn get_checkpoint(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(_name): axum::extract::Path<String>,
) -> Result<Json<Option<CheckpointData>>, (StatusCode, String)> {
    // checkpoint_db removed — not yet migrated to PG
    Ok(Json(None))
}

/// Request body for saving a checkpoint.
#[derive(Debug, Deserialize)]
pub struct SaveCheckpointRequest {
    workflow_name: String,
    current_phase: u32,
    #[serde(default)]
    total_phases: Option<u32>,
    #[serde(default)]
    completed: bool,
    #[serde(default)]
    restart_permitted: bool,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    repos_to_process: Option<Vec<String>>,
    #[serde(default)]
    work_completed: Option<serde_json::Value>,
    #[serde(default)]
    items_needing_user_input: Option<Vec<String>>,
    #[serde(default)]
    error_message: Option<String>,
}

/// Save or update a checkpoint.
pub async fn save_checkpoint(
    State(_state): State<Arc<ApiState>>,
    Json(_req): Json<SaveCheckpointRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // checkpoint_db removed — not yet migrated to PG
    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Checkpoint saved (no-op: SQLite removed)"
    })))
}

/// Delete a checkpoint by workflow name.
pub async fn delete_checkpoint(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(_name): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // checkpoint_db removed — not yet migrated to PG
    Ok(Json(serde_json::json!({
        "success": false,
        "message": "Checkpoint not found (SQLite removed)"
    })))
}

/// Query params for checkpoint status.
#[derive(Debug, Deserialize)]
pub struct CheckpointStatusQuery {
    completion_value: Option<u32>,
}

/// Check checkpoint status for cross-session continuation.
pub async fn get_checkpoint_status(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(_name): axum::extract::Path<String>,
    axum::extract::Query(_query): axum::extract::Query<CheckpointStatusQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // checkpoint_db removed — not yet migrated to PG
    Ok(Json(serde_json::json!({
        "found": false,
        "is_complete": false,
        "current_phase": 0
    })))
}

/// Query params for checkpoint history.
#[derive(Debug, Deserialize)]
pub struct CheckpointHistoryQuery {
    workflow_name: Option<String>,
    limit: Option<u32>,
}

/// Get checkpoint/session history.
pub async fn get_checkpoint_history(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(_query): axum::extract::Query<CheckpointHistoryQuery>,
) -> Result<Json<Vec<SessionEvent>>, (StatusCode, String)> {
    // checkpoint_db removed — not yet migrated to PG
    Ok(Json(vec![]))
}

// ============================================================================
// End Checkpoint HTTP API Handlers
// ============================================================================

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::get;
    axum::Router::new()
        .route("/checkpoints", get(list_checkpoints).post(save_checkpoint))
        .route(
            "/checkpoints/{name}",
            get(get_checkpoint).delete(delete_checkpoint),
        )
        .route("/checkpoints/{name}/status", get(get_checkpoint_status))
        .route("/checkpoints/history", get(get_checkpoint_history))
}
