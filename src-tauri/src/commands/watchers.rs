//! Tauri commands for watchers (screenpipe-inspired scheduled reactive AI agents).
//!
//! CRUD operations for watcher definitions. Each watcher queries the activity
//! timeline on a schedule, reasons with AI, and triggers an action.

use std::sync::Arc;
use tauri::State;
use tracing::info;

use crate::commands::AppState;
use crate::database::types::*;

/// Create a new watcher. Returns the watcher ID.
#[tauri::command]
pub async fn create_watcher(
    input: CreateWatcherInput,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let pg = &state.pg_db;

    let id = pg.create_watcher(&input).await?;
    info!("Created watcher '{}' ({})", input.name, id);
    Ok(id)
}

/// Get a single watcher by ID.
#[tauri::command]
pub async fn get_watcher(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Option<Watcher>, String> {
    let pg = &state.pg_db;

    pg.get_watcher(&id).await
}

/// List all watchers.
#[tauri::command]
pub async fn list_watchers(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<Watcher>, String> {
    let pg = &state.pg_db;

    pg.list_watchers().await
}

/// Update a watcher's fields.
#[tauri::command]
pub async fn update_watcher(
    input: UpdateWatcherInput,
    state: State<'_, Arc<AppState>>,
) -> Result<Option<String>, String> {
    let pg = &state.pg_db;

    let result = pg.update_watcher(&input).await?;
    if result.is_some() {
        info!("Updated watcher {}", input.id);
    }
    Ok(result)
}

/// Delete a watcher permanently.
#[tauri::command]
pub async fn delete_watcher(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let pg = &state.pg_db;

    let deleted = pg.delete_watcher(&id).await?;
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
    state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let pg = &state.pg_db;

    let updated = pg.set_watcher_enabled(&id, enabled).await?;
    if updated {
        info!("Set watcher {} enabled={}", id, enabled);
    }
    Ok(updated)
}
