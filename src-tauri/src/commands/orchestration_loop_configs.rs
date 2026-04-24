//! Tauri commands for orchestration loop config CRUD operations.

use std::sync::Arc;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;
use tracing::info;

use crate::commands::AppState;
use crate::orchestration_loop_configs::{CreateOlConfigRequest, OlConfig, UpdateOlConfigRequest};

#[tauri::command]
pub async fn ol_list_configs(app_state: State<'_, Arc<AppState>>) -> Result<Vec<OlConfig>, String> {
    app_state.pg_db.list_ol_configs().await
}

#[tauri::command]
pub async fn ol_get_config(
    id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<Option<OlConfig>, String> {
    app_state.pg_db.get_ol_config(&id).await
}

#[tauri::command]
pub async fn ol_save_config(
    request: CreateOlConfigRequest,
    app_state: State<'_, Arc<AppState>>,
) -> Result<OlConfig, String> {
    let config = app_state.pg_db.insert_ol_config(&request).await?;
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
    let config = app_state.pg_db.update_ol_config(&id, &request).await?;
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
    let deleted = app_state.pg_db.delete_ol_config(&id).await?;
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
    let req = UpdateOlConfigRequest {
        name: None,
        description: None,
        is_favorite: Some(is_favorite),
        config_json: None,
    };
    app_state.pg_db.update_ol_config(&id, &req).await
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_orchestration_loop_configs")
        .invoke_handler(tauri::generate_handler![
            ol_list_configs,
            ol_get_config,
            ol_save_config,
            ol_update_config,
            ol_delete_config,
            ol_toggle_favorite,
        ])
        .build()
}
