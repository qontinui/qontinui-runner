//! Python executor lifecycle commands
//!
//! Commands for starting, stopping, and configuring the Python executor.

use crate::config::{ConfigLoader, ScreenshotCaptureSettings};
use crate::executor::PythonBridge;
use crate::settings;
use std::sync::Arc;
use tauri::{Emitter, State};
use tracing::{error, info, warn};

use super::super::{AppState, CommandResponse};

/// Start the Python executor.
///
/// This command:
/// 1. Creates a new Python bridge instance
/// 2. Starts the Python process
/// 3. Sends previously loaded configuration if available
/// 4. Applies current debug settings
///
/// # Arguments
/// * `app_handle` - Tauri application handle for event emission
/// * `state` - Application state containing the Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if executor fails to start or is already running
#[tauri::command]
pub fn start_python_executor(
    app_handle: tauri::AppHandle,
    state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Starting Python executor");
    let mut bridge_lock = state
        .python_bridge
        .lock()
        .map_err(|e| format!("Python bridge mutex poisoned: {}", e))?;

    // Check if already running
    if let Some(ref mut bridge) = *bridge_lock {
        if bridge.is_running() {
            info!("Python executor already running, checking if config needs to be reloaded");

            // When app restarts but Python is still running, we need to reload the config
            // because the Rust state was cleared but Python might have stale or no config
            if let Some(config_path) = settings::get_last_config_path() {
                info!(
                    "Reloading configuration to Python executor: {}",
                    config_path
                );

                // Load and validate the configuration file
                match ConfigLoader::load_from_file(&config_path) {
                    Ok(config) => {
                        // Create config data for event emission
                        let config_data = serde_json::json!({
                            "metadata": config.metadata.clone(),
                            "workflows": config.workflows.clone(),
                            "states": config.states.clone(),
                            "transitions": config.transitions.clone(),
                            "images": config.images.clone()
                        });

                        // Store in app state
                        match state.current_config.lock() {
                            Ok(mut guard) => *guard = Some(config),
                            Err(e) => warn!("Failed to store config (mutex poisoned): {}", e),
                        }
                        info!("Configuration stored in app state");

                        // Send debug settings first
                        let debug_settings = settings::get_debug_settings();
                        if let Err(e) = bridge.set_debug_settings(
                            debug_settings.enable_image_debug,
                            debug_settings.top_matches_count,
                        ) {
                            warn!("Failed to send debug settings: {}", e);
                        }

                        // Send configuration to Python
                        if let Err(e) = bridge.load_configuration(&config_path) {
                            warn!("Failed to reload configuration to Python executor: {}", e);
                        } else {
                            info!("Configuration reloaded to Python executor successfully");
                        }

                        // Get saved workflow and monitor for event
                        let workflow_id = settings::get_last_workflow_id();
                        let monitor_index = settings::get_last_monitor_index();

                        // Emit event to notify frontend of config load
                        let event_payload = serde_json::json!({
                            "event": "config_loaded",
                            "data": {
                                "path": config_path,
                                "config": config_data,
                                "workflow_id": workflow_id,
                                "monitor_index": monitor_index
                            }
                        });

                        if let Err(e) = app_handle.emit("executor-event", &event_payload) {
                            warn!("Failed to emit config_loaded event: {}", e);
                        } else {
                            info!("Emitted config_loaded event to frontend");
                        }
                    }
                    Err(e) => {
                        warn!("Failed to load configuration file {}: {}", config_path, e);
                    }
                }
            }

            return Ok(CommandResponse {
                success: true, // Changed to success since executor is running
                message: Some("Python executor already running".to_string()),
                data: None,
            });
        }
    }

    // Create and start new bridge
    let mut bridge = PythonBridge::new(app_handle);
    bridge.start().map_err(|e| {
        error!("Failed to start Python executor: {}", e);
        format!("Failed to start Python executor: {}", e)
    })?;

    // If a configuration was already loaded, send it to the Python executor
    let config_lock =
        crate::safe_lock::safe_lock_or_recover(&state.current_config, "current_config");
    let has_config = config_lock.is_some();
    drop(config_lock); // Release lock before calling bridge methods

    if has_config {
        // Get the last config path from settings
        if let Some(config_path) = settings::get_last_config_path() {
            info!(
                "Sending previously loaded configuration to Python executor: {}",
                config_path
            );

            // Send debug settings first
            let debug_settings = settings::get_debug_settings();
            if let Err(e) = bridge.set_debug_settings(
                debug_settings.enable_image_debug,
                debug_settings.top_matches_count,
            ) {
                warn!("Failed to send debug settings: {}", e);
            }

            // Send configuration
            if let Err(e) = bridge.load_configuration(&config_path) {
                error!("Failed to send configuration to Python executor: {}", e);
                // Don't fail the start operation, just warn
                warn!("Python executor started but configuration could not be sent");
            } else {
                info!("Configuration sent to Python executor successfully");
            }
        }
    }

    *bridge_lock = Some(bridge);
    info!("Python executor started successfully");

    Ok(CommandResponse {
        success: true,
        message: Some("Python executor started successfully".to_string()),
        data: None,
    })
}

/// Stop the Python executor.
///
/// Gracefully shuts down the Python process and clears the bridge instance.
///
/// This command is intentionally not exposed in the UI. The executor auto-starts
/// on app launch and stays running between workflow runs. Users control workflows
/// via `start_execution`/`stop_execution`, not the underlying executor.
///
/// This command exists for:
/// - Internal use during app shutdown
/// - Recovery scenarios (stuck executor)
/// - MCP API access for debugging
///
/// Normal users should never need to stop the executor directly - if it's broken,
/// restarting the app is simpler than managing executor lifecycle manually.
///
/// # Arguments
/// * `state` - Application state containing the Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if executor fails to stop
#[tauri::command]
pub fn stop_python_executor(state: State<Arc<AppState>>) -> Result<CommandResponse, String> {
    info!("Stopping Python executor");
    let mut bridge_lock =
        crate::safe_lock::safe_lock_or_recover(&state.python_bridge, "python_bridge");

    if let Some(ref mut bridge) = *bridge_lock {
        bridge.stop().map_err(|e| {
            error!("Failed to stop Python executor: {}", e);
            format!("Failed to stop Python executor: {}", e)
        })?;
        info!("Python executor stopped successfully");
    }

    *bridge_lock = None;

    Ok(CommandResponse {
        success: true,
        message: Some("Python executor stopped".to_string()),
        data: None,
    })
}

/// Update screenshot capture settings.
///
/// Sends new capture settings to the Python executor.
///
/// # Arguments
/// * `settings` - The new screenshot capture settings
/// * `state` - Application state containing the Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if executor not running or update fails
#[tauri::command]
pub fn update_capture_settings(
    settings: ScreenshotCaptureSettings,
    state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!(
        "Updating screenshot capture settings: enabled={}",
        settings.enabled
    );
    let bridge_lock = crate::safe_lock::safe_lock_or_recover(&state.python_bridge, "python_bridge");
    if let Some(ref bridge) = *bridge_lock {
        if !bridge.is_running() {
            return Err("Python executor is not running. Please start the executor first by clicking 'Start Executor' in the Control tab.".to_string());
        }
        bridge.update_capture_settings(settings).map_err(|e| {
            error!("Failed to update capture settings: {}", e);
            format!("Failed to update capture settings: {}", e)
        })?;
        Ok(CommandResponse {
            success: true,
            message: Some("Capture settings updated".to_string()),
            data: None,
        })
    } else {
        Err("Python executor not initialized. Please start the executor first by clicking 'Start Executor' in the Control tab.".to_string())
    }
}
