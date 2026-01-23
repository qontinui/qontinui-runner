//! WebSocket integration commands
//!
//! This module handles WebSocket connectivity for remote monitoring and control:
//! - Configuring WebSocket connection parameters
//! - Connecting and disconnecting from WebSocket servers
//! - Managing project association

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;
use tracing::{error, info};

use super::{AppState, CommandResponse};

/// WebSocket configuration structure
#[derive(Debug, Serialize, Deserialize)]
pub struct WebSocketConfig {
    pub enabled: bool,
    pub url: String,
    pub token: String,
    /// Project ID as UUID string (e.g., "fb93478d-98bd-4e40-99f4-0f2c08c1fd5a")
    pub project_id: Option<String>,
    /// Custom user-defined name for this runner (e.g., "My Laptop")
    pub runner_name: Option<String>,
}

/// Configure WebSocket connection parameters.
///
/// Sets up the WebSocket configuration including URL, authentication token,
/// optional project ID, and optional runner name for the Python executor.
///
/// # Arguments
/// * `config` - WebSocket configuration parameters
/// * `state` - Application state containing the Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if executor not running or configuration fails
#[tauri::command]
pub fn configure_websocket(
    config: WebSocketConfig,
    state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!(
        "Configuring WebSocket: enabled={}, url={}, runner_name={:?}",
        config.enabled, config.url, config.runner_name
    );

    let mut bridge_lock =
        crate::safe_lock::safe_lock_or_recover(&state.python_bridge, "python_bridge");
    if let Some(ref mut bridge) = *bridge_lock {
        if !bridge.is_running() {
            return Err(
                "Python executor is not running. Please start the executor first.".to_string(),
            );
        }

        bridge
            .configure_websocket(
                config.enabled,
                config.url,
                config.token,
                config.project_id,
                config.runner_name,
            )
            .map_err(|e| {
                error!("Failed to configure WebSocket: {}", e);
                format!("Failed to configure WebSocket: {}", e)
            })?;

        Ok(CommandResponse {
            success: true,
            message: Some("WebSocket configured successfully".to_string()),
            data: None,
        })
    } else {
        Err("Python executor not initialized. Please start the executor first.".to_string())
    }
}

/// Connect to the configured WebSocket server.
///
/// Initiates a WebSocket connection using previously configured parameters.
///
/// # Arguments
/// * `state` - Application state containing the Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if executor not running or connection fails
#[tauri::command]
pub fn connect_websocket(state: State<Arc<AppState>>) -> Result<CommandResponse, String> {
    info!("Connecting WebSocket");

    let mut bridge_lock =
        crate::safe_lock::safe_lock_or_recover(&state.python_bridge, "python_bridge");
    if let Some(ref mut bridge) = *bridge_lock {
        if !bridge.is_running() {
            return Err(
                "Python executor is not running. Please start the executor first.".to_string(),
            );
        }

        bridge.connect_websocket().map_err(|e| {
            error!("Failed to connect WebSocket: {}", e);
            format!("Failed to connect WebSocket: {}", e)
        })?;

        Ok(CommandResponse {
            success: true,
            message: Some("WebSocket connection initiated".to_string()),
            data: None,
        })
    } else {
        Err("Python executor not initialized. Please start the executor first.".to_string())
    }
}

/// Disconnect from the WebSocket server.
///
/// Gracefully closes the WebSocket connection.
///
/// # Arguments
/// * `state` - Application state containing the Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if executor not running or disconnection fails
#[tauri::command]
pub fn disconnect_websocket(state: State<Arc<AppState>>) -> Result<CommandResponse, String> {
    info!("Disconnecting WebSocket");

    let mut bridge_lock =
        crate::safe_lock::safe_lock_or_recover(&state.python_bridge, "python_bridge");
    if let Some(ref mut bridge) = *bridge_lock {
        if !bridge.is_running() {
            return Err(
                "Python executor is not running. Please start the executor first.".to_string(),
            );
        }

        bridge.disconnect_websocket().map_err(|e| {
            error!("Failed to disconnect WebSocket: {}", e);
            format!("Failed to disconnect WebSocket: {}", e)
        })?;

        Ok(CommandResponse {
            success: true,
            message: Some("WebSocket disconnection initiated".to_string()),
            data: None,
        })
    } else {
        Err("Python executor not initialized. Please start the executor first.".to_string())
    }
}
