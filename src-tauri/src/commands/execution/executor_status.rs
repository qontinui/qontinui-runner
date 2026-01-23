//! Executor status and monitoring commands
//!
//! Commands for querying executor status, detecting monitors, and managing input capture.

use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
use tracing::{debug, info, warn};

use super::super::{AppState, CommandResponse};

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
    // Use unwrap_or_else to recover from poisoned mutex (after a panic in another thread)
    let mut bridge_lock = state.python_bridge.lock().unwrap_or_else(|poisoned| {
        warn!("[GET_EXECUTOR_STATUS] python_bridge mutex was poisoned, recovering");
        poisoned.into_inner()
    });
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

        let config_loaded = state
            .current_config
            .lock()
            .unwrap_or_else(|poisoned| {
                warn!("[GET_EXECUTOR_STATUS] current_config mutex was poisoned, recovering");
                poisoned.into_inner()
            })
            .is_some();
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
                "config_loaded": crate::safe_lock::safe_lock_or_recover(&state.current_config, "current_config").is_some()
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
/// Python IPC for physical pixel coordinates. Use this command for workflow execution
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

    // Build detailed monitor info with all x positions for position calculation
    let x_positions: Vec<i32> = monitors.iter().map(|m| m.position().x).collect();

    let monitor_details: Vec<serde_json::Value> = monitors
        .iter()
        .enumerate()
        .map(|(idx, monitor)| {
            let mon_position = monitor.position();
            let size = monitor.size();
            let scale_factor = monitor.scale_factor();
            let name = monitor.name().map(|n| n.to_string());
            let is_primary = if let Some(ref current) = current_monitor {
                // Check if this is the primary monitor by comparing position and size
                mon_position.x == current.position().x
                    && mon_position.y == current.position().y
                    && size.width == current.size().width
                    && size.height == current.size().height
            } else {
                idx == 0 // fallback to first monitor
            };

            // Determine spatial position based on X coordinate relative to other monitors
            let position_label = if monitors.len() == 1 {
                "center"
            } else {
                let min_x = x_positions.iter().min().copied().unwrap_or(0);
                let max_x = x_positions.iter().max().copied().unwrap_or(0);
                if mon_position.x == min_x {
                    "left"
                } else if mon_position.x == max_x {
                    "right"
                } else {
                    "center"
                }
            };

            serde_json::json!({
                "index": idx,
                "x": mon_position.x,
                "y": mon_position.y,
                "width": size.width,
                "height": size.height,
                "position": position_label,
                "is_primary": is_primary,
                "scale_factor": scale_factor,
                "name": name,
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

    let mut bridge_lock =
        crate::safe_lock::safe_lock_or_recover(&state.python_bridge, "python_bridge");
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
    let mut bridge_lock =
        crate::safe_lock::safe_lock_or_recover(&state.python_bridge, "python_bridge");
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
