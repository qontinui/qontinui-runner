//! Video recording commands
//!
//! This module handles video recording operations:
//! - Starting and stopping video recording
//! - Querying recording status
//! - Managing recording sessions

use crate::error::AppError;
use crate::video_recorder::VideoRecordingConfig;
use serde_json;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;
use tracing::{error, info};

use super::compartments::StorageCompartment;
use super::CommandResponse;

// Migrated to StorageCompartment (Workstream C).

/// Start video recording for the current automation session.
///
/// Initiates screen recording with the specified configuration.
///
/// # Arguments
/// * `session_id` - Unique identifier for this recording session
/// * `config` - Video recording configuration (resolution, framerate, etc.)
/// * `state` - Application state containing the video recorder service
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if recording fails to start
#[tauri::command]
pub fn start_video_recording(
    session_id: String,
    config: VideoRecordingConfig,
    state: State<StorageCompartment>,
) -> Result<CommandResponse, String> {
    start_video_recording_impl(session_id, config, &state).map_err(String::from)
}

fn start_video_recording_impl(
    session_id: String,
    config: VideoRecordingConfig,
    state: &StorageCompartment,
) -> Result<CommandResponse, AppError> {
    info!("Starting video recording for session: {}", session_id);

    let recorder = state.video_recorder().lock()?;

    recorder.start_recording(&session_id, config).map_err(|e| {
        error!("Failed to start video recording: {}", e);
        AppError::ExecutorError(format!("Failed to start video recording: {}", e))
    })?;

    Ok(CommandResponse {
        success: true,
        message: Some("Video recording started".to_string()),
        data: None,
    })
}

/// Stop the current video recording.
///
/// Stops the active recording and saves the video file.
///
/// # Arguments
/// * `state` - Application state containing the video recorder service
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with saved video path
/// * `Err(String)` - Error if recording fails to stop
#[tauri::command]
pub fn stop_video_recording(state: State<StorageCompartment>) -> Result<CommandResponse, String> {
    stop_video_recording_impl(&state).map_err(String::from)
}

fn stop_video_recording_impl(state: &StorageCompartment) -> Result<CommandResponse, AppError> {
    info!("Stopping video recording");

    let recorder = state.video_recorder().lock()?;

    let video_path = recorder.stop_recording().map_err(|e| {
        error!("Failed to stop video recording: {}", e);
        AppError::ExecutorError(format!("Failed to stop video recording: {}", e))
    })?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Video saved to: {}", video_path)),
        data: Some(serde_json::json!({
            "path": video_path
        })),
    })
}

/// Get the current video recording status.
///
/// Returns information about whether recording is active and current session details.
///
/// # Arguments
/// * `state` - Application state containing the video recorder service
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with status data
/// * `Err(String)` - Error if status query fails
#[tauri::command]
pub fn get_video_recording_status(
    state: State<StorageCompartment>,
) -> Result<CommandResponse, String> {
    get_video_recording_status_impl(&state).map_err(String::from)
}

fn get_video_recording_status_impl(
    state: &StorageCompartment,
) -> Result<CommandResponse, AppError> {
    let recorder = state.video_recorder().lock()?;

    let status = recorder.get_status().map_err(|e| {
        error!("Failed to get video recording status: {}", e);
        AppError::ExecutorError(format!("Failed to get video recording status: {}", e))
    })?;

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::to_value(status)?),
    })
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_video")
        .invoke_handler(tauri::generate_handler![
            start_video_recording,
            stop_video_recording,
            get_video_recording_status,
        ])
        .build()
}
