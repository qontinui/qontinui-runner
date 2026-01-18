//! AI settings commands
//!
//! This module handles AI configuration operations:
//! - Getting and setting AI provider settings
//! - Secure API key storage in OS keychain
//! - Testing AI connections

use crate::ai_router::RoutingConfig;
use crate::orchestrator::{CompressionConfig, RetryConfig};
use crate::settings::{
    self, AiProvider, AiSettings, ClaudeApiSettings, ClaudeCliSettings, CliExecutionMode,
    GeminiApiSettings, GeminiAuthMethod, GeminiCliSettings,
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
/// * `config_dir` - Optional CLAUDE_CONFIG_DIR for multi-account support
/// * `model` - Claude API model name
/// * `max_tokens` - Maximum tokens for API calls
/// * `auto_refine_video_after_iterations` - Default iteration threshold for video in auto-refine
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
    model: String,
    max_tokens: u32,
    auto_refine_video_after_iterations: Option<u32>,
) -> Result<CommandResponse, String> {
    info!(
        "Saving AI settings: provider={}, execution_mode={}, timeout={}s, config_dir={:?}, video_after_iterations={:?}",
        provider, execution_mode, timeout_seconds, config_dir, auto_refine_video_after_iterations
    );

    let ai_provider = match provider.as_str() {
        "claude_cli" => AiProvider::ClaudeCli,
        "claude_api" => AiProvider::ClaudeApi,
        "gemini_cli" => AiProvider::GeminiCli,
        "gemini_api" => AiProvider::GeminiApi,
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

    let ai_settings = AiSettings {
        provider: ai_provider,
        claude_cli: ClaudeCliSettings {
            execution_mode: cli_execution_mode,
            custom_path,
            timeout_seconds,
            config_dir,
        },
        claude_api: ClaudeApiSettings { model, max_tokens },
        // Preserve existing Gemini settings
        gemini_cli: existing_settings.gemini_cli,
        gemini_api: existing_settings.gemini_api,
        auto_refine_video_after_iterations: auto_refine_video_after_iterations.unwrap_or(3),
        // Preserve compression, retry, and routing settings
        compression: existing_settings.compression,
        retry: existing_settings.retry,
        routing: existing_settings.routing,
    };

    settings::save_ai_settings(ai_settings)
        .map_err(|e| format!("Failed to save AI settings: {}", e))?;

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
        auto_refine_video_after_iterations: existing_settings.auto_refine_video_after_iterations,
        // Preserve compression, retry, and routing settings
        compression: existing_settings.compression,
        retry: existing_settings.retry,
        routing: existing_settings.routing,
    };

    settings::save_ai_settings(ai_settings)
        .map_err(|e| format!("Failed to save Gemini settings: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Gemini settings saved".to_string()),
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
        AiProvider::GeminiCli => test_gemini_cli_connection(&ai_settings.gemini_cli).await,
        AiProvider::GeminiApi => test_gemini_api_connection(&ai_settings.gemini_api).await,
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
            tokio::process::Command::new("cmd.exe")
                .args(["/c", gemini_program, "--version"])
                .output()
                .await
                .map_err(|e| {
                    format!(
                        "Failed to execute gemini CLI: {}. Is Gemini CLI installed and in PATH?",
                        e
                    )
                })?
        }
        CliExecutionMode::Wsl => tokio::process::Command::new("wsl")
            .args([gemini_program, "--version"])
            .output()
            .await
            .map_err(|e| format!("Failed to execute gemini via WSL: {}. Is WSL installed?", e))?,
        CliExecutionMode::Native => tokio::process::Command::new(gemini_program)
            .args(["--version"])
            .output()
            .await
            .map_err(|e| {
                format!(
                    "Failed to execute gemini CLI: {}. Is Gemini CLI installed and in PATH?",
                    e
                )
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
        .map_err(|e| format!("Failed to retrieve API key: {}", e))?
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
        .map_err(|e| format!("Network error: {}", e))?;

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
        auto_refine_video_after_iterations: existing_settings.auto_refine_video_after_iterations,
        compression: compression_config,
        retry: retry_config,
        routing: routing_config,
    };

    settings::save_ai_settings(ai_settings)
        .map_err(|e| format!("Failed to save agentic settings: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Agentic settings saved".to_string()),
        data: None,
    })
}

/// Get the API key for a provider (used by mcp_api.rs for API calls)
#[allow(dead_code)]
pub fn get_provider_api_key(provider: &str) -> Result<Option<String>, String> {
    get_ai_api_key(provider).map_err(|e| format!("Failed to get API key: {}", e))
}
