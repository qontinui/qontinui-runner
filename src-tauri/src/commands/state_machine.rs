//! State machine navigation commands
//!
//! This module handles state navigation and action log operations:
//! - Navigating to a single state
//! - Action log viewing and management

use crate::commands::compartments::{BridgeCompartment, StorageCompartment};
use crate::error::AppError;
use crate::executor::{require_running_bridge_compartment, with_default_bridge_compartment};
use crate::safe_eprintln;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;
use tracing::{error, info};

use super::CommandResponse;

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
    bridge: State<'_, BridgeCompartment>,
    state_id: String,
) -> Result<CommandResponse, String> {
    info!("Navigating to state: {}", state_id);

    require_running_bridge_compartment(&bridge)?;

    let params = serde_json::json!({
        "state_id": state_id
    });

    with_default_bridge_compartment(&bridge, |bridge| {
        bridge.send_command("navigate_to_state", Some(params))
    })??;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Navigate to state {} command sent", state_id)),
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
    storage: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    safe_eprintln!("[DEBUG] get_action_log_view called");
    info!("Getting action log view");

    safe_eprintln!("[DEBUG] Acquiring display_processor lock...");
    let processor = storage.display_processor().lock().await;
    safe_eprintln!("[DEBUG] Got display_processor lock");

    safe_eprintln!("[DEBUG] Calling processor.get_view(\"action_log\")...");
    let view_data = processor.get_view("action_log").map_err(|e| {
        safe_eprintln!("[DEBUG] get_view failed: {}", e);
        error!("Failed to get action log view: {}", e);
        String::from(AppError::StateError(format!(
            "Failed to get action log view: {}",
            e
        )))
    })?;

    safe_eprintln!("[DEBUG] get_view succeeded, view_data: {:?}", view_data);
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
    storage: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    info!("Clearing action log");

    let mut processor = storage.display_processor().lock().await;
    processor.clear_events();

    info!("Action log cleared successfully");
    Ok(CommandResponse {
        success: true,
        message: Some("Action log cleared".to_string()),
        data: None,
    })
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_state_machine")
        .invoke_handler(tauri::generate_handler![
            navigate_to_state,
            get_action_log_view,
            clear_action_log,
        ])
        .build()
}
