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
/// * `state` - Application state containing the Python bridge
///
/// Note: Monitor offset calculation is handled by the qontinui Python library
/// using MSS, ensuring coordinate consistency with screenshot capture.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if executor not running or workflow ID missing
#[tauri::command]
pub fn start_execution(
    process_id: Option<String>,
    monitor_indices: Option<Vec<i32>>,
    monitor_index: Option<i32>, // Legacy single monitor support
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
    let config_lock = state.current_config.lock().unwrap();
    if config_lock.is_none() {
        return Ok(CommandResponse {
            success: false,
            message: Some("No configuration loaded".to_string()),
            data: None,
        });
    }

    let config = config_lock.as_ref().unwrap();

    // Find the workflow
    let workflow = config
        .workflows
        .iter()
        .find(|w| w.get("id").and_then(|id| id.as_str()) == Some(&workflow_id));

    if workflow.is_none() {
        return Ok(CommandResponse {
            success: false,
            message: Some(format!("Workflow '{}' not found", workflow_id)),
            data: None,
        });
    }

    let workflow = workflow.unwrap();

    // Collect monitor associations from states
    let mut screens = std::collections::HashSet::new();

    // Build state -> monitors mapping by examining stateImages.monitors
    let mut state_monitors_map: std::collections::HashMap<String, Vec<i32>> =
        std::collections::HashMap::new();
    for state in &config.states {
        if let Some(state_id) = state.get("id").and_then(|id| id.as_str()) {
            let mut state_monitors = Vec::new();

            // Check stateImages for monitors field
            if let Some(state_images) = state.get("stateImages").and_then(|si| si.as_array()) {
                for state_image in state_images {
                    if let Some(monitors) = state_image.get("monitors").and_then(|m| m.as_array()) {
                        for monitor in monitors {
                            if let Some(monitor_idx) = monitor.as_i64() {
                                state_monitors.push(monitor_idx as i32);
                            }
                        }
                    }
                }
            }

            // Also check regions, locations, and strings for monitors
            for field_name in &["regions", "locations", "strings"] {
                if let Some(items) = state.get(*field_name).and_then(|f| f.as_array()) {
                    for item in items {
                        if let Some(monitors) = item.get("monitors").and_then(|m| m.as_array()) {
                            for monitor in monitors {
                                if let Some(monitor_idx) = monitor.as_i64() {
                                    state_monitors.push(monitor_idx as i32);
                                }
                            }
                        }
                    }
                }
            }

            if !state_monitors.is_empty() {
                // Deduplicate
                state_monitors.sort();
                state_monitors.dedup();
                state_monitors_map.insert(state_id.to_string(), state_monitors);
            }
        }
    }

    // Check initial states
    if let Some(initial_states) = workflow
        .get("initialStateIds")
        .and_then(|ids| ids.as_array())
    {
        for state_id in initial_states {
            if let Some(state_id_str) = state_id.as_str() {
                if let Some(state_monitors) = state_monitors_map.get(state_id_str) {
                    for &monitor in state_monitors {
                        screens.insert(monitor);
                    }
                }
            }
        }
    }

    // Analyze actions
    if let Some(actions) = workflow.get("actions").and_then(|a| a.as_array()) {
        for action in actions {
            if let Some(action_type) = action.get("type").and_then(|t| t.as_str()) {
                // GO_TO_STATE actions - check stateIds field (can be array)
                if action_type == "GO_TO_STATE" {
                    // Check stateIds array (preferred)
                    if let Some(state_ids) = action
                        .get("config")
                        .and_then(|c| c.get("stateIds"))
                        .and_then(|ids| ids.as_array())
                    {
                        for state_id in state_ids {
                            if let Some(state_id_str) = state_id.as_str() {
                                if let Some(state_monitors) = state_monitors_map.get(state_id_str) {
                                    for &monitor in state_monitors {
                                        screens.insert(monitor);
                                    }
                                }
                            }
                        }
                    }
                    // Legacy: check targetState string
                    else if let Some(target_state) = action
                        .get("config")
                        .and_then(|c| c.get("targetState"))
                        .and_then(|ts| ts.as_str())
                    {
                        if let Some(state_monitors) = state_monitors_map.get(target_state) {
                            for &monitor in state_monitors {
                                screens.insert(monitor);
                            }
                        }
                    }
                }

                // FIND actions may have target with stateImage that specifies monitors
                if action_type == "FIND" || action_type == "RAG_FIND" {
                    if let Some(action_config) = action.get("config") {
                        // Check if target references a stateImage with monitors
                        if let Some(target) = action_config.get("target") {
                            if let Some(target_type) = target.get("type").and_then(|t| t.as_str()) {
                                if target_type == "stateImage" {
                                    // The stateImage may have monitors defined
                                    // Search through states to find the stateImage
                                    if let Some(state_image_id) =
                                        target.get("stateImageId").and_then(|id| id.as_str())
                                    {
                                        // Search all states in the main config for this stateImage
                                        for state in &config.states {
                                            if let Some(state_images) = state
                                                .get("stateImages")
                                                .and_then(|si| si.as_array())
                                            {
                                                for state_image in state_images {
                                                    if state_image
                                                        .get("id")
                                                        .and_then(|id| id.as_str())
                                                        == Some(state_image_id)
                                                    {
                                                        if let Some(monitors) = state_image
                                                            .get("monitors")
                                                            .and_then(|m| m.as_array())
                                                        {
                                                            for monitor in monitors {
                                                                if let Some(monitor_idx) =
                                                                    monitor.as_i64()
                                                                {
                                                                    screens
                                                                        .insert(monitor_idx as i32);
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Also check transitions in the config
    for transition in &config.transitions {
        // Check fromState
        if let Some(from_state) = transition.get("fromState").and_then(|s| s.as_str()) {
            if let Some(state_monitors) = state_monitors_map.get(from_state) {
                for &monitor in state_monitors {
                    screens.insert(monitor);
                }
            }
        }
        // Check toState
        if let Some(to_state) = transition.get("toState").and_then(|s| s.as_str()) {
            if let Some(state_monitors) = state_monitors_map.get(to_state) {
                for &monitor in state_monitors {
                    screens.insert(monitor);
                }
            }
        }
        // Check activateStates array
        if let Some(activate_states) = transition.get("activateStates").and_then(|a| a.as_array()) {
            for activate_state in activate_states {
                if let Some(state_id) = activate_state.as_str() {
                    if let Some(state_monitors) = state_monitors_map.get(state_id) {
                        for &monitor in state_monitors {
                            screens.insert(monitor);
                        }
                    }
                }
            }
        }
    }

    // Convert to sorted vec
    let mut screen_list: Vec<i32> = screens.into_iter().collect();
    screen_list.sort();

    // Default to screen 0 if no screens found
    if screen_list.is_empty() {
        screen_list.push(0);
    }

    info!(
        "Workflow '{}' requires screens: {:?} (found {} states with monitor info)",
        workflow_id,
        screen_list,
        state_monitors_map.len()
    );
    debug!("State -> monitors mapping: {:?}", state_monitors_map);

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
