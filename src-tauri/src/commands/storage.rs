//! Storage management commands
//!
//! This module handles local disk storage operations:
//! - Saving screenshots and videos to disk
//! - Querying storage usage statistics
//! - Managing storage cleanup (deleting old sessions)
//! - Getting storage configuration paths

use serde_json;
use std::sync::Arc;
use tauri::State;
use tracing::{error, info};

use super::{AppState, CommandResponse};

/// Save a screenshot to local disk storage.
///
/// Stores the screenshot image data in the configured screenshots directory,
/// organized by session ID.
///
/// # Arguments
/// * `session_id` - Unique identifier for the automation session
/// * `screenshot_id` - Unique identifier for this specific screenshot
/// * `data` - Raw image data bytes
/// * `state` - Application state containing the local storage service
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with saved file path
/// * `Err(String)` - Error if save operation fails
#[tauri::command]
pub fn save_screenshot_to_disk(
    session_id: String,
    screenshot_id: String,
    data: Vec<u8>,
    state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!(
        "Saving screenshot to disk: session={}, id={}, size={} bytes",
        session_id,
        screenshot_id,
        data.len()
    );

    let storage = state.local_storage.lock().unwrap();
    let path = storage
        .save_screenshot(&session_id, &screenshot_id, &data)
        .map_err(|e| {
            error!("Failed to save screenshot: {}", e);
            format!("Failed to save screenshot: {}", e)
        })?;

    info!("Screenshot saved successfully: {:?}", path);

    Ok(CommandResponse {
        success: true,
        message: Some("Screenshot saved successfully".to_string()),
        data: Some(serde_json::json!({
            "path": path.to_string_lossy().to_string(),
        })),
    })
}

/// Save a video to local disk storage.
///
/// Stores the video file data in the configured videos directory,
/// organized by session ID.
///
/// # Arguments
/// * `session_id` - Unique identifier for the automation session
/// * `data` - Raw video file data bytes
/// * `state` - Application state containing the local storage service
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with saved file path
/// * `Err(String)` - Error if save operation fails
#[tauri::command]
pub fn save_video_to_disk(
    session_id: String,
    data: Vec<u8>,
    state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!(
        "Saving video to disk: session={}, size={} bytes",
        session_id,
        data.len()
    );

    let storage = state.local_storage.lock().unwrap();
    let path = storage.save_video(&session_id, &data).map_err(|e| {
        error!("Failed to save video: {}", e);
        format!("Failed to save video: {}", e)
    })?;

    info!("Video saved successfully: {:?}", path);

    Ok(CommandResponse {
        success: true,
        message: Some("Video saved successfully".to_string()),
        data: Some(serde_json::json!({
            "path": path.to_string_lossy().to_string(),
        })),
    })
}

/// Get local storage usage statistics.
///
/// Returns information about disk space usage for screenshots and videos,
/// including file counts and total size in megabytes.
///
/// # Arguments
/// * `state` - Application state containing the local storage service
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with usage statistics
/// * `Err(String)` - Error if usage query fails
#[tauri::command]
pub fn get_local_storage_usage(state: State<Arc<AppState>>) -> Result<CommandResponse, String> {
    info!("Getting local storage usage");

    let storage = state.local_storage.lock().unwrap();
    let usage = storage.get_storage_usage().map_err(|e| {
        error!("Failed to get storage usage: {}", e);
        format!("Failed to get storage usage: {}", e)
    })?;

    info!(
        "Storage usage: screenshots={:.2} MB ({} files), videos={:.2} MB ({} files)",
        usage.screenshots_mb, usage.screenshot_count, usage.videos_mb, usage.video_count
    );

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::to_value(&usage).map_err(|e| {
            error!("Failed to serialize storage usage: {}", e);
            format!("Failed to serialize storage usage: {}", e)
        })?),
    })
}

/// Delete old sessions from local storage.
///
/// Removes session directories older than the specified number of days.
///
/// # Arguments
/// * `storage_type` - Type of storage to clean ("screenshots" or "videos")
/// * `older_than_days` - Delete sessions older than this many days
/// * `state` - Application state containing the local storage service
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if deletion fails
#[tauri::command]
pub fn delete_old_sessions(
    storage_type: String,
    older_than_days: u32,
    state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!(
        "Deleting old sessions: type={}, older_than_days={}",
        storage_type, older_than_days
    );

    let storage = state.local_storage.lock().unwrap();
    storage
        .delete_old_sessions(&storage_type, older_than_days)
        .map_err(|e| {
            error!("Failed to delete old sessions: {}", e);
            format!("Failed to delete old sessions: {}", e)
        })?;

    info!("Old sessions deleted successfully");

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Deleted {} sessions older than {} days",
            storage_type, older_than_days
        )),
        data: None,
    })
}

/// Clear all local storage (screenshots and videos).
///
/// Removes all stored screenshots and videos from disk.
///
/// # Arguments
/// * `state` - Application state containing the local storage service
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if clear operation fails
#[tauri::command]
pub fn clear_all_storage(state: State<Arc<AppState>>) -> Result<CommandResponse, String> {
    info!("Clearing all local storage");

    let storage = state.local_storage.lock().unwrap();
    storage.clear_all_storage().map_err(|e| {
        error!("Failed to clear all storage: {}", e);
        format!("Failed to clear all storage: {}", e)
    })?;

    info!("All storage cleared successfully");

    Ok(CommandResponse {
        success: true,
        message: Some("All storage cleared successfully".to_string()),
        data: None,
    })
}

/// Get the storage paths configuration.
///
/// Returns the configured paths for screenshots and videos, along with
/// storage limits and auto-cleanup settings.
///
/// # Arguments
/// * `state` - Application state containing the local storage service
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with storage configuration
/// * `Err(String)` - Error message if configuration cannot be retrieved
#[tauri::command]
pub fn get_storage_paths(state: State<Arc<AppState>>) -> Result<CommandResponse, String> {
    info!("Getting storage paths");

    let storage = state.local_storage.lock().unwrap();
    let config = &storage.config;

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "screenshot_path": config.screenshot_path.to_string_lossy().to_string(),
            "video_path": config.video_path.to_string_lossy().to_string(),
            "max_screenshot_mb": config.max_screenshot_mb,
            "max_video_mb": config.max_video_mb,
            "auto_cleanup": config.auto_cleanup,
        })),
    })
}
