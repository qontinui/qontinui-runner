//! Execution control commands
//!
//! This module handles all Python executor lifecycle and workflow execution operations:
//! - Starting and stopping the Python executor
//! - Starting and stopping workflow execution
//! - Querying executor status
//! - Monitor detection
//! - System operations (updates, folder opening)

use crate::config::{ConfigLoader, ScreenshotCaptureSettings};
use crate::error::UserFacingError;
use crate::executor::PythonBridge;
use crate::settings;
use std::process::Command;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tracing::{debug, error, info, warn};

use super::{AppState, CommandResponse};

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
    let mut bridge_lock = state.python_bridge.lock().unwrap();

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
                        *state.current_config.lock().unwrap() = Some(config);
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
/// * `monitor_indices` - Array of monitor indices to use (defaults to [0])
/// * `monitor_index` - Legacy single monitor index (deprecated, use monitor_indices)
/// * `initial_state_ids` - Optional override for initial active states (session-only)
/// * `state` - Application state containing the Python bridge
///
/// Note: Monitor offset calculation is handled by the qontinui Python library
/// using MSS, ensuring coordinate consistency with screenshot capture.
///
/// Initial states priority (highest to lowest):
/// 1. `initial_state_ids` parameter (runner override, session-only)
/// 2. Workflow's `initialStateIds` field (workflow-level override)
/// 3. States with `is_initial: true` (state machine defaults)
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if executor not running or workflow ID missing
#[tauri::command]
pub fn start_execution(
    process_id: Option<String>,
    monitor_indices: Option<Vec<i32>>,
    monitor_index: Option<i32>, // Legacy single monitor support
    initial_state_ids: Option<Vec<String>>, // Override for initial active states
    state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    let mut bridge_lock = state.python_bridge.lock().unwrap();

    if let Some(ref mut bridge) = *bridge_lock {
        if !bridge.is_running() {
            return Err("Python executor not running".to_string());
        }

        // Build params
        let mut params = serde_json::Map::new();

        // Resolve monitor indices (prefer array, fall back to legacy single index)
        let resolved_monitors = monitor_indices.unwrap_or_else(|| vec![monitor_index.unwrap_or(0)]);

        // Pass both formats for compatibility
        params.insert(
            "monitor_indices".to_string(),
            serde_json::json!(resolved_monitors),
        );
        // Also pass single monitor_index for backward compatibility with Python
        params.insert(
            "monitor_index".to_string(),
            serde_json::json!(resolved_monitors.first().copied().unwrap_or(0)),
        );
        debug!("Using monitor indices: {:?}", resolved_monitors);

        // Add workflow_id (required)
        let workflow_id = if let Some(pid) = process_id {
            params.insert("workflow_id".to_string(), serde_json::json!(&pid));
            pid
        } else {
            return Err("Workflow ID is required".to_string());
        };

        // Resolve and add initial_state_ids
        // Priority: override param > workflow.initialStateIds > states with is_initial=true
        let config_lock = state.current_config.lock().unwrap();
        let resolved_initial_states =
            resolve_initial_states(config_lock.as_ref(), &workflow_id, initial_state_ids);
        drop(config_lock);

        if !resolved_initial_states.is_empty() {
            params.insert(
                "initial_state_ids".to_string(),
                serde_json::json!(resolved_initial_states),
            );
            debug!("Using initial state IDs: {:?}", resolved_initial_states);
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
pub fn stop_execution(state: State<Arc<AppState>>) -> Result<CommandResponse, String> {
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
pub fn get_executor_status(state: State<Arc<AppState>>) -> Result<CommandResponse, String> {
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
        info!(
            "[GET_EXECUTOR_STATUS] Returning: python_running={}, state={}, config_loaded={}",
            is_running, state_name, config_loaded
        );

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
/// Uses Tauri's window API to get **logical (DPI-scaled) coordinates**.
///
/// Note: This is distinct from `get_screenshot_monitors` in screenshot.rs, which uses
/// qontinui-api for physical pixel coordinates. Use this command for workflow execution
/// monitor selection, and `get_screenshot_monitors` when capturing screenshots at
/// physical resolution.
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
                            "current_version": env!("CARGO_PKG_VERSION"),
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
                            "current_version": env!("CARGO_PKG_VERSION"),
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
                "current_version": env!("CARGO_PKG_VERSION"),
                "development": true,
            })),
        })
    }
}

