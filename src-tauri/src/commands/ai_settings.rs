//! AI settings commands
//!
//! This module handles AI configuration operations:
//! - Getting and setting AI provider settings
//! - Secure API key storage in OS keychain (using KeychainHelper from config_facade)
//! - Testing AI connections

use crate::ai_router::RoutingConfig;
use crate::config_facade::ai_keychain;
use crate::error::AppError;
use crate::orchestrator::{CompressionConfig, RetryConfig};
use crate::settings::{
    self, AccountSelectionMode, AiProvider, AiSettings, ClaudeApiSettings, ClaudeCliSettings,
    CliExecutionMode, GeminiApiSettings, GeminiAuthMethod, GeminiCliSettings, OllamaSettings,
    OpenAiCompatibleSettings,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tracing::info;

use super::CommandResponse;

// ============================================================================
// Keychain Operations for API Keys
// ============================================================================

/// Stores an AI API key in the OS keychain
fn store_ai_api_key(provider: &str, api_key: &str) -> Result<()> {
    ai_keychain().store(provider, api_key)
}

/// Retrieves an AI API key from the OS keychain
fn get_ai_api_key(provider: &str) -> Result<Option<String>> {
    ai_keychain().get(provider)
}

/// Deletes an AI API key from the OS keychain
fn delete_ai_api_key(provider: &str) -> Result<()> {
    ai_keychain().delete(provider)
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
    get_ai_settings_impl().map_err(String::from)
}

fn get_ai_settings_impl() -> Result<CommandResponse, AppError> {
    info!("Getting AI settings");

    let ai_settings = settings::get_ai_settings();

    Ok(CommandResponse {
        success: true,
        message: Some("AI settings retrieved".to_string()),
        data: Some(serde_json::to_value(&ai_settings)?),
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
/// * `config_dir` - Optional CLAUDE_CONFIG_DIR for multi-account support
/// * `model` - Claude API model name
/// * `max_tokens` - Maximum tokens for API calls
/// * `auto_refine_video_after_iterations` - Default iteration threshold for video in auto-refine
/// * `interactive_sessions_enabled` - Enable interactive bidirectional CLI sessions
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
    config_dir: Option<String>,
    account_selection_mode: Option<String>,
    model: String,
    max_tokens: u32,
    auto_refine_video_after_iterations: Option<u32>,
    interactive_sessions_enabled: Option<bool>,
    memory_federation_enabled: Option<bool>,
) -> Result<CommandResponse, String> {
    info!(
        "Saving AI settings: provider={}, execution_mode={}, timeout={}s, config_dir={:?}, account_selection={:?}, video_after_iterations={:?}, interactive={:?}, memory_federation={:?}",
        provider, execution_mode, timeout_seconds, config_dir, account_selection_mode, auto_refine_video_after_iterations, interactive_sessions_enabled, memory_federation_enabled
    );

    let ai_provider = match provider.as_str() {
        "claude_cli" => AiProvider::ClaudeCli,
        "claude_api" => AiProvider::ClaudeApi,
        "gemini_cli" => AiProvider::GeminiCli,
        "gemini_api" => AiProvider::GeminiApi,
        "ollama" => AiProvider::Ollama,
        "openai_compatible" => AiProvider::OpenAiCompatible,
        _ => return Err(format!("Invalid provider: {}", provider)),
    };

    let cli_execution_mode = match execution_mode.as_str() {
        "auto" => CliExecutionMode::Auto,
        "windows_native" => CliExecutionMode::WindowsNative,
        "wsl" => CliExecutionMode::Wsl,
        "native" => CliExecutionMode::Native,
        _ => return Err(format!("Invalid execution mode: {}", execution_mode)),
    };

    // Get existing settings to preserve Gemini configuration when saving Claude settings
    let existing_settings = settings::get_ai_settings();

    let selection_mode = match account_selection_mode.as_deref() {
        Some("least_usage") => AccountSelectionMode::LeastUsage,
        _ => AccountSelectionMode::Manual,
    };

    let ai_settings = AiSettings {
        provider: ai_provider,
        claude_cli: ClaudeCliSettings {
            execution_mode: cli_execution_mode,
            custom_path,
            timeout_seconds,
            config_dir,
            account_selection_mode: selection_mode,
        },
        claude_api: ClaudeApiSettings { model, max_tokens },
        // Preserve existing Gemini settings
        gemini_cli: existing_settings.gemini_cli,
        gemini_api: existing_settings.gemini_api,
        // Preserve existing Ollama / OpenAI-compatible settings (Tier 0 substrate)
        ollama: existing_settings.ollama,
        openai_compatible: existing_settings.openai_compatible,
        auto_refine_video_after_iterations: auto_refine_video_after_iterations.unwrap_or(3),
        // Preserve compression, retry, and routing settings
        compression: existing_settings.compression,
        retry: existing_settings.retry,
        routing: existing_settings.routing,
        interactive_sessions_enabled: interactive_sessions_enabled
            .unwrap_or(existing_settings.interactive_sessions_enabled),
        ai_path_prediction_enabled: existing_settings.ai_path_prediction_enabled,
        memory_federation_enabled: memory_federation_enabled
            .unwrap_or(existing_settings.memory_federation_enabled),
    };

    settings::save_ai_settings(ai_settings).map_err(|e| {
        String::from(AppError::ConfigError(format!(
            "Failed to save AI settings: {}",
            e
        )))
    })?;

    Ok(CommandResponse {
        success: true,
        message: Some("AI settings saved".to_string()),
        data: None,
    })
}

/// Save Gemini-specific AI settings.
///
/// Updates the Gemini settings in the persistent settings file.
/// NOTE: This function preserves the existing provider selection - it only updates Gemini-specific settings.
///
/// # Arguments
/// * `execution_mode` - CLI execution mode ("auto", "windows_native", "wsl", "native")
/// * `custom_path` - Optional custom path to gemini executable
/// * `timeout_seconds` - CLI timeout in seconds
/// * `auth_method` - Gemini CLI auth method ("oauth" or "api_key")
/// * `model` - Gemini model name (e.g., "gemini-3-flash-preview")
/// * `max_output_tokens` - Maximum output tokens for API calls
/// * `temperature` - Temperature for API calls
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if settings cannot be saved
#[tauri::command]
pub fn save_gemini_settings(
    execution_mode: String,
    custom_path: Option<String>,
    timeout_seconds: u64,
    auth_method: String,
    model: String,
    max_output_tokens: u32,
    temperature: f32,
) -> Result<CommandResponse, String> {
    info!(
        "Saving Gemini settings: model={}, auth={}",
        model, auth_method
    );

    let cli_execution_mode = match execution_mode.as_str() {
        "auto" => CliExecutionMode::Auto,
        "windows_native" => CliExecutionMode::WindowsNative,
        "wsl" => CliExecutionMode::Wsl,
        "native" => CliExecutionMode::Native,
        _ => return Err(format!("Invalid execution mode: {}", execution_mode)),
    };

    let gemini_auth_method = match auth_method.as_str() {
        "oauth" | "o_auth" => GeminiAuthMethod::OAuth,
        "api_key" => GeminiAuthMethod::ApiKey,
        _ => return Err(format!("Invalid auth method: {}", auth_method)),
    };

    // Get existing settings to preserve Claude configuration AND provider selection
    let existing_settings = settings::get_ai_settings();

    let ai_settings = AiSettings {
        // IMPORTANT: Preserve the existing provider - don't overwrite it!
        provider: existing_settings.provider,
        // Preserve existing Claude settings
        claude_cli: existing_settings.claude_cli,
        claude_api: existing_settings.claude_api,
        gemini_cli: GeminiCliSettings {
            execution_mode: cli_execution_mode,
            custom_path,
            timeout_seconds,
            auth_method: gemini_auth_method,
            model: model.clone(),
        },
        gemini_api: GeminiApiSettings {
            model,
            max_output_tokens,
            temperature,
        },
        // Preserve existing Ollama / OpenAI-compatible settings
        ollama: existing_settings.ollama,
        openai_compatible: existing_settings.openai_compatible,
        auto_refine_video_after_iterations: existing_settings.auto_refine_video_after_iterations,
        // Preserve compression, retry, routing, and interactive settings
        compression: existing_settings.compression,
        retry: existing_settings.retry,
        routing: existing_settings.routing,
        interactive_sessions_enabled: existing_settings.interactive_sessions_enabled,
        ai_path_prediction_enabled: existing_settings.ai_path_prediction_enabled,
        memory_federation_enabled: existing_settings.memory_federation_enabled,
    };

    settings::save_ai_settings(ai_settings).map_err(|e| {
        String::from(AppError::ConfigError(format!(
            "Failed to save Gemini settings: {}",
            e
        )))
    })?;

    Ok(CommandResponse {
        success: true,
        message: Some("Gemini settings saved".to_string()),
        data: None,
    })
}

/// Save Ollama-specific AI settings (Tier 0).
///
/// Updates the Ollama settings in the persistent settings file while preserving
/// every other provider's configuration AND the current `provider` selection.
/// Symmetric to `save_gemini_settings` so the React-side wiring is uniform.
///
/// # Arguments
/// * `base_url` - Ollama HTTP API base URL (e.g. "http://127.0.0.1:11434")
/// * `model` - Model identifier (e.g. "llama3.1:8b")
/// * `timeout_seconds` - Per-request timeout
#[tauri::command]
pub fn save_ollama_settings(
    base_url: String,
    model: String,
    timeout_seconds: u64,
) -> Result<CommandResponse, String> {
    info!(
        "Saving Ollama settings: base_url={}, model={}, timeout={}s",
        base_url, model, timeout_seconds
    );

    let existing_settings = settings::get_ai_settings();
    let ai_settings = AiSettings {
        // IMPORTANT: preserve the existing provider — don't auto-switch to Ollama.
        provider: existing_settings.provider,
        claude_cli: existing_settings.claude_cli,
        claude_api: existing_settings.claude_api,
        gemini_cli: existing_settings.gemini_cli,
        gemini_api: existing_settings.gemini_api,
        ollama: OllamaSettings {
            base_url,
            model,
            timeout_seconds,
        },
        openai_compatible: existing_settings.openai_compatible,
        auto_refine_video_after_iterations: existing_settings.auto_refine_video_after_iterations,
        compression: existing_settings.compression,
        retry: existing_settings.retry,
        routing: existing_settings.routing,
        interactive_sessions_enabled: existing_settings.interactive_sessions_enabled,
        ai_path_prediction_enabled: existing_settings.ai_path_prediction_enabled,
        memory_federation_enabled: existing_settings.memory_federation_enabled,
    };

    settings::save_ai_settings(ai_settings).map_err(|e| {
        String::from(AppError::ConfigError(format!(
            "Failed to save Ollama settings: {}",
            e
        )))
    })?;

    Ok(CommandResponse {
        success: true,
        message: Some("Ollama settings saved".to_string()),
        data: None,
    })
}

/// Save OpenAI-compatible endpoint settings (Tier 0/1).
///
/// Updates the OpenAI-compatible settings in the persistent settings file
/// while preserving every other provider's configuration AND the current
/// `provider` selection.
///
/// # Arguments
/// * `base_url` - Full base URL (e.g. "http://localhost:8080/v1")
/// * `model` - Model identifier the server expects
/// * `timeout_seconds` - Per-request timeout
#[tauri::command]
pub fn save_openai_compatible_settings(
    base_url: String,
    model: String,
    timeout_seconds: u64,
) -> Result<CommandResponse, String> {
    info!(
        "Saving OpenAI-compatible settings: base_url={}, model={}, timeout={}s",
        base_url, model, timeout_seconds
    );

    let existing_settings = settings::get_ai_settings();
    let ai_settings = AiSettings {
        provider: existing_settings.provider,
        claude_cli: existing_settings.claude_cli,
        claude_api: existing_settings.claude_api,
        gemini_cli: existing_settings.gemini_cli,
        gemini_api: existing_settings.gemini_api,
        ollama: existing_settings.ollama,
        openai_compatible: OpenAiCompatibleSettings {
            base_url,
            model,
            timeout_seconds,
        },
        auto_refine_video_after_iterations: existing_settings.auto_refine_video_after_iterations,
        compression: existing_settings.compression,
        retry: existing_settings.retry,
        routing: existing_settings.routing,
        interactive_sessions_enabled: existing_settings.interactive_sessions_enabled,
        ai_path_prediction_enabled: existing_settings.ai_path_prediction_enabled,
        memory_federation_enabled: existing_settings.memory_federation_enabled,
    };

    settings::save_ai_settings(ai_settings).map_err(|e| {
        String::from(AppError::ConfigError(format!(
            "Failed to save OpenAI-compatible settings: {}",
            e
        )))
    })?;

    Ok(CommandResponse {
        success: true,
        message: Some("OpenAI-compatible settings saved".to_string()),
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
    save_ai_api_key_command_impl(provider, api_key).map_err(String::from)
}

fn save_ai_api_key_command_impl(
    provider: String,
    api_key: String,
) -> Result<CommandResponse, AppError> {
    info!("Saving AI API key for provider: {}", provider);

    store_ai_api_key(&provider, &api_key)?;

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
    delete_ai_api_key_command_impl(provider).map_err(String::from)
}

fn delete_ai_api_key_command_impl(provider: String) -> Result<CommandResponse, AppError> {
    info!("Deleting AI API key for provider: {}", provider);

    delete_ai_api_key(&provider)?;

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
    has_ai_api_key_impl(provider).map_err(String::from)
}

fn has_ai_api_key_impl(provider: String) -> Result<CommandResponse, AppError> {
    info!("Checking if AI API key exists for provider: {}", provider);

    let has_key = get_ai_api_key(&provider)?.is_some();

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
    test_ai_connection_impl().await.map_err(String::from)
}

async fn test_ai_connection_impl() -> Result<CommandResponse, AppError> {
    info!("Testing AI connection");

    let ai_settings = settings::get_ai_settings();

    let result = match ai_settings.provider {
        AiProvider::ClaudeCli => test_claude_cli_connection(&ai_settings.claude_cli).await,
        AiProvider::ClaudeApi => test_claude_api_connection(&ai_settings.claude_api).await,
        AiProvider::GeminiCli => test_gemini_cli_connection(&ai_settings.gemini_cli).await,
        AiProvider::GeminiApi => test_gemini_api_connection(&ai_settings.gemini_api).await,
        AiProvider::Ollama => test_ollama_connection(&ai_settings.ollama).await,
        AiProvider::OpenAiCompatible => {
            test_openai_compatible_connection(&ai_settings.openai_compatible).await
        }
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
        data: Some(serde_json::to_value(&test_result)?),
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
            crate::process_helpers::tokio_cmd_no_window()
                .args(["/c", claude_program, "--version"])
                .output()
                .await
                .map_err(|e| {
                    String::from(AppError::Raw(format!(
                        "Failed to execute claude CLI: {}. Is Claude Code installed and in PATH?",
                        e
                    )))
                })?
        }
        CliExecutionMode::Wsl => crate::process_helpers::tokio_no_window("wsl")
            .args([claude_program, "--version"])
            .output()
            .await
            .map_err(|e| {
                String::from(AppError::Raw(format!(
                    "Failed to execute claude via WSL: {}. Is WSL installed?",
                    e
                )))
            })?,
        CliExecutionMode::Native => crate::process_helpers::tokio_no_window(claude_program)
            .args(["--version"])
            .output()
            .await
            .map_err(|e| {
                String::from(AppError::Raw(format!(
                    "Failed to execute claude CLI: {}. Is Claude Code installed and in PATH?",
                    e
                )))
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
        .map_err(|e| {
            String::from(AppError::ConfigError(format!(
                "Failed to retrieve API key: {}",
                e
            )))
        })?
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
        .map_err(|e| String::from(AppError::NetworkError(e.to_string())))?;

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

/// Test Gemini CLI connection
async fn test_gemini_cli_connection(settings: &GeminiCliSettings) -> Result<String, String> {
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

    // Get the gemini program name (custom or default)
    let gemini_program = settings.custom_path.as_deref().unwrap_or("gemini");

    info!(
        "Testing Gemini CLI connection with mode: {:?}, program: {}, auth: {:?}",
        effective_mode, gemini_program, settings.auth_method
    );

    let output = match effective_mode {
        CliExecutionMode::WindowsNative | CliExecutionMode::Auto => {
            // On Windows, use cmd.exe /c to handle .cmd files from npm install
            crate::process_helpers::tokio_cmd_no_window()
                .args(["/c", gemini_program, "--version"])
                .output()
                .await
                .map_err(|e| {
                    String::from(AppError::Raw(format!(
                        "Failed to execute gemini CLI: {}. Is Gemini CLI installed and in PATH?",
                        e
                    )))
                })?
        }
        CliExecutionMode::Wsl => crate::process_helpers::tokio_no_window("wsl")
            .args([gemini_program, "--version"])
            .output()
            .await
            .map_err(|e| {
                String::from(AppError::Raw(format!(
                    "Failed to execute gemini via WSL: {}. Is WSL installed?",
                    e
                )))
            })?,
        CliExecutionMode::Native => crate::process_helpers::tokio_no_window(gemini_program)
            .args(["--version"])
            .output()
            .await
            .map_err(|e| {
                String::from(AppError::Raw(format!(
                    "Failed to execute gemini CLI: {}. Is Gemini CLI installed and in PATH?",
                    e
                )))
            })?,
    };

    if output.status.success() {
        let version = String::from_utf8_lossy(&output.stdout);
        let auth_info = match settings.auth_method {
            GeminiAuthMethod::OAuth => "OAuth authentication",
            GeminiAuthMethod::ApiKey => "API key authentication",
        };
        Ok(format!(
            "Gemini CLI connected successfully ({}) using model: {}. {}",
            auth_info,
            settings.model,
            version.trim()
        ))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("Gemini CLI returned error: {}", stderr.trim()))
    }
}

/// Test Gemini API connection
async fn test_gemini_api_connection(settings: &GeminiApiSettings) -> Result<String, String> {
    // Get API key from keychain
    let api_key = get_ai_api_key("gemini_api")
        .map_err(|e| {
            String::from(AppError::ConfigError(format!(
                "Failed to retrieve API key: {}",
                e
            )))
        })?
        .ok_or_else(|| "No API key configured. Please enter your Gemini API key.".to_string())?;

    info!(
        "Testing Gemini API connection with model: {}",
        settings.model
    );

    // Make a minimal API request to test the connection
    // Using the Gemini API generateContent endpoint
    let client = reqwest::Client::new();
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        settings.model, api_key
    );

    let response = client
        .post(&url)
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "contents": [{"parts": [{"text": "Hi"}]}],
            "generationConfig": {
                "maxOutputTokens": 10
            }
        }))
        .send()
        .await
        .map_err(|e| String::from(AppError::NetworkError(e.to_string())))?;

    if response.status().is_success() {
        Ok(format!(
            "Gemini API connected successfully using model: {}",
            settings.model
        ))
    } else {
        let status = response.status();
        let error_body = response.text().await.unwrap_or_default();

        // Parse error for user-friendly message
        if status.as_u16() == 400 {
            // Check if it's an invalid API key error
            if error_body.contains("API_KEY_INVALID") {
                return Err("Invalid API key. Please check your API key and try again.".to_string());
            }
            Err(format!("Bad request: {}", error_body))
        } else if status.as_u16() == 403 {
            Err(
                "API key doesn't have permission. Please check your API key permissions."
                    .to_string(),
            )
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

/// Test Ollama connection by hitting GET <base_url>/api/tags.
async fn test_ollama_connection(settings: &OllamaSettings) -> Result<String, String> {
    let url = format!("{}/api/tags", settings.base_url.trim_end_matches('/'));
    info!("Testing Ollama connection: GET {}", url);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| String::from(AppError::NetworkError(e.to_string())))?;
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            // Optionally extract the model list and confirm `settings.model` is present.
            let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::json!({}));
            let model_count = body
                .get("models")
                .and_then(|m| m.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            Ok(format!(
                "Ollama connected ({} models available, configured: {})",
                model_count, settings.model
            ))
        }
        Ok(resp) => Err(format!("Ollama returned {}", resp.status())),
        Err(e) => Err(format!("Ollama network error: {}", e)),
    }
}

/// Test an OpenAI-compatible endpoint by hitting <base_url>/models.
async fn test_openai_compatible_connection(
    settings: &OpenAiCompatibleSettings,
) -> Result<String, String> {
    if settings.base_url.trim().is_empty() {
        return Err("OpenAI-compatible base_url is empty — configure it first.".to_string());
    }
    let url = format!("{}/models", settings.base_url.trim_end_matches('/'));
    info!("Testing OpenAI-compatible: GET {}", url);
    let api_key = get_ai_api_key("openai_compatible").ok().flatten();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| String::from(AppError::NetworkError(e.to_string())))?;
    let mut req = client.get(&url);
    if let Some(k) = api_key {
        req = req.bearer_auth(k);
    }
    match req.send().await {
        Ok(resp) if resp.status().is_success() => {
            Ok(format!("OpenAI-compatible endpoint reachable at {}", url))
        }
        Ok(resp) => Err(format!("Endpoint returned {}", resp.status())),
        Err(e) => Err(format!("Network error: {}", e)),
    }
}

/// Get the agentic settings (compression, retry, routing).
///
/// Returns the agentic features settings from the persistent settings file.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with agentic settings data
/// * `Err(String)` - Error message if settings cannot be loaded
#[tauri::command]
pub fn get_agentic_settings() -> Result<CommandResponse, String> {
    info!("Getting agentic settings");

    let ai_settings = settings::get_ai_settings();

    let agentic_data = serde_json::json!({
        "compression": ai_settings.compression,
        "retry": ai_settings.retry,
        "routing": ai_settings.routing,
    });

    Ok(CommandResponse {
        success: true,
        message: Some("Agentic settings retrieved".to_string()),
        data: Some(agentic_data),
    })
}

/// Save agentic settings (compression, retry, routing).
///
/// Updates the agentic features settings in the persistent settings file.
///
/// # Arguments
/// * `compression_enabled` - Enable memory compression
/// * `compression_threshold_tokens` - Token count to trigger compression
/// * `compression_target_tokens` - Target token count after compression
/// * `compression_keep_recent_items` - Number of recent items to always keep
/// * `compression_summarize_batch_size` - Items to summarize together
/// * `retry_enabled` - Enable retry with feedback
/// * `retry_max_retries` - Maximum retry attempts
/// * `retry_base_delay_ms` - Base delay before first retry
/// * `retry_max_delay_ms` - Maximum delay (caps exponential growth)
/// * `retry_exponential_base` - Base for exponential backoff
/// * `retry_jitter` - Add random jitter to delays
/// * `retry_feedback_injection` - Inject error feedback into retry prompts
/// * `routing_enabled` - Enable intelligent task routing
/// * `routing_simple_model` - Model for simple tasks
/// * `routing_medium_model` - Model for medium tasks
/// * `routing_complex_model` - Model for complex tasks
/// * `routing_file_threshold_simple` - Max files for simple classification
/// * `routing_file_threshold_medium` - Max files for medium classification
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if settings cannot be saved
#[tauri::command]
pub fn save_agentic_settings(
    // Compression settings
    compression_enabled: bool,
    compression_threshold_tokens: usize,
    compression_target_tokens: usize,
    compression_keep_recent_items: usize,
    compression_summarize_batch_size: usize,
    // Retry settings
    retry_enabled: bool,
    retry_max_retries: u32,
    retry_base_delay_ms: u64,
    retry_max_delay_ms: u64,
    retry_exponential_base: f32,
    retry_jitter: bool,
    retry_feedback_injection: bool,
    // Routing settings
    routing_enabled: bool,
    routing_simple_model: String,
    routing_medium_model: String,
    routing_complex_model: String,
    routing_file_threshold_simple: usize,
    routing_file_threshold_medium: usize,
) -> Result<CommandResponse, String> {
    info!(
        "Saving agentic settings: compression={}, retry={}, routing={}",
        compression_enabled, retry_enabled, routing_enabled
    );

    // Get existing settings to preserve other configurations
    let existing_settings = settings::get_ai_settings();

    let compression_config = CompressionConfig {
        enabled: compression_enabled,
        threshold_tokens: compression_threshold_tokens,
        target_tokens: compression_target_tokens,
        keep_recent_items: compression_keep_recent_items,
        summarize_batch_size: compression_summarize_batch_size,
        tokens_per_char: existing_settings.compression.tokens_per_char, // Preserve advanced setting
    };

    let retry_config = RetryConfig {
        enabled: retry_enabled,
        max_retries: retry_max_retries,
        base_delay_ms: retry_base_delay_ms,
        max_delay_ms: retry_max_delay_ms,
        exponential_base: retry_exponential_base,
        jitter: retry_jitter,
        feedback_injection: retry_feedback_injection,
        retryable_errors: existing_settings.retry.retryable_errors, // Preserve custom patterns
    };

    let routing_config = RoutingConfig {
        enabled: routing_enabled,
        simple_model: routing_simple_model,
        medium_model: routing_medium_model,
        complex_model: routing_complex_model,
        file_count_thresholds: (routing_file_threshold_simple, routing_file_threshold_medium),
        prompt_length_thresholds: existing_settings.routing.prompt_length_thresholds, // Preserve
        complex_keywords: existing_settings.routing.complex_keywords,                 // Preserve
        simple_keywords: existing_settings.routing.simple_keywords,                   // Preserve
    };

    let ai_settings = AiSettings {
        provider: existing_settings.provider,
        claude_cli: existing_settings.claude_cli,
        claude_api: existing_settings.claude_api,
        gemini_cli: existing_settings.gemini_cli,
        gemini_api: existing_settings.gemini_api,
        ollama: existing_settings.ollama,
        openai_compatible: existing_settings.openai_compatible,
        auto_refine_video_after_iterations: existing_settings.auto_refine_video_after_iterations,
        compression: compression_config,
        retry: retry_config,
        routing: routing_config,
        interactive_sessions_enabled: existing_settings.interactive_sessions_enabled,
        ai_path_prediction_enabled: existing_settings.ai_path_prediction_enabled,
        memory_federation_enabled: existing_settings.memory_federation_enabled,
    };

    settings::save_ai_settings(ai_settings).map_err(|e| {
        String::from(AppError::ConfigError(format!(
            "Failed to save agentic settings: {}",
            e
        )))
    })?;

    Ok(CommandResponse {
        success: true,
        message: Some("Agentic settings saved".to_string()),
        data: None,
    })
}

// ============================================================================
// Multi-Account Usage Check
// ============================================================================

/// Usage information for a single Claude account
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AccountUsageInfo {
    pub config_dir: String,
    pub label: String,
    pub utilization: f64,
    pub rate_limit_type: Option<String>,
    pub resets_at: Option<u64>,
    pub status: Option<String>,
    pub error: Option<String>,
    /// Expected utilization at this point in the billing period (0.0–1.0).
    /// E.g. if 3 of 7 days have elapsed, expected = 3/7 ≈ 0.4286.
    pub expected_utilization: Option<f64>,
    /// Actual minus expected utilization. Negative = under budget, positive = over.
    pub usage_delta: Option<f64>,
    /// Fraction of the billing period that has elapsed (0.0–1.0).
    pub period_elapsed_fraction: Option<f64>,
    /// Days remaining until the billing period resets.
    pub period_remaining_days: Option<f64>,
}

/// Compute expected usage fields from the reset timestamp.
///
/// The Anthropic 7-day window resets at `resets_at`. We compute how far
/// through the current 7-day period we are and derive expected linear usage.
fn compute_expected_usage(
    utilization: f64,
    resets_at: Option<u64>,
) -> (Option<f64>, Option<f64>, Option<f64>, Option<f64>) {
    let Some(reset_ts) = resets_at else {
        return (None, None, None, None);
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if now == 0 || reset_ts <= now {
        // Period already expired or clock issue
        return (None, None, None, None);
    }
    const PERIOD_SECS: f64 = 7.0 * 24.0 * 3600.0; // 7 days in seconds
    let remaining_secs = (reset_ts - now) as f64;
    let elapsed_secs = PERIOD_SECS - remaining_secs;
    // Clamp: if elapsed_secs < 0 the period just started fresh
    let elapsed_secs = elapsed_secs.max(0.0);
    let elapsed_fraction = (elapsed_secs / PERIOD_SECS).clamp(0.0, 1.0);
    let remaining_days = remaining_secs / 86400.0;
    let expected = elapsed_fraction; // linear expectation
    let delta = utilization - expected;
    (
        Some(expected),
        Some(delta),
        Some(elapsed_fraction),
        Some(remaining_days),
    )
}

/// Read the OAuth access token from a Claude config directory's credentials file.
pub(crate) fn read_oauth_token(config_dir: &str) -> Result<String, String> {
    let creds_path = std::path::PathBuf::from(config_dir).join(".credentials.json");
    let content = std::fs::read_to_string(&creds_path).map_err(|e| {
        String::from(AppError::IoError(std::io::Error::new(
            e.kind(),
            format!("Cannot read {}: {}", creds_path.display(), e),
        )))
    })?;
    let json: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        String::from(AppError::ParseError(format!(
            "Invalid credentials JSON: {}",
            e
        )))
    })?;
    json["claudeAiOauth"]["accessToken"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "No accessToken in credentials".to_string())
}

/// Whether the OAuth credentials at `creds_path` carry an `expiresAt` that is
/// already in the past. Returns `false` on any read/parse failure or when the
/// field is absent — callers fall back to the existing error-surfacing path.
pub(crate) fn is_oauth_token_expired(creds_path: &std::path::Path) -> bool {
    let Ok(content) = std::fs::read_to_string(creds_path) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    let expires_at_ms = json["claudeAiOauth"]["expiresAt"].as_i64().unwrap_or(0);
    if expires_at_ms <= 0 {
        return false;
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    now_ms >= expires_at_ms
}

/// Probe a single account for its weekly rate limit utilization.
///
/// Makes a minimal API call (1 token, cheapest model) and reads the
/// `anthropic-ratelimit-unified-7d-utilization` response header, which
/// always contains the exact weekly usage fraction regardless of threshold.
pub async fn probe_account_usage(config_dir: String) -> AccountUsageInfo {
    let label = std::path::Path::new(&config_dir)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| config_dir.clone());

    // Pre-flight: if the stored OAuth token is already expired, attempt a
    // silent refresh BEFORE issuing the probe. Mirrors the warm-path discipline
    // at `claude_api_warm.rs:174-188`. Refresh failure is non-fatal — we still
    // try the probe with whatever token we have so the caller sees a real auth
    // error rather than a swallowed "couldn't refresh."
    let creds_path = std::path::PathBuf::from(&config_dir).join(".credentials.json");
    if is_oauth_token_expired(&creds_path) {
        // try_refresh_credentials uses reqwest::blocking::Client internally.
        // Calling it directly from async context panics ("Cannot drop a runtime
        // in a context where blocking is not allowed"), so offload to a blocking
        // thread. Refresh failure is non-fatal — we still probe with whatever
        // token we have.
        let creds_for_refresh = creds_path.clone();
        let _ = tokio::task::spawn_blocking(move || {
            crate::ai_provider::oauth_refresh::try_refresh_credentials(&creds_for_refresh)
        })
        .await;
    }

    let token = match read_oauth_token(&config_dir) {
        Ok(t) => t,
        Err(e) => {
            return AccountUsageInfo {
                config_dir,
                label,
                utilization: 1.0,
                rate_limit_type: None,
                resets_at: None,
                status: None,
                error: Some(e),
                expected_utilization: None,
                usage_delta: None,
                period_elapsed_fraction: None,
                period_remaining_days: None,
            };
        }
    };

    // Make a minimal API call — Haiku with max_tokens=1 is the cheapest possible.
    // OAuth tokens (`sk-ant-oat*`) must go via `Authorization: Bearer` +
    // `anthropic-beta: oauth-2025-04-20`; API keys (`sk-ant-api*`) go via
    // `x-api-key`. `anthropic_auth::apply_async` dispatches on token prefix.
    let client = reqwest::Client::new();
    let request = crate::ai_provider::anthropic_auth::apply_async(
        client.post("https://api.anthropic.com/v1/messages"),
        &token,
    )
    .header("anthropic-version", "2023-06-01")
    .header("content-type", "application/json")
    .json(&serde_json::json!({
        "model": "claude-haiku-4-5-20251001",
        "max_tokens": 1,
        "messages": [{"role": "user", "content": "hi"}]
    }));
    let response = request.send().await;

    match response {
        Ok(resp) => {
            let headers = resp.headers();

            // Parse weekly (7-day) utilization from response headers
            let utilization_7d = headers
                .get("anthropic-ratelimit-unified-7d-utilization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(0.0);

            let status_7d = headers
                .get("anthropic-ratelimit-unified-7d-status")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());

            let resets_at_7d = headers
                .get("anthropic-ratelimit-unified-7d-reset")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());

            // Also grab 5-hour utilization for context
            let utilization_5h = headers
                .get("anthropic-ratelimit-unified-5h-utilization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<f64>().ok());

            info!(
                "Account '{}': 7d={:.0}% 5h={:.0}% status={:?}",
                label,
                utilization_7d * 100.0,
                utilization_5h.unwrap_or(0.0) * 100.0,
                status_7d
            );

            let error = if resp.status().is_success() {
                None
            } else {
                let status_code = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                Some(format!("API error ({}): {}", status_code, body))
            };

            let (expected, delta, elapsed_frac, remaining_days) =
                compute_expected_usage(utilization_7d, resets_at_7d);

            AccountUsageInfo {
                config_dir,
                label,
                utilization: utilization_7d,
                rate_limit_type: Some("seven_day".to_string()),
                resets_at: resets_at_7d,
                status: status_7d,
                error,
                expected_utilization: expected,
                usage_delta: delta,
                period_elapsed_fraction: elapsed_frac,
                period_remaining_days: remaining_days,
            }
        }
        Err(e) => AccountUsageInfo {
            config_dir,
            label,
            utilization: 1.0,
            rate_limit_type: None,
            resets_at: None,
            status: None,
            error: Some(format!("Network error: {}", e)),
            expected_utilization: None,
            usage_delta: None,
            period_elapsed_fraction: None,
            period_remaining_days: None,
        },
    }
}

/// Check usage for all configured Claude accounts.
///
/// Probes each account by making a minimal API call (Haiku, 1 token) and
/// reading the `anthropic-ratelimit-unified-7d-utilization` response header.
/// This gives the exact weekly utilization for every account.
#[tauri::command]
pub async fn check_accounts_usage(config_dirs: Vec<String>) -> Result<CommandResponse, String> {
    info!("Checking usage for {} Claude accounts", config_dirs.len());

    // Probe all accounts concurrently
    let futures: Vec<_> = config_dirs
        .into_iter()
        .map(|dir| async move { probe_account_usage(dir).await })
        .collect();

    let results = futures::future::join_all(futures).await;

    // Feed the selection hot path's usage snapshot for free, so
    // `pick_best_account` can rank by weekly-usage headroom without probing
    // inline (see `ai_provider::account_usage`).
    record_usage_snapshot(&results);

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Checked {} accounts", results.len())),
        data: Some(
            serde_json::to_value(&results).map_err(|e| String::from(AppError::JsonError(e)))?,
        ),
    })
}

