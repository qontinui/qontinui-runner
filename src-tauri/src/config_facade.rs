//! Configuration Facade - Unified interface for settings management
//!
//! This module provides a generic interface for getting, saving, and updating
//! settings fields, eliminating the repetitive load-modify-save patterns in settings.rs.
//!
//! # Example Usage
//!
//! ```rust
//! use crate::config_facade::{get_setting, save_setting, update_setting};
//! use crate::settings::AiSettings;
//!
//! // Get a setting
//! let ai: AiSettings = get_setting();
//!
//! // Save a setting
//! save_setting(AiSettings::default())?;
//!
//! // Update a setting with a closure
//! update_setting::<AiSettings, _>(|ai| {
//!     ai.provider = AiProvider::ClaudeApi;
//! })?;
//! ```

use anyhow::Result;
use keyring::Entry;
use serde::{de::DeserializeOwned, Serialize};
use tracing::info;

use crate::settings::{
    load_settings, save_settings, AccessibilitySettings, AiSettings, DebugSettings,
    ExecutionVariablesSettings, GlobalLogSourceSettings, MobileSettings, PathSettings,
    PlaywrightSettings, SelfHealingSettings, Settings,
};

// ============================================================================
// SettingsField Trait
// ============================================================================

/// Trait for settings fields that can be get/set generically.
///
/// Implement this trait for each settings type to enable use with
/// the generic `get_setting`, `save_setting`, and `update_setting` functions.
pub trait SettingsField: Serialize + DeserializeOwned + Clone + Default {
    /// Get a reference to this field from the Settings struct
    fn get_from(settings: &Settings) -> &Self;

    /// Set this field in the Settings struct
    fn set_in(settings: &mut Settings, value: Self);

    /// Field name for logging purposes
    fn field_name() -> &'static str;
}

// ============================================================================
// SettingsField Implementations
// ============================================================================

impl SettingsField for AiSettings {
    fn get_from(settings: &Settings) -> &Self {
        &settings.ai
    }

    fn set_in(settings: &mut Settings, value: Self) {
        settings.ai = value;
    }

    fn field_name() -> &'static str {
        "ai"
    }
}

impl SettingsField for PlaywrightSettings {
    fn get_from(settings: &Settings) -> &Self {
        &settings.playwright
    }

    fn set_in(settings: &mut Settings, value: Self) {
        settings.playwright = value;
    }

    fn field_name() -> &'static str {
        "playwright"
    }
}

impl SettingsField for MobileSettings {
    fn get_from(settings: &Settings) -> &Self {
        &settings.mobile
    }

    fn set_in(settings: &mut Settings, value: Self) {
        settings.mobile = value;
    }

    fn field_name() -> &'static str {
        "mobile"
    }
}

impl SettingsField for SelfHealingSettings {
    fn get_from(settings: &Settings) -> &Self {
        &settings.self_healing
    }

    fn set_in(settings: &mut Settings, value: Self) {
        settings.self_healing = value;
    }

    fn field_name() -> &'static str {
        "self_healing"
    }
}

impl SettingsField for AccessibilitySettings {
    fn get_from(settings: &Settings) -> &Self {
        &settings.accessibility
    }

    fn set_in(settings: &mut Settings, value: Self) {
        settings.accessibility = value;
    }

    fn field_name() -> &'static str {
        "accessibility"
    }
}

impl SettingsField for DebugSettings {
    fn get_from(settings: &Settings) -> &Self {
        &settings.debug
    }

    fn set_in(settings: &mut Settings, value: Self) {
        settings.debug = value;
    }

    fn field_name() -> &'static str {
        "debug"
    }
}

impl SettingsField for PathSettings {
    fn get_from(settings: &Settings) -> &Self {
        &settings.paths
    }

    fn set_in(settings: &mut Settings, value: Self) {
        settings.paths = value;
    }

    fn field_name() -> &'static str {
        "paths"
    }
}

impl SettingsField for ExecutionVariablesSettings {
    fn get_from(settings: &Settings) -> &Self {
        &settings.execution_variables
    }

    fn set_in(settings: &mut Settings, value: Self) {
        settings.execution_variables = value;
    }

    fn field_name() -> &'static str {
        "execution_variables"
    }
}

impl SettingsField for GlobalLogSourceSettings {
    fn get_from(settings: &Settings) -> &Self {
        &settings.log_sources
    }

    fn set_in(settings: &mut Settings, value: Self) {
        settings.log_sources = value;
    }

    fn field_name() -> &'static str {
        "log_sources"
    }
}

