//! Database/persistence handlers for MCP API
//!
//! Provides handlers for checkpoint and task run management.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use std::sync::Arc;
use tracing::{error, info};

use super::types::{
    api_error, ApiResponse, ApiState, CheckpointHistoryQuery, CheckpointStatusQuery,
    CreateTaskRunRequest, ListTaskRunsQuery, SaveCheckpointRequest, SetTaskAutoContinueRequest,
    TaskOutputQuery,
};
use crate::database::{CheckpointData, CreateTaskRunInput, SessionEvent, TaskRun};

/// List all active checkpoints
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

/// Get a checkpoint by name
pub async fn get_checkpoint(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Result<Json<Option<CheckpointData>>, (StatusCode, String)> {
    state
        .app_state
        .checkpoint_db
        .get_checkpoint(&name)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Save a checkpoint
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

/// Delete a checkpoint
pub async fn delete_checkpoint(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
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

/// Get checkpoint status
pub async fn get_checkpoint_status(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
    Query(query): Query<CheckpointStatusQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let completion_value = query.completion_value.unwrap_or(12);

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

/// Get checkpoint history
pub async fn get_checkpoint_history(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<CheckpointHistoryQuery>,
) -> Result<Json<Vec<SessionEvent>>, (StatusCode, String)> {
    let limit = query.limit.unwrap_or(50);

    state
        .app_state
        .checkpoint_db
        .get_session_history(query.workflow_name.as_deref(), limit)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// List task runs
pub async fn list_task_runs(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListTaskRunsQuery>,
) -> Result<Json<Vec<TaskRun>>, (StatusCode, String)> {
    let limit = query.limit.unwrap_or(50);
    let db = state.app_state.checkpoint_db.clone();

    tokio::task::spawn_blocking(move || db.get_recent_task_runs(limit))
        .await
        .map_err(|e| {
            error!("spawn_blocking error in list_task_runs: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// List running task runs
pub async fn list_running_task_runs(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Vec<TaskRun>>, (StatusCode, String)> {
    let db = state.app_state.checkpoint_db.clone();

    tokio::task::spawn_blocking(move || db.get_running_task_runs())
        .await
        .map_err(|e| {
            error!("spawn_blocking error in list_running_task_runs: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Create a task run
pub async fn create_task_run(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CreateTaskRunRequest>,
) -> Result<Json<TaskRun>, (StatusCode, String)> {
    let id = uuid::Uuid::new_v4().to_string();
    let mut input = CreateTaskRunInput::new(&id, &req.task_name)
        .with_prompt(&req.prompt);
    if let Some(ms) = req.max_sessions {
        input = input.with_max_sessions(ms);
    }
    if let Some(ac) = req.auto_continue {
        input = input.with_auto_continue(ac);
    }
    state
        .app_state
        .checkpoint_db
        .create_task_run(&input)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Get a task run
pub async fn get_task_run(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<Option<TaskRun>>, (StatusCode, String)> {
    state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Get task output
pub async fn get_task_output(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(query): Query<TaskOutputQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let task_run = state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    let output = if let Some(tail_chars) = query.tail_chars {
        state
            .app_state
            .checkpoint_db
            .get_task_output_tail(&id, tail_chars)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
    } else {
        task_run.output_log
    };

    Ok(Json(serde_json::json!({
        "id": id,
        "output": output,
        "status": task_run.status,
        "sessions_count": task_run.sessions_count
    })))
}

/// Stop a task run
pub async fn stop_task_run(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let task_run = state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    if task_run.status != "running" {
        return Ok(Json(serde_json::json!({
            "success": false,
            "message": format!("Task is not running (status: {})", task_run.status)
        })));
    }

    state
        .app_state
        .checkpoint_db
        .stop_task_run(&id)
        .map(|_| {
            Json(serde_json::json!({
                "success": true,
                "message": "Task run stopped"
            }))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Delete a task run
pub async fn delete_task_run(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .app_state
        .checkpoint_db
        .delete_task_run(&id)
        .map(|deleted| {
            Json(serde_json::json!({
                "success": deleted,
                "message": if deleted { "Task run deleted" } else { "Task run not found" }
            }))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Get task auto-continue setting
pub async fn get_task_auto_continue(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .app_state
        .checkpoint_db
        .get_task_auto_continue(&id)
        .map(|auto_continue| {
            Json(serde_json::json!({
                "id": id,
                "auto_continue": auto_continue
            }))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Set task auto-continue setting
pub async fn set_task_auto_continue(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(req): Json<SetTaskAutoContinueRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .app_state
        .checkpoint_db
        .set_task_auto_continue(&id, req.auto_continue)
        .map(|_| {
            Json(serde_json::json!({
                "success": true,
                "id": id,
                "auto_continue": req.auto_continue
            }))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}