/// Weekly utilization at/above which an account is treated as "out of tokens"
/// for selection — close enough to the cap that it won't reliably serve a
/// request. A backstop alongside the server `status` / probe-error signals in
/// [`probe_result_exhausted`].
const EXHAUSTION_UTILIZATION: f64 = 0.99;

/// Whether a probe result means the account **won't serve a request right
/// now**. True when the probe call itself failed (the probe hits the same
/// per-account quota the CLI uses — so a 429/403/spend-limit rejection shows
/// up here even if weekly *token* utilization still looks low), when the
/// server reports the account rejected/blocked/exceeded, or when weekly
/// utilization is at/over [`EXHAUSTION_UTILIZATION`].
fn probe_result_exhausted(info: &AccountUsageInfo) -> bool {
    if info.error.is_some() || info.utilization >= EXHAUSTION_UTILIZATION {
        return true;
    }
    info.status.as_deref().is_some_and(|s| {
        let s = s.to_ascii_lowercase();
        s.contains("reject") || s.contains("block") || s.contains("exceed")
    })
}

/// Record probe results into the account-selection usage snapshot.
///
/// Shared by every usage-probe caller so the hot-path picker
/// (`ai_provider::pick_best_account`) always reads from a cache rather than
/// issuing its own (self-rate-limiting) probe. Computes the per-account
/// `exhausted` flag here, where the full probe result (status + error) is
/// available.
pub fn record_usage_snapshot(results: &[AccountUsageInfo]) {
    let samples: Vec<(String, f64, Option<f64>, bool)> = results
        .iter()
        .map(|r| {
            (
                r.config_dir.clone(),
                r.utilization,
                r.usage_delta,
                probe_result_exhausted(r),
            )
        })
        .collect();
    crate::ai_provider::record_account_usage(&samples);
}

