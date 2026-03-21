//! Checkpoint handlers for MCP API
//!
//! Provides HTTP handlers for checkpoint CRUD, status, and history.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Deserialize;
use std::sync::Arc;

use crate::database::{CheckpointData, SessionEvent};
use crate::mcp::types::ApiState;

/// List all active (non-completed) checkpoints.
pub async fn list_checkpoints(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Vec<CheckpointData>>, (StatusCode, String)> {
    state
        .app_state
        .checkpoint_db
        .list_active_checkpoints()
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Get a checkpoint by workflow name.
pub async fn get_checkpoint(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Json<Option<CheckpointData>>, (StatusCode, String)> {
    state
        .app_state
        .checkpoint_db
        .get_checkpoint(&name)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
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
    State(state): State<Arc<ApiState>>,
    Json(req): Json<SaveCheckpointRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let data = CheckpointData {
        session_id: None,
        workflow_name: Some(req.workflow_name),
        current_phase: req.current_phase,
        total_phases: req.total_phases,
        completed: req.completed,
        restart_permitted: req.restart_permitted,
        status: req.status,
        run_id: req.run_id,
        repos_to_process: req.repos_to_process,
        work_completed: req.work_completed,
        items_needing_user_input: req.items_needing_user_input,
        created_at: None,
        updated_at: None,
        error_message: req.error_message,
        extra: None,
    };

    state
        .app_state
        .checkpoint_db
        .save_checkpoint(&data)
        .map(|_| {
            Json(serde_json::json!({
                "success": true,
                "message": "Checkpoint saved"
            }))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Delete a checkpoint by workflow name.
pub async fn delete_checkpoint(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .app_state
        .checkpoint_db
        .delete_checkpoint(&name)
        .map(|deleted| {
            Json(serde_json::json!({
                "success": deleted,
                "message": if deleted { "Checkpoint deleted" } else { "Checkpoint not found" }
            }))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Query params for checkpoint status.
#[derive(Debug, Deserialize)]
pub struct CheckpointStatusQuery {
    completion_value: Option<u32>,
}

/// Check checkpoint status for cross-session continuation.
pub async fn get_checkpoint_status(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<CheckpointStatusQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let completion_value = query.completion_value.unwrap_or(12); // Default for improve-all

    state
        .app_state
        .checkpoint_db
        .check_checkpoint_status(&name, completion_value)
        .map(|result| {
            Json(match result {
                Some((is_complete, current_phase)) => serde_json::json!({
                    "found": true,
                    "is_complete": is_complete,
                    "current_phase": current_phase
                }),
                None => serde_json::json!({
                    "found": false,
                    "is_complete": false,
                    "current_phase": 0
                }),
            })
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Query params for checkpoint history.
#[derive(Debug, Deserialize)]
pub struct CheckpointHistoryQuery {
    workflow_name: Option<String>,
    limit: Option<u32>,
}

/// Get checkpoint/session history.
pub async fn get_checkpoint_history(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(query): axum::extract::Query<CheckpointHistoryQuery>,
) -> Result<Json<Vec<SessionEvent>>, (StatusCode, String)> {
    let limit = query.limit.unwrap_or(50);

    state
        .app_state
        .checkpoint_db
        .get_session_history(query.workflow_name.as_deref(), limit)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
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