// ============================================================================
// Generic Settings Functions
// ============================================================================

/// Generic get for any settings field.
///
/// # Example
///
/// ```rust
/// let ai: AiSettings = get_setting();
/// ```
pub fn get_setting<T: SettingsField>() -> T {
    T::get_from(&load_settings()).clone()
}

/// Generic save for any settings field.
///
/// # Example
///
/// ```rust
/// save_setting(AiSettings::default())?;
/// ```
pub fn save_setting<T: SettingsField>(value: T) -> Result<(), String> {
    info!("Saving {} settings", T::field_name());
    let mut settings = load_settings();
    T::set_in(&mut settings, value);
    save_settings(&settings)?;
    Ok(())
}

/// Generic update for any settings field using a closure.
///
/// This is useful when you want to modify only part of a settings struct
/// without needing to load and save the entire thing manually.
///
/// # Example
///
/// ```rust
/// update_setting::<AiSettings, _>(|ai| {
///     ai.provider = AiProvider::ClaudeApi;
/// })?;
/// ```
pub fn update_setting<T: SettingsField, F>(f: F) -> Result<(), String>
where
    F: FnOnce(&mut T),
{
    info!("Updating {} settings", T::field_name());
    let mut settings = load_settings();
    let mut value = T::get_from(&settings).clone();
    f(&mut value);
    T::set_in(&mut settings, value);
    save_settings(&settings)?;
    Ok(())
}

// ============================================================================
// Keychain Helper
// ============================================================================

/// A unified helper for storing secrets in the OS keychain.
///
/// This provides a consistent interface for storing, retrieving, and deleting
/// secrets across different service namespaces.
///
/// # Example
///
/// ```rust
/// let keychain = KeychainHelper::new("com.qontinui.runner.ai");
/// keychain.store("api_key", "sk-...")?;
/// let key = keychain.get("api_key")?;
/// keychain.delete("api_key")?;
/// ```
pub struct KeychainHelper {
    service: String,
}

impl KeychainHelper {
    /// Create a new keychain helper for the given service namespace.
    pub fn new(service: &str) -> Self {
        Self {
            service: service.to_string(),
        }
    }

    /// Store a secret in the keychain.
    pub fn store(&self, key: &str, value: &str) -> Result<()> {
        let entry = Entry::new(&self.service, key)?;
        entry.set_password(value)?;
        info!("Stored {} in keychain (service: {})", key, self.service);
        Ok(())
    }