/// Probe every configured account and refresh the selection usage snapshot.
///
/// Called off the hot path — at startup and on a periodic timer (see
/// `main.rs`) — so a runner whose Settings/Terminal UI is never opened (e.g. a
/// headless or co-pilot-only runner) still has fresh weekly-usage headroom
/// data for `pick_best_account`. No-op when fewer than two accounts are
/// configured (single-account runners have nothing to choose between).
pub async fn refresh_account_usage_snapshot() {
    let config_dirs = crate::settings::get_claude_config_dirs();
    if config_dirs.len() < 2 {
        return;
    }
    let futures: Vec<_> = config_dirs
        .into_iter()
        .map(|dir| async move { probe_account_usage(dir).await })
        .collect();
    let results = futures::future::join_all(futures).await;
    record_usage_snapshot(&results);
}

// ============================================================================
// Account Switching
// ============================================================================

/// Info about a configured Claude account for the UI.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AccountInfo {
    pub config_dir: String,
    pub label: String,
    pub is_active: bool,
    pub is_rate_limited: bool,
}

/// Get the status of all configured Claude accounts.
///
/// Returns which account is active, which are rate-limited, etc.
#[tauri::command]
pub fn get_claude_accounts() -> Result<CommandResponse, String> {
    get_claude_accounts_impl().map_err(String::from)
}

