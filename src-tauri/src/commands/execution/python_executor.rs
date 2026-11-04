//! Python executor lifecycle commands
//!
//! Commands for starting, stopping, and configuring the Python executor.

use crate::config::{ConfigLoader, ScreenshotCaptureSettings};
use crate::executor::{
    get_or_create_default_bridge_compartment, is_default_bridge_running_compartment,
    run_bridge_blocking, stop_default_bridge_compartment, with_default_bridge_compartment,
};
use crate::settings;
use tauri::{Emitter, State};
use tracing::{error, info, warn};

use super::super::compartments::{BridgeCompartment, ExecutionCompartment};
use super::super::CommandResponse;

/// Serializes `start_python_executor` / `stop_python_executor` bodies, with
/// each other and with `POST /executor/restart` (one shared lock,
/// [`crate::executor::bridge_helpers::executor_lifecycle_lock`]).
///
/// While these were sync commands, Tauri ran them on the main thread, which
/// serialized them for free. The frontend calls `start_python_executor` twice
/// at startup; run concurrently on blocking threads, both could see "not
/// running" and race into `get_or_create_default_bridge`, and the loser would
/// fail on the GUI lock instead of answering "already running".
///
/// Only call from a blocking thread (`blocking_lock` panics in async context).
fn executor_lifecycle_guard() -> tokio::sync::MutexGuard<'static, ()> {
    crate::executor::bridge_helpers::executor_lifecycle_lock().blocking_lock()
}

/// Start the Python executor.
///
/// This command:
/// 1. Creates a new Python bridge instance via BridgeManager
/// 2. Starts the Python process
/// 3. Sends previously loaded configuration if available
/// 4. Applies current debug settings
///
/// # Arguments
/// * `app_handle` - Tauri application handle for event emission
/// * `state` - Application state containing the bridge manager
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if executor fails to start or is already running
///
/// `async`, with the body on a blocking thread: as a sync command Tauri ran it
/// on the main thread, where it waited on the bridge behind the boot-time
/// `load_state_machine` round-trip and froze the window for ~45 s (plan
/// `2026-11-04-runner-wedge-detector-reads-a-boot-as-a-wedge-and-never-bound-is-silent`
/// Phase 3). The command name and its JS contract are unchanged.
#[tauri::command]
pub async fn start_python_executor(
    app_handle: tauri::AppHandle,
    bridge: State<'_, BridgeCompartment>,
    execution: State<'_, ExecutionCompartment>,
) -> Result<CommandResponse, String> {
    let bridge = bridge.inner().clone();
    let execution = execution.inner().clone();
    run_bridge_blocking(move || start_python_executor_blocking(app_handle, &bridge, &execution))
        .await?
}

fn start_python_executor_blocking(
    app_handle: tauri::AppHandle,
    bridge: &BridgeCompartment,
    execution: &ExecutionCompartment,
) -> Result<CommandResponse, String> {
    let _serial = executor_lifecycle_guard();
    info!("Starting Python executor");

    // Check if already running
    if is_default_bridge_running_compartment(bridge) {
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
                    match execution.current_config().lock() {
                        Ok(mut guard) => *guard = Some(config),
                        Err(e) => warn!("Failed to store config (mutex poisoned): {}", e),
                    }
                    info!("Configuration stored in app state");

                    // Send debug settings and configuration via bridge manager
                    let debug_settings = settings::get_debug_settings();
                    let config_path_clone = config_path.clone();
                    let reload_result = with_default_bridge_compartment(bridge, |bridge| {
                        // Send debug settings first
                        if let Err(e) = bridge.set_debug_settings(
                            debug_settings.enable_image_debug,
                            debug_settings.top_matches_count,
                        ) {
                            warn!("Failed to send debug settings: {}", e);
                        }

                        // Send configuration to Python
                        if let Err(e) = bridge.load_configuration(&config_path_clone) {
                            warn!("Failed to reload configuration to Python executor: {}", e);
                        } else {
                            info!("Configuration reloaded to Python executor successfully");
                        }
                    });

                    if let Err(e) = reload_result {
                        warn!("Failed to access bridge for config reload: {}", e);
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

    // Create and start new bridge via BridgeManager
    get_or_create_default_bridge_compartment(bridge).map_err(|e| {
        error!("Failed to start Python executor: {}", e);
        format!("Failed to start Python executor: {}", e)
    })?;

    // If a configuration was already loaded, send it to the Python executor
    let config_lock =
        crate::safe_lock::safe_lock_or_recover(execution.current_config(), "current_config");
    let has_config = config_lock.is_some();
    drop(config_lock); // Release lock before calling bridge methods

    if has_config {
        // Get the last config path from settings
        if let Some(config_path) = settings::get_last_config_path() {
            info!(
                "Sending previously loaded configuration to Python executor: {}",
                config_path
            );

            // Send debug settings and configuration via bridge manager
            let debug_settings = settings::get_debug_settings();
            let send_result = with_default_bridge_compartment(bridge, |bridge| {
                // Send debug settings first
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
            });

            if let Err(e) = send_result {
                warn!("Failed to access bridge for config send: {}", e);
            }
        }
    }

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
/// * `state` - Application state containing the bridge manager
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if executor fails to stop
#[tauri::command]
pub async fn stop_python_executor(
    bridge: State<'_, BridgeCompartment>,
) -> Result<CommandResponse, String> {
    let bridge = bridge.inner().clone();
    run_bridge_blocking(move || stop_python_executor_blocking(&bridge)).await?
}

fn stop_python_executor_blocking(bridge: &BridgeCompartment) -> Result<CommandResponse, String> {
    let _serial = executor_lifecycle_guard();
    info!("Stopping Python executor");

    stop_default_bridge_compartment(bridge).map_err(|e| {
        error!("Failed to stop Python executor: {}", e);
        format!("Failed to stop Python executor: {}", e)
    })?;

    info!("Python executor stopped successfully");

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
/// * `state` - Application state containing the bridge manager
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if executor not running or update fails
#[tauri::command]
pub async fn update_capture_settings(
    settings: ScreenshotCaptureSettings,
    bridge: State<'_, BridgeCompartment>,
) -> Result<CommandResponse, String> {
    let bridge = bridge.inner().clone();
    run_bridge_blocking(move || update_capture_settings_blocking(settings, &bridge)).await?
}

fn update_capture_settings_blocking(
    settings: ScreenshotCaptureSettings,
    bridge: &BridgeCompartment,
) -> Result<CommandResponse, String> {
    info!(
        "Updating screenshot capture settings: enabled={}",
        settings.enabled
    );

    if !is_default_bridge_running_compartment(bridge) {
        return Err("Python executor is not running. Please start the executor first by clicking 'Start Executor' in the Control tab.".to_string());
    }

    with_default_bridge_compartment(bridge, |bridge| {
        bridge.update_capture_settings(settings.clone())
    })
    .map_err(|e| {
        error!("Failed to access bridge: {}", e);
        "Python executor not initialized. Please start the executor first by clicking 'Start Executor' in the Control tab.".to_string()
    })?
    .map_err(|e| {
        error!("Failed to update capture settings: {}", e);
        format!("Failed to update capture settings: {}", e)
    })?;

    Ok(CommandResponse {
        success: true,
        message: Some("Capture settings updated".to_string()),
        data: None,
    })
}
