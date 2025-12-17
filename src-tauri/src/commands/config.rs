//! Configuration management commands
//!
//! This module handles all configuration-related operations including:
//! - Loading and validating YAML configuration files
//! - Managing the current configuration state
//! - Persisting configuration paths and workflow IDs
//! - Auto-load settings

use crate::config::{ConfigLoader, QontinuiConfig};
use crate::error::AppError;
use crate::settings;
use std::sync::Arc;
use tauri::State;
use tracing::{error, info, warn};

use super::{AppState, CommandResponse};

/// Load a configuration file from the specified path.
///
/// This command:
/// 1. Loads and validates the YAML configuration
/// 2. Stores it in the app state
/// 3. Persists the path for auto-load functionality
/// 4. Sends the configuration to the Python executor if running
/// 5. Applies current debug settings to the executor
///
/// # Arguments
/// * `path` - Absolute path to the YAML configuration file
/// * `state` - Application state containing config and Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with configuration summary
/// * `Err(String)` - Error message if loading fails
#[tauri::command]
pub fn load_configuration(
    path: String,
    state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Loading configuration from: {}", path);

    // Load the configuration file
    let config = ConfigLoader::load_from_file(&path)
        .map_err(|e| {
            error!("Failed to load configuration from {}: {}", path, e);
            AppError::ConfigError(format!("Failed to load configuration: {}", e))
        })
        .map_err(|e| e.to_string())?;

    let summary = config.summary();

    // Create data object with configuration info
    let config_data = serde_json::json!({
        "workflows": config.workflows.clone(),
        "states": config.states.clone(),
        "transitions": config.transitions.clone(),
        "images": config.images.clone()
    });

    // Store the configuration
    *state.current_config.lock().unwrap() = Some(config);
    info!("Configuration loaded successfully: {}", summary);

    // Save the path as the last loaded config
    if let Err(e) = settings::save_last_config_path(&path) {
        warn!("Failed to save last config path: {}", e);
    }

    // If Python bridge is running, send the configuration and debug settings
    if let Some(ref mut bridge) = *state.python_bridge.lock().unwrap() {
        if bridge.is_running() {
            // First send debug settings to ensure they're applied before config execution
            let debug_settings = settings::get_debug_settings();
            if let Err(e) = bridge.set_debug_settings(
                debug_settings.enable_image_debug,
                debug_settings.top_matches_count,
            ) {
                warn!("Failed to send debug settings before config load: {}", e);
            } else {
                info!(
                    "Debug settings sent before config load: enable={}, top_matches={}",
                    debug_settings.enable_image_debug, debug_settings.top_matches_count
                );
            }

            bridge.load_configuration(&path).map_err(|e| {
                error!("Failed to send configuration to Python: {}", e);
                format!("Failed to send configuration to Python: {}", e)
            })?;
            info!("Configuration sent to Python executor");
        }
    }

    Ok(CommandResponse {
        success: true,
        message: Some(summary),
        data: Some(config_data),
    })
}

/// Get the currently loaded configuration.
///
/// # Arguments
/// * `state` - Application state containing the current configuration
///
/// # Returns
/// * `Ok(QontinuiConfig)` - The current configuration
/// * `Err(String)` - Error if no configuration is loaded
#[tauri::command]
pub fn get_current_configuration(state: State<Arc<AppState>>) -> Result<QontinuiConfig, String> {
    state
        .current_config
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "No configuration loaded".to_string())
}

/// Get the path of the last loaded configuration file.
///
/// Returns the path if it exists and the file is still present on disk.
/// Also returns the last workflow_id and monitor_indices if saved.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with path, workflow_id, and monitor_indices, or message if not found
/// * `Err(String)` - Error message if settings cannot be read
#[tauri::command]
pub fn get_last_config_path() -> Result<CommandResponse, String> {
    info!("Getting last config path");

    if let Some(path) = settings::get_last_config_path() {
        // Check if the file still exists
        if std::path::Path::new(&path).exists() {
            let workflow_id = settings::get_last_workflow_id();
            let monitor_indices = settings::get_last_monitor_indices();
            // Also provide legacy single monitor_index for backward compatibility
            let monitor_index = monitor_indices.as_ref().and_then(|v| v.first().copied());
            Ok(CommandResponse {
                success: true,
                message: Some(format!("Last config found: {}", path)),
                data: Some(serde_json::json!({
                    "path": path,
                    "workflow_id": workflow_id,
                    "monitor_indices": monitor_indices,
                    "monitor_index": monitor_index
                })),
            })
        } else {
            info!(
                "Last config path exists in settings but file not found: {}",
                path
            );
            Ok(CommandResponse {
                success: false,
                message: Some("Last config file not found".to_string()),
                data: None,
            })
        }
    } else {
        Ok(CommandResponse {
            success: false,
            message: Some("No last config path saved".to_string()),
            data: None,
        })
    }
}

/// Save the last used workflow ID to persistent settings.
///
/// # Arguments
/// * `workflow_id` - The workflow ID to save
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if settings cannot be saved
#[tauri::command]
pub fn save_last_workflow_id(workflow_id: String) -> Result<CommandResponse, String> {
    info!("Saving last workflow ID: {}", workflow_id);

    settings::save_last_workflow_id(&workflow_id)
        .map_err(|e| format!("Failed to save last workflow ID: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Last workflow ID saved".to_string()),
        data: None,
    })
}

/// Save the last used monitor index to persistent settings.
///
/// # Arguments
/// * `monitor_index` - The monitor index to save
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if settings cannot be saved
#[tauri::command]
pub fn save_last_monitor_index(monitor_index: i32) -> Result<CommandResponse, String> {
    info!("Saving last monitor index: {}", monitor_index);

    settings::save_last_monitor_index(monitor_index)
        .map_err(|e| format!("Failed to save last monitor index: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Last monitor index saved".to_string()),
        data: None,
    })
}

/// Save the last used monitor indices to persistent settings (multi-monitor support).
///
/// # Arguments
/// * `monitor_indices` - Array of monitor indices to save
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if settings cannot be saved
#[tauri::command]
pub fn save_last_monitor_indices(monitor_indices: Vec<i32>) -> Result<CommandResponse, String> {
    info!("Saving last monitor indices: {:?}", monitor_indices);

    settings::save_last_monitor_indices(monitor_indices)
        .map_err(|e| format!("Failed to save last monitor indices: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Last monitor indices saved".to_string()),
        data: None,
    })
}

/// Get the auto-load last config setting.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with enabled status
/// * `Err(String)` - Error message if settings cannot be read
#[tauri::command]
pub fn get_auto_load_last_config() -> Result<CommandResponse, String> {
    let enabled = settings::get_auto_load_last_config();

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "enabled": enabled
        })),
    })
}

/// Save the auto-load last config setting.
///
/// # Arguments
/// * `enabled` - Whether to auto-load the last config on startup
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if settings cannot be saved
#[tauri::command]
pub fn save_auto_load_last_config(enabled: bool) -> Result<CommandResponse, String> {
    info!("Saving auto-load last config setting: {}", enabled);

    settings::save_auto_load_last_config(enabled)
        .map_err(|e| format!("Failed to save auto-load setting: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Auto-load last config {}",
            if enabled { "enabled" } else { "disabled" }
        )),
        data: None,
    })
}