fn get_claude_accounts_impl() -> Result<CommandResponse, AppError> {
    let statuses = crate::ai_provider::get_account_statuses();
    let accounts: Vec<AccountInfo> = statuses
        .into_iter()
        .map(|(dir, label, active, cooled)| AccountInfo {
            config_dir: dir,
            label,
            is_active: active,
            is_rate_limited: cooled,
        })
        .collect();

    Ok(CommandResponse {
        success: true,
        message: Some(format!("{} accounts configured", accounts.len())),
        data: Some(serde_json::to_value(&accounts)?),
    })
}

/// Manually switch to a specific Claude account by its config_dir path.
#[tauri::command]
pub fn switch_claude_account(config_dir: String) -> Result<CommandResponse, String> {
    let switched = crate::ai_provider::switch_to_account(&config_dir);
    if switched {
        Ok(CommandResponse {
            success: true,
            message: Some(format!(
                "Switched to account '{}'",
                std::path::Path::new(&config_dir)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&config_dir)
            )),
            data: None,
        })
    } else {
        Ok(CommandResponse {
            success: false,
            message: Some(format!(
                "Account '{}' not found in configured claude_config_dirs",
                config_dir
            )),
            data: None,
        })
    }
}

// ============================================================================
// Claude CLI Auth Status
// ============================================================================

