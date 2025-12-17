//! AI settings commands
//!
//! This module handles AI configuration operations:
//! - Getting and setting AI provider settings
//! - Secure API key storage in OS keychain
//! - Testing AI connections

use crate::settings::{
    self, AiProvider, AiSettings, ClaudeApiSettings, ClaudeCliSettings, CliExecutionMode,
};
use anyhow::{Context, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use tracing::info;

use super::CommandResponse;

/// Service name for AI API keys in the keychain
const AI_SERVICE_NAME: &str = "com.qontinui.runner.ai";

// ============================================================================
// Keychain Operations for API Keys
// ============================================================================

/// Stores an AI API key in the OS keychain
fn store_ai_api_key(provider: &str, api_key: &str) -> Result<()> {
    let entry = Entry::new(AI_SERVICE_NAME, provider)
        .context("Failed to create keychain entry for AI API key")?;
    entry
        .set_password(api_key)
        .context("Failed to store AI API key in keychain")?;
    info!("AI API key stored for provider: {}", provider);
    Ok(())
}

/// Retrieves an AI API key from the OS keychain
fn get_ai_api_key(provider: &str) -> Result<Option<String>> {
    let entry = Entry::new(AI_SERVICE_NAME, provider)
        .context("Failed to create keychain entry for AI API key")?;
    match entry.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(anyhow::anyhow!("Failed to retrieve AI API key: {}", e)),
    }
}

/// Deletes an AI API key from the OS keychain
fn delete_ai_api_key(provider: &str) -> Result<()> {
    let entry = Entry::new(AI_SERVICE_NAME, provider)
        .context("Failed to create keychain entry for AI API key")?;
    match entry.delete_credential() {
        Ok(_) => {
            info!("AI API key deleted for provider: {}", provider);
            Ok(())
        }
        Err(keyring::Error::NoEntry) => {
            info!("No AI API key found for provider: {}", provider);
            Ok(())
        }
        Err(e) => Err(anyhow::anyhow!("Failed to delete AI API key: {}", e)),
    }
}

// ============================================================================
// Response Types
// ============================================================================

/// Result of testing an AI connection
#[derive(Debug, Serialize, Deserialize)]
pub struct AiConnectionTestResult {
    pub success: bool,
    pub message: String,
    pub provider: String,
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Get the current AI settings.
///
/// Returns the AI settings stored in the persistent settings file.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with AI settings data
/// * `Err(String)` - Error message if settings cannot be loaded
#[tauri::command]
pub fn get_ai_settings() -> Result<CommandResponse, String> {
    info!("Getting AI settings");

    let ai_settings = settings::get_ai_settings();

    Ok(CommandResponse {
        success: true,
        message: Some("AI settings retrieved".to_string()),
        data: Some(
            serde_json::to_value(&ai_settings)
                .map_err(|e| format!("Failed to serialize AI settings: {}", e))?,
        ),
    })
}

/// Save AI settings.
///
/// Updates the AI settings in the persistent settings file.
///
/// # Arguments
/// * `provider` - AI provider ("claude_cli" or "claude_api")
/// * `execution_mode` - CLI execution mode ("auto", "windows_native", "wsl", "native")
/// * `custom_path` - Optional custom path to claude executable
/// * `timeout_seconds` - CLI timeout in seconds
/// * `model` - Claude API model name
/// * `max_tokens` - Maximum tokens for API calls
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if settings cannot be saved
#[tauri::command]
pub fn save_ai_settings(
    provider: String,
    execution_mode: String,
    custom_path: Option<String>,
    timeout_seconds: u64,
    model: String,
    max_tokens: u32,
) -> Result<CommandResponse, String> {
    info!(
        "Saving AI settings: provider={}, execution_mode={}, timeout={}s",
        provider, execution_mode, timeout_seconds
    );

    let ai_provider = match provider.as_str() {
        "claude_cli" => AiProvider::ClaudeCli,
        "claude_api" => AiProvider::ClaudeApi,
        _ => return Err(format!("Invalid provider: {}", provider)),
    };

    let cli_execution_mode = match execution_mode.as_str() {
        "auto" => CliExecutionMode::Auto,
        "windows_native" => CliExecutionMode::WindowsNative,
        "wsl" => CliExecutionMode::Wsl,
        "native" => CliExecutionMode::Native,
        _ => return Err(format!("Invalid execution mode: {}", execution_mode)),
    };

    let ai_settings = AiSettings {
        provider: ai_provider,
        claude_cli: ClaudeCliSettings {
            execution_mode: cli_execution_mode,
            custom_path,
            timeout_seconds,
        },
        claude_api: ClaudeApiSettings { model, max_tokens },
    };

    settings::save_ai_settings(ai_settings)
        .map_err(|e| format!("Failed to save AI settings: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("AI settings saved".to_string()),
        data: None,
    })
}

/// Save an AI API key to the secure keychain.
///
/// # Arguments
/// * `provider` - Provider identifier (e.g., "claude_api")
/// * `api_key` - The API key to store
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if key cannot be saved
#[tauri::command]
pub fn save_ai_api_key_command(
    provider: String,
    api_key: String,
) -> Result<CommandResponse, String> {
    info!("Saving AI API key for provider: {}", provider);

    store_ai_api_key(&provider, &api_key).map_err(|e| format!("Failed to save API key: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("API key saved for {}", provider)),
        data: None,
    })
}

/// Delete an AI API key from the secure keychain.
///
/// # Arguments
/// * `provider` - Provider identifier (e.g., "claude_api")
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if key cannot be deleted
#[tauri::command]
pub fn delete_ai_api_key_command(provider: String) -> Result<CommandResponse, String> {
    info!("Deleting AI API key for provider: {}", provider);

    delete_ai_api_key(&provider).map_err(|e| format!("Failed to delete API key: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("API key deleted for {}", provider)),
        data: None,
    })
}

/// Check if an AI API key exists in the keychain.
///
/// # Arguments
/// * `provider` - Provider identifier (e.g., "claude_api")
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with `has_key` boolean in data
/// * `Err(String)` - Error message if check fails
#[tauri::command]
pub fn has_ai_api_key(provider: String) -> Result<CommandResponse, String> {
    info!("Checking if AI API key exists for provider: {}", provider);

    let has_key = get_ai_api_key(&provider)
        .map_err(|e| format!("Failed to check API key: {}", e))?
        .is_some();

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "has_key": has_key })),
    })
}

