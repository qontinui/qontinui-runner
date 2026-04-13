//! Mobile settings commands
//!
//! This module handles mobile development feedback configuration:
//! - Getting and setting ADB path
//! - Configuring default device and app package
//! - Logcat capture settings

use crate::settings::{self, MobileSettings};
use tracing::info;

use super::CommandResponse;

// ============================================================================
// Tauri Commands
// ============================================================================

/// Get the current Mobile settings.
///
/// Returns the Mobile settings stored in the persistent settings file.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with Mobile settings data
/// * `Err(String)` - Error message if settings cannot be loaded
#[tauri::command]
pub fn get_mobile_settings() -> Result<CommandResponse, String> {
    info!("Getting Mobile settings");

    let mobile_settings = settings::get_mobile_settings();

    Ok(CommandResponse {
        success: true,
        message: Some("Mobile settings retrieved".to_string()),
        data: Some(
            serde_json::to_value(&mobile_settings)
                .map_err(|e| format!("Failed to serialize Mobile settings: {}", e))?,
        ),
    })
}

/// Save Mobile settings.
///
/// Persists the provided Mobile settings to the settings file.
///
/// # Arguments
/// * `adb_path` - Custom path to ADB executable (None = auto-detect)
/// * `default_device_id` - Default device ID to use when multiple connected
/// * `app_package` - App package name for filtering logcat
/// * `logcat_lines` - Number of logcat lines to capture
/// * `filter_react_native` - Whether to filter to React Native logs
/// * `output_dir` - Custom output directory for captures
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error message if settings cannot be saved
#[tauri::command]
pub fn save_mobile_settings(
    adb_path: Option<String>,
    default_device_id: Option<String>,
    app_package: Option<String>,
    logcat_lines: u32,
    filter_react_native: bool,
    output_dir: Option<String>,
) -> Result<CommandResponse, String> {
    info!("Saving Mobile settings");

    let mobile_settings = MobileSettings {
        adb_path,
        default_device_id,
        app_package,
        logcat_lines,
        filter_react_native,
        output_dir,
        // New fields use their defaults when saving via legacy command
        ..MobileSettings::default()
    };

    settings::save_mobile_settings(mobile_settings)
        .map_err(|e| format!("Failed to save Mobile settings: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Mobile settings saved".to_string()),
        data: None,
    })
}
