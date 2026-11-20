//! Tauri commands for checkpoint/session management via PostgreSQL.

use crate::commands::compartments::StorageCompartment;
use crate::commands::CommandResponse;
use crate::database::{CheckpointData, SessionEvent};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::{Emitter, State};

/// Get a checkpoint by workflow name.
#[tauri::command]
pub async fn checkpoint_get(
    app_state: State<'_, StorageCompartment>,
    workflow_name: String,
) -> Result<Option<CheckpointData>, String> {
    app_state.pg_db().get_checkpoint(&workflow_name).await
}

/// Save or update a checkpoint.
#[tauri::command]
pub async fn checkpoint_save(
    app_state: State<'_, StorageCompartment>,
    data: CheckpointData,
) -> Result<CommandResponse, String> {
    app_state.pg_db().save_checkpoint(&data).await?;
    Ok(CommandResponse {
        success: true,
        message: Some("Checkpoint saved".to_string()),
        data: None,
    })
}

/// Delete a checkpoint by workflow name.
#[tauri::command]
pub async fn checkpoint_delete(
    app_state: State<'_, StorageCompartment>,
    workflow_name: String,
) -> Result<CommandResponse, String> {
    let deleted = app_state.pg_db().delete_checkpoint(&workflow_name).await?;
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
    app_state: State<'_, StorageCompartment>,
) -> Result<Vec<CheckpointData>, String> {
    app_state.pg_db().list_active_checkpoints().await
}

/// Check checkpoint status for cross-session continuation.
/// Returns (is_complete, current_phase) or None if not found.
#[tauri::command]
pub async fn checkpoint_status(
    app_state: State<'_, StorageCompartment>,
    workflow_name: String,
    completion_value: u32,
) -> Result<Option<(bool, u32)>, String> {
    // checkpoint_db removed — checkpoint status was SQLite-only, returns None
    let _ = (&workflow_name, completion_value);
    Ok(None)
}

/// Get session/checkpoint history.
#[tauri::command]
pub async fn checkpoint_history(
    app_state: State<'_, StorageCompartment>,
    workflow_name: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<SessionEvent>, String> {
    let _limit = limit.unwrap_or(50);
    // checkpoint_db removed — session history was SQLite-only, returns empty
    Ok(Vec::new())
}

/// Get a setting value.
#[tauri::command]
pub async fn setting_get(
    app_state: State<'_, StorageCompartment>,
    key: String,
) -> Result<Option<serde_json::Value>, String> {
    app_state.pg_db().get_setting(&key).await
}

/// Set a setting value. Emits a `setting-changed` event so frontend hooks
/// that cache settings in React state can refresh without a page reload.
#[tauri::command]
pub async fn setting_set<R: Runtime>(
    app_handle: tauri::AppHandle<R>,
    app_state: State<'_, StorageCompartment>,
    key: String,
    value: serde_json::Value,
) -> Result<CommandResponse, String> {
    app_state.pg_db().set_setting(&key, &value).await?;
    let _ = app_handle.emit("setting-changed", &key);
    Ok(CommandResponse {
        success: true,
        message: Some(format!("Setting '{}' saved", key)),
        data: None,
    })
}

/// Get all settings.
#[tauri::command]
pub async fn settings_get_all(
    app_state: State<'_, StorageCompartment>,
) -> Result<serde_json::Value, String> {
    app_state.pg_db().get_all_settings().await
}

/// Build the Tauri plugin that registers this module's command handlers.
///
/// See `commands/mod.rs` for the migration guide explaining the plugin pattern.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_checkpoints")
        .invoke_handler(tauri::generate_handler![
            checkpoint_get,
            checkpoint_save,
            checkpoint_delete,
            checkpoint_list_active,
            checkpoint_status,
            checkpoint_history,
            setting_get,
            setting_set,
            settings_get_all,
        ])
        .build()
}
