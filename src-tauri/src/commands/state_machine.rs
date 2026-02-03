//! State machine navigation commands
//!
//! This module handles all state machine navigation and query operations:
//! - Executing specific transitions
//! - Navigating to single or multiple states
//! - Querying active states
//! - Getting available transitions
//! - Action log viewing and management

use crate::executor::{require_running_bridge, with_default_bridge};
use crate::safe_eprintln;
use std::sync::Arc;
use tauri::State;
use tracing::{error, info};

use super::{AppState, CommandResponse};

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
    state: State<'_, Arc<AppState>>,
    transition_id: String,
) -> Result<CommandResponse, String> {
    info!("Executing transition: {}", transition_id);

    require_running_bridge(&state)?;

    let params = serde_json::json!({
        "transition_id": transition_id
    });

    with_default_bridge(&state, |bridge| {
        bridge
            .send_command("execute_transition", Some(params))
            .map_err(|e| e.to_string())
    })??;

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Transition {} execution command sent",
            transition_id
        )),
        data: None,
    })
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
    state: State<'_, Arc<AppState>>,
    state_id: String,
) -> Result<CommandResponse, String> {
    info!("Navigating to state: {}", state_id);

    require_running_bridge(&state)?;

    let params = serde_json::json!({
        "state_id": state_id
    });

    with_default_bridge(&state, |bridge| {
        bridge
            .send_command("navigate_to_state", Some(params))
            .map_err(|e| e.to_string())
    })??;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Navigate to state {} command sent", state_id)),
        data: None,
    })
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
    state: State<'_, Arc<AppState>>,
    state_ids: Vec<String>,
) -> Result<CommandResponse, String> {
    info!("Navigating to multiple states: {:?}", state_ids);

    require_running_bridge(&state)?;

    let state_ids_len = state_ids.len();
    let params = serde_json::json!({
        "state_ids": state_ids
    });

    with_default_bridge(&state, |bridge| {
        bridge
            .send_command("navigate_to_multiple_states", Some(params))
            .map_err(|e| e.to_string())
    })??;

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Navigate to {} states command sent",
            state_ids_len
        )),
        data: None,
    })
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
pub async fn get_active_states(state: State<'_, Arc<AppState>>) -> Result<CommandResponse, String> {
    info!("Getting active states");

    require_running_bridge(&state)?;

    let params = serde_json::json!({});

    with_default_bridge(&state, |bridge| {
        bridge
            .send_command("get_active_states", Some(params))
            .map_err(|e| e.to_string())
    })??;

    Ok(CommandResponse {
        success: true,
        message: Some("Get active states command sent".to_string()),
        data: None,
    })
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
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Getting available transitions");

    require_running_bridge(&state)?;

    let params = serde_json::json!({});

    with_default_bridge(&state, |bridge| {
        bridge
            .send_command("get_available_transitions", Some(params))
            .map_err(|e| e.to_string())
    })??;

    Ok(CommandResponse {
        success: true,
        message: Some("Get available transitions command sent".to_string()),
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
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    safe_eprintln!("[DEBUG] get_action_log_view called");
    info!("Getting action log view");

    safe_eprintln!("[DEBUG] Acquiring display_processor lock...");
    let processor = state.display_processor.lock().await;
    safe_eprintln!("[DEBUG] Got display_processor lock");

    safe_eprintln!("[DEBUG] Calling processor.get_view(\"action_log\")...");
    let view_data = processor.get_view("action_log").map_err(|e| {
        safe_eprintln!("[DEBUG] get_view failed: {}", e);
        error!("Failed to get action log view: {}", e);
        format!("Failed to get action log view: {}", e)
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
pub async fn clear_action_log(state: State<'_, Arc<AppState>>) -> Result<CommandResponse, String> {
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
