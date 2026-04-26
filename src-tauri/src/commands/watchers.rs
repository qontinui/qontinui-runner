//! Tauri commands for watchers (screenpipe-inspired scheduled reactive AI agents).
//!
//! CRUD operations for watcher definitions. Each watcher queries the activity
//! timeline on a schedule, reasons with AI, and triggers an action.

use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;
use tracing::info;

use crate::commands::compartments::StorageCompartment;
use crate::database::types::*;

/// Create a new watcher. Returns the watcher ID.
#[tauri::command]
pub async fn create_watcher(
    input: CreateWatcherInput,
    storage: State<'_, StorageCompartment>,
) -> Result<String, String> {
    let id = storage.pg_db().create_watcher(&input).await?;
    info!("Created watcher '{}' ({})", input.name, id);
    Ok(id)
}

/// Get a single watcher by ID.
#[tauri::command]
pub async fn get_watcher(
    id: String,
    storage: State<'_, StorageCompartment>,
) -> Result<Option<Watcher>, String> {
    storage.pg_db().get_watcher(&id).await
}

/// List all watchers.
#[tauri::command]
pub async fn list_watchers(storage: State<'_, StorageCompartment>) -> Result<Vec<Watcher>, String> {
    storage.pg_db().list_watchers().await
}

/// Update a watcher's fields.
#[tauri::command]
pub async fn update_watcher(
    input: UpdateWatcherInput,
    storage: State<'_, StorageCompartment>,
) -> Result<Option<String>, String> {
    let result = storage.pg_db().update_watcher(&input).await?;
    if result.is_some() {
        info!("Updated watcher {}", input.id);
    }
    Ok(result)
}

/// Delete a watcher permanently.
#[tauri::command]
pub async fn delete_watcher(
    id: String,
    storage: State<'_, StorageCompartment>,
) -> Result<bool, String> {
    let deleted = storage.pg_db().delete_watcher(&id).await?;
    if deleted {
        info!("Deleted watcher {}", id);
    }
    Ok(deleted)
}

/// Enable or disable a watcher.
#[tauri::command]
pub async fn set_watcher_enabled(
    id: String,
    enabled: bool,
    storage: State<'_, StorageCompartment>,
) -> Result<bool, String> {
    let updated = storage.pg_db().set_watcher_enabled(&id, enabled).await?;
    if updated {
        info!("Set watcher {} enabled={}", id, enabled);
    }
    Ok(updated)
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_watchers")
        .invoke_handler(tauri::generate_handler![
            create_watcher,
            get_watcher,
            list_watchers,
            update_watcher,
            delete_watcher,
            set_watcher_enabled,
        ])
        .build()
}
