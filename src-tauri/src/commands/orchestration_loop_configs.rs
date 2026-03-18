//! Tauri commands for orchestration loop config CRUD operations.

use std::sync::Arc;
use tauri::State;
use tracing::info;

use crate::commands::AppState;
use crate::orchestration_loop_configs::{
    storage, CreateOlConfigRequest, OlConfig, UpdateOlConfigRequest,
};

#[tauri::command]
pub async fn ol_list_configs(app_state: State<'_, Arc<AppState>>) -> Result<Vec<OlConfig>, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    storage::list_configs(&conn)
}

#[tauri::command]
pub async fn ol_get_config(
    id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<Option<OlConfig>, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    storage::get_config(&conn, &id)
}

#[tauri::command]
pub async fn ol_save_config(
    request: CreateOlConfigRequest,
    app_state: State<'_, Arc<AppState>>,
) -> Result<OlConfig, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    let config = storage::insert_config(&conn, &request)?;
    info!(
        "Saved orchestration loop config: {} ({})",
        config.name, config.id
    );
    Ok(config)
}

#[tauri::command]
pub async fn ol_update_config(
    id: String,
    request: UpdateOlConfigRequest,
    app_state: State<'_, Arc<AppState>>,
) -> Result<OlConfig, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    let config = storage::update_config(&conn, &id, &request)?;
    info!(
        "Updated orchestration loop config: {} ({})",
        config.name, config.id
    );
    Ok(config)
}

#[tauri::command]
pub async fn ol_delete_config(
    id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    let deleted = storage::delete_config(&conn, &id)?;
    if deleted {
        info!("Deleted orchestration loop config: {}", id);
    }
    Ok(deleted)
}

#[tauri::command]
pub async fn ol_toggle_favorite(
    id: String,
    is_favorite: bool,
    app_state: State<'_, Arc<AppState>>,
) -> Result<OlConfig, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    let req = UpdateOlConfigRequest {
        name: None,
        description: None,
        is_favorite: Some(is_favorite),
        config_json: None,
    };
    storage::update_config(&conn, &id, &req)
}
