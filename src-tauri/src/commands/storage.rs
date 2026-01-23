//! Storage management commands
//!
//! This module handles local disk storage operations:
//! - Saving screenshots and videos to disk
//! - Querying storage usage statistics
//! - Managing storage cleanup (deleting old sessions)
//! - Getting storage configuration paths
//! - Reading image files as base64 data URLs

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json;
use std::fs;
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

    let storage = crate::safe_lock::safe_lock_or_recover(&state.local_storage, "local_storage");
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

    let storage = crate::safe_lock::safe_lock_or_recover(&state.local_storage, "local_storage");
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

    let storage = crate::safe_lock::safe_lock_or_recover(&state.local_storage, "local_storage");
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

    let storage = crate::safe_lock::safe_lock_or_recover(&state.local_storage, "local_storage");
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

    let storage = crate::safe_lock::safe_lock_or_recover(&state.local_storage, "local_storage");
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

    let storage = crate::safe_lock::safe_lock_or_recover(&state.local_storage, "local_storage");
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

/// Read an image file and return it as a base64 data URL.
///
/// This is used by the frontend to display local image files (like Playwright screenshots)
/// that can't be accessed directly due to security restrictions.
///
/// # Arguments
/// * `path` - Absolute path to the image file
///
/// # Returns
/// * `Ok(String)` - Base64 data URL (e.g., "data:image/png;base64,...")
/// * `Err(String)` - Error if file cannot be read
#[tauri::command]
pub fn read_image_as_base64(path: String) -> Result<String, String> {
    info!("Reading image as base64: {}", path);

    // Read the file
    let data = fs::read(&path).map_err(|e| {
        error!("Failed to read image file {}: {}", path, e);
        format!("Failed to read file: {}", e)
    })?;

    // Determine MIME type from extension
    let mime_type = if path.to_lowercase().ends_with(".png") {
        "image/png"
    } else if path.to_lowercase().ends_with(".jpg") || path.to_lowercase().ends_with(".jpeg") {
        "image/jpeg"
    } else if path.to_lowercase().ends_with(".gif") {
        "image/gif"
    } else if path.to_lowercase().ends_with(".webp") {
        "image/webp"
    } else {
        "application/octet-stream"
    };

    // Encode to base64 and return as data URL
    let base64_data = STANDARD.encode(&data);
    info!(
        "Successfully encoded image: {} bytes -> {} base64 chars",
        data.len(),
        base64_data.len()
    );

    Ok(format!("data:{};base64,{}", mime_type, base64_data))
}

/// Get the findings data file path.
pub fn get_findings_data_path() -> std::path::PathBuf {
    // Store in .dev-logs directory (parent of runner)
    let dev_logs_dir = std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .parent()
        .map(|p| p.join(".dev-logs"))
        .unwrap_or_else(|| std::path::PathBuf::from(".dev-logs"));

    // Create directory if it doesn't exist
    if !dev_logs_dir.exists() {
        let _ = fs::create_dir_all(&dev_logs_dir);
    }

    dev_logs_dir.join("findings-history.json")
}

/// Save findings data to disk.
///
/// Persists the findings tracker data (findings, reports, session history) to a JSON file.
/// This allows session history to survive runner restarts and context resets.
///
/// # Arguments
/// * `data` - JSON string containing the findings data
///
/// # Returns
/// * `Ok(())` - Success
/// * `Err(String)` - Error if save fails
#[tauri::command]
pub fn save_findings_data(data: String) -> Result<(), String> {
    let path = get_findings_data_path();
    info!("Saving findings data to: {:?}", path);

    fs::write(&path, &data).map_err(|e| {
        error!("Failed to save findings data: {}", e);
        format!("Failed to save findings data: {}", e)
    })?;

    info!("Findings data saved successfully ({} bytes)", data.len());
    Ok(())
}

/// Load findings data from disk.
///
/// Loads previously persisted findings tracker data from the JSON file.
///
/// # Returns
/// * `Ok(String)` - JSON string containing the findings data
/// * `Ok("")` - Empty string if file doesn't exist (not an error)
/// * `Err(String)` - Error if read fails
#[tauri::command]
pub fn load_findings_data() -> Result<String, String> {
    let path = get_findings_data_path();
    info!("Loading findings data from: {:?}", path);

    if !path.exists() {
        info!("Findings data file does not exist");
        return Ok(String::new());
    }

    let data = fs::read_to_string(&path).map_err(|e| {
        error!("Failed to load findings data: {}", e);
        format!("Failed to load findings data: {}", e)
    })?;

    info!("Findings data loaded successfully ({} bytes)", data.len());
    Ok(data)
}
