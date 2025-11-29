//! Execution control commands
//!
//! This module handles all Python executor lifecycle and workflow execution operations:
//! - Starting and stopping the Python executor
//! - Starting and stopping workflow execution
//! - Querying executor status
//! - Monitor detection
//! - System operations (updates, folder opening)

use crate::config::ScreenshotCaptureSettings;
use crate::error::UserFacingError;
use crate::executor::PythonBridge;
use crate::settings;
use serde_json;
use std::process::Command;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};
use tracing::{debug, error, info, warn};

use super::{AppState, CommandResponse};

/// Start the Python executor.
///
/// This command:
/// 1. Creates a new Python bridge instance
/// 2. Starts the Python process in "real" mode
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
    state: State<AppState>,
) -> Result<CommandResponse, String> {
    info!("Starting Python executor");
    let mut bridge_lock = state.python_bridge.lock().unwrap();

    // Check if already running
    if let Some(ref bridge) = *bridge_lock {
        if bridge.is_running() {
            warn!("Attempt to start Python executor but it's already running");
            return Ok(CommandResponse {
                success: false,
                message: Some("Python executor already running".to_string()),
                data: None,
            });
        }
    }

    // Create and start new bridge (always uses real mode)
    let mut bridge = PythonBridge::new(app_handle);
    bridge.start_with_executor("real").map_err(|e| {
        error!("Failed to start Python executor: {}", e);
        format!("Failed to start Python executor: {}", e)
    })?;

    // If a configuration was already loaded, send it to the Python executor
    let config_lock = state.current_config.lock().unwrap();
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
/// # Arguments
/// * `state` - Application state containing the Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if executor fails to stop
#[tauri::command]
pub fn stop_python_executor(state: State<AppState>) -> Result<CommandResponse, String> {
    info!("Stopping Python executor");
    let mut bridge_lock = state.python_bridge.lock().unwrap();

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

/// Start workflow execution.
///
/// Begins executing a workflow with the specified parameters.
///
/// # Arguments
/// * `process_id` - The workflow ID to execute (required)
/// * `monitor_index` - The monitor to capture (defaults to 0)
/// * `state` - Application state containing the Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if executor not running or workflow ID missing
#[tauri::command]
pub fn start_execution(
    process_id: Option<String>,
    monitor_index: Option<i32>,
    state: State<AppState>,
) -> Result<CommandResponse, String> {
    let mut bridge_lock = state.python_bridge.lock().unwrap();

    if let Some(ref mut bridge) = *bridge_lock {
        if !bridge.is_running() {
            return Err("Python executor not running".to_string());
        }

        // Build params
        let mut params = serde_json::Map::new();

        // Add monitor index (default to 0 if not provided)
        params.insert(
            "monitor_index".to_string(),
            serde_json::json!(monitor_index.unwrap_or(0)),
        );

        // Add workflow_id (required)
        if let Some(pid) = process_id {
            params.insert("workflow_id".to_string(), serde_json::json!(pid));
        } else {
            return Err("Workflow ID is required".to_string());
        }

        bridge
            .start_execution_with_params(Some(serde_json::Value::Object(params)))
            .map_err(|e| format!("Failed to start execution: {}", e))?;

        Ok(CommandResponse {
            success: true,
            message: Some("Execution started".to_string()),
            data: None,
        })
    } else {
        Err("Python executor not initialized".to_string())
    }
}

/// Stop the current workflow execution.
///
/// Stops any running workflow but keeps the executor active.
///
/// # Arguments
/// * `state` - Application state containing the Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if executor not initialized or stop fails
#[tauri::command]
pub fn stop_execution(state: State<AppState>) -> Result<CommandResponse, String> {
    let mut bridge_lock = state.python_bridge.lock().unwrap();

    if let Some(ref mut bridge) = *bridge_lock {
        bridge
            .stop_execution()
            .map_err(|e| format!("Failed to stop execution: {}", e))?;

        Ok(CommandResponse {
            success: true,
            message: Some("Execution stopped".to_string()),
            data: None,
        })
    } else {
        Err("Python executor not initialized".to_string())
    }
}

/// Get the executor status.
///
/// Returns whether the Python executor is running and if a configuration is loaded.
///
/// # Arguments
/// * `state` - Application state containing the Python bridge and config
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with status data
/// * `Err(String)` - Error if status query fails
#[tauri::command]
pub fn get_executor_status(state: State<AppState>) -> Result<CommandResponse, String> {
    debug!("[GET_EXECUTOR_STATUS] Called - checking bridge lock...");
    let mut bridge_lock = state.python_bridge.lock().unwrap();
    debug!("[GET_EXECUTOR_STATUS] Got bridge lock");

    if let Some(ref mut bridge) = *bridge_lock {
        debug!("[GET_EXECUTOR_STATUS] Bridge exists, checking is_running()...");
        let is_running = bridge.is_running();
        debug!("[GET_EXECUTOR_STATUS] is_running() = {}", is_running);

        // Also get the actual state name for debugging
        let state_name = bridge.get_state().name();
        debug!("[GET_EXECUTOR_STATUS] Current state: {}", state_name);

        if is_running {
            debug!("[GET_EXECUTOR_STATUS] Calling bridge.get_status()...");
            bridge
                .get_status()
                .map_err(|e| format!("Failed to get status: {}", e))?;
            debug!("[GET_EXECUTOR_STATUS] get_status() completed");
        }

        let config_loaded = state.current_config.lock().unwrap().is_some();
        debug!("[GET_EXECUTOR_STATUS] config_loaded = {}", config_loaded);
        info!("[GET_EXECUTOR_STATUS] Returning: python_running={}, state={}, config_loaded={}",
              is_running, state_name, config_loaded);

        Ok(CommandResponse {
            success: true,
            message: None,
            data: Some(serde_json::json!({
                "python_running": is_running,
                "executor_state": state_name,
                "config_loaded": config_loaded
            })),
        })
    } else {
        debug!("[GET_EXECUTOR_STATUS] Bridge is None - Python not started");
        info!("[GET_EXECUTOR_STATUS] Returning: python_running=false (no bridge)");
        Ok(CommandResponse {
            success: true,
            message: None,
            data: Some(serde_json::json!({
                "python_running": false,
                "executor_state": "not_started",
                "config_loaded": state.current_config.lock().unwrap().is_some()
            })),
        })
    }
}

/// Detect and return information about system monitors.
///
/// Returns details about all connected monitors including position, size, and primary status.
///
/// # Arguments
/// * `app_handle` - Tauri application handle for window access
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with monitor details
/// * `Err(String)` - Error if monitors cannot be detected
#[tauri::command]
pub fn get_monitors(app_handle: AppHandle) -> Result<CommandResponse, String> {
    info!("Detecting system monitors");

    let window = app_handle
        .get_webview_window("main")
        .ok_or("Failed to get main window")?;

    // Get available monitors
    let monitors = window
        .available_monitors()
        .map_err(|e| format!("Failed to get monitors: {}", e))?;

    // Get current monitor (primary)
    let current_monitor = window
        .current_monitor()
        .map_err(|e| format!("Failed to get current monitor: {}", e))?;

    let monitor_count = monitors.len();

    // Build detailed monitor info
    let monitor_details: Vec<serde_json::Value> = monitors
        .iter()
        .enumerate()
        .map(|(idx, monitor)| {
            let position = monitor.position();
            let size = monitor.size();
            let is_primary = if let Some(ref current) = current_monitor {
                // Check if this is the primary monitor by comparing position and size
                position.x == current.position().x
                    && position.y == current.position().y
                    && size.width == current.size().width
                    && size.height == current.size().height
            } else {
                idx == 0 // fallback to first monitor
            };

            serde_json::json!({
                "index": idx,
                "x": position.x,
                "y": position.y,
                "width": size.width,
                "height": size.height,
                "is_primary": is_primary,
            })
        })
        .collect();

    info!("Detected {} monitors", monitor_count);

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Detected {} monitors", monitor_count)),
        data: Some(serde_json::json!({
            "count": monitor_count,
            "indices": (0..monitor_count as i32).collect::<Vec<i32>>(),
            "monitors": monitor_details,
        })),
    })
}

