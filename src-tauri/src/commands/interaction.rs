//! Interaction recording commands
//!
//! This module handles interaction recording for State Machine creation:
//! - Starting and stopping interaction recording (video + input capture)
//! - Querying recording status
//! - Managing recording sessions
//!
//! Interaction recording captures user mouse and keyboard input along with
//! screen video to enable creating State Machines from user interactions.
//! This complements Web Extraction which is used for creating State Machines
//! from websites.

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tauri::State;
use tracing::{error, info};

use super::{AppState, CommandResponse};
use crate::executor::{is_default_bridge_running, with_default_bridge};

/// Configuration for interaction recording
#[derive(Debug, Serialize, Deserialize)]
pub struct InteractionRecordingConfig {
    /// Optional session ID (auto-generated if not provided)
    pub session_id: Option<String>,
    /// Frames per second for video recording (default: 30)
    pub fps: Option<u32>,
    /// Optional output directory (defaults to .dev-logs/interactions)
    pub output_dir: Option<String>,
}

/// Status response for interaction recording
#[derive(Debug, Serialize, Deserialize)]
pub struct InteractionRecordingStatus {
    pub is_recording: bool,
    pub session_id: Option<String>,
    pub duration: Option<f64>,
    pub events_count: Option<u64>,
    pub fps: Option<u32>,
}

/// Start interaction recording (video + input capture).
///
/// This captures user mouse and keyboard input along with screen video
/// for use in creating State Machines from user interactions.
///
/// # Arguments
/// * `config` - Recording configuration (session_id, fps, output_dir)
/// * `state` - Application state containing the Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with session_id and output_dir
/// * `Err(String)` - Error if recording fails to start
#[tauri::command]
pub fn start_interaction_recording(
    config: InteractionRecordingConfig,
    state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!(
        "Starting interaction recording: session_id={:?}, fps={:?}",
        config.session_id, config.fps
    );

    if !is_default_bridge_running(&state) {
        return Err("Python executor is not running. Please start the executor first.".to_string());
    }

    let params = json!({
        "session_id": config.session_id,
        "fps": config.fps.unwrap_or(30),
        "output_dir": config.output_dir,
    });

    with_default_bridge(&state, |bridge| {
        bridge
            .send_command("start_interaction_recording", Some(params.clone()))
            .map_err(|e| {
                error!("Failed to start interaction recording: {}", e);
                format!("Failed to start interaction recording: {}", e)
            })
    })??;

    Ok(CommandResponse {
        success: true,
        message: Some("Interaction recording started".to_string()),
        data: None,
    })
}

/// Stop interaction recording and save the captured data.
///
/// Stops the current recording session and returns paths to the saved files.
///
/// # Arguments
/// * `state` - Application state containing the Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with file paths and event count
/// * `Err(String)` - Error if recording fails to stop
#[tauri::command]
pub fn stop_interaction_recording(state: State<Arc<AppState>>) -> Result<CommandResponse, String> {
    info!("Stopping interaction recording");

    if !is_default_bridge_running(&state) {
        return Err("Python executor is not running. Please start the executor first.".to_string());
    }

    with_default_bridge(&state, |bridge| {
        bridge
            .send_command("stop_interaction_recording", None)
            .map_err(|e| {
                error!("Failed to stop interaction recording: {}", e);
                format!("Failed to stop interaction recording: {}", e)
            })
    })??;

    Ok(CommandResponse {
        success: true,
        message: Some("Interaction recording stopped".to_string()),
        data: None,
    })
}

/// Get the current interaction recording status.
///
/// Returns whether recording is active and current session details.
///
/// # Arguments
/// * `state` - Application state containing the Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with status data
/// * `Err(String)` - Error if status query fails
#[tauri::command]
pub fn get_interaction_recording_status(
    state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    // Return not recording if executor is not running
    if !is_default_bridge_running(&state) {
        return Ok(CommandResponse {
            success: true,
            message: None,
            data: Some(json!({
                "is_recording": false,
            })),
        });
    }

    with_default_bridge(&state, |bridge| {
        bridge
            .send_command("get_interaction_recording_status", None)
            .map_err(|e| {
                error!("Failed to get interaction recording status: {}", e);
                format!("Failed to get interaction recording status: {}", e)
            })
    })??;

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(json!({
            "is_recording": false,
        })),
    })
}
