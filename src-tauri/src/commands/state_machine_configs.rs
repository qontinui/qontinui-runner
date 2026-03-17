//! Tauri commands for state machine config CRUD operations.
//!
//! These commands allow the runner frontend to create, read, update, and delete
//! state machine configurations (configs, states, transitions) stored in SQLite.

use std::sync::Arc;
use base64::Engine as _;
use tauri::State;
use tracing::info;

use crate::commands::AppState;
use crate::state_machine_configs::{
    storage, CreateSmConfigRequest, CreateSmStateRequest, CreateSmTransitionRequest, SmConfig,
    SmConfigFull, SmImportRequest, SmState, SmTransition, UpdateSmConfigRequest,
    UpdateSmStateRequest, UpdateSmTransitionRequest, SmCaptureScreenshotMeta,
    SmCaptureScreenshotSave,
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
    info!("Loading thumbnails for config {}", config_id);
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
    info!(
        "Loaded {} thumbnails for config {}",
        result.len(),
        config_id
    );
    Ok(result)
}

// =============================================================================
// Capture Screenshot Commands
// =============================================================================

#[tauri::command]
pub async fn sm_save_capture_screenshots(
    config_id: String,
    screenshots: Vec<SmCaptureScreenshotSave>,
    app_state: State<'_, Arc<AppState>>,
) -> Result<usize, String> {
    let db = app_state.checkpoint_db.clone();

    // Run on a blocking thread to avoid stalling the async runtime during
    // CPU-intensive PNG→WebP conversion.
    tokio::task::spawn_blocking(move || {
        let conn = db.get_conn_string()?;
        let mut count = 0;
        for ss in &screenshots {
            let id = uuid::Uuid::new_v4().to_string();

            // Convert base64 PNG to WebP for ~60-70% size reduction.
            // If conversion fails, store the original PNG bytes.
            let png_bytes = base64::engine::general_purpose::STANDARD
                .decode(&ss.screenshot_base64)
                .map_err(|e| format!("Failed to decode screenshot base64: {}", e))?;

            let webp_bytes = match image::load_from_memory(&png_bytes) {
                Ok(img) => {
                    let mut buf = std::io::Cursor::new(Vec::new());
                    match img.write_to(&mut buf, image::ImageFormat::WebP) {
                        Ok(()) => buf.into_inner(),
                        Err(_) => png_bytes.clone(),
                    }
                }
                Err(_) => png_bytes,
            };

            conn.execute(
                "INSERT INTO sm_capture_screenshots (id, config_id, capture_index, screenshot_webp, width, height, element_bounds_json, fingerprint_hashes_json, captured_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    id,
                    config_id,
                    ss.capture_index,
                    webp_bytes,
                    ss.width,
                    ss.height,
                    ss.element_bounds_json,
                    ss.fingerprint_hashes_json,
                    ss.captured_at,
                ],
            )
            .map_err(|e| format!("Failed to save capture screenshot: {}", e))?;
            count += 1;
        }
        info!("Saved {} capture screenshots for config {}", count, config_id);
        Ok(count)
    })
    .await
    .map_err(|e| format!("Screenshot save task panicked: {}", e))?
}

#[tauri::command]
pub async fn sm_get_capture_screenshots(
    config_id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<Vec<SmCaptureScreenshotMeta>, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    let mut stmt = conn
        .prepare("SELECT id, config_id, capture_index, width, height, element_bounds_json, fingerprint_hashes_json, captured_at FROM sm_capture_screenshots WHERE config_id = ?1 ORDER BY capture_index")
        .map_err(|e| format!("Failed to query capture screenshots: {}", e))?;
    let rows = stmt
        .query_map(rusqlite::params![config_id], |row| {
            Ok(SmCaptureScreenshotMeta {
                id: row.get(0)?,
                config_id: row.get(1)?,
                capture_index: row.get(2)?,
                width: row.get(3)?,
                height: row.get(4)?,
                element_bounds_json: row.get(5)?,
                fingerprint_hashes_json: row.get(6)?,
                captured_at: row.get(7)?,
            })
        })
        .map_err(|e| format!("Failed to fetch capture screenshots: {}", e))?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(|e| format!("Row error: {}", e))?);
    }
    info!(
        "Loaded {} capture screenshot metadata for config {}",
        result.len(),
        config_id
    );
    Ok(result)
}

#[tauri::command]
pub async fn sm_get_capture_screenshot_image(
    screenshot_id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    let webp_bytes: Vec<u8> = conn
        .query_row(
            "SELECT screenshot_webp FROM sm_capture_screenshots WHERE id = ?1",
            rusqlite::params![screenshot_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to get screenshot image: {}", e))?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&webp_bytes);
    Ok(format!("data:image/webp;base64,{}", b64))
}

/// Move pending capture screenshots from a temporary config_id to the real config.
/// Used after exploration saves screenshots with a placeholder config_id, then
/// the real config is created during saveConfig().
#[tauri::command]
pub async fn sm_move_pending_screenshots(
    from_config_id: String,
    to_config_id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<usize, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    let count = conn
        .execute(
            "UPDATE sm_capture_screenshots SET config_id = ?1 WHERE config_id = ?2",
            rusqlite::params![to_config_id, from_config_id],
        )
        .map_err(|e| format!("Failed to move screenshots: {}", e))?;
    if count > 0 {
        info!(
            "Moved {} capture screenshots from '{}' to config '{}'",
            count, from_config_id, to_config_id
        );
    }
    Ok(count)
}

/// Delete capture screenshots for a specific config_id.
/// Used to clean up pending screenshots that were never claimed.
#[tauri::command]
pub async fn sm_delete_capture_screenshots(
    config_id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<usize, String> {
    let conn = app_state.checkpoint_db.get_conn()?;
    let count = conn
        .execute(
            "DELETE FROM sm_capture_screenshots WHERE config_id = ?1",
            rusqlite::params![config_id],
        )
        .map_err(|e| format!("Failed to delete screenshots: {}", e))?;
    Ok(count)
}