/// Handle and emit user-facing errors.
///
/// Logs the error and emits it to the frontend for display.
///
/// # Arguments
/// * `error` - The user-facing error to handle
/// * `app_handle` - Tauri application handle for event emission
///
/// # Returns
/// * `Ok(())` - Success
/// * `Err(String)` - Error if event emission fails
#[tauri::command]
pub fn handle_error(error: UserFacingError, app_handle: AppHandle) -> Result<(), String> {
    error!("User-facing error: {:?}", error);

    // Emit error event to frontend
    app_handle
        .emit("error", &error)
        .map_err(|e| format!("Failed to emit error event: {}", e))?;

    Ok(())
}

/// Check for application updates.
///
/// In release builds, checks for available updates via Tauri updater.
/// In debug builds, returns a development mode message.
///
/// # Arguments
/// * `app_handle` - Tauri application handle for updater access
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with update availability information
/// * `Err(String)` - Error if update check fails
#[tauri::command]
pub async fn check_for_updates(
    #[allow(unused_variables)] app_handle: AppHandle,
) -> Result<CommandResponse, String> {
    info!("Checking for updates");

    #[cfg(not(debug_assertions))]
    {
        use tauri_plugin_updater::UpdaterExt;

        match app_handle.updater_builder().build() {
            Ok(updater) => match updater.check().await {
                Ok(Some(update)) => {
                    info!("Update available: {}", update.version);
                    Ok(CommandResponse {
                        success: true,
                        message: Some(format!("Update available: {}", update.version)),
                        data: Some(serde_json::json!({
                            "available": true,
                            "version": update.version.to_string(),
                            "notes": update.body,
                        })),
                    })
                }
                Ok(None) => {
                    info!("No updates available");
                    Ok(CommandResponse {
                        success: true,
                        message: Some("No updates available".to_string()),
                        data: Some(serde_json::json!({
                            "available": false,
                        })),
                    })
                }
                Err(e) => {
                    error!("Failed to check for updates: {}", e);
                    Err(format!("Failed to check for updates: {}", e))
                }
            },
            Err(e) => {
                error!("Failed to build updater: {}", e);
                Err(format!("Failed to build updater: {}", e))
            }
        }
    }

    #[cfg(debug_assertions)]
    {
        info!("Update check skipped in development mode");
        Ok(CommandResponse {
            success: true,
            message: Some("Update check disabled in development".to_string()),
            data: Some(serde_json::json!({
                "available": false,
                "development": true,
            })),
        })
    }
}

/// Open a folder in the system file explorer.
///
/// Uses platform-specific commands (explorer/open/xdg-open).
///
/// # Arguments
/// * `path` - The folder path to open
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if path doesn't exist or open fails
#[tauri::command]
pub fn open_folder(path: String) -> Result<CommandResponse, String> {
    info!("Opening folder: {}", path);

    // Check if path exists
    if !std::path::Path::new(&path).exists() {
        return Err(format!("Path does not exist: {}", path));
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("explorer")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Opened folder: {}", path)),
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
    state: State<AppState>,
) -> Result<CommandResponse, String> {
    info!(
        "Updating screenshot capture settings: enabled={}",
        settings.enabled
    );
    let bridge_lock = state.python_bridge.lock().unwrap();
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
