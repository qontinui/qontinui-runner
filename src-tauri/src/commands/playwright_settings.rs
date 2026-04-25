//! Playwright settings commands
//!
//! This module handles Playwright test configuration:
//! - Getting and setting Playwright test credentials
//! - Configuring test environment variables
//! - Secure password storage in OS keychain
//!
//! # Typed errors (Workstream D)
//!
//! Handlers keep `-> Result<T, String>` at the Tauri boundary and build
//! errors via [`AppError`] internally, converting with
//! `.map_err(String::from)`. See `commands/mod.rs` for the migration guide.

use crate::config_facade::{keychain_keys, playwright_keychain};
use crate::error::AppError;
use crate::settings::{self, PlaywrightSettings};
use anyhow::Result;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
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

/// Internal implementation of [`get_playwright_settings`] returning [`AppError`].
fn get_playwright_settings_impl() -> Result<CommandResponse, AppError> {
    info!("Getting Playwright settings");

    let playwright_settings = settings::get_playwright_settings();

    // Check if password exists in keychain (for backward compatibility, also check settings).
    // `?` lifts anyhow::Error via From impl into AppError::UnexpectedError.
    let has_password = has_playwright_password()? || playwright_settings.test_password.is_some();

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

/// Get the current Playwright settings.
#[tauri::command]
pub fn get_playwright_settings() -> Result<CommandResponse, String> {
    get_playwright_settings_impl().map_err(String::from)
}

/// Internal implementation of [`save_playwright_settings`] returning [`AppError`].
fn save_playwright_settings_impl(
    test_username: Option<String>,
    test_password: Option<String>,
    base_url: Option<String>,
    skip_web_server: bool,
) -> Result<CommandResponse, AppError> {
    info!("Saving Playwright settings");

    // Handle password storage in keychain.
    // `?` lifts anyhow::Error via From impl into AppError::UnexpectedError.
    if let Some(ref password) = test_password {
        if !password.is_empty() && password != "********" {
            // Only store if it's a real password (not the placeholder)
            store_playwright_password(password)?;
            info!("Playwright test password stored in keychain");
        }
    } else {
        // If password is None, delete from keychain
        delete_playwright_password()?;
    }

    // Save settings without password (password is in keychain)
    let playwright_settings = PlaywrightSettings {
        test_username,
        test_password: None, // Don't store in plaintext
        base_url,
        skip_web_server,
    };

    settings::save_playwright_settings(playwright_settings).map_err(AppError::ConfigError)?;

    Ok(CommandResponse {
        success: true,
        message: Some("Playwright settings saved".to_string()),
        data: None,
    })
}

/// Save Playwright settings.
#[tauri::command]
pub fn save_playwright_settings(
    test_username: Option<String>,
    test_password: Option<String>,
    base_url: Option<String>,
    skip_web_server: bool,
) -> Result<CommandResponse, String> {
    save_playwright_settings_impl(test_username, test_password, base_url, skip_web_server)
        .map_err(String::from)
}

/// Check if a Playwright test password is configured.
#[tauri::command]
pub fn has_playwright_test_password() -> Result<CommandResponse, String> {
    info!("Checking if Playwright test password exists");

    // Check keychain first, then fall back to settings for backward compatibility.
    // `?` lifts anyhow::Error via From impl into AppError, then String::from converts.
    let keychain_has = has_playwright_password()
        .map_err(|e: anyhow::Error| String::from(AppError::from(e)))?;
    let settings_has = settings::get_playwright_settings().test_password.is_some();

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "has_password": keychain_has || settings_has })),
    })
}

/// Internal implementation of [`delete_playwright_test_password`] returning [`AppError`].
fn delete_playwright_test_password_impl() -> Result<CommandResponse, AppError> {
    info!("Deleting Playwright test password");

    // `?` lifts anyhow::Error via From impl into AppError::UnexpectedError.
    delete_playwright_password()?;

    // Also clear from settings if present (backward compatibility)
    let mut playwright_settings = settings::get_playwright_settings();
    if playwright_settings.test_password.is_some() {
        playwright_settings.test_password = None;
        settings::save_playwright_settings(playwright_settings).map_err(AppError::ConfigError)?;
    }

    Ok(CommandResponse {
        success: true,
        message: Some("Playwright test password deleted".to_string()),
        data: None,
    })
}

/// Delete the Playwright test password from the keychain.
#[tauri::command]
pub fn delete_playwright_test_password() -> Result<CommandResponse, String> {
    delete_playwright_test_password_impl().map_err(String::from)
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

/// Tauri plugin exposing all Playwright settings commands.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_playwright_settings")
        .invoke_handler(tauri::generate_handler![
            get_playwright_settings,
            save_playwright_settings,
            has_playwright_test_password,
            delete_playwright_test_password,
        ])
        .build()
}
