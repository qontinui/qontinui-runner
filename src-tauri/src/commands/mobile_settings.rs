//! Mobile settings commands
//!
//! This module handles mobile development feedback configuration:
//! - Getting and setting ADB path
//! - Configuring default device and app package
//! - Logcat capture settings
//!
//! # Typed errors (Workstream D)
//!
//! Handlers keep `-> Result<T, String>` at the Tauri boundary and build
//! errors via [`AppError`] internally, converting with
//! `.map_err(String::from)`. See `commands/mod.rs` for the migration guide.

use crate::error::AppError;
use crate::settings::{self, MobileSettings};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tracing::info;

use super::CommandResponse;

// ============================================================================
// Tauri Commands
// ============================================================================

/// Internal implementation of [`get_mobile_settings`] returning [`AppError`].
fn get_mobile_settings_impl() -> Result<CommandResponse, AppError> {
    info!("Getting Mobile settings");

    let mobile_settings = settings::get_mobile_settings();
    let data = serde_json::to_value(&mobile_settings)?;

    Ok(CommandResponse {
        success: true,
        message: Some("Mobile settings retrieved".to_string()),
        data: Some(data),
    })
}

/// Get the current Mobile settings.
#[tauri::command]
pub fn get_mobile_settings() -> Result<CommandResponse, String> {
    get_mobile_settings_impl().map_err(String::from)
}

/// Internal implementation of [`save_mobile_settings`] returning [`AppError`].
fn save_mobile_settings_impl(
    adb_path: Option<String>,
    default_device_id: Option<String>,
    app_package: Option<String>,
    logcat_lines: u32,
    filter_react_native: bool,
    output_dir: Option<String>,
) -> Result<CommandResponse, AppError> {
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

    settings::save_mobile_settings(mobile_settings).map_err(AppError::ConfigError)?;

    Ok(CommandResponse {
        success: true,
        message: Some("Mobile settings saved".to_string()),
        data: None,
    })
}

/// Save Mobile settings.
#[tauri::command]
pub fn save_mobile_settings(
    adb_path: Option<String>,
    default_device_id: Option<String>,
    app_package: Option<String>,
    logcat_lines: u32,
    filter_react_native: bool,
    output_dir: Option<String>,
) -> Result<CommandResponse, String> {
    save_mobile_settings_impl(
        adb_path,
        default_device_id,
        app_package,
        logcat_lines,
        filter_react_native,
        output_dir,
    )
    .map_err(String::from)
}

/// Tauri plugin exposing all mobile settings commands.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_mobile_settings")
        .invoke_handler(tauri::generate_handler![
            get_mobile_settings,
            save_mobile_settings,
        ])
        .build()
}
