//! System operations commands
//!
//! Commands for system-level operations like error handling, updates, and folder opening.

use crate::error::UserFacingError;
use std::process::Command;
use tauri::{AppHandle, Emitter};
use tracing::{error, info};

use super::super::CommandResponse;

/// Handle and emit user-facing errors.
///
/// Logs the error and emits it to the frontend for display.
///
/// # Arguments
/// * `error` - The user-facing error to handle
/// * `app_handle` - Tauri application handle for event emission
///
/// # Returns
/// * `Ok(())` - Success
/// * `Err(String)` - Error if event emission fails
#[tauri::command]
pub fn handle_error(error: UserFacingError, app_handle: AppHandle) -> Result<(), String> {
    error!("User-facing error: {:?}", error);

    // Emit error event to frontend
    app_handle
        .emit("error", &error)
        .map_err(|e| format!("Failed to emit error event: {}", e))?;

    Ok(())
}

/// Check for application updates.
///
/// In release builds, checks for available updates via Tauri updater.
/// In debug builds, returns a development mode message.
///
/// # Arguments
/// * `app_handle` - Tauri application handle for updater access
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with update availability information
/// * `Err(String)` - Error if update check fails
#[tauri::command]
pub async fn check_for_updates(
    #[allow(unused_variables)] app_handle: AppHandle,
) -> Result<CommandResponse, String> {
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
                            "current_version": env!("CARGO_PKG_VERSION"),
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
                            "current_version": env!("CARGO_PKG_VERSION"),
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
                "current_version": env!("CARGO_PKG_VERSION"),
                "development": true,
            })),
        })
    }
}

/// Download and install an available update.
///
/// In release builds, downloads and installs the update, then restarts the application.
/// In debug builds, returns a development mode message.
///
/// # Arguments
/// * `app_handle` - Tauri application handle for updater access
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message (note: app will restart on success)
/// * `Err(String)` - Error if update installation fails
#[tauri::command]
pub async fn install_update(
    #[allow(unused_variables)] app_handle: AppHandle,
) -> Result<CommandResponse, String> {
    info!("Installing update");

    #[cfg(not(debug_assertions))]
    {
        use tauri_plugin_updater::UpdaterExt;

        match app_handle.updater_builder().build() {
            Ok(updater) => match updater.check().await {
                Ok(Some(update)) => {
                    info!("Downloading update version {}", update.version);

                    // Download and install the update
                    match update
                        .download_and_install(|_chunk_length, _content_length| {}, || {})
                        .await
                    {
                        Ok(_) => {
                            info!("Update installed successfully, restarting application");
                            Ok(CommandResponse {
                                success: true,
                                message: Some(
                                    "Update installed. The application will restart.".to_string(),
                                ),
                                data: Some(serde_json::json!({
                                    "installed": true,
                                    "version": update.version.to_string(),
                                })),
                            })
                        }
                        Err(e) => {
                            error!("Failed to install update: {}", e);
                            Err(format!("Failed to install update: {}", e))
                        }
                    }
                }
                Ok(None) => {
                    info!("No update available to install");
                    Ok(CommandResponse {
                        success: false,
                        message: Some("No update available".to_string()),
                        data: Some(serde_json::json!({
                            "installed": false,
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
        info!("Update installation skipped in development mode");
        Ok(CommandResponse {
            success: false,
            message: Some("Updates are disabled in development mode".to_string()),
            data: Some(serde_json::json!({
                "installed": false,
                "development": true,
            })),
        })
    }
}

/// Open a folder in the system file explorer.
///
/// Uses platform-specific commands (explorer/open/xdg-open).
///
/// # Arguments
/// * `path` - The folder path to open
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if path doesn't exist or open fails
#[tauri::command]
pub fn open_folder(path: String) -> Result<CommandResponse, String> {
    info!("Opening folder: {}", path);

    // Check if path exists
    if !std::path::Path::new(&path).exists() {
        return Err(format!("Path does not exist: {}", path));
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("explorer")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Opened folder: {}", path)),
        data: None,
    })
}
