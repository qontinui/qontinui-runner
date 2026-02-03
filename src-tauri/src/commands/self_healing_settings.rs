//! Self-Healing settings commands
//!
//! This module handles self-healing configuration operations:
//! - Getting and setting self-healing settings
//! - Secure API key storage in OS keychain for remote LLM providers (using KeychainHelper)

use crate::config_facade::self_healing_keychain;
use crate::settings::{self, SelfHealingApiProvider, SelfHealingLlmMode, SelfHealingSettings};
use anyhow::Result;
use tracing::info;

use super::CommandResponse;

// ============================================================================
// Keychain Operations for API Keys
// ============================================================================

/// Stores a self-healing API key in the OS keychain
fn store_self_healing_api_key(provider: &str, api_key: &str) -> Result<()> {
    self_healing_keychain().store(provider, api_key)
}

/// Retrieves a self-healing API key from the OS keychain
fn get_self_healing_api_key(provider: &str) -> Result<Option<String>> {
    self_healing_keychain().get(provider)
}

/// Deletes a self-healing API key from the OS keychain
fn delete_api_key_from_keychain(provider: &str) -> Result<()> {
    self_healing_keychain().delete(provider)
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Get the current self-healing settings.
///
/// Returns the self-healing settings stored in the persistent settings file.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with self-healing settings data
/// * `Err(String)` - Error message if settings cannot be loaded
#[tauri::command]
pub fn get_self_healing_settings() -> Result<CommandResponse, String> {
    info!("Getting self-healing settings");

    let self_healing_settings = settings::get_self_healing_settings();

    Ok(CommandResponse {
        success: true,
        message: Some("Self-healing settings retrieved".to_string()),
        data: Some(
            serde_json::to_value(&self_healing_settings)
                .map_err(|e| format!("Failed to serialize self-healing settings: {}", e))?,
        ),
    })
}

/// Save self-healing settings.
///
/// Updates the self-healing settings in the persistent settings file.
///
/// # Arguments
/// * `action_caching_enabled` - Enable action caching
/// * `cache_ttl_seconds` - Cache TTL in seconds
/// * `visual_validation_enabled` - Enable visual validation
/// * `llm_mode` - LLM mode ("disabled", "local_ollama", "remote_api")
/// * `ollama_model` - Ollama model name
/// * `api_provider` - API provider ("open_ai", "anthropic")
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if settings cannot be saved
#[tauri::command]
pub fn save_self_healing_settings(
    action_caching_enabled: bool,
    cache_ttl_seconds: u32,
    visual_validation_enabled: bool,
    llm_mode: String,
    ollama_model: String,
    api_provider: String,
) -> Result<CommandResponse, String> {
    info!(
        "Saving self-healing settings: caching={}, ttl={}s, validation={}, llm_mode={}, ollama_model={}, api_provider={}",
        action_caching_enabled, cache_ttl_seconds, visual_validation_enabled, llm_mode, ollama_model, api_provider
    );

    let llm_mode_enum = match llm_mode.as_str() {
        "disabled" => SelfHealingLlmMode::Disabled,
        "local_ollama" => SelfHealingLlmMode::LocalOllama,
        "remote_api" => SelfHealingLlmMode::RemoteApi,
        _ => return Err(format!("Invalid LLM mode: {}", llm_mode)),
    };

    let api_provider_enum = match api_provider.as_str() {
        "open_ai" => SelfHealingApiProvider::OpenAi,
        "anthropic" => SelfHealingApiProvider::Anthropic,
        _ => return Err(format!("Invalid API provider: {}", api_provider)),
    };

    let self_healing_settings = SelfHealingSettings {
        action_caching_enabled,
        cache_ttl_seconds,
        visual_validation_enabled,
        llm_mode: llm_mode_enum,
        ollama_model,
        api_provider: api_provider_enum,
    };

    settings::save_self_healing_settings(self_healing_settings)
        .map_err(|e| format!("Failed to save self-healing settings: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Self-healing settings saved".to_string()),
        data: None,
    })
}

/// Save a self-healing API key to the secure keychain.
///
/// # Arguments
/// * `provider` - Provider identifier ("open_ai" or "anthropic")
/// * `api_key` - The API key to store
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if key cannot be saved
#[tauri::command]
pub fn save_self_healing_api_key(
    provider: String,
    api_key: String,
) -> Result<CommandResponse, String> {
    info!("Saving self-healing API key for provider: {}", provider);

    store_self_healing_api_key(&provider, &api_key)
        .map_err(|e| format!("Failed to save API key: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("API key saved for {}", provider)),
        data: None,
    })
}

/// Delete a self-healing API key from the secure keychain.
///
/// # Arguments
/// * `provider` - Provider identifier ("open_ai" or "anthropic")
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if key cannot be deleted
#[tauri::command]
pub fn delete_self_healing_api_key(provider: String) -> Result<CommandResponse, String> {
    info!("Deleting self-healing API key for provider: {}", provider);

    delete_api_key_from_keychain(&provider)
        .map_err(|e| format!("Failed to delete API key: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("API key deleted for {}", provider)),
        data: None,
    })
}

/// Check if a self-healing API key exists in the keychain.
///
/// # Arguments
/// * `provider` - Provider identifier ("open_ai" or "anthropic")
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with `has_key` boolean in data
/// * `Err(String)` - Error message if check fails
#[tauri::command]
pub fn has_self_healing_api_key(provider: String) -> Result<CommandResponse, String> {
    info!(
        "Checking if self-healing API key exists for provider: {}",
        provider
    );

    let has_key = get_self_healing_api_key(&provider)
        .map_err(|e| format!("Failed to check API key: {}", e))?
        .is_some();

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "has_key": has_key })),
    })
}

/// Get the self-healing API key for a provider (used internally)
#[allow(dead_code)]
pub fn get_provider_api_key(provider: &str) -> Result<Option<String>, String> {
    get_self_healing_api_key(provider).map_err(|e| format!("Failed to get API key: {}", e))
}