/// Download and install an available update.
///
/// In release builds, downloads and installs the update, then restarts the application.
/// In debug builds, returns a development mode message.
///
/// # Arguments
/// * `app_handle` - Tauri application handle for updater access
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message (note: app will restart on success)
/// * `Err(String)` - Error if update installation fails
#[tauri::command]
pub async fn install_update(
    #[allow(unused_variables)] app_handle: AppHandle,
) -> Result<CommandResponse, String> {
    info!("Installing update");

    #[cfg(not(debug_assertions))]
    {
        use tauri_plugin_updater::UpdaterExt;

        match app_handle.updater_builder().build() {
            Ok(updater) => match updater.check().await {
                Ok(Some(update)) => {
                    info!("Downloading update version {}", update.version);

                    // Download and install the update
                    match update
                        .download_and_install(|_chunk_length, _content_length| {}, || {})
                        .await
                    {
                        Ok(_) => {
                            info!("Update installed successfully, restarting application");
                            Ok(CommandResponse {
                                success: true,
                                message: Some(
                                    "Update installed. The application will restart.".to_string(),
                                ),
                                data: Some(serde_json::json!({
                                    "installed": true,
                                    "version": update.version.to_string(),
                                })),
                            })
                        }
                        Err(e) => {
                            error!("Failed to install update: {}", e);
                            Err(format!("Failed to install update: {}", e))
                        }
                    }
                }
                Ok(None) => {
                    info!("No update available to install");
                    Ok(CommandResponse {
                        success: false,
                        message: Some("No update available".to_string()),
                        data: Some(serde_json::json!({
                            "installed": false,
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
        info!("Update installation skipped in development mode");
        Ok(CommandResponse {
            success: false,
            message: Some("Updates are disabled in development mode".to_string()),
            data: Some(serde_json::json!({
                "installed": false,
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
    state: State<Arc<AppState>>,
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

/// Get the required screens/monitors for a workflow based on automatic calculation.
///
/// Analyzes the workflow's actions and their associated states to determine
/// which monitors will be used during execution.
///
/// Note: This is a lightweight analysis that happens synchronously. The Python
/// executor performs the analysis and returns the result immediately.
///
/// # Arguments
/// * `workflow_id` - The workflow ID to analyze
/// * `state` - Application state containing the Python bridge and config
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with array of monitor indices in data.screens
/// * `Err(String)` - Error if executor not running or analysis fails
#[tauri::command]
pub fn get_workflow_required_screens(
    workflow_id: String,
    state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Getting required screens for workflow: {}", workflow_id);

    // Get the current config from state
    let config_lock = state
        .current_config
        .lock()
        .expect("current_config mutex poisoned");
    let config = match config_lock.as_ref() {
        Some(c) => c,
        None => {
            return Ok(CommandResponse {
                success: false,
                message: Some("No configuration loaded".to_string()),
                data: None,
            });
        }
    };

    // Find the workflow
    let workflow = match config
        .workflows
        .iter()
        .find(|w| w.get("id").and_then(|id| id.as_str()) == Some(&workflow_id))
    {
        Some(w) => w,
        None => {
            return Ok(CommandResponse {
                success: false,
                message: Some(format!("Workflow '{}' not found", workflow_id)),
                data: None,
            });
        }
    };

    // Collect monitor associations from states
    let mut screens = std::collections::HashSet::new();

    // Build stateimage_id -> monitors mapping (more precise than per-state)
    let mut stateimage_monitors_map: std::collections::HashMap<String, Vec<i32>> =
        std::collections::HashMap::new();
    // Also build region/location/string -> monitors mapping
    let mut element_monitors_map: std::collections::HashMap<String, Vec<i32>> =
        std::collections::HashMap::new();

    for state in &config.states {
        // Check stateImages for monitors field
        if let Some(state_images) = state.get("stateImages").and_then(|si| si.as_array()) {
            for state_image in state_images {
                if let Some(state_image_id) = state_image.get("id").and_then(|id| id.as_str()) {
                    if let Some(monitors) = state_image.get("monitors").and_then(|m| m.as_array()) {
                        let mut image_monitors = Vec::new();
                        for monitor in monitors {
                            if let Some(monitor_idx) = monitor.as_i64() {
                                image_monitors.push(monitor_idx as i32);
                            }
                        }
                        if !image_monitors.is_empty() {
                            image_monitors.sort();
                            image_monitors.dedup();
                            stateimage_monitors_map
                                .insert(state_image_id.to_string(), image_monitors);
                        }
                    }
                }
            }
        }

        // Also check regions, locations, and strings for monitors
        for field_name in &["regions", "locations", "strings"] {
            if let Some(items) = state.get(*field_name).and_then(|f| f.as_array()) {
                for item in items {
                    if let Some(item_id) = item.get("id").and_then(|id| id.as_str()) {
                        if let Some(monitors) = item.get("monitors").and_then(|m| m.as_array()) {
                            let mut item_monitors = Vec::new();
                            for monitor in monitors {
                                if let Some(monitor_idx) = monitor.as_i64() {
                                    item_monitors.push(monitor_idx as i32);
                                }
                            }
                            if !item_monitors.is_empty() {
                                item_monitors.sort();
                                item_monitors.dedup();
                                element_monitors_map.insert(item_id.to_string(), item_monitors);
                            }
                        }
                    }
                }
            }
        }
    }

    // Helper closure to add monitors from a stateimage ID
    let add_stateimage_monitors = |screens: &mut std::collections::HashSet<i32>, id: &str| {
        if let Some(monitors) = stateimage_monitors_map.get(id) {
            for &monitor in monitors {
                screens.insert(monitor);
            }
        }
    };

    // Helper closure to add monitors from an element ID (region/location/string)
    let add_element_monitors = |screens: &mut std::collections::HashSet<i32>, id: &str| {
        if let Some(monitors) = element_monitors_map.get(id) {
            for &monitor in monitors {
                screens.insert(monitor);
            }
        }
    };

    // NOTE: We intentionally skip initialStateIds and GO_TO_STATE for monitor calculation.
    // These represent navigation targets, not the actual elements being interacted with.
    // The monitors should be determined by the specific images/elements used in actions.

    // Analyze actions - collect monitors from specific image/element references
    if let Some(actions) = workflow.get("actions").and_then(|a| a.as_array()) {
        for action in actions {
            if let Some(action_config) = action.get("config") {
                // Check target for imageIds array (handles CLICK, FIND with type "image" or "stateImage")
                if let Some(target) = action_config.get("target") {
                    // Check imageIds array (most common pattern)
                    if let Some(image_ids) = target.get("imageIds").and_then(|ids| ids.as_array()) {
                        for image_id in image_ids {
                            if let Some(id_str) = image_id.as_str() {
                                add_stateimage_monitors(&mut screens, id_str);
                            }
                        }
                    }
                    // Check single imageId field (alternative format)
                    else if let Some(image_id) = target.get("imageId").and_then(|id| id.as_str())
                    {
                        add_stateimage_monitors(&mut screens, image_id);
                    }
                    // Check stateImageId field (used in some FIND actions)
                    else if let Some(state_image_id) =
                        target.get("stateImageId").and_then(|id| id.as_str())
                    {
                        add_stateimage_monitors(&mut screens, state_image_id);
                    }

                    // Check regionId for region-based actions
                    if let Some(region_id) = target.get("regionId").and_then(|id| id.as_str()) {
                        add_element_monitors(&mut screens, region_id);
                    }

                    // Check locationId for location-based actions
                    if let Some(location_id) = target.get("locationId").and_then(|id| id.as_str()) {
                        add_element_monitors(&mut screens, location_id);
                    }
                }
            }
        }
    }

    // NOTE: We intentionally skip transitions for monitor calculation.
    // Transitions are about state navigation, not element interaction.

    // Convert to sorted vec
    let mut screen_list: Vec<i32> = screens.into_iter().collect();
    screen_list.sort();

    // Default to screen 0 if no screens found
    if screen_list.is_empty() {
        screen_list.push(0);
    }

    info!(
        "Workflow '{}' requires screens: {:?} (found {} stateImages with monitor info)",
        workflow_id,
        screen_list,
        stateimage_monitors_map.len()
    );
    debug!(
        "StateImage -> monitors mapping: {:?}",
        stateimage_monitors_map
    );
    debug!("Element -> monitors mapping: {:?}", element_monitors_map);

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "screens": screen_list
        })),
    })
}

/// Enable or disable input capture for coordinate validation.
///
/// When enabled, input events (mouse clicks, keyboard) will be automatically
/// captured during workflow execution. This allows comparing reported click
/// positions with actual captured positions for coordinate validation.
///
/// # Arguments
/// * `enabled` - Whether to enable input capture during execution
/// * `state` - Application state containing the Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with enabled status
/// * `Err(String)` - Error if executor not running
#[tauri::command]
pub fn set_input_capture_enabled(
    enabled: bool,
    state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Setting input capture enabled: {}", enabled);

    let mut bridge_lock = state.python_bridge.lock().unwrap();
    if let Some(ref mut bridge) = *bridge_lock {
        if !bridge.is_running() {
            return Err("Python executor is not running".to_string());
        }

        let params = serde_json::json!({
            "enabled": enabled,
        });

        bridge
            .send_command("set_input_capture_enabled", Some(params))
            .map_err(|e| format!("Failed to set input capture: {}", e))?;

        Ok(CommandResponse {
            success: true,
            message: Some(format!(
                "Input capture {}",
                if enabled { "enabled" } else { "disabled" }
            )),
            data: Some(serde_json::json!({ "enabled": enabled })),
        })
    } else {
        Err("Python executor not initialized".to_string())
    }
}

/// Get input validation capture status.
///
/// # Arguments
/// * `state` - Application state containing the Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with is_monitoring, events_count, session_id
/// * `Err(String)` - Error if executor not running
#[tauri::command]
pub fn get_input_validation_status(state: State<Arc<AppState>>) -> Result<CommandResponse, String> {
    let mut bridge_lock = state.python_bridge.lock().unwrap();
    if let Some(ref mut bridge) = *bridge_lock {
        if !bridge.is_running() {
            return Ok(CommandResponse {
                success: true,
                message: None,
                data: Some(serde_json::json!({
                    "is_monitoring": false,
                    "events_count": 0
                })),
            });
        }

        bridge
            .send_command("get_input_validation_status", None)
            .map_err(|e| format!("Failed to get input validation status: {}", e))?;

        // Note: The actual response comes from Python asynchronously
        // For now, just confirm the command was sent
        Ok(CommandResponse {
            success: true,
            message: Some("Status query sent".to_string()),
            data: None,
        })
    } else {
        Ok(CommandResponse {
            success: true,
            message: None,
            data: Some(serde_json::json!({
                "is_monitoring": false,
                "events_count": 0
            })),
        })
    }
}

/// Resolve initial state IDs with priority system.
///
/// Priority (highest to lowest):
/// 1. Override IDs passed directly (runner session override)
/// 2. Workflow's `initialStateIds` field (workflow-level configuration)
/// 3. States with `is_initial: true` (state machine defaults)
///
/// # Arguments
/// * `config` - Optional reference to loaded configuration
/// * `workflow_id` - The workflow ID to resolve initial states for
/// * `override_ids` - Optional override from runner UI (highest priority)
///
/// # Returns
/// Vector of state IDs to use as initial states (may be empty)
fn resolve_initial_states(
    config: Option<&crate::config::QontinuiConfig>,
    workflow_id: &str,
    override_ids: Option<Vec<String>>,
) -> Vec<String> {
    // Priority 1: Use override if provided
    if let Some(ids) = override_ids {
        if !ids.is_empty() {
            debug!("Using override initial states: {:?}", ids);
            return ids;
        }
    }

    // Need config for remaining priorities
    let config = match config {
        Some(c) => c,
        None => {
            debug!("No config loaded, returning empty initial states");
            return Vec::new();
        }
    };

    // Priority 2: Check workflow.initialStateIds
    if let Some(workflow) = config
        .workflows
        .iter()
        .find(|w| w.get("id").and_then(|id| id.as_str()) == Some(workflow_id))
    {
        if let Some(initial_ids) = workflow
            .get("initialStateIds")
            .and_then(|ids| ids.as_array())
        {
            let ids: Vec<String> = initial_ids
                .iter()
                .filter_map(|id| id.as_str().map(String::from))
                .collect();
            if !ids.is_empty() {
                debug!("Using workflow initial states: {:?}", ids);
                return ids;
            }
        }
    }

    // Priority 3: Fall back to states with is_initial=true (or isInitial for JSON compat)
    let default_ids: Vec<String> = config
        .states
        .iter()
        .filter(|s| {
            s.get("isInitial")
                .or_else(|| s.get("is_initial"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .filter_map(|s| s.get("id").and_then(|id| id.as_str()).map(String::from))
        .collect();

    if !default_ids.is_empty() {
        debug!(
            "Using default initial states (is_initial=true): {:?}",
            default_ids
        );
    }

    default_ids
}

/// Determine the source of resolved initial states.
///
/// Returns the source type: "override", "workflow", or "defaults"
fn get_initial_states_source(
    config: Option<&crate::config::QontinuiConfig>,
    workflow_id: &str,
    override_ids: &Option<Vec<String>>,
) -> &'static str {
    // Check override first
    if let Some(ids) = override_ids {
        if !ids.is_empty() {
            return "override";
        }
    }

    // Need config for remaining checks
    let config = match config {
        Some(c) => c,
        None => return "defaults",
    };

    // Check workflow.initialStateIds
    if let Some(workflow) = config
        .workflows
        .iter()
        .find(|w| w.get("id").and_then(|id| id.as_str()) == Some(workflow_id))
    {
        if let Some(initial_ids) = workflow
            .get("initialStateIds")
            .and_then(|ids| ids.as_array())
        {
            if !initial_ids.is_empty() {
                return "workflow";
            }
        }
    }

    "defaults"
}

/// Get the resolved initial states for a workflow.
///
/// Returns the initial states that would be used for execution, along with
/// the source of those states (defaults, workflow, or override).
///
/// This is useful for the UI to display current initial states before execution
/// without actually starting a workflow.
///
/// # Arguments
/// * `workflow_id` - The workflow ID to get initial states for
/// * `state` - Application state containing the config
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with stateIds array and source
/// * `Err(String)` - Error if query fails
#[tauri::command]
pub fn get_resolved_initial_states(
    workflow_id: String,
    state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    debug!(
        "Getting resolved initial states for workflow: {}",
        workflow_id
    );

    let config_lock = state.current_config.lock().unwrap();
    let config = config_lock.as_ref();

    // Get resolved states (without override, since this is for display)
    let state_ids = resolve_initial_states(config, &workflow_id, None);
    let source = get_initial_states_source(config, &workflow_id, &None);

    // Also get state names for better UI display
    let state_names: Vec<serde_json::Value> = if let Some(cfg) = config {
        state_ids
            .iter()
            .map(|id| {
                let name = cfg
                    .states
                    .iter()
                    .find(|s| s.get("id").and_then(|i| i.as_str()) == Some(id))
                    .and_then(|s| s.get("name").and_then(|n| n.as_str()))
                    .unwrap_or(id);
                serde_json::json!({
                    "id": id,
                    "name": name
                })
            })
            .collect()
    } else {
        state_ids
            .iter()
            .map(|id| serde_json::json!({ "id": id, "name": id }))
            .collect()
    };

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "stateIds": state_ids,
            "source": source,
            "states": state_names,
            "workflowId": workflow_id
        })),
    })
}
