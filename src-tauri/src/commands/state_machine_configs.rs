//! Tauri commands for state machine config CRUD operations.
//!
//! These commands allow the runner frontend to create, read, update, and delete
//! state machine configurations (configs, states, transitions) stored in PostgreSQL.

use std::sync::Arc;
use tauri::State;
use tracing::info;

use crate::commands::AppState;
use crate::state_machine_configs::{
    CreateSmConfigRequest, CreateSmStateRequest, CreateSmTransitionRequest,
    SmCaptureScreenshotMeta, SmCaptureScreenshotSave, SmConfig, SmConfigFull, SmImportRequest,
    SmState, SmTransition, UpdateSmConfigRequest, UpdateSmStateRequest, UpdateSmTransitionRequest,
};

// =============================================================================
// Config Commands
// =============================================================================

#[tauri::command]
pub async fn sm_list_configs(app_state: State<'_, Arc<AppState>>) -> Result<Vec<SmConfig>, String> {
    app_state.pg_db.list_sm_configs().await
}

#[tauri::command]
pub async fn sm_get_config(
    id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<Option<SmConfigFull>, String> {
    app_state.pg_db.get_sm_config_full(&id).await
}

#[tauri::command]
pub async fn sm_create_config(
    request: CreateSmConfigRequest,
    app_state: State<'_, Arc<AppState>>,
) -> Result<SmConfig, String> {
    let config = app_state.pg_db.insert_sm_config(&request).await?;
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
    let config = app_state.pg_db.update_sm_config(&id, &request).await?;
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
    let deleted = app_state.pg_db.delete_sm_config(&id).await?;
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
    let state = app_state
        .pg_db
        .insert_sm_state(&config_id, &request)
        .await?;
    info!("Created state: {} ({})", state.name, state.id);
    Ok(state)
}

#[tauri::command]
pub async fn sm_update_state(
    id: String,
    request: UpdateSmStateRequest,
    app_state: State<'_, Arc<AppState>>,
) -> Result<SmState, String> {
    let state = app_state.pg_db.update_sm_state(&id, &request).await?;
    info!("Updated state: {} ({})", state.name, state.id);
    Ok(state)
}

#[tauri::command]
pub async fn sm_delete_state(
    id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    app_state.pg_db.delete_sm_state(&id).await
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
    let transition = app_state
        .pg_db
        .insert_sm_transition(&config_id, &request)
        .await?;
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
    let transition = app_state.pg_db.update_sm_transition(&id, &request).await?;
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
    app_state.pg_db.delete_sm_transition(&id).await
}

// =============================================================================
// Import Command
// =============================================================================

#[tauri::command]
pub async fn sm_import_config(
    request: SmImportRequest,
    app_state: State<'_, Arc<AppState>>,
) -> Result<SmConfigFull, String> {
    let result = app_state.pg_db.import_sm_config(&request).await?;
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
    let count = app_state
        .pg_db
        .save_sm_thumbnails(&config_id, &thumbnails)
        .await?;
    info!("Saved {} thumbnails for config {}", count, config_id);
    Ok(count)
}

#[tauri::command]
pub async fn sm_get_thumbnails(
    config_id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<std::collections::HashMap<String, String>, String> {
    info!("Loading thumbnails for config {}", config_id);
    app_state.pg_db.get_sm_thumbnails(&config_id).await
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
    let pg_db = app_state.pg_db.clone();
    let config_id_clone = config_id.clone();

    // Run on a blocking-compatible spawn since PNG->WebP is CPU-intensive.
    // The PG methods themselves are async, so we use spawn instead of spawn_blocking.
    let count = pg_db
        .save_capture_screenshots(&config_id_clone, &screenshots)
        .await?;
    info!(
        "Saved {} capture screenshots for config {}",
        count, config_id
    );
    Ok(count)
}

#[tauri::command]
pub async fn sm_get_capture_screenshots(
    config_id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<Vec<SmCaptureScreenshotMeta>, String> {
    app_state.pg_db.get_capture_screenshots(&config_id).await
}

#[tauri::command]
pub async fn sm_get_capture_screenshot_image(
    screenshot_id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    app_state
        .pg_db
        .get_capture_screenshot_image(&screenshot_id)
        .await
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
    app_state
        .pg_db
        .move_pending_screenshots(&from_config_id, &to_config_id)
        .await
}

/// Delete capture screenshots for a specific config_id.
/// Used to clean up pending screenshots that were never claimed.
#[tauri::command]
pub async fn sm_delete_capture_screenshots(
    config_id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<usize, String> {
    app_state.pg_db.delete_capture_screenshots(&config_id).await
}

/// One-shot backfill that rewrites every `sm_capture_screenshots` row's
/// `width`/`height` from the actual decoded WebP dimensions. Fixes rows
/// written before the dimension-fix landed (frontend was multiplying physical
/// pixel dims by device-pixel-ratio a second time).
///
/// Returns `(scanned, rewritten)`.
#[tauri::command]
pub async fn sm_backfill_capture_screenshot_dimensions(
    app_state: State<'_, Arc<AppState>>,
) -> Result<(usize, usize), String> {
    app_state
        .pg_db
        .backfill_capture_screenshot_dimensions()
        .await
}

// =============================================================================
// Static Analysis Generation
// =============================================================================

/// Generate a state machine from the Runner's source code using the static builder.
/// Runs `buildStateMachine()` + `emitPersistedStateMachineJSON()` via Node.js.
#[tauri::command]
pub async fn sm_generate_static(project_root: String) -> Result<String, String> {
    let script = format!(
        r#"
const {{ buildStateMachine, emitPersistedStateMachineJSON }} = require('./ui-bridge-auto/dist/index.js');
const result = buildStateMachine({{
    projectRoot: '{root}',
    routeFile: 'src/components/app/TabContent.tsx',
    routeFunction: 'TabContent',
    routeDiscriminant: 'activeTab',
    appShellFile: 'src/App.tsx',
    navigationFunctions: ['setActiveTab'],
    navigationEvents: ['ui-bridge-navigate', 'navigate-to-active', 'navigate-to-error-monitor'],
    tsconfigPath: 'tsconfig.json',
    maxComponentDepth: 2,
}});
process.stdout.write(emitPersistedStateMachineJSON(result.states, result.transitions));
"#,
        root = project_root.replace('\\', "/")
    );

    // Run in the qontinui-root directory (parent of both runner and ui-bridge-auto)
    let working_dir = std::path::Path::new(&project_root)
        .parent()
        .unwrap_or(std::path::Path::new(&project_root));

    let output = tokio::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .current_dir(working_dir)
        .output()
        .await
        .map_err(|e| format!("Failed to run node: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Static analysis failed: {}", stderr));
    }

    let json = String::from_utf8_lossy(&output.stdout).to_string();
    if json.is_empty() {
        return Err("Static analysis produced no output".to_string());
    }

    info!(
        "Generated state machine: {} bytes",
        json.len()
    );

    Ok(json)
}
