//! Tauri commands for state machine config CRUD operations.
//!
//! These commands allow the runner frontend to create, read, update, and delete
//! state machine configurations (configs, states, transitions) stored in SQLite.

use std::sync::Arc;
use tauri::State;
use tracing::info;

use crate::commands::AppState;
use crate::state_machine_configs::{
    storage, CreateSmConfigRequest, CreateSmStateRequest, CreateSmTransitionRequest, SmConfig,
    SmConfigFull, SmImportRequest, SmState, SmTransition, UpdateSmConfigRequest,
    UpdateSmStateRequest, UpdateSmTransitionRequest,
};

// =============================================================================
// Config Commands
// =============================================================================

#[tauri::command]
pub async fn sm_list_configs(app_state: State<'_, Arc<AppState>>) -> Result<Vec<SmConfig>, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    storage::list_configs(&conn)
}

#[tauri::command]
pub async fn sm_get_config(
    id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<Option<SmConfigFull>, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    storage::get_config_full(&conn, &id)
}

#[tauri::command]
pub async fn sm_create_config(
    request: CreateSmConfigRequest,
    app_state: State<'_, Arc<AppState>>,
) -> Result<SmConfig, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    let config = storage::insert_config(&conn, &request)?;
    info!(
        "Created state machine config: {} ({})",
        config.name, config.id
    );
    Ok(config)
}

#[tauri::command]
pub async fn sm_update_config(
    id: String,
    request: UpdateSmConfigRequest,
    app_state: State<'_, Arc<AppState>>,
) -> Result<SmConfig, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    let config = storage::update_config(&conn, &id, &request)?;
    info!(
        "Updated state machine config: {} ({})",
        config.name, config.id
    );
    Ok(config)
}

#[tauri::command]
pub async fn sm_delete_config(
    id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    let deleted = storage::delete_config(&conn, &id)?;
    if deleted {
        info!("Deleted state machine config: {}", id);
    }
    Ok(deleted)
}

// =============================================================================
// State Commands
// =============================================================================

#[tauri::command]
pub async fn sm_create_state(
    config_id: String,
    request: CreateSmStateRequest,
    app_state: State<'_, Arc<AppState>>,
) -> Result<SmState, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    let state = storage::insert_state(&conn, &config_id, &request)?;
    info!("Created state: {} ({})", state.name, state.id);
    Ok(state)
}

#[tauri::command]
pub async fn sm_update_state(
    id: String,
    request: UpdateSmStateRequest,
    app_state: State<'_, Arc<AppState>>,
) -> Result<SmState, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    let state = storage::update_state(&conn, &id, &request)?;
    info!("Updated state: {} ({})", state.name, state.id);
    Ok(state)
}

#[tauri::command]
pub async fn sm_delete_state(
    id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    storage::delete_state(&conn, &id)
}

// =============================================================================
// Transition Commands
// =============================================================================

#[tauri::command]
pub async fn sm_create_transition(
    config_id: String,
    request: CreateSmTransitionRequest,
    app_state: State<'_, Arc<AppState>>,
) -> Result<SmTransition, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    let transition = storage::insert_transition(&conn, &config_id, &request)?;
    info!(
        "Created transition: {} ({})",
        transition.name, transition.id
    );
    Ok(transition)
}

#[tauri::command]
pub async fn sm_update_transition(
    id: String,
    request: UpdateSmTransitionRequest,
    app_state: State<'_, Arc<AppState>>,
) -> Result<SmTransition, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    let transition = storage::update_transition(&conn, &id, &request)?;
    info!(
        "Updated transition: {} ({})",
        transition.name, transition.id
    );
    Ok(transition)
}

#[tauri::command]
pub async fn sm_delete_transition(
    id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    storage::delete_transition(&conn, &id)
}

// =============================================================================
// Import Command
// =============================================================================

#[tauri::command]
pub async fn sm_import_config(
    request: SmImportRequest,
    app_state: State<'_, Arc<AppState>>,
) -> Result<SmConfigFull, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    let result = storage::import_config(&conn, &request)?;
    info!(
        "Imported state machine config: {} ({} states, {} transitions)",
        result.config.name,
        result.states.len(),
        result.transitions.len()
    );
    Ok(result)
}

// =============================================================================
// Thumbnail Commands
// =============================================================================

#[tauri::command]
pub async fn sm_save_thumbnails(
    config_id: String,
    thumbnails: std::collections::HashMap<String, String>,
    app_state: State<'_, Arc<AppState>>,
) -> Result<usize, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    let mut count = 0;
    for (hash, data) in &thumbnails {
        conn.execute(
            "INSERT OR REPLACE INTO sm_element_thumbnails (config_id, fingerprint_hash, thumbnail_base64) VALUES (?1, ?2, ?3)",
            rusqlite::params![config_id, hash, data],
        )
        .map_err(|e| format!("Failed to save thumbnail: {}", e))?;
        count += 1;
    }
    info!("Saved {} thumbnails for config {}", count, config_id);
    Ok(count)
}

#[tauri::command]
pub async fn sm_get_thumbnails(
    config_id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<std::collections::HashMap<String, String>, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    let mut stmt = conn
        .prepare("SELECT fingerprint_hash, thumbnail_base64 FROM sm_element_thumbnails WHERE config_id = ?1")
        .map_err(|e| format!("Failed to query thumbnails: {}", e))?;
    let rows = stmt
        .query_map(rusqlite::params![config_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| format!("Failed to fetch thumbnails: {}", e))?;
    let mut result = std::collections::HashMap::new();
    for row in rows {
        let (hash, data) = row.map_err(|e| format!("Row error: {}", e))?;
        result.insert(hash, data);
    }
    Ok(result)
}