/// Status of Claude CLI OAuth credentials
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CliAuthStatus {
    /// Whether credentials exist at all
    pub has_credentials: bool,
    /// Whether the access token has expired
    pub expired: bool,
    /// Whether the provider is claude_cli (relevant to check)
    pub is_cli_provider: bool,
    /// ISO 8601 expiry time
    pub expires_at: Option<String>,
    /// Minutes until expiry (negative = already expired)
    pub minutes_until_expiry: Option<i64>,
    /// Subscription type from credentials
    pub subscription_type: Option<String>,
    /// Path to the credentials file found
    pub credentials_path: Option<String>,
}

/// Find the Claude CLI credentials file, respecting config_dir settings.
pub(crate) fn find_claude_credentials_path() -> Option<std::path::PathBuf> {
    // 1. Check effective config_dir from runner AI settings (respects least-usage mode)
    let ai_settings = settings::get_ai_settings();
    let effective_dir = crate::ai_provider::get_effective_config_dir(&ai_settings.claude_cli);
    if let Some(ref dir) = effective_dir {
        let path = std::path::PathBuf::from(dir).join(".credentials.json");
        if path.exists() {
            return Some(path);
        }
    }

    // 2. Check CLAUDE_CONFIG_DIR env var
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let path = std::path::PathBuf::from(&dir).join(".credentials.json");
        if path.exists() {
            return Some(path);
        }
    }

    // 3. Check default ~/.claude/
    if let Some(home) = dirs::home_dir() {
        let path = home.join(".claude").join(".credentials.json");
        if path.exists() {
            return Some(path);
        }
    }

    None
}