    /// Get a secret from the keychain.
    ///
    /// Returns `Ok(Some(value))` if the secret exists,
    /// `Ok(None)` if it doesn't exist,
    /// or an error if something went wrong.
    pub fn get(&self, key: &str) -> Result<Option<String>> {
        let entry = Entry::new(&self.service, key)?;
        match entry.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete a secret from the keychain.
    ///
    /// This is idempotent - it won't error if the secret doesn't exist.
    pub fn delete(&self, key: &str) -> Result<()> {
        let entry = Entry::new(&self.service, key)?;
        // Ignore NoEntry error - it's fine if it doesn't exist
        match entry.delete_credential() {
            Ok(()) => {
                info!("Deleted {} from keychain (service: {})", key, self.service);
            }
            Err(keyring::Error::NoEntry) => {
                info!(
                    "{} not found in keychain (service: {}), nothing to delete",
                    key, self.service
                );
            }
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }

    /// Check if a secret exists in the keychain.
    pub fn exists(&self, key: &str) -> Result<bool> {
        Ok(self.get(key)?.is_some())
    }
}

// ============================================================================
// Pre-configured Keychain Helpers
// ============================================================================

/// Get a keychain helper for AI-related secrets (API keys, etc.)
pub fn ai_keychain() -> KeychainHelper {
    KeychainHelper::new("com.qontinui.runner.ai")
}

/// Get a keychain helper for self-healing related secrets
pub fn self_healing_keychain() -> KeychainHelper {
    KeychainHelper::new("com.qontinui.runner.self_healing")
}

/// Get a keychain helper for Gemini-related secrets
pub fn gemini_keychain() -> KeychainHelper {
    KeychainHelper::new("com.qontinui.runner.gemini")
}

/// Get a keychain helper for Playwright-related secrets
pub fn playwright_keychain() -> KeychainHelper {
    KeychainHelper::new("com.qontinui.runner.playwright")
}

// ============================================================================
// API Key Constants
// ============================================================================

/// Keychain key names for API keys
pub mod keychain_keys {
    pub const CLAUDE_API_KEY: &str = "claude_api_key";
    pub const GEMINI_API_KEY: &str = "gemini_api_key";
    pub const OPENAI_API_KEY: &str = "openai_api_key";
    pub const ANTHROPIC_API_KEY: &str = "anthropic_api_key";
    pub const PLAYWRIGHT_PASSWORD: &str = "test_password";
}

// ============================================================================
// Migration Functions
// ============================================================================

/// Migrate existing plaintext API keys from settings to the secure keychain.
///
/// This function checks for API keys stored in plaintext in the settings file
/// and moves them to the OS keychain for secure storage.
///
/// # Returns
/// * `Ok(())` - Migration successful (or nothing to migrate)
/// * `Err(String)` - Error during migration
pub fn migrate_api_keys_to_keychain() -> Result<(), String> {
    use crate::settings::{load_settings, save_settings};
    use tracing::{info, warn};

    info!("Checking for API keys to migrate to keychain...");
    let mut settings = load_settings();
    let mut migrated_any = false;

    // Note: The current AiSettings doesn't store API keys in plaintext,
    // but this function is here for future-proofing and backward compatibility
    // if any legacy settings had plaintext keys.

    // Migrate Playwright password if stored in settings
    if let Some(ref password) = settings.playwright.test_password {
        if !password.is_empty() {
            match playwright_keychain().store(keychain_keys::PLAYWRIGHT_PASSWORD, password) {
                Ok(()) => {
                    info!("Migrated Playwright test password to keychain");
                    settings.playwright.test_password = None;
                    migrated_any = true;
                }
                Err(e) => {
                    warn!("Failed to migrate Playwright password to keychain: {}", e);
                }
            }
        }
    }

    // Save settings if we migrated anything (to clear plaintext values)
    if migrated_any {
        save_settings(&settings).map_err(|e| format!("Failed to save settings after migration: {}", e))?;
        info!("API key migration complete");
    } else {
        info!("No API keys to migrate");
    }

    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_setting_returns_default() {
        // This test verifies that get_setting works with default values
        let ai: AiSettings = get_setting();
        // Just verify it doesn't panic and returns something
        assert!(ai.claude_cli.timeout_seconds > 0);
    }

    #[test]
    fn test_field_names() {
        // Verify all field names are correct
        assert_eq!(AiSettings::field_name(), "ai");
        assert_eq!(PlaywrightSettings::field_name(), "playwright");
        assert_eq!(MobileSettings::field_name(), "mobile");
        assert_eq!(SelfHealingSettings::field_name(), "self_healing");
        assert_eq!(AccessibilitySettings::field_name(), "accessibility");
        assert_eq!(DebugSettings::field_name(), "debug");
        assert_eq!(PathSettings::field_name(), "paths");
        assert_eq!(ExecutionVariablesSettings::field_name(), "execution_variables");
        assert_eq!(GlobalLogSourceSettings::field_name(), "log_sources");
    }
}

// ============================================================================
// TODO: Remaining settings to migrate from settings.rs
// ============================================================================
//
// The following settings are managed with individual functions in settings.rs
// and could be migrated to use this facade:
//
// Already migrated (using config_facade):
// - AiSettings (get_ai_settings, save_ai_settings)
// - PlaywrightSettings (get_playwright_settings, save_playwright_settings)
//
// Remaining to migrate:
// - DebugSettings (get_debug_settings, save_debug_settings)
// - MobileSettings (get_mobile_settings, save_mobile_settings)
// - SelfHealingSettings (get_self_healing_settings, save_self_healing_settings)
// - AccessibilitySettings (get_accessibility_settings, save_accessibility_settings)
// - PathSettings (get_path_settings, save_path_settings)
// - ExecutionVariablesSettings (get_execution_variables_settings, save_execution_variables_settings)
// - GlobalLogSourceSettings (get_global_log_source_settings, save_global_log_source_settings)
//
// Scalar settings that use different patterns (not struct-based):
// - last_config_path (save_last_config_path, get_last_config_path)
// - last_workflow_id (save_last_workflow_id, get_last_workflow_id)
// - last_monitor_index (save_last_monitor_index, get_last_monitor_index)
// - last_monitor_indices (save_last_monitor_indices, get_last_monitor_indices)
// - auto_load_last_config (get_auto_load_last_config, save_auto_load_last_config)
// - auto_continue_ai_workflow (get_auto_continue_ai_workflow, save_auto_continue_ai_workflow)
// - session_auto_fix_on_failure (get_session_auto_fix_on_failure, save_session_auto_fix_on_failure)
// - include_summary_step_by_default (get_include_summary_step_by_default, save_include_summary_step_by_default)
