//! Playwright settings commands
//!
//! This module handles Playwright test configuration:
//! - Getting and setting Playwright test credentials
//! - Configuring test environment variables

use crate::settings::{self, PlaywrightSettings};
use tracing::info;

use super::CommandResponse;

// ============================================================================
// Tauri Commands
// ============================================================================

/// Get the current Playwright settings.
///
/// Returns the Playwright settings stored in the persistent settings file.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with Playwright settings data
/// * `Err(String)` - Error message if settings cannot be loaded
#[tauri::command]
pub fn get_playwright_settings() -> Result<CommandResponse, String> {
    info!("Getting Playwright settings");

    let playwright_settings = settings::get_playwright_settings();

    Ok(CommandResponse {
        success: true,
        message: Some("Playwright settings retrieved".to_string()),
        data: Some(
            serde_json::to_value(&playwright_settings)
                .map_err(|e| format!("Failed to serialize Playwright settings: {}", e))?,
        ),
    })
}

/// Save Playwright settings.
///
/// Persists the provided Playwright settings to the settings file.
///
/// # Arguments
/// * `test_username` - Username or email for test authentication (optional)
/// * `test_password` - Password for test authentication (optional)
/// * `base_url` - Base URL for Playwright tests (optional)
/// * `skip_web_server` - Whether to skip starting web server
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error message if settings cannot be saved
#[tauri::command]
pub fn save_playwright_settings(
    test_username: Option<String>,
    test_password: Option<String>,
    base_url: Option<String>,
    skip_web_server: bool,
) -> Result<CommandResponse, String> {
    info!("Saving Playwright settings");

    let playwright_settings = PlaywrightSettings {
        test_username,
        test_password,
        base_url,
        skip_web_server,
    };

    settings::save_playwright_settings(playwright_settings)
        .map_err(|e| format!("Failed to save Playwright settings: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Playwright settings saved".to_string()),
        data: None,
    })
}
