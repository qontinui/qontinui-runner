//! Tauri commands for checkpoint/session management via SQLite database.

use crate::commands::{AppState, CommandResponse};
use crate::database::{CheckpointData, SessionEvent};
use std::sync::Arc;
use tauri::State;

/// Get a checkpoint by workflow name.
#[tauri::command]
pub async fn checkpoint_get(
    app_state: State<'_, Arc<AppState>>,
    workflow_name: String,
) -> Result<Option<CheckpointData>, String> {
    app_state.pg_db.get_checkpoint(&workflow_name).await
}

/// Save or update a checkpoint.
#[tauri::command]
pub async fn checkpoint_save(
    app_state: State<'_, Arc<AppState>>,
    data: CheckpointData,
) -> Result<CommandResponse, String> {
    app_state.pg_db.save_checkpoint(&data).await?;
    Ok(CommandResponse {
        success: true,
        message: Some("Checkpoint saved".to_string()),
        data: None,
    })
}

/// Delete a checkpoint by workflow name.
#[tauri::command]
pub async fn checkpoint_delete(
    app_state: State<'_, Arc<AppState>>,
    workflow_name: String,
) -> Result<CommandResponse, String> {
    let deleted = app_state.pg_db.delete_checkpoint(&workflow_name).await?;
    Ok(CommandResponse {
        success: deleted,
        message: Some(if deleted {
            "Checkpoint deleted".to_string()
        } else {
            "Checkpoint not found".to_string()
        }),
        data: None,
    })
}

/// List all active (non-completed) checkpoints.
#[tauri::command]
pub async fn checkpoint_list_active(
    app_state: State<'_, Arc<AppState>>,
) -> Result<Vec<CheckpointData>, String> {
    app_state.pg_db.list_active_checkpoints().await
}

/// Check checkpoint status for cross-session continuation.
/// Returns (is_complete, current_phase) or None if not found.
#[tauri::command]
pub async fn checkpoint_status(
    app_state: State<'_, Arc<AppState>>,
    workflow_name: String,
    completion_value: u32,
) -> Result<Option<(bool, u32)>, String> {
    app_state
        .checkpoint_db
        .check_checkpoint_status(&workflow_name, completion_value)
}

/// Get session/checkpoint history.
#[tauri::command]
pub async fn checkpoint_history(
    app_state: State<'_, Arc<AppState>>,
    workflow_name: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<SessionEvent>, String> {
    let limit = limit.unwrap_or(50);
    app_state
        .checkpoint_db
        .get_session_history(workflow_name.as_deref(), limit)
}

/// Create a new session record.
#[tauri::command]
pub async fn session_create(
    app_state: State<'_, Arc<AppState>>,
    id: String,
    session_type: String,
    name: String,
    workflow_name: Option<String>,
    run_id: Option<String>,
) -> Result<CommandResponse, String> {
    app_state.pg_db.create_session(&id, &session_type, &name, workflow_name.as_deref(), run_id.as_deref()).await?;
    Ok(CommandResponse {
        success: true,
        message: Some("Session created".to_string()),
        data: None,
    })
}

/// Update session status.
#[tauri::command]
pub async fn session_update_status(
    app_state: State<'_, Arc<AppState>>,
    session_id: String,
    status: String,
    current_phase: Option<u32>,
    error_message: Option<String>,
) -> Result<CommandResponse, String> {
    app_state.pg_db.update_session_status(&session_id, &status, current_phase, error_message.as_deref()).await?;
    Ok(CommandResponse {
        success: true,
        message: Some(format!("Session status updated to {}", status)),
        data: None,
    })
}

/// Get a setting value.
#[tauri::command]
pub async fn setting_get(
    app_state: State<'_, Arc<AppState>>,
    key: String,
) -> Result<Option<serde_json::Value>, String> {
    app_state.pg_db.get_setting(&key).await
}

/// Set a setting value.
#[tauri::command]
pub async fn setting_set(
    app_state: State<'_, Arc<AppState>>,
    key: String,
    value: serde_json::Value,
) -> Result<CommandResponse, String> {
    app_state.pg_db.set_setting(&key, &value).await?;
    Ok(CommandResponse {
        success: true,
        message: Some(format!("Setting '{}' saved", key)),
        data: None,
    })
}

/// Get all settings.
#[tauri::command]
pub async fn settings_get_all(
    app_state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    app_state.pg_db.get_all_settings().await
}