/// Read and parse Claude CLI auth status from credentials file.
pub fn get_cli_auth_status() -> CliAuthStatus {
    let ai_settings = settings::get_ai_settings();
    let is_cli = matches!(ai_settings.provider, AiProvider::ClaudeCli);

    let creds_path = match find_claude_credentials_path() {
        Some(p) => p,
        None => {
            return CliAuthStatus {
                has_credentials: false,
                expired: true,
                is_cli_provider: is_cli,
                expires_at: None,
                minutes_until_expiry: None,
                subscription_type: None,
                credentials_path: None,
            };
        }
    };

    let content = match std::fs::read_to_string(&creds_path) {
        Ok(c) => c,
        Err(_) => {
            return CliAuthStatus {
                has_credentials: false,
                expired: true,
                is_cli_provider: is_cli,
                expires_at: None,
                minutes_until_expiry: None,
                subscription_type: None,
                credentials_path: Some(creds_path.to_string_lossy().to_string()),
            };
        }
    };

    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => {
            return CliAuthStatus {
                has_credentials: false,
                expired: true,
                is_cli_provider: is_cli,
                expires_at: None,
                minutes_until_expiry: None,
                subscription_type: None,
                credentials_path: Some(creds_path.to_string_lossy().to_string()),
            };
        }
    };

    let oauth = &json["claudeAiOauth"];
    if oauth.is_null() {
        return CliAuthStatus {
            has_credentials: false,
            expired: true,
            is_cli_provider: is_cli,
            expires_at: None,
            minutes_until_expiry: None,
            subscription_type: None,
            credentials_path: Some(creds_path.to_string_lossy().to_string()),
        };
    }

    let expires_at_ms = oauth["expiresAt"].as_i64().unwrap_or(0);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    let diff_minutes = (expires_at_ms - now_ms) / 60_000;
    let expired = now_ms > expires_at_ms;

    // Format expiry as ISO 8601
    let expires_at_str = if expires_at_ms > 0 {
        let secs = expires_at_ms / 1000;
        let nanos = ((expires_at_ms % 1000) * 1_000_000) as u32;
        std::time::UNIX_EPOCH
            .checked_add(std::time::Duration::new(secs as u64, nanos))
            .map(|t| {
                let datetime: chrono::DateTime<chrono::Utc> = t.into();
                datetime.to_rfc3339()
            })
    } else {
        None
    };

    let subscription_type = oauth["subscriptionType"].as_str().map(|s| s.to_string());

    CliAuthStatus {
        has_credentials: true,
        expired,
        is_cli_provider: is_cli,
        expires_at: expires_at_str,
        minutes_until_expiry: Some(diff_minutes),
        subscription_type,
        credentials_path: Some(creds_path.to_string_lossy().to_string()),
    }
}

