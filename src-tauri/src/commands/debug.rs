//! Debug settings commands
//!
//! This module handles debug configuration operations:
//! - Getting current debug settings
//! - Updating debug settings (image debug, top matches count)
//! - Persisting settings and syncing with Python executor

use crate::settings;
use serde_json;
use tauri::State;
use tracing::{error, info};

use super::{AppState, CommandResponse};

/// Get the current debug settings.
///
/// Returns the debug settings stored in the persistent settings file.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with debug settings data
/// * `Err(String)` - Error message if settings cannot be loaded
#[tauri::command]
pub fn get_debug_settings() -> Result<CommandResponse, String> {
    info!("Getting debug settings");

    let debug_settings = settings::get_debug_settings();

    Ok(CommandResponse {
        success: true,
        message: Some("Debug settings retrieved".to_string()),
        data: Some(
            serde_json::to_value(&debug_settings)
                .map_err(|e| format!("Failed to serialize debug settings: {}", e))?,
        ),
    })
}

/// Set debug settings.
///
/// Updates the debug settings in the persistent settings file and sends them
/// to the running Python executor if active.
///
/// # Arguments
/// * `enable_image_debug` - Enable detailed image matching debug information
/// * `top_matches_count` - Number of top matches to include in debug output
/// * `state` - The application state containing the Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if settings cannot be saved
#[tauri::command]
pub fn set_debug_settings(
    enable_image_debug: bool,
    top_matches_count: u32,
    state: State<AppState>,
) -> Result<CommandResponse, String> {
    info!(
        "Setting debug settings: enable_image_debug={}, top_matches_count={}",
        enable_image_debug, top_matches_count
    );

    let debug_settings = settings::DebugSettings {
        enable_image_debug,
        top_matches_count,
    };

    // Save settings to disk
    settings::save_debug_settings(debug_settings)
        .map_err(|e| format!("Failed to save debug settings: {}", e))?;

    // If Python bridge is running, send the settings
    let mut bridge_lock = state.python_bridge.lock().unwrap();
    if let Some(ref mut bridge) = *bridge_lock {
        if bridge.is_running() {
            bridge
                .set_debug_settings(enable_image_debug, top_matches_count)
                .map_err(|e| {
                    error!("Failed to send debug settings to Python: {}", e);
                    format!("Failed to send debug settings to Python: {}", e)
                })?;
            info!("Debug settings sent to Python executor");
        }
    }

    Ok(CommandResponse {
        success: true,
        message: Some("Debug settings saved".to_string()),
        data: None,
    })
}
