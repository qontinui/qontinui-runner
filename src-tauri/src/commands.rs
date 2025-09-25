use crate::config::{ConfigLoader, QontinuiConfig};
use crate::error::{AppError, UserFacingError};
use crate::executor::PythonBridge;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, State};
use tracing::{error, info, warn};

pub struct AppState {
    pub python_bridge: Mutex<Option<PythonBridge>>,
    pub current_config: Mutex<Option<QontinuiConfig>>,
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
        "processes": config.processes.clone(),
        "states": config.states.clone(),
        "transitions": config.transitions.clone()
    });

    // Store the configuration
    *state.current_config.lock().unwrap() = Some(config);
    info!("Configuration loaded successfully: {}", summary);

    // If Python bridge is running, send the configuration
    if let Some(ref mut bridge) = *state.python_bridge.lock().unwrap() {
        if bridge.is_running() {
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
    start_python_executor_with_type(app_handle, state, "simple".to_string())
}

#[tauri::command]
pub fn start_python_executor_with_type(
    app_handle: tauri::AppHandle,
    state: State<AppState>,
    executor_type: String,
) -> Result<CommandResponse, String> {
    info!("Starting Python executor with type: {}", executor_type);
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

    // Create and start new bridge with specified executor type
    let mut bridge = PythonBridge::new(app_handle);
    bridge.start_with_executor(&executor_type).map_err(|e| {
        error!("Failed to start Python executor: {}", e);
        format!("Failed to start Python executor: {}", e)
    })?;

    *bridge_lock = Some(bridge);
    info!(
        "Python executor started successfully in {} mode",
        executor_type
    );

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Python executor started with {} mode",
            executor_type
        )),
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
    mode: String,
    process_id: Option<String>,
    monitor_index: Option<i32>,
    state: State<AppState>,
) -> Result<CommandResponse, String> {
    let mut bridge_lock = state.python_bridge.lock().unwrap();

    if let Some(ref mut bridge) = *bridge_lock {
        if !bridge.is_running() {
            return Err("Python executor not running".to_string());
        }

        // Build params based on mode and monitor
        let mut params = serde_json::Map::new();

        // Add monitor index (default to 0 if not provided)
        params.insert(
            "monitor_index".to_string(),
            serde_json::json!(monitor_index.unwrap_or(0)),
        );

        // Add process_id if in process mode
        if mode == "process" {
            if let Some(pid) = process_id {
                params.insert("process_id".to_string(), serde_json::json!(pid));
            } else {
                return Err("Process ID required for process mode".to_string());
            }
        }

        bridge
            .start_execution_with_params(&mode, Some(serde_json::Value::Object(params)))
            .map_err(|e| format!("Failed to start execution: {}", e))?;

        Ok(CommandResponse {
            success: true,
            message: Some(format!("Execution started in {} mode", mode)),
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
pub async fn check_for_updates(_app_handle: AppHandle) -> Result<CommandResponse, String> {
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
