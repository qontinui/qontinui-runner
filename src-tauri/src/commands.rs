use crate::config::{ConfigLoader, QontinuiConfig};
use crate::executor::PythonBridge;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::State;

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
pub fn load_configuration(
    path: String,
    state: State<AppState>,
) -> Result<CommandResponse, String> {
    // Load the configuration file
    let config = ConfigLoader::load_from_file(&path)
        .map_err(|e| format!("Failed to load configuration: {}", e))?;

    let summary = config.summary();
    
    // Create data object with configuration info
    let config_data = serde_json::json!({
        "processes": config.processes.clone(),
        "states": config.states.clone(),
        "transitions": config.transitions.clone()
    });

    // Store the configuration
    *state.current_config.lock().unwrap() = Some(config);

    // If Python bridge is running, send the configuration
    if let Some(ref mut bridge) = *state.python_bridge.lock().unwrap() {
        if bridge.is_running() {
            bridge.load_configuration(&path)
                .map_err(|e| format!("Failed to send configuration to Python: {}", e))?;
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
    let mut bridge_lock = state.python_bridge.lock().unwrap();

    // Check if already running
    if let Some(ref bridge) = *bridge_lock {
        if bridge.is_running() {
            return Ok(CommandResponse {
                success: false,
                message: Some("Python executor already running".to_string()),
                data: None,
            });
        }
    }

    // Create and start new bridge with specified executor type
    let mut bridge = PythonBridge::new(app_handle);
    bridge.start_with_executor(&executor_type)
        .map_err(|e| format!("Failed to start Python executor: {}", e))?;

    *bridge_lock = Some(bridge);

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Python executor started with {} mode", executor_type)),
        data: None,
    })
}

#[tauri::command]
pub fn stop_python_executor(state: State<AppState>) -> Result<CommandResponse, String> {
    let mut bridge_lock = state.python_bridge.lock().unwrap();

    if let Some(ref mut bridge) = *bridge_lock {
        bridge.stop()
            .map_err(|e| format!("Failed to stop Python executor: {}", e))?;
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
    state: State<AppState>,
) -> Result<CommandResponse, String> {
    let mut bridge_lock = state.python_bridge.lock().unwrap();

    if let Some(ref mut bridge) = *bridge_lock {
        if !bridge.is_running() {
            return Err("Python executor not running".to_string());
        }

        // Build params based on mode
        let params = if mode == "process" {
            if let Some(pid) = process_id {
                Some(serde_json::json!({
                    "process_id": pid
                }))
            } else {
                return Err("Process ID required for process mode".to_string());
            }
        } else {
            None
        };

        bridge.start_execution_with_params(&mode, params)
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
        bridge.stop_execution()
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
            bridge.get_status()
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