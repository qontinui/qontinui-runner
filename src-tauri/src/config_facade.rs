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

use std::collections::HashMap;

use crate::settings::{
    load_settings, save_settings, AccessibilitySettings, AiSettings, AuthSource,
    CloudRelaySettings, DebugSettings, ExecutionVariablesSettings, GlobalLogSource,
    GlobalLogSourceSettings, LogSourceAiSelectionMode, LogSourceCategory, MobileSettings,
    PathSettings, PlaywrightSettings, RunnerInstanceConfig, SelfHealingSettings, Settings,
    SpawnPlacement, TunnelSettings, VariableSource, WorldStateVerifierSettings,
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

impl SettingsField for TunnelSettings {
    fn get_from(settings: &Settings) -> &Self {
        &settings.tunnel
    }

    fn set_in(settings: &mut Settings, value: Self) {
        settings.tunnel = value;
    }

    fn field_name() -> &'static str {
        "tunnel"
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

impl SettingsField for CloudRelaySettings {
    fn get_from(settings: &Settings) -> &Self {
        &settings.cloud_relay
    }

    fn set_in(settings: &mut Settings, value: Self) {
        settings.cloud_relay = value;
    }

    fn field_name() -> &'static str {
        "cloud_relay"
    }
}

impl SettingsField for crate::otel::OtelConfig {
    fn get_from(settings: &Settings) -> &Self {
        &settings.otel
    }

    fn set_in(settings: &mut Settings, value: Self) {
        settings.otel = value;
    }

    fn field_name() -> &'static str {
        "otel"
    }
}

impl SettingsField for WorldStateVerifierSettings {
    fn get_from(settings: &Settings) -> &Self {
        &settings.world_state_verifier
    }

    fn set_in(settings: &mut Settings, value: Self) {
        settings.world_state_verifier = value;
    }

    fn field_name() -> &'static str {
        "world_state_verifier"
    }
}

impl SettingsField for crate::step_output::script_emitter::ScriptedOutputSettings {
    fn get_from(settings: &Settings) -> &Self {
        &settings.scripted_output
    }

    fn set_in(settings: &mut Settings, value: Self) {
        settings.scripted_output = value;
    }

    fn field_name() -> &'static str {
        "scripted_output"
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
        save_settings(&settings)
            .map_err(|e| format!("Failed to save settings after migration: {}", e))?;
        info!("API key migration complete");
    } else {
        info!("No API keys to migrate");
    }

    Ok(())
}

// ============================================================================
// Scalar Settings Accessors
// ============================================================================
//
// These access individual fields on the top-level Settings struct rather than
// sub-struct fields. They follow the same load-modify-save pattern but for
// scalar values.

/// Get the last loaded config path.
pub fn get_last_config_path() -> Option<String> {
    load_settings().last_config_path
}

/// Save the last loaded config path.
pub fn save_last_config_path(path: &str) -> Result<(), String> {
    info!("Saving last config path: {}", path);
    let mut settings = load_settings();
    settings.last_config_path = Some(path.to_string());
    save_settings(&settings)
}

/// Get the last used workflow ID.
pub fn get_last_workflow_id() -> Option<String> {
    load_settings().last_workflow_id
}

/// Save the last used workflow ID.
pub fn save_last_workflow_id(workflow_id: &str) -> Result<(), String> {
    info!("Saving last workflow ID: {}", workflow_id);
    let mut settings = load_settings();
    settings.last_workflow_id = Some(workflow_id.to_string());
    save_settings(&settings)
}

/// Get the last used monitor index.
pub fn get_last_monitor_index() -> Option<i32> {
    load_settings().last_monitor_index
}

/// Save the last used monitor index.
pub fn save_last_monitor_index(monitor_index: i32) -> Result<(), String> {
    info!("Saving last monitor index: {}", monitor_index);
    let mut settings = load_settings();
    settings.last_monitor_index = Some(monitor_index);
    save_settings(&settings)
}

/// Get the last used monitor indices (multi-monitor support).
/// Falls back to legacy single monitor index if not set.
pub fn get_last_monitor_indices() -> Option<Vec<i32>> {
    let settings = load_settings();
    settings
        .last_monitor_indices
        .or_else(|| settings.last_monitor_index.map(|idx| vec![idx]))
}

/// Save the last used monitor indices (multi-monitor support).
pub fn save_last_monitor_indices(monitor_indices: Vec<i32>) -> Result<(), String> {
    info!("Saving last monitor indices: {:?}", monitor_indices);
    let mut settings = load_settings();
    settings.last_monitor_indices = Some(monitor_indices.clone());
    // Also update legacy single monitor for backward compatibility
    if let Some(first) = monitor_indices.first() {
        settings.last_monitor_index = Some(*first);
    }
    save_settings(&settings)
}

/// Get the auto-load last config setting.
pub fn get_auto_load_last_config() -> bool {
    load_settings().auto_load_last_config
}

/// Save the auto-load last config setting.
pub fn save_auto_load_last_config(enabled: bool) -> Result<(), String> {
    info!("Saving auto-load last config setting: {}", enabled);
    let mut settings = load_settings();
    settings.auto_load_last_config = enabled;
    save_settings(&settings)
}

/// Get the auto-continue AI workflow setting.
pub fn get_auto_continue_ai_workflow() -> bool {
    load_settings().auto_continue_ai_workflow
}

/// Save the auto-continue AI workflow setting.
pub fn save_auto_continue_ai_workflow(enabled: bool) -> Result<(), String> {
    info!("Saving auto-continue AI workflow setting: {}", enabled);
    let mut settings = load_settings();
    settings.auto_continue_ai_workflow = enabled;
    save_settings(&settings)
}

/// Get the session auto-fix on failure setting.
#[allow(dead_code)]
pub fn get_session_auto_fix_on_failure() -> bool {
    load_settings().session_auto_fix_on_failure
}

/// Save the session auto-fix on failure setting.
#[allow(dead_code)]
pub fn save_session_auto_fix_on_failure(enabled: bool) -> Result<(), String> {
    info!("Saving session auto-fix on failure setting: {}", enabled);
    let mut settings = load_settings();
    settings.session_auto_fix_on_failure = enabled;
    save_settings(&settings)
}

/// Get the include summary step by default setting.
pub fn get_include_summary_step_by_default() -> bool {
    load_settings().include_summary_step_by_default
}

/// Save the include summary step by default setting.
pub fn save_include_summary_step_by_default(enabled: bool) -> Result<(), String> {
    info!(
        "Saving include summary step by default setting: {}",
        enabled
    );
    let mut settings = load_settings();
    settings.include_summary_step_by_default = enabled;
    save_settings(&settings)
}

/// Check if setup wizard has been completed.
pub fn get_setup_completed() -> bool {
    load_settings().setup_completed
}

/// Mark setup wizard as completed.
pub fn save_setup_completed(completed: bool) -> Result<(), String> {
    let mut settings = load_settings();
    settings.setup_completed = completed;
    save_settings(&settings)
}

/// Get the interactive sessions enabled setting.
pub fn get_interactive_sessions_enabled() -> bool {
    load_settings().ai.interactive_sessions_enabled
}

/// Save the interactive sessions enabled setting.
pub fn save_interactive_sessions_enabled(enabled: bool) -> Result<(), String> {
    info!("Saving interactive sessions enabled setting: {}", enabled);
    let mut settings = load_settings();
    settings.ai.interactive_sessions_enabled = enabled;
    save_settings(&settings)
}

/// Get the user-configured custom UI Bridge discovery ports.
/// These are merged with the hardcoded `DESKTOP_APP_PORTS` at scan time.
pub fn get_discovery_ports() -> Vec<u16> {
    load_settings().discovery_ports
}

/// Save the user-configured custom UI Bridge discovery ports.
/// Values outside the valid port range (1..=65535) are filtered out; duplicates
/// are collapsed. Hardcoded default ports may appear here without effect —
/// `desktop_ports_merged()` in `app_discovery.rs` dedupes against defaults.
pub fn save_discovery_ports(ports: Vec<u16>) -> Result<(), String> {
    let mut cleaned: Vec<u16> = ports.into_iter().filter(|p| *p > 0).collect();
    cleaned.sort_unstable();
    cleaned.dedup();
    info!("Saving {} custom discovery port(s)", cleaned.len());
    let mut settings = load_settings();
    settings.discovery_ports = cleaned;
    save_settings(&settings)
}

/// Get the configured Claude Code config directories.
pub fn get_claude_config_dirs() -> Vec<String> {
    load_settings().claude_config_dirs
}

/// Save the configured Claude Code config directories.
///
/// Writes the machine-global `claude-accounts.json` (NOT the per-instance
/// settings.json): the roster is a machine-global fact and the
/// `load_settings()` overlay makes per-instance copies irrelevant.
pub fn save_claude_config_dirs(dirs: Vec<String>) -> Result<(), String> {
    info!(
        "Saving {} Claude config dirs (machine-global claude-accounts.json)",
        dirs.len()
    );
    crate::claude_accounts::update(move |roster| roster.claude_config_dirs = dirs)
}

/// Get custom launch commands per account config directory.
pub fn get_claude_account_launch_commands() -> std::collections::HashMap<String, String> {
    load_settings().claude_account_launch_commands
}

/// Save custom launch commands per account config directory.
///
/// Writes the machine-global `claude-accounts.json` (NOT the per-instance
/// settings.json) — see `save_claude_config_dirs`.
pub fn save_claude_account_launch_commands(
    commands: std::collections::HashMap<String, String>,
) -> Result<(), String> {
    info!(
        "Saving {} account launch commands (machine-global claude-accounts.json)",
        commands.len()
    );
    crate::claude_accounts::update(move |roster| roster.claude_account_launch_commands = commands)
}

/// Get the dev_logs_dir override (used by paths module).
pub fn get_dev_logs_dir_override() -> Option<String> {
    load_settings().paths.dev_logs_dir
}

/// Save the dev_logs_dir override.
pub fn save_dev_logs_dir(dev_logs_dir: Option<String>) -> Result<(), String> {
    info!("Saving dev_logs_dir: {:?}", dev_logs_dir);
    let mut settings = load_settings();
    settings.paths.dev_logs_dir = dev_logs_dir;
    save_settings(&settings)
}

// ============================================================================
// Log Source Helpers
// ============================================================================

/// Get enabled log sources, optionally filtered by profile.
pub fn get_enabled_log_sources(profile_id: Option<&str>) -> Vec<GlobalLogSource> {
    let settings: GlobalLogSourceSettings = get_setting();

    // If profile specified, filter by profile's source_ids
    if let Some(pid) = profile_id {
        if let Some(profile) = settings.profiles.iter().find(|p| p.id == pid) {
            return settings
                .sources
                .into_iter()
                .filter(|s| s.enabled && profile.source_ids.contains(&s.id))
                .collect();
        }
    }

    // If default profile is set and no explicit profile, use default
    if let Some(default_pid) = &settings.default_profile_id {
        if let Some(profile) = settings.profiles.iter().find(|p| p.id == *default_pid) {
            return settings
                .sources
                .into_iter()
                .filter(|s| s.enabled && profile.source_ids.contains(&s.id))
                .collect();
        }
    }

    // Fall back to all enabled sources if configured to do so
    if settings.include_all_when_no_profile {
        settings.sources.into_iter().filter(|s| s.enabled).collect()
    } else {
        Vec::new()
    }
}

/// Seed default log sources if the sources list is empty.
///
/// Called on startup to ensure there are always some log sources configured.
/// If the user has any sources (even disabled ones), this is a no-op.
/// Creates 4 default sources: Backend, Frontend, Runner, Runner Actions.
pub fn seed_default_log_sources_if_empty() {
    let settings: GlobalLogSourceSettings = get_setting();
    if !settings.sources.is_empty() {
        return;
    }

    info!("No log sources configured, seeding defaults");
    let dev_logs_dir = crate::paths::get_dev_logs_dir();
    let dev_logs = dev_logs_dir.to_string_lossy().to_string();

    let defaults = vec![
        GlobalLogSource {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Backend".to_string(),
            description: "FastAPI backend logs including HTTP requests and errors".to_string(),
            category: LogSourceCategory::Backend,
            source_type: "file".to_string(),
            path: format!("{}/backend.log", dev_logs.replace('\\', "/")),
            pattern: None,
            tail_lines: 100,
            enabled: true,
            color: Some("#22c55e".to_string()),
            keywords: vec![
                "python".to_string(),
                "fastapi".to_string(),
                "http".to_string(),
                "api".to_string(),
            ],
            format: "plaintext".to_string(),
            parser: "python".to_string(),
            timestamp_pattern: None,
            timezone: "local".to_string(),
            error_patterns: vec![],
            warning_patterns: vec![],
            ignore_patterns: vec![],
            poll_interval_ms: 5000,
        },
        GlobalLogSource {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Frontend".to_string(),
            description: "Next.js frontend logs including build and runtime errors".to_string(),
            category: LogSourceCategory::Frontend,
            source_type: "file".to_string(),
            path: format!("{}/frontend.log", dev_logs.replace('\\', "/")),
            pattern: None,
            tail_lines: 100,
            enabled: true,
            color: Some("#3b82f6".to_string()),
            keywords: vec![
                "javascript".to_string(),
                "nextjs".to_string(),
                "react".to_string(),
                "frontend".to_string(),
            ],
            format: "plaintext".to_string(),
            parser: "javascript".to_string(),
            timestamp_pattern: None,
            timezone: "local".to_string(),
            error_patterns: vec![],
            warning_patterns: vec![],
            ignore_patterns: vec![],
            poll_interval_ms: 5000,
        },
        GlobalLogSource {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Runner".to_string(),
            description: "Qontinui runner Vite + Rust logs".to_string(),
            category: LogSourceCategory::Runner,
            source_type: "file".to_string(),
            path: format!("{}/runner-tauri.log", dev_logs.replace('\\', "/")),
            pattern: None,
            tail_lines: 100,
            enabled: true,
            color: Some("#f97316".to_string()),
            keywords: vec![
                "rust".to_string(),
                "tauri".to_string(),
                "runner".to_string(),
                "vite".to_string(),
            ],
            format: "plaintext".to_string(),
            parser: "rust".to_string(),
            timestamp_pattern: None,
            timezone: "local".to_string(),
            error_patterns: vec![],
            warning_patterns: vec![],
            ignore_patterns: vec![],
            poll_interval_ms: 5000,
        },
        GlobalLogSource {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Runner Actions".to_string(),
            description: "Workflow action execution events in JSONL format".to_string(),
            category: LogSourceCategory::Runner,
            source_type: "file".to_string(),
            path: format!("{}/runner-actions.jsonl", dev_logs.replace('\\', "/")),
            pattern: None,
            tail_lines: 100,
            enabled: true,
            color: Some("#a855f7".to_string()),
            keywords: vec![
                "actions".to_string(),
                "workflow".to_string(),
                "execution".to_string(),
            ],
            format: "jsonl".to_string(),
            parser: "generic".to_string(),
            timestamp_pattern: None,
            timezone: "local".to_string(),
            error_patterns: vec![],
            warning_patterns: vec![],
            ignore_patterns: vec![],
            poll_interval_ms: 5000,
        },
    ];

    let new_settings = GlobalLogSourceSettings {
        sources: defaults,
        ..settings
    };

    if let Err(e) = save_setting(new_settings) {
        tracing::error!("Failed to seed default log sources: {}", e);
    } else {
        info!("Seeded 4 default log sources");
    }
}

/// Get log sources for AI selection prompt.
/// Returns sources with their descriptions and categories for AI to choose from.
pub fn get_log_sources_for_ai_selection() -> Vec<GlobalLogSource> {
    let settings: GlobalLogSourceSettings = get_setting();
    settings.sources.into_iter().filter(|s| s.enabled).collect()
}

/// Get the AI selection mode.
pub fn get_log_source_ai_selection_mode() -> LogSourceAiSelectionMode {
    let settings: GlobalLogSourceSettings = get_setting();
    settings.ai_selection_mode
}

// ============================================================================
// Managed Processes Helpers
// ============================================================================

/// Get all managed process configurations.
pub fn get_managed_process_configs() -> Vec<crate::process_capture::ProcessConfig> {
    load_settings().managed_processes
}

/// Save a managed process config (add or update).
pub fn save_managed_process_config(
    config: crate::process_capture::ProcessConfig,
) -> Result<(), String> {
    let mut settings = load_settings();
    if let Some(existing) = settings
        .managed_processes
        .iter_mut()
        .find(|p| p.id == config.id)
    {
        *existing = config;
    } else {
        settings.managed_processes.push(config);
    }
    save_settings(&settings)
}

/// Delete a managed process config by ID.
pub fn delete_managed_process_config(id: &str) -> Result<(), String> {
    let mut settings = load_settings();
    settings.managed_processes.retain(|p| p.id != id);
    save_settings(&settings)
}

/// Replace the entire managed-process list in one atomic save.
///
/// Used by the dev-service upgrade/dedupe pass at boot. Caller has already
/// reasoned about the full list, so a per-entry save loop would needlessly
/// re-read and re-write settings N times.
pub fn replace_managed_process_configs(
    configs: Vec<crate::process_capture::ProcessConfig>,
) -> Result<(), String> {
    let mut settings = load_settings();
    settings.managed_processes = configs;
    save_settings(&settings)
}

// ============================================================================
// Execution Variables Helpers
// ============================================================================

/// Get the resolved auth token based on current settings.
///
/// Returns the auth token to use based on the configured auth source:
/// - Captured: Returns None (use original captured headers)
/// - Manual: Returns the manually configured token
/// - Environment: Returns the value from the configured environment variable
pub fn get_resolved_auth_token() -> Option<String> {
    let settings: ExecutionVariablesSettings = get_setting();

    match settings.auth_source {
        AuthSource::Captured => None,
        AuthSource::Manual => settings.auth_token,
        AuthSource::Environment => std::env::var(&settings.auth_env_var).ok(),
    }
}

/// Get resolved custom variables.
///
/// Returns all custom variables with their resolved values.
/// For environment variables, resolves them from the environment.
pub fn get_resolved_custom_variables() -> HashMap<String, String> {
    let settings: ExecutionVariablesSettings = get_setting();
    let mut resolved = HashMap::new();

    for var in settings.custom_variables {
        let value = match var.source {
            VariableSource::Manual => var.value,
            VariableSource::Environment => var
                .env_var
                .and_then(|env_name| std::env::var(&env_name).ok()),
        };

        if let Some(v) = value {
            resolved.insert(var.name, v);
        }
    }

    resolved
}

// ============================================================================
// Runner Instance CRUD Helpers
// ============================================================================

/// Once-per-process guard for the duplicate-port warning emitted by
/// `get_runner_instances`. Restarting the runner re-arms it, so a user who
/// fixes their duplicates can confirm by reloading and seeing no warning.
static DUPLICATE_PORT_WARNING_FIRED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

/// Walk `runner_instances` for duplicate ports and emit a structured warning
/// per offending port. Logged at most once per process via the OnceLock guard
/// when called from `get_runner_instances`; called eagerly from
/// `save_runner_instance` to catch newly-introduced duplicates.
fn warn_on_duplicate_ports(instances: &[RunnerInstanceConfig]) {
    let mut by_port: HashMap<u16, Vec<&str>> = HashMap::new();
    for inst in instances {
        by_port.entry(inst.port).or_default().push(&inst.name);
    }
    for (port, names) in by_port.iter() {
        if names.len() > 1 {
            tracing::warn!(
                target = "config_facade",
                "settings.json runner_instances has duplicate port {}: {:?}. \
                 Consumers that dedupe by port (the Tauri get_runner_instances \
                 fallback in the Orchestration Loop picker) will silently drop \
                 all but one entry. Rename or delete the duplicates in \
                 Settings → Runner Instances.",
                port,
                names
            );
        }
    }
}

/// Get all configured runner instances.
pub fn get_runner_instances() -> Vec<RunnerInstanceConfig> {
    let instances = load_settings().runner_instances;
    DUPLICATE_PORT_WARNING_FIRED.get_or_init(|| warn_on_duplicate_ports(&instances));
    instances
}

/// Save or update a runner instance configuration.
pub fn save_runner_instance(config: RunnerInstanceConfig) -> Result<(), String> {
    let mut settings = load_settings();
    if let Some(existing) = settings
        .runner_instances
        .iter_mut()
        .find(|i| i.id == config.id)
    {
        existing.name = config.name;
        existing.port = config.port;
    } else {
        settings.runner_instances.push(config);
    }
    save_settings(&settings)?;
    // After every save, re-check the full set for duplicates — catches the
    // moment when a user (or the auto-suggest port logic, if it ever drifts)
    // introduces a collision with an existing slot.
    warn_on_duplicate_ports(&settings.runner_instances);
    Ok(())
}

/// Delete a runner instance configuration by ID.
pub fn delete_runner_instance(id: &str) -> Result<(), String> {
    let mut settings = load_settings();
    settings.runner_instances.retain(|i| i.id != id);
    save_settings(&settings)
}

// ============================================================================
// Temp Spawn Placement CRUD Helpers
// ============================================================================

/// Get the list of temp-runner spawn placements. Used by the supervisor when
/// spawning temp runners — separate from per-named-instance placements stored
/// in `runner_instances[i].spawn_placement`.
pub fn get_temp_spawn_placements() -> Vec<SpawnPlacement> {
    load_settings().temp_spawn_placements
}

/// Replace the temp-runner spawn placement list with the supplied list.
/// Returns the persisted list on success.
pub fn save_temp_spawn_placements(placements: Vec<SpawnPlacement>) -> Result<(), String> {
    let mut settings = load_settings();
    settings.temp_spawn_placements = placements;
    save_settings(&settings)
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
        assert_eq!(
            ExecutionVariablesSettings::field_name(),
            "execution_variables"
        );
        assert_eq!(GlobalLogSourceSettings::field_name(), "log_sources");
        assert_eq!(CloudRelaySettings::field_name(), "cloud_relay");
    }
}