/// Test the AI connection based on current settings.
///
/// For Claude CLI: Attempts to run a simple command and checks for response.
/// For Claude API: Makes a test API call to verify the key is valid.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with test result
/// * `Err(String)` - Error message if test fails
#[tauri::command]
pub async fn test_ai_connection() -> Result<CommandResponse, String> {
    info!("Testing AI connection");

    let ai_settings = settings::get_ai_settings();

    let result = match ai_settings.provider {
        AiProvider::ClaudeCli => test_claude_cli_connection(&ai_settings.claude_cli).await,
        AiProvider::ClaudeApi => test_claude_api_connection(&ai_settings.claude_api).await,
    };

    let test_result = match result {
        Ok(msg) => AiConnectionTestResult {
            success: true,
            message: msg,
            provider: format!("{:?}", ai_settings.provider),
        },
        Err(e) => AiConnectionTestResult {
            success: false,
            message: e,
            provider: format!("{:?}", ai_settings.provider),
        },
    };

    Ok(CommandResponse {
        success: test_result.success,
        message: Some(test_result.message.clone()),
        data: Some(
            serde_json::to_value(&test_result)
                .map_err(|e| format!("Failed to serialize test result: {}", e))?,
        ),
    })
}

/// Test Claude CLI connection
async fn test_claude_cli_connection(settings: &ClaudeCliSettings) -> Result<String, String> {
    let system = std::env::consts::OS;

    // Determine effective execution mode
    let effective_mode = match settings.execution_mode {
        CliExecutionMode::Auto => {
            if system == "windows" {
                CliExecutionMode::WindowsNative
            } else {
                CliExecutionMode::Native
            }
        }
        mode => mode,
    };

    // Get the claude program name (custom or default)
    let claude_program = settings.custom_path.as_deref().unwrap_or("claude");

    info!(
        "Testing CLI connection with mode: {:?}, program: {}",
        effective_mode, claude_program
    );

    let output = match effective_mode {
        CliExecutionMode::WindowsNative | CliExecutionMode::Auto => {
            // On Windows, use cmd.exe /c to handle .cmd files from npm install
            tokio::process::Command::new("cmd.exe")
                .args(["/c", claude_program, "--version"])
                .output()
                .await
                .map_err(|e| {
                    format!(
                        "Failed to execute claude CLI: {}. Is Claude Code installed and in PATH?",
                        e
                    )
                })?
        }
        CliExecutionMode::Wsl => tokio::process::Command::new("wsl")
            .args([claude_program, "--version"])
            .output()
            .await
            .map_err(|e| format!("Failed to execute claude via WSL: {}. Is WSL installed?", e))?,
        CliExecutionMode::Native => tokio::process::Command::new(claude_program)
            .args(["--version"])
            .output()
            .await
            .map_err(|e| {
                format!(
                    "Failed to execute claude CLI: {}. Is Claude Code installed and in PATH?",
                    e
                )
            })?,
    };

    if output.status.success() {
        let version = String::from_utf8_lossy(&output.stdout);
        Ok(format!(
            "Claude CLI connected successfully. {}",
            version.trim()
        ))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("Claude CLI returned error: {}", stderr.trim()))
    }
}

/// Test Claude API connection
async fn test_claude_api_connection(settings: &ClaudeApiSettings) -> Result<String, String> {
    // Get API key from keychain
    let api_key = get_ai_api_key("claude_api")
        .map_err(|e| format!("Failed to retrieve API key: {}", e))?
        .ok_or_else(|| "No API key configured. Please enter your Claude API key.".to_string())?;

    info!(
        "Testing Claude API connection with model: {}",
        settings.model
    );

    // Make a minimal API request to test the connection
    let client = reqwest::Client::new();
    let response = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "model": settings.model,
            "max_tokens": 10,
            "messages": [{"role": "user", "content": "Hi"}]
        }))
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if response.status().is_success() {
        Ok(format!(
            "Claude API connected successfully using model: {}",
            settings.model
        ))
    } else {
        let status = response.status();
        let error_body = response.text().await.unwrap_or_default();

        // Parse error for user-friendly message
        if status.as_u16() == 401 {
            Err("Invalid API key. Please check your API key and try again.".to_string())
        } else if status.as_u16() == 404 {
            Err(format!(
                "Model '{}' not found. Please check the model name.",
                settings.model
            ))
        } else {
            Err(format!("API error ({}): {}", status, error_body))
        }
    }
}

/// Get the API key for a provider (used by mcp_api.rs for API calls)
#[allow(dead_code)]
pub fn get_provider_api_key(provider: &str) -> Result<Option<String>, String> {
    get_ai_api_key(provider).map_err(|e| format!("Failed to get API key: {}", e))
}