/// Check Claude CLI authentication status.
///
/// Reads the credentials file and checks if the OAuth token is expired.
#[tauri::command]
pub async fn check_claude_cli_auth() -> Result<CommandResponse, String> {
    check_claude_cli_auth_impl().await.map_err(String::from)
}

async fn check_claude_cli_auth_impl() -> Result<CommandResponse, AppError> {
    let status = get_cli_auth_status();

    Ok(CommandResponse {
        success: true,
        message: if status.expired {
            Some("Claude CLI authentication has expired".to_string())
        } else {
            Some("Claude CLI authentication is valid".to_string())
        },
        data: Some(serde_json::to_value(&status)?),
    })
}

/// Open a terminal window for the user to re-authenticate Claude CLI.
///
/// Spawns `claude auth login` in a visible terminal so the user can complete
/// the browser-based OAuth flow.
#[tauri::command]
pub async fn refresh_claude_cli_auth() -> Result<CommandResponse, String> {
    let ai_settings = settings::get_ai_settings();
    let claude_program = ai_settings
        .claude_cli
        .custom_path
        .as_deref()
        .unwrap_or("claude");

    let effective_dir = crate::ai_provider::get_effective_config_dir(&ai_settings.claude_cli);
    let config_env = effective_dir
        .as_ref()
        .map(|dir| format!("$env:CLAUDE_CONFIG_DIR = '{}'; ", dir))
        .unwrap_or_default();

    // Open a visible PowerShell window with claude auth login
    #[cfg(target_os = "windows")]
    {
        let ps_script = format!(
            "{}Write-Host 'Authenticating Claude CLI...' -ForegroundColor Cyan; {} auth login; Write-Host ''; Write-Host 'Authentication complete. You can close this window.' -ForegroundColor Green; Read-Host 'Press Enter to close'",
            config_env, claude_program
        );

        tokio::process::Command::new("cmd")
            .args([
                "/c",
                "start",
                "Claude CLI Authentication",
                "powershell",
                "-NoProfile",
                "-Command",
                &ps_script,
            ])
            .output()
            .await
            .map_err(|e| {
                String::from(AppError::ProcessError(format!(
                    "Failed to open authentication window: {}",
                    e
                )))
            })?;
    }

    #[cfg(not(target_os = "windows"))]
    {
        // On macOS/Linux, open a terminal with the auth command
        let script = format!(
            "echo 'Authenticating Claude CLI...'; {}{} auth login; echo ''; echo 'Authentication complete. You can close this window.'; read -p 'Press Enter to close'",
            config_env.replace("$env:", "export ").replace(" = ", "="),
            claude_program
        );

        // Try common terminal emulators
        let terminals = [
            (
                "open",
                vec!["-a", "Terminal", "--args", "bash", "-c", &script],
            ),
            ("gnome-terminal", vec!["--", "bash", "-c", &script]),
            ("xterm", vec!["-e", "bash", "-c", &script]),
        ];

        let mut launched = false;
        for (term, args) in &terminals {
            if tokio::process::Command::new(term)
                .args(args)
                .output()
                .await
                .is_ok()
            {
                launched = true;
                break;
            }
        }

        if !launched {
            return Err(format!(
                "Could not open a terminal. Please run '{} auth login' manually.",
                claude_program
            ));
        }
    }

    Ok(CommandResponse {
        success: true,
        message: Some(
            "Authentication window opened. Complete the browser login flow, then check status again."
                .to_string(),
        ),
        data: None,
    })
}

/// Get the API key for a provider (used by mcp_api.rs for API calls)
#[allow(dead_code)]
pub fn get_provider_api_key(provider: &str) -> Result<Option<String>, String> {
    get_ai_api_key(provider).map_err(|e: anyhow::Error| String::from(AppError::from(e)))
}

// ============================================================================
// Circuit Breaker Commands
// ============================================================================

/// Get circuit breaker states for all AI providers.
///
/// Returns a snapshot of every provider that has been seen by the circuit breaker
/// registry, along with its current state and availability.
#[tauri::command]
pub async fn get_provider_circuit_states(
) -> Result<Vec<crate::ai_provider::circuit_breaker::ProviderCircuitState>, String> {
    Ok(crate::ai_provider::circuit_breaker::all_provider_circuit_states())
}

/// Manually reset a provider's circuit breaker to the Closed (healthy) state.
#[tauri::command]
pub async fn reset_provider_circuit(provider_key: String) -> Result<(), String> {
    info!("Resetting circuit breaker for provider: {}", provider_key);
    crate::ai_provider::circuit_breaker::reset_provider(&provider_key);
    Ok(())
}

// ============================================================================
// World State Verifier Settings
// ============================================================================

use crate::settings::{WorldStateVerifierSettings, WsvMode};

/// Get the persisted World State Verifier settings.
#[tauri::command]
pub fn get_wsv_settings() -> Result<CommandResponse, String> {
    let settings = settings::get_world_state_verifier_settings();
    let data = serde_json::json!({
        "mode": settings.mode,
        "endpoint": settings.endpoint,
        "model": settings.model,
        "show_screenshot_evidence": settings.show_screenshot_evidence,
    });
    Ok(CommandResponse {
        success: true,
        message: Some("WSV settings retrieved".to_string()),
        data: Some(data),
    })
}

