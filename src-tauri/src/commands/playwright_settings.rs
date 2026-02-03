//! Playwright settings commands
//!
//! This module handles Playwright test configuration:
//! - Getting and setting Playwright test credentials
//! - Configuring test environment variables
//! - Secure password storage in OS keychain

use crate::config_facade::{keychain_keys, playwright_keychain};
use crate::settings::{self, PlaywrightSettings};
use anyhow::Result;
use tracing::info;

use super::CommandResponse;

// ============================================================================
// Keychain Operations for Playwright
// ============================================================================

/// Stores the Playwright test password in the OS keychain
fn store_playwright_password(password: &str) -> Result<()> {
    playwright_keychain().store(keychain_keys::PLAYWRIGHT_PASSWORD, password)
}

/// Retrieves the Playwright test password from the OS keychain
fn get_playwright_password() -> Result<Option<String>> {
    playwright_keychain().get(keychain_keys::PLAYWRIGHT_PASSWORD)
}

/// Deletes the Playwright test password from the OS keychain
fn delete_playwright_password() -> Result<()> {
    playwright_keychain().delete(keychain_keys::PLAYWRIGHT_PASSWORD)
}

/// Check if a Playwright password exists in the keychain
fn has_playwright_password() -> Result<bool> {
    playwright_keychain().exists(keychain_keys::PLAYWRIGHT_PASSWORD)
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Get the current Playwright settings.
///
/// Returns the Playwright settings stored in the persistent settings file.
/// Note: The password field in the response indicates whether a password is set,
/// but the actual password is stored securely in the OS keychain.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with Playwright settings data
/// * `Err(String)` - Error message if settings cannot be loaded
#[tauri::command]
pub fn get_playwright_settings() -> Result<CommandResponse, String> {
    info!("Getting Playwright settings");

    let playwright_settings = settings::get_playwright_settings();

    // Check if password exists in keychain (for backward compatibility, also check settings)
    let has_password = has_playwright_password()
        .map_err(|e| format!("Failed to check keychain: {}", e))?
        || playwright_settings.test_password.is_some();

    // Build response - include a placeholder if password is set (don't expose actual password)
    let response_settings = serde_json::json!({
        "test_username": playwright_settings.test_username,
        "test_password": if has_password { Some("********".to_string()) } else { None::<String> },
        "base_url": playwright_settings.base_url,
        "skip_web_server": playwright_settings.skip_web_server,
        "has_password": has_password,
    });

    Ok(CommandResponse {
        success: true,
        message: Some("Playwright settings retrieved".to_string()),
        data: Some(response_settings),
    })
}

/// Save Playwright settings.
///
/// Persists the provided Playwright settings to the settings file.
/// The password is stored securely in the OS keychain, not in the settings file.
///
/// # Arguments
/// * `test_username` - Username or email for test authentication (optional)
/// * `test_password` - Password for test authentication (optional). If provided, stored in keychain.
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

    // Handle password storage in keychain
    if let Some(ref password) = test_password {
        if !password.is_empty() && password != "********" {
            // Only store if it's a real password (not the placeholder)
            store_playwright_password(password)
                .map_err(|e| format!("Failed to store password in keychain: {}", e))?;
            info!("Playwright test password stored in keychain");
        }
    } else {
        // If password is None, delete from keychain
        delete_playwright_password()
            .map_err(|e| format!("Failed to delete password from keychain: {}", e))?;
    }

    // Save settings without password (password is in keychain)
    let playwright_settings = PlaywrightSettings {
        test_username,
        test_password: None, // Don't store in plaintext
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

/// Check if a Playwright test password is configured.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with `has_password` boolean in data
/// * `Err(String)` - Error message if check fails
#[tauri::command]
pub fn has_playwright_test_password() -> Result<CommandResponse, String> {
    info!("Checking if Playwright test password exists");

    // Check keychain first, then fall back to settings for backward compatibility
    let keychain_has = has_playwright_password()
        .map_err(|e| format!("Failed to check keychain: {}", e))?;
    let settings_has = settings::get_playwright_settings().test_password.is_some();

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "has_password": keychain_has || settings_has })),
    })
}

/// Delete the Playwright test password from the keychain.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if deletion fails
#[tauri::command]
pub fn delete_playwright_test_password() -> Result<CommandResponse, String> {
    info!("Deleting Playwright test password");

    delete_playwright_password()
        .map_err(|e| format!("Failed to delete password from keychain: {}", e))?;

    // Also clear from settings if present (backward compatibility)
    let mut playwright_settings = settings::get_playwright_settings();
    if playwright_settings.test_password.is_some() {
        playwright_settings.test_password = None;
        settings::save_playwright_settings(playwright_settings)
            .map_err(|e| format!("Failed to clear password from settings: {}", e))?;
    }

    Ok(CommandResponse {
        success: true,
        message: Some("Playwright test password deleted".to_string()),
        data: None,
    })
}

/// Get the actual Playwright test password for use in test execution.
///
/// This is an internal function, not exposed as a Tauri command.
/// It retrieves the password from keychain, falling back to settings.
pub fn get_playwright_test_password_internal() -> Option<String> {
    // Try keychain first
    if let Ok(Some(password)) = get_playwright_password() {
        return Some(password);
    }

    // Fall back to settings for backward compatibility
    settings::get_playwright_settings().test_password
}
