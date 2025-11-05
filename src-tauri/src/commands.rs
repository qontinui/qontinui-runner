use crate::config::{ConfigLoader, QontinuiConfig};
use crate::display::DisplayProcessor;
use crate::error::{AppError, UserFacingError};
use crate::executor::PythonBridge;
use crate::settings;
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex as TokioMutex;
use tracing::{error, info, warn};

pub struct AppState {
    pub python_bridge: Mutex<Option<PythonBridge>>,
    pub current_config: Mutex<Option<QontinuiConfig>>,
    pub display_processor: Arc<TokioMutex<DisplayProcessor>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CommandResponse {
    pub success: bool,
    pub message: Option<String>,
    pub data: Option<serde_json::Value>,
}

#[tauri::command]
pub fn load_configuration(path: String, state: State<AppState>) -> Result<CommandResponse, String> {
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
                info!("Debug settings sent before config load: enable={}, top_matches={}",
                      debug_settings.enable_image_debug, debug_settings.top_matches_count);
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
            info!("Sending previously loaded configuration to Python executor: {}", config_path);

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

#[tauri::command]
pub fn get_executor_status(state: State<AppState>) -> Result<CommandResponse, String> {
    let mut bridge_lock = state.python_bridge.lock().unwrap();

    if let Some(ref mut bridge) = *bridge_lock {
        let is_running = bridge.is_running();

        if is_running {
            bridge
                .get_status()
                .map_err(|e| format!("Failed to get status: {}", e))?;
        }

        Ok(CommandResponse {
            success: true,
            message: None,
            data: Some(serde_json::json!({
                "python_running": is_running,
                "config_loaded": state.current_config.lock().unwrap().is_some()
            })),
        })
    } else {
        Ok(CommandResponse {
            success: true,
            message: None,
            data: Some(serde_json::json!({
                "python_running": false,
                "config_loaded": state.current_config.lock().unwrap().is_some()
            })),
        })
    }
}

#[tauri::command]
pub fn get_current_configuration(state: State<AppState>) -> Result<QontinuiConfig, String> {
    state
        .current_config
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "No configuration loaded".to_string())
}

#[tauri::command]
pub fn handle_error(error: UserFacingError, app_handle: AppHandle) -> Result<(), String> {
    error!("User-facing error: {:?}", error);

    // Emit error event to frontend
    app_handle
        .emit("error", &error)
        .map_err(|e| format!("Failed to emit error event: {}", e))?;

    Ok(())
}

#[tauri::command]
pub fn get_monitors(app_handle: AppHandle) -> Result<CommandResponse, String> {
    info!("Detecting system monitors");

    // Get available monitors from the main window
    let monitors = app_handle
        .get_webview_window("main")
        .ok_or("Failed to get main window")?
        .available_monitors()
        .map_err(|e| format!("Failed to get monitors: {}", e))?;

    let monitor_count = monitors.len();
    let monitor_indices: Vec<i32> = (0..monitor_count as i32).collect();

    info!("Detected {} monitors", monitor_count);

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Detected {} monitors", monitor_count)),
        data: Some(serde_json::json!({
            "count": monitor_count,
            "indices": monitor_indices,
        })),
    })
}

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

#[tauri::command]
pub fn start_recording(
    base_dir: String,
    state: State<AppState>,
) -> Result<CommandResponse, String> {
    info!("Starting recording with base_dir: {}", base_dir);
    let mut bridge_lock = state.python_bridge.lock().unwrap();

    if let Some(ref mut bridge) = *bridge_lock {
        if !bridge.is_running() {
            return Err("Python executor not running".to_string());
        }

        bridge
            .start_recording(&base_dir)
            .map_err(|e| format!("Failed to start recording: {}", e))?;

        Ok(CommandResponse {
            success: true,
            message: Some("Recording start command sent".to_string()),
            data: Some(serde_json::json!({
                "base_dir": base_dir
            })),
        })
    } else {
        Err("Python executor not initialized".to_string())
    }
}

#[tauri::command]
pub fn stop_recording(state: State<AppState>) -> Result<CommandResponse, String> {
    info!("Stopping recording");
    let mut bridge_lock = state.python_bridge.lock().unwrap();

    if let Some(ref mut bridge) = *bridge_lock {
        if !bridge.is_running() {
            return Err("Python executor not running".to_string());
        }

        bridge
            .stop_recording()
            .map_err(|e| format!("Failed to stop recording: {}", e))?;

        Ok(CommandResponse {
            success: true,
            message: Some("Recording stop command sent".to_string()),
            data: None,
        })
    } else {
        Err("Python executor not initialized".to_string())
    }
}