/// Save World State Verifier settings.
///
/// Persists to the runner settings file AND updates the in-process live
/// config so the next agentic verification iteration picks up the change
/// without a restart.
#[tauri::command]
pub fn save_wsv_settings(
    mode: String,
    endpoint: String,
    model: String,
    show_screenshot_evidence: bool,
) -> Result<CommandResponse, String> {
    let parsed_mode = match mode.to_lowercase().as_str() {
        "disabled" => WsvMode::Disabled,
        "enabled" => WsvMode::Enabled,
        "shadow" => WsvMode::Shadow,
        other => return Err(format!("Invalid mode: {}", other)),
    };

    let trimmed_endpoint = endpoint.trim().to_string();
    let trimmed_model = model.trim().to_string();
    if trimmed_endpoint.is_empty() {
        return Err("Endpoint cannot be empty".to_string());
    }
    if trimmed_model.is_empty() {
        return Err("Model cannot be empty".to_string());
    }

    info!(
        "Saving WSV settings: mode={:?} endpoint={} model={} show_evidence={}",
        parsed_mode, trimmed_endpoint, trimmed_model, show_screenshot_evidence
    );

    let new_settings = WorldStateVerifierSettings {
        mode: parsed_mode,
        endpoint: trimmed_endpoint,
        model: trimmed_model,
        show_screenshot_evidence,
        // Mark that the user has explicitly expressed an opinion so
        // the env-var fallback in init_from_persisted won't override
        // this on the next startup.
        ever_saved: true,
    };

    settings::save_world_state_verifier_settings(new_settings.clone()).map_err(|e| {
        String::from(AppError::ConfigError(format!(
            "Failed to save WSV settings: {}",
            e
        )))
    })?;

    // Update in-process live config so the next iteration picks it up.
    crate::verification::WsvConfig::set_global(crate::verification::WsvConfig::from_settings(
        &new_settings,
    ));

    Ok(CommandResponse {
        success: true,
        message: Some("WSV settings saved".to_string()),
        data: None,
    })
}

#[derive(Debug, Serialize)]
pub struct WsvConnectionTestResult {
    pub ok: bool,
    pub error: Option<String>,
    pub models_available: Vec<String>,
    pub latency_ms: u64,
}

/// Test connectivity to the WSV endpoint by hitting `/v1/models`.
///
/// Uses a 5s timeout — cold-start first-request weight downloads will
/// exceed this and return a failure, which is the desired behavior for
/// the settings UI's "Test connection" button: we want a fast yes/no
/// signal, not a blocking 5-minute download.
#[tauri::command]
pub async fn test_wsv_connection(endpoint: String) -> Result<WsvConnectionTestResult, String> {
    let url = format!("{}/v1/models", endpoint.trim().trim_end_matches('/'));
    info!("Testing WSV connection: GET {}", url);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| String::from(AppError::NetworkError(e.to_string())))?;

    let start = std::time::Instant::now();
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            return Ok(WsvConnectionTestResult {
                ok: false,
                error: Some(format!("HTTP error: {}", e)),
                models_available: vec![],
                latency_ms: start.elapsed().as_millis() as u64,
            });
        }
    };
    let latency_ms = start.elapsed().as_millis() as u64;

    if !resp.status().is_success() {
        return Ok(WsvConnectionTestResult {
            ok: false,
            error: Some(format!("HTTP {}", resp.status().as_u16())),
            models_available: vec![],
            latency_ms,
        });
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            return Ok(WsvConnectionTestResult {
                ok: false,
                error: Some(format!("Invalid JSON response: {}", e)),
                models_available: vec![],
                latency_ms,
            });
        }
    };

    // Parse `{data: [{id: "...", ...}, ...]}` — the standard OpenAI
    // /v1/models format that llama-swap emits.
    let models_available = body
        .get("data")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    m.get("id")
                        .and_then(|id| id.as_str())
                        .map(|s| s.to_string())
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(WsvConnectionTestResult {
        ok: true,
        error: None,
        models_available,
        latency_ms,
    })
}

/// List recent shadow-mode disagreements for the calibration UI.
///
/// Called by the Settings → World State Verifier calibration section.
/// Returns rows most-recent-first. Limit clamped to [1, 1000] via
/// `wsv_disagreements::normalize_limit`.
#[tauri::command]
pub async fn list_wsv_disagreements(
    storage: tauri::State<'_, crate::commands::compartments::StorageCompartment>,
    task_run_id: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<crate::database::pg::wsv_disagreements::WsvDisagreementRow>, String> {
    let limit = crate::database::pg::wsv_disagreements::normalize_limit(limit);
    storage
        .pg_db()
        .list_wsv_disagreements(task_run_id.as_deref(), limit)
        .await
}

/// Tauri plugin exposing all AI-settings commands.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_ai_settings")
        .invoke_handler(tauri::generate_handler![
            get_ai_settings,
            save_ai_settings,
            save_gemini_settings,
            save_ai_api_key_command,
            delete_ai_api_key_command,
            has_ai_api_key,
            test_ai_connection,
            check_claude_cli_auth,
            check_accounts_usage,
            get_claude_accounts,
            switch_claude_account,
            refresh_claude_cli_auth,
            get_agentic_settings,
            save_agentic_settings,
            get_wsv_settings,
            save_wsv_settings,
            test_wsv_connection,
            list_wsv_disagreements,
            get_provider_circuit_states,
            reset_provider_circuit,
        ])
        .build()
}

#[cfg(test)]
mod exhaustion_tests {
    use super::*;

    fn usage(utilization: f64, status: Option<&str>, error: Option<&str>) -> AccountUsageInfo {
        AccountUsageInfo {
            config_dir: "/test/acct".to_string(),
            label: "acct".to_string(),
            utilization,
            rate_limit_type: Some("seven_day".to_string()),
            resets_at: None,
            status: status.map(|s| s.to_string()),
            error: error.map(|s| s.to_string()),
            expected_utilization: None,
            usage_delta: None,
            period_elapsed_fraction: None,
            period_remaining_days: None,
        }
    }

    #[test]
    fn under_cap_and_allowed_is_not_exhausted() {
        assert!(!probe_result_exhausted(&usage(0.80, Some("allowed"), None)));
        // Even fully under projection but high (98%) — still usable.
        assert!(!probe_result_exhausted(&usage(
            0.98,
            Some("allowed_warning"),
            None
        )));
    }

    #[test]
    fn at_or_over_cap_is_exhausted() {
        assert!(probe_result_exhausted(&usage(0.99, Some("allowed"), None)));
        assert!(probe_result_exhausted(&usage(1.0, None, None)));
    }

    #[test]
    fn probe_error_is_exhausted() {
        // Probe rejected (429/403/spend-limit) → won't serve a request, even if
        // the weekly token utilization header still reads low.
        assert!(probe_result_exhausted(&usage(
            0.10,
            None,
            Some("API error (429): rate limit")
        )));
    }

    #[test]
    fn rejected_status_is_exhausted() {
        assert!(probe_result_exhausted(&usage(0.50, Some("rejected"), None)));
        assert!(probe_result_exhausted(&usage(0.50, Some("BLOCKED"), None)));
    }
}