#[tauri::command]
pub fn get_recording_status(state: State<AppState>) -> Result<CommandResponse, String> {
    let mut bridge_lock = state.python_bridge.lock().unwrap();

    if let Some(ref mut bridge) = *bridge_lock {
        if !bridge.is_running() {
            return Ok(CommandResponse {
                success: true,
                message: None,
                data: Some(serde_json::json!({
                    "is_recording": false,
                })),
            });
        }

        bridge
            .get_recording_status()
            .map_err(|e| format!("Failed to get recording status: {}", e))?;

        Ok(CommandResponse {
            success: true,
            message: Some("Recording status command sent".to_string()),
            data: None,
        })
    } else {
        Ok(CommandResponse {
            success: true,
            message: None,
            data: Some(serde_json::json!({
                "is_recording": false,
            })),
        })
    }
}

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

#[tauri::command]
pub fn get_last_config_path() -> Result<CommandResponse, String> {
    info!("Getting last config path");

    if let Some(path) = settings::get_last_config_path() {
        // Check if the file still exists
        if std::path::Path::new(&path).exists() {
            let workflow_id = settings::get_last_workflow_id();
            Ok(CommandResponse {
                success: true,
                message: Some(format!("Last config found: {}", path)),
                data: Some(serde_json::json!({
                    "path": path,
                    "workflow_id": workflow_id
                })),
            })
        } else {
            info!("Last config path exists in settings but file not found: {}", path);
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

#[tauri::command]
pub fn save_auto_load_last_config(enabled: bool) -> Result<CommandResponse, String> {
    info!("Saving auto-load last config setting: {}", enabled);

    settings::save_auto_load_last_config(enabled)
        .map_err(|e| format!("Failed to save auto-load setting: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Auto-load last config {}", if enabled { "enabled" } else { "disabled" })),
        data: None,
    })
}

/// Execute a specific transition in the state machine.
///
/// Sends a command to the Python executor to trigger a transition by ID.
/// The executor will validate the transition is available from the current state(s)
/// and execute any associated actions.
///
/// # Arguments
/// * `state` - The application state containing the Python bridge
/// * `transition_id` - The unique identifier of the transition to execute
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with optional response data from Python
/// * `Err(String)` - Error message if the executor is not running or command fails
#[tauri::command]
pub async fn execute_transition(
    state: tauri::State<'_, AppState>,
    transition_id: String,
) -> Result<CommandResponse, String> {
    info!("Executing transition: {}", transition_id);
    let mut bridge = state.python_bridge.lock().unwrap();

    if let Some(ref mut bridge) = *bridge {
        if !bridge.is_running() {
            return Err("Python executor not running".to_string());
        }

        let params = serde_json::json!({
            "transition_id": transition_id
        });

        bridge
            .send_command("execute_transition", Some(params))
            .map_err(|e| e.to_string())?;

        Ok(CommandResponse {
            success: true,
            message: Some(format!("Transition {} execution command sent", transition_id)),
            data: None,
        })
    } else {
        Err("Python executor not initialized".to_string())
    }
}

/// Navigate to a specific state in the state machine.
///
/// Sends a command to the Python executor to directly navigate to a target state.
/// This bypasses normal transition logic and forces the state machine into the specified state.
///
/// # Arguments
/// * `state` - The application state containing the Python bridge
/// * `state_id` - The unique identifier of the state to navigate to
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with optional response data from Python
/// * `Err(String)` - Error message if the executor is not running or command fails
#[tauri::command]
pub async fn navigate_to_state(
    state: tauri::State<'_, AppState>,
    state_id: String,
) -> Result<CommandResponse, String> {
    info!("Navigating to state: {}", state_id);
    let mut bridge = state.python_bridge.lock().unwrap();

    if let Some(ref mut bridge) = *bridge {
        if !bridge.is_running() {
            return Err("Python executor not running".to_string());
        }

        let params = serde_json::json!({
            "state_id": state_id
        });

        bridge
            .send_command("navigate_to_state", Some(params))
            .map_err(|e| e.to_string())?;

        Ok(CommandResponse {
            success: true,
            message: Some(format!("Navigate to state {} command sent", state_id)),
            data: None,
        })
    } else {
        Err("Python executor not initialized".to_string())
    }
}

/// Navigate to multiple states simultaneously in the state machine.
///
/// Sends a command to the Python executor to activate multiple states at once.
/// This is useful for hierarchical state machines or parallel regions where
/// multiple states can be active simultaneously.
///
/// # Arguments
/// * `state` - The application state containing the Python bridge
/// * `state_ids` - Vector of unique state identifiers to navigate to
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with optional response data from Python
/// * `Err(String)` - Error message if the executor is not running or command fails
#[tauri::command]
pub async fn navigate_to_multiple_states(
    state: tauri::State<'_, AppState>,
    state_ids: Vec<String>,
) -> Result<CommandResponse, String> {
    info!("Navigating to multiple states: {:?}", state_ids);
    let mut bridge = state.python_bridge.lock().unwrap();

    if let Some(ref mut bridge) = *bridge {
        if !bridge.is_running() {
            return Err("Python executor not running".to_string());
        }

        let params = serde_json::json!({
            "state_ids": state_ids
        });

        bridge
            .send_command("navigate_to_multiple_states", Some(params))
            .map_err(|e| e.to_string())?;

        Ok(CommandResponse {
            success: true,
            message: Some(format!("Navigate to {} states command sent", state_ids.len())),
            data: None,
        })
    } else {
        Err("Python executor not initialized".to_string())
    }
}

/// Get the currently active states from the state machine.
///
/// Sends a command to the Python executor to query which states are currently active.
/// The response will be emitted as an event to the frontend with the active state information.
///
/// # Arguments
/// * `state` - The application state containing the Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success, active states will be sent via event
/// * `Err(String)` - Error message if the executor is not running or command fails
#[tauri::command]
pub async fn get_active_states(
    state: tauri::State<'_, AppState>,
) -> Result<CommandResponse, String> {
    info!("Getting active states");
    let mut bridge = state.python_bridge.lock().unwrap();

    if let Some(ref mut bridge) = *bridge {
        if !bridge.is_running() {
            return Err("Python executor not running".to_string());
        }

        let params = serde_json::json!({});

        bridge
            .send_command("get_active_states", Some(params))
            .map_err(|e| e.to_string())?;

        Ok(CommandResponse {
            success: true,
            message: Some("Get active states command sent".to_string()),
            data: None,
        })
    } else {
        Err("Python executor not initialized".to_string())
    }
}

/// Get available transitions from the current state(s).
///
/// Sends a command to the Python executor to query which transitions are currently
/// available based on the active state(s). The response will be emitted as an event
/// to the frontend with the available transition information.
///
/// # Arguments
/// * `state` - The application state containing the Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success, available transitions will be sent via event
/// * `Err(String)` - Error message if the executor is not running or command fails
#[tauri::command]
pub async fn get_available_transitions(
    state: tauri::State<'_, AppState>,
) -> Result<CommandResponse, String> {
    info!("Getting available transitions");
    let mut bridge = state.python_bridge.lock().unwrap();

    if let Some(ref mut bridge) = *bridge {
        if !bridge.is_running() {
            return Err("Python executor not running".to_string());
        }

        let params = serde_json::json!({});

        bridge
            .send_command("get_available_transitions", Some(params))
            .map_err(|e| e.to_string())?;

        Ok(CommandResponse {
            success: true,
            message: Some("Get available transitions command sent".to_string()),
            data: None,
        })
    } else {
        Err("Python executor not initialized".to_string())
    }
}

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
        data: Some(serde_json::to_value(&debug_settings)
            .map_err(|e| format!("Failed to serialize debug settings: {}", e))?),
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

/// Get action log view data from the display processor.
///
/// Returns the current action log view with filtered actions based on the
/// ActionLogProfile configuration.
///
/// # Arguments
/// * `state` - The application state containing the display processor
///
/// # Returns
/// * `Ok(serde_json::Value)` - Action log view data as JSON
/// * `Err(String)` - Error message if view cannot be retrieved
#[tauri::command]
pub async fn get_action_log_view(
    state: State<'_, AppState>,
) -> Result<CommandResponse, String> {
    info!("Getting action log view");

    let processor = state.display_processor.lock().await;
    let view_data = processor.get_view("action_log")
        .map_err(|e| {
            error!("Failed to get action log view: {}", e);
            format!("Failed to get action log view: {}", e)
        })?;

    info!("Action log view retrieved successfully");
    Ok(CommandResponse {
        success: true,
        message: Some("Action log view retrieved".to_string()),
        data: Some(view_data),
    })
}

/// Clear the action log by clearing all events from the display processor.
///
/// This removes all stored events from the EventLog, effectively resetting
/// all display views.
///
/// # Arguments
/// * `state` - The application state containing the display processor
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error message if clear fails
#[tauri::command]
pub async fn clear_action_log(
    state: State<'_, AppState>,
) -> Result<CommandResponse, String> {
    info!("Clearing action log");

    let mut processor = state.display_processor.lock().await;
    processor.clear_events();

    info!("Action log cleared successfully");
    Ok(CommandResponse {
        success: true,
        message: Some("Action log cleared".to_string()),
        data: None,
    })
}
