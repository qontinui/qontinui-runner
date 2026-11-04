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
    OpenAiCompatibleSettings, PiCliSettings,
};
use anyhow::Result;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::time::Duration;
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
/// Returns the AI settings stored in the persistent settings file, flattened
/// together with four READ-ONLY derived keys — `effective_account_selection_mode`,
/// `account_selection_source` (`fleet` | `local` | `pinned`),
/// `fleet_account_selection_mode` (the raw fleet value, `null` = no opinion)
/// and `fleet_account_selection_answered` — see [`settings::AiSettingsView`].
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

    let view = settings::AiSettingsView::current();

    Ok(CommandResponse {
        success: true,
        message: Some("AI settings retrieved".to_string()),
        data: Some(serde_json::to_value(&view)?),
    })
}

/// The account-selection mode a Tauri `save_ai_settings` call stores. PURE.
///
/// An ABSENT mode (`None` — a caller that does not send it) KEEPS the stored
/// mode: the mode is machine-global (mirrored into `claude-accounts.json`), so
/// defaulting it would silently re-point every runner on the machine. A sent
/// value is parsed with `AccountSelectionMode::from_wire`; an unknown spelling
/// is `AccountSelectionMode::default()` (settings.rs). Plan
/// `2026-10-01-fleet-account-selection-effective-mode-visibility-and-pin-safe-saves`
/// Phase 1.
fn resolve_saved_selection_mode(
    sent: Option<&str>,
    stored: AccountSelectionMode,
) -> AccountSelectionMode {
    // A named mode is taken as given; an unknown spelling resolves to the
    // default (`AccountSelectionMode::default()` in settings.rs).
    match sent {
        None => stored,
        Some(raw) => AccountSelectionMode::from_wire(raw).unwrap_or_default(),
    }
}

/// Save AI settings.
///
/// Updates the AI settings in the persistent settings file.
///
/// Absent-means-keep for `account_selection_mode`, `account_selection_pinned`
/// and the migration toggles; `config_dir` stays EXPLICIT — over Tauri IPC an
/// absent and a `null` value both arrive as `None`, and the Settings page
/// always sends it, so `None` clears it.
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
    auto_migrate_on_token_exhaustion: Option<bool>,
    auto_continue_after_migration: Option<bool>,
    account_selection_pinned: Option<bool>,
) -> Result<CommandResponse, String> {
    info!(
        "Saving AI settings: provider={}, execution_mode={}, timeout={}s, config_dir={:?}, account_selection={:?}, account_selection_pinned={:?}, video_after_iterations={:?}, interactive={:?}",
        provider, execution_mode, timeout_seconds, config_dir, account_selection_mode, account_selection_pinned, auto_refine_video_after_iterations, interactive_sessions_enabled
    );

    let ai_provider = match provider.as_str() {
        "claude_cli" => AiProvider::ClaudeCli,
        "claude_api" => AiProvider::ClaudeApi,
        "gemini_cli" => AiProvider::GeminiCli,
        "gemini_api" => AiProvider::GeminiApi,
        "pi_cli" => AiProvider::PiCli,
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
    // Read-modify-write under the AI-settings write lock so a concurrent
    // save (HTTP PUT, another command) cannot be lost between the read and
    // the write.
    settings::update_ai_settings(|existing_settings| {
        let selection_mode = resolve_saved_selection_mode(
            account_selection_mode.as_deref(),
            existing_settings.claude_cli.account_selection_mode,
        );

        let ai_settings = AiSettings {
            provider: ai_provider,
            claude_cli: ClaudeCliSettings {
                execution_mode: cli_execution_mode,
                custom_path,
                timeout_seconds,
                config_dir,
                account_selection_mode: selection_mode,
                // `None` (an older frontend that does not send the pin) PRESERVES
                // the configured value — it must never silently unpin a machine
                // and hand it to the fleet policy.
                account_selection_pinned: account_selection_pinned
                    .unwrap_or(existing_settings.claude_cli.account_selection_pinned),
                // `None` (older frontends) preserves whatever is configured.
                auto_migrate_on_token_exhaustion: auto_migrate_on_token_exhaustion.unwrap_or(
                    existing_settings
                        .claude_cli
                        .auto_migrate_on_token_exhaustion,
                ),
                auto_continue_after_migration: auto_continue_after_migration
                    .unwrap_or(existing_settings.claude_cli.auto_continue_after_migration),
            },
            claude_api: ClaudeApiSettings { model, max_tokens },
            // Preserve existing Gemini settings
            gemini_cli: existing_settings.gemini_cli,
            gemini_api: existing_settings.gemini_api,
            // Preserve existing pi CLI settings
            pi_cli: existing_settings.pi_cli,
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
            // PRESERVE — no frontend surface sends this yet, so a save that
            // reconstructed it from the default would silently re-enable git-op
            // federation for anyone who had opted out, every time they touched an
            // unrelated AI setting. Plan:
            // `2026-07-28-git-op-federation-lost-its-kill-switch`.
            git_op_federation_enabled: existing_settings.git_op_federation_enabled,
        };

        Ok::<_, std::convert::Infallible>(ai_settings)
    })
    .map_err(|e| {
        let e = match e {
            settings::AiSettingsUpdateError::SaveFailed(msg) => msg,
            settings::AiSettingsUpdateError::Rejected(never) => match never {},
        };
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
    // Read-modify-write under the AI-settings write lock so a concurrent
    // save (HTTP PUT, another command) cannot be lost between the read and
    // the write.
    settings::update_ai_settings(|existing_settings| {
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
            // Preserve existing pi CLI / Ollama / OpenAI-compatible settings
            pi_cli: existing_settings.pi_cli,
            ollama: existing_settings.ollama,
            openai_compatible: existing_settings.openai_compatible,
            auto_refine_video_after_iterations: existing_settings
                .auto_refine_video_after_iterations,
            // Preserve compression, retry, routing, and interactive settings
            compression: existing_settings.compression,
            retry: existing_settings.retry,
            routing: existing_settings.routing,
            interactive_sessions_enabled: existing_settings.interactive_sessions_enabled,
            ai_path_prediction_enabled: existing_settings.ai_path_prediction_enabled,
            // Preserve — see the note in `save_ai_settings`; reconstructing this
            // from the default would silently re-enable federation on every save.
            git_op_federation_enabled: existing_settings.git_op_federation_enabled,
        };

        Ok::<_, std::convert::Infallible>(ai_settings)
    })
    .map_err(|e| {
        let e = match e {
            settings::AiSettingsUpdateError::SaveFailed(msg) => msg,
            settings::AiSettingsUpdateError::Rejected(never) => match never {},
        };
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

    let refusal = crate::model_gateway::provider_refusal(&ai_settings.provider);
    let result = match ai_settings.provider {
        // Review M5: refused while a model gateway is declared.
        _ if refusal.is_some() => Err(refusal.unwrap_or_default()),
        AiProvider::ClaudeCli => test_claude_cli_connection(&ai_settings.claude_cli).await,
        AiProvider::ClaudeApi => test_claude_api_connection(&ai_settings.claude_api).await,
        AiProvider::GeminiCli => test_gemini_cli_connection(&ai_settings.gemini_cli).await,
        AiProvider::GeminiApi => test_gemini_api_connection(&ai_settings.gemini_api).await,
        AiProvider::PiCli => test_pi_cli_connection(&ai_settings.pi_cli).await,
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
    // Vendor host + keychain key, or the declared model gateway + its
    // api-key-helper key (`crate::model_gateway`). Resolved on a blocking
    // thread: the helper is a subprocess.
    let call = spawn_blocking_tracked(|| {
        crate::model_gateway::ModelCall::resolve(|| {
            get_ai_api_key("claude_api")
                .map_err(|e| {
                    String::from(AppError::ConfigError(format!(
                        "Failed to retrieve API key: {}",
                        e
                    )))
                })?
                .ok_or_else(|| {
                    "No API key configured. Please enter your Claude API key.".to_string()
                })
        })
    })
    .await
    .map_err(|e| format!("credential resolution task failed: {e}"))??;

    info!(
        "Testing Claude API connection with model: {} (route: {})",
        settings.model,
        call.route_label()
    );

    // Make a minimal API request to test the connection
    let client = reqwest::Client::new();
    // Not a coord write: Anthropic's API or the operator's declared model
    // gateway, built by `ModelCall` (which never attaches a coord bearer — that
    // would be a credential leak to a third party).
    let response = call
        .post_async(&client, crate::model_gateway::MESSAGES_PATH)
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

/// Test pi CLI connection
///
/// `pi --version` verifies the binary resolves and runs; it does NOT verify
/// provider auth (pi validates the API key only when a prompt actually runs).
async fn test_pi_cli_connection(settings: &PiCliSettings) -> Result<String, String> {
    let system = std::env::consts::OS;

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

    let pi_program = settings.custom_path.as_deref().unwrap_or("pi");

    info!(
        "Testing pi CLI connection with mode: {:?}, program: {}, provider: {:?}",
        effective_mode, pi_program, settings.provider
    );

    let output = match effective_mode {
        CliExecutionMode::WindowsNative | CliExecutionMode::Auto => {
            // On Windows, use cmd.exe /c to handle the .cmd shim from npm install
            crate::process_helpers::tokio_cmd_no_window()
                .args(["/c", pi_program, "--version"])
                .output()
                .await
                .map_err(|e| {
                    String::from(AppError::Raw(format!(
                        "Failed to execute pi CLI: {}. Is pi installed (npm i -g \
                         @earendil-works/pi-coding-agent) and in PATH?",
                        e
                    )))
                })?
        }
        CliExecutionMode::Wsl => crate::process_helpers::tokio_no_window("wsl")
            .args([pi_program, "--version"])
            .output()
            .await
            .map_err(|e| {
                String::from(AppError::Raw(format!(
                    "Failed to execute pi via WSL: {}. Is WSL installed?",
                    e
                )))
            })?,
        CliExecutionMode::Native => crate::process_helpers::tokio_no_window(pi_program)
            .args(["--version"])
            .output()
            .await
            .map_err(|e| {
                String::from(AppError::Raw(format!(
                    "Failed to execute pi CLI: {}. Is pi installed (npm i -g \
                     @earendil-works/pi-coding-agent) and in PATH?",
                    e
                )))
            })?,
    };

    if output.status.success() {
        let version = String::from_utf8_lossy(&output.stdout);
        Ok(format!(
            "pi CLI connected successfully (provider: {}, model: {}). {}",
            settings.provider.as_deref().unwrap_or("pi default"),
            settings.model.as_deref().unwrap_or("pi default"),
            version.trim()
        ))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("pi CLI returned error: {}", stderr.trim()))
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

    // coord-auth-exempt(not-coord): Google's Gemini API.
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
    // Read-modify-write under the AI-settings write lock so a concurrent
    // save (HTTP PUT, another command) cannot be lost between the read and
    // the write.
    settings::update_ai_settings(|existing_settings| {
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
            complex_keywords: existing_settings.routing.complex_keywords, // Preserve
            simple_keywords: existing_settings.routing.simple_keywords,   // Preserve
        };

        let ai_settings = AiSettings {
            provider: existing_settings.provider,
            claude_cli: existing_settings.claude_cli,
            claude_api: existing_settings.claude_api,
            gemini_cli: existing_settings.gemini_cli,
            gemini_api: existing_settings.gemini_api,
            pi_cli: existing_settings.pi_cli,
            ollama: existing_settings.ollama,
            openai_compatible: existing_settings.openai_compatible,
            auto_refine_video_after_iterations: existing_settings
                .auto_refine_video_after_iterations,
            compression: compression_config,
            retry: retry_config,
            routing: routing_config,
            interactive_sessions_enabled: existing_settings.interactive_sessions_enabled,
            ai_path_prediction_enabled: existing_settings.ai_path_prediction_enabled,
            // Preserve — see the note in `save_ai_settings`; reconstructing this
            // from the default would silently re-enable federation on every save.
            git_op_federation_enabled: existing_settings.git_op_federation_enabled,
        };

        Ok::<_, std::convert::Infallible>(ai_settings)
    })
    .map_err(|e| {
        let e = match e {
            settings::AiSettingsUpdateError::SaveFailed(msg) => msg,
            settings::AiSettingsUpdateError::Rejected(never) => match never {},
        };
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
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
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
    /// Session-window (5-hour) utilization 0.0–1.0. Only populated when the
    /// stats came from the OAuth usage endpoint (`source == "oauth_usage"`).
    #[serde(default)]
    pub session_utilization: Option<f64>,
    /// Unix seconds when the 5-hour session window resets.
    #[serde(default)]
    pub session_resets_at: Option<u64>,
    /// Per-model scoped weekly limits (e.g. the Fable-only cap).
    #[serde(default)]
    pub model_limits: Vec<ModelLimitInfo>,
    /// Where these stats came from: `"oauth_usage"` (free endpoint) or
    /// `"probe"` (Haiku header probe fallback).
    #[serde(default)]
    pub source: Option<String>,
}

/// One per-model scoped weekly limit, mirrored to the frontend.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModelLimitInfo {
    pub model: String,
    /// Fraction used, 0.0–1.0.
    pub utilization: f64,
    pub resets_at: Option<u64>,
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

/// How long a probe result — success OR failure — is reused before the
/// account is asked again.
///
/// Sized to swallow the burst: the four independent callers
/// (`check_accounts_usage`, `/analytics/account-usage`, the 10-minute
/// snapshot timer, `account_migration`'s confirmation) no longer each cost a
/// request, and a 429'd account is not immediately re-probed into another
/// 429. A minute is well inside the resolution of the weekly/5-hour windows
/// these numbers describe, so nothing downstream reads a materially staler
/// figure than before.
const USAGE_PROBE_TTL: Duration = Duration::from_secs(60);

/// Shared single-flight + TTL cache in front of the live probe. Keyed by
/// `config_dir`, which is exactly the per-account identity the probe and the
/// selection snapshot already use.
static USAGE_PROBE_CACHE: Lazy<
    crate::commands::usage_probe_cache::CoalescingCache<AccountUsageInfo>,
> = Lazy::new(|| crate::commands::usage_probe_cache::CoalescingCache::new(USAGE_PROBE_TTL));

/// An account's label: the basename of its config dir. A dir that names no
/// final component lexically (`…/..`, a trailing `.`) is resolved on disk
/// first. One that still has none (a filesystem root, or a path that does not
/// exist) keeps its full path, which serves local displays only: the coord
/// report sends no row whose label is not a bare name
/// ([`usage_twin_report::wire_label`]).
fn account_label(config_dir: &str) -> String {
    let path = std::path::Path::new(config_dir);
    if let Some(name) = path.file_name() {
        return name.to_string_lossy().into_owned();
    }
    std::fs::canonicalize(path)
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| config_dir.to_string())
}

/// Probe a single account for its weekly rate limit utilization.
///
/// Reads through a shared single-flight + TTL cache
/// ([`USAGE_PROBE_CACHE`]): concurrent callers for the same account await ONE
/// upstream request, and a result is reused for [`USAGE_PROBE_TTL`].
///
/// This is not an optimization. The probe has four independent callers that
/// each fanned out one request per configured account with no coalescing and
/// no TTL — measured at 25 usage checks per 5-minute tick and **18,981 HTTP
/// 429s per day** in the dev logs. Because the probe consumes the same
/// per-account quota the CLI does, that stampede both competed with real work
/// and then reported the 429s it earned as account exhaustion.
///
/// See [`crate::commands::usage_probe_cache`] for the mechanism.
pub async fn probe_account_usage(config_dir: String) -> AccountUsageInfo {
    // Subscription accounts are off while a model gateway is declared, and a
    // subscription token must not be spent or sent on their behalf
    // (`crate::model_gateway`). Answered before the cache so no stale sample
    // outlives the switch.
    if crate::model_gateway::gateway_declared() {
        let label = account_label(&config_dir);
        return AccountUsageInfo {
            config_dir,
            label,
            error: Some(crate::model_gateway::SUBSCRIPTION_OFF_REASON.to_string()),
            ..Default::default()
        };
    }
    let key = config_dir.clone();
    USAGE_PROBE_CACHE
        .get_or_fetch(&key, move || probe_account_usage_uncached(config_dir))
        .await
}

/// The live probe. Makes a minimal API call (1 token, cheapest model) and
/// reads the `anthropic-ratelimit-unified-7d-utilization` response header,
/// which always contains the exact weekly usage fraction regardless of
/// threshold.
///
/// Call [`probe_account_usage`] instead — going straight to this bypasses the
/// coalescing that keeps the runner off Anthropic's rate limiter.
async fn probe_account_usage_uncached(config_dir: String) -> AccountUsageInfo {
    let label = account_label(&config_dir);

    // Pre-flight: if the stored OAuth token is already expired, attempt a
    // silent refresh BEFORE issuing the probe. Mirrors the warm-path discipline
    // at `claude_api_warm.rs:174-188`. Refresh failure is non-fatal — we still
    // try the probe with whatever token we have so the caller sees a real auth
    // error rather than a swallowed "couldn't refresh."
    let creds_path = std::path::PathBuf::from(&config_dir).join(".credentials.json");
    if is_oauth_token_expired(&creds_path) {
        // refresh_credentials_blocking uses reqwest::blocking::Client internally.
        // Calling it directly from async context panics ("Cannot drop a runtime
        // in a context where blocking is not allowed"), so offload to a blocking
        // thread. Refresh failure is non-fatal — we still probe with whatever
        // token we have.
        let creds_for_refresh = creds_path.clone();
        let _ = spawn_blocking_tracked(move || {
            crate::ai_provider::oauth_refresh::refresh_credentials_blocking(&creds_for_refresh)
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
                error: Some(e),
                ..Default::default()
            };
        }
    };

    // Preferred source: the OAuth usage endpoint — zero token cost, exact
    // session-window (5h) + weekly utilization and reset times (the same
    // numbers `/usage` shows). Subscription OAuth tokens only; API keys have
    // no usage windows, so they skip straight to the header probe below.
    if token.starts_with("sk-ant-oat") {
        match crate::ai_provider::oauth_usage::fetch(&token).await {
            Ok(snap) => {
                let weekly = snap.seven_day.clone();
                let session = snap.five_hour.clone();
                let utilization_7d = weekly.as_ref().map(|w| w.utilization).unwrap_or(0.0);
                let resets_at_7d = weekly.as_ref().and_then(|w| w.resets_at);
                let (expected, delta, elapsed_frac, remaining_days) =
                    compute_expected_usage(utilization_7d, resets_at_7d);
                info!(
                    "Account '{}' (oauth_usage): 7d={:.0}% 5h={:.0}%",
                    label,
                    utilization_7d * 100.0,
                    session
                        .as_ref()
                        .map(|s| s.utilization * 100.0)
                        .unwrap_or(0.0),
                );
                return AccountUsageInfo {
                    config_dir,
                    label,
                    utilization: utilization_7d,
                    rate_limit_type: Some("seven_day".to_string()),
                    resets_at: resets_at_7d,
                    status: snap.blocking_severity.then(|| "blocked".to_string()),
                    error: None,
                    expected_utilization: expected,
                    usage_delta: delta,
                    period_elapsed_fraction: elapsed_frac,
                    period_remaining_days: remaining_days,
                    session_utilization: session.as_ref().map(|s| s.utilization),
                    session_resets_at: session.as_ref().and_then(|s| s.resets_at),
                    model_limits: snap
                        .model_limits
                        .into_iter()
                        .map(|m| ModelLimitInfo {
                            model: m.model,
                            utilization: m.utilization,
                            resets_at: m.resets_at,
                        })
                        .collect(),
                    source: Some("oauth_usage".to_string()),
                };
            }
            Err(e) => {
                info!(
                    "Account '{}': oauth usage endpoint unavailable ({e}) — falling back to header probe",
                    label
                );
            }
        }
    }

    // Make a minimal API call — Haiku with max_tokens=1 is the cheapest possible.
    // OAuth tokens (`sk-ant-oat*`) must go via `Authorization: Bearer` +
    // `anthropic-beta: oauth-2025-04-20`; API keys (`sk-ant-api*`) go via
    // `x-api-key`. `anthropic_auth::apply_async` dispatches on token prefix.
    let client = reqwest::Client::new();
    // Not a coord write: Anthropic's API, authenticated by the operator's own
    // OAuth/API credential via `anthropic_auth::apply_async`.
    // Always the vendor route: this measures a subscription account's own quota,
    // and `probe_account_usage` never reaches here while a gateway is declared.
    let request = crate::model_gateway::ModelCall::Vendor {
        token: token.to_string(),
    }
    .post_async(&client, crate::model_gateway::MESSAGES_PATH)
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
                session_utilization: utilization_5h,
                source: Some("probe".to_string()),
                ..Default::default()
            }
        }
        Err(e) => AccountUsageInfo {
            config_dir,
            label,
            utilization: 1.0,
            error: Some(format!("Network error: {}", e)),
            source: Some("probe".to_string()),
            ..Default::default()
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
    // `pick_best_account` can rank by weekly-usage pace without probing
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
/// server reports the account rejected/blocked/exceeded, when weekly
/// utilization is at/over [`EXHAUSTION_UTILIZATION`], or when the 5-hour
/// session window is at/over it (the OAuth usage endpoint reports the session
/// window; the header probe leaves it `None`). The session-window arm is what
/// lets a "5-hour limit reached" migration hint confirm — the weekly number
/// alone stays low when only the session window is exhausted.
fn probe_result_exhausted(info: &AccountUsageInfo) -> bool {
    if info.error.is_some() || info.utilization >= EXHAUSTION_UTILIZATION {
        return true;
    }
    if info
        .session_utilization
        .is_some_and(|u| u >= EXHAUSTION_UTILIZATION)
    {
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
    let samples: Vec<(String, f64, Option<f64>, Option<f64>, bool)> = results
        .iter()
        .map(|r| {
            (
                r.config_dir.clone(),
                r.utilization,
                r.usage_delta,
                r.expected_utilization,
                probe_result_exhausted(r),
            )
        })
        .collect();
    crate::ai_provider::record_account_usage(&samples);
}

/// Probe every configured account and refresh the selection usage snapshot.
///
/// Called on demand — `account_migration` re-probes before acting on a usage
/// limit — directly at startup, and through [`refresh_fleet_usage_report`] on
/// the periodic timer (see `main.rs`), so a runner whose Settings/Terminal UI is
/// never opened (e.g. a headless or co-pilot-only runner) still has fresh
/// weekly-usage pace data for `pick_best_account`. No-op when fewer than two
/// accounts are configured. This path sits in front of the startup pick and the
/// migration decision, so a single-account runner must not pay a probe here.
/// The periodic fleet report covers that runner instead (see
/// [`plan_usage_refresh`]).
///
/// Returns the fresh per-account results so callers that need more than the
/// recorded snapshot (e.g. `account_migration`'s reset-time-aware cooldown)
/// don't have to re-probe. Also mirrors the stats to coord's twin
/// (best-effort, fire-and-forget) — accounts only; the prepaid probe is a
/// network round-trip to the provider that the latency-sensitive migration
/// path must not pay.
pub async fn refresh_account_usage_snapshot() -> Vec<AccountUsageInfo> {
    refresh_usage(UsageRefreshCaller::SelectionSnapshot).await
}

/// The periodic fleet report: [`refresh_account_usage_snapshot`] PLUS the
/// prepaid (pay-as-you-go) balances, mirrored to coord on the same body.
///
/// Both halves report on a runner with fewer than two Claude accounts. A
/// prepaid balance is money on the account whatever the Claude roster looks
/// like (plan `2026-09-12-prepaid-balance-is-a-fleet-fact-with-no-ingest`). A
/// lone Claude account spends a usage window that coord's launch budget has to
/// count (plan `2026-10-29-launch-budget-is-a-coord-answer-not-a-per-box-script`
/// Phase 2). That includes a runner with NO configured account list, whose one
/// account is the one it spends (see [`spent_account_dir`]). Before that plan,
/// this half sent nothing for such a runner.
pub async fn refresh_fleet_usage_report() -> Vec<AccountUsageInfo> {
    refresh_usage(UsageRefreshCaller::FleetReport).await
}

/// Which of the two refresh entry points is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageRefreshCaller {
    /// Startup and `account_migration`: feeds the account picker.
    SelectionSnapshot,
    /// The 10-minute timer: the fleet report to coord.
    FleetReport,
}

/// What one refresh does, decided from how many Claude config dirs the runner
/// has configured and who is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UsageRefreshPlan {
    /// Probe the configured dirs.
    probe_configured: bool,
    /// Below two configured dirs, probe instead the one account this runner
    /// spends ([`spent_account_dir`]).
    probe_spent: bool,
    /// Record the results into the account picker's snapshot.
    record_snapshot: bool,
}

/// The picker's snapshot is recorded only from two configured accounts up,
/// exactly as before plan `2026-10-29-launch-budget-…` Phase 2, so picker
/// behaviour does not change. The fleet report also covers a runner with fewer
/// than two, by probing the account it spends, because coord's launch budget
/// has to count it. That probe's only consumer is the report, so it runs only
/// when `reports_lone_account`: the runner has a coord to report to, and it is
/// the box's primary. A secondary (a named or temp runner, or a subject under
/// an instance root) resolves the same machine-global roster and the same
/// `~/.claude`, so its report would write the primary's account row a second
/// time.
fn plan_usage_refresh(
    dir_count: usize,
    caller: UsageRefreshCaller,
    reports_lone_account: bool,
) -> UsageRefreshPlan {
    let multi = dir_count >= 2;
    UsageRefreshPlan {
        probe_configured: multi,
        probe_spent: !multi && caller == UsageRefreshCaller::FleetReport && reports_lone_account,
        record_snapshot: multi,
    }
}

/// The account a runner with fewer than two configured accounts actually
/// spends: what a `claude` it spawns runs on. That is the dir the spawn paths
/// pin as `CLAUDE_CONFIG_DIR` ([`crate::ai_provider::get_effective_config_dir`]:
/// the picker's pick or `claude_cli.config_dir`, when it has credentials).
/// When nothing is pinned the child inherits the runner's own environment, so
/// it is then the runner's `CLAUDE_CONFIG_DIR`, else the default `~/.claude`
/// when it exists. A lone roster entry is therefore NOT assumed to be the
/// account in use: in `Manual` mode the spawn takes `config_dir`, whatever the
/// roster lists. `None` under a model gateway, which has no subscription
/// account, and when no candidate exists.
fn spent_account_dir() -> Option<String> {
    if crate::model_gateway::gateway_declared() {
        return None;
    }
    let (pinned, _source) =
        crate::ai_provider::get_effective_config_dir(&settings::get_ai_settings().claude_cli);
    pinned
        .or_else(|| {
            std::env::var("CLAUDE_CONFIG_DIR")
                .ok()
                .filter(|dir| !dir.trim().is_empty())
        })
        .or_else(|| {
            default_claude_config_dir()
                .filter(|dir| dir.is_dir())
                .map(|dir| dir.to_string_lossy().into_owned())
        })
}

async fn refresh_usage(caller: UsageRefreshCaller) -> Vec<AccountUsageInfo> {
    let mut config_dirs = crate::settings::get_claude_config_dirs();
    let report_base = usage_twin_report::report_base();
    let reports_lone_account =
        report_base.is_some() && !qontinui_runner_lib::instance_env::is_secondary();
    let plan = plan_usage_refresh(config_dirs.len(), caller, reports_lone_account);
    if !plan.probe_configured {
        config_dirs.clear();
    }
    // Which probed accounts are the ones in use. From a roster, the picker's
    // selection (`account_infos`, one source with the Tauri command). A lone
    // spent account is in use by construction: it is what a spawn runs on.
    let active_dirs: std::collections::HashSet<String> = if plan.probe_spent {
        let spent = spent_account_dir();
        config_dirs.extend(spent.clone());
        spent.into_iter().collect()
    } else {
        account_infos()
            .into_iter()
            .filter(|a| a.is_active)
            .map(|a| a.config_dir)
            .collect()
    };
    let futures: Vec<_> = config_dirs
        .into_iter()
        .map(|dir| async move { probe_account_usage(dir).await })
        .collect();
    let results = futures::future::join_all(futures).await;
    if plan.record_snapshot {
        record_usage_snapshot(&results);
    }
    let prepaid = if caller == UsageRefreshCaller::FleetReport {
        crate::ai_provider::prepaid_balance::get_prepaid_balances().await
    } else {
        Vec::new()
    };
    // Nothing measured, nothing to say. An empty `prepaid` makes coord write
    // nothing for that half, so the accounts-only caller can never blank a
    // balance the timer reported.
    if let Some(base) = report_base.filter(|_| !results.is_empty() || !prepaid.is_empty()) {
        usage_twin_report::report_to_coord(&base, &results, &active_dirs, &prepaid).await;
    }
    results
}

/// Mirror per-account usage stats into coord's digital twin.
///
/// Best-effort and strictly fire-and-forget, mirroring the discipline of
/// `terminal::auto_response::report`: bounded timeout, no retries, any
/// failure is a `warn!` and a return. A coord that predates the
/// `/coord/claude-accounts/usage` route 404s — silently tolerated so the
/// runner PR can land independently of the coord one.
mod usage_twin_report {
    use serde::Serialize;
    use tracing::{debug, warn};

    /// Wire shape — SHARED CONTRACT with coord's
    /// `POST /coord/claude-accounts/usage` ingest. Account identity is the
    /// config-dir basename (label), never the full local path. Utilizations
    /// are 0.0–1.0 fractions; reset times unix seconds.
    #[derive(Serialize)]
    struct WireAccount<'a> {
        label: &'a str,
        weekly_utilization: f64,
        weekly_resets_at: Option<u64>,
        session_utilization: Option<f64>,
        session_resets_at: Option<u64>,
        model_limits: &'a [super::ModelLimitInfo],
        exhausted: bool,
        source: Option<&'a str>,
        error: bool,
        /// The provider's own verdict on this account, passed through unread:
        /// the 7-day status header on the probe path (`allowed`,
        /// `allowed_warning`, `rejected`), `blocked` from the OAuth usage
        /// endpoint. Absent when the source gave none, which is UNKNOWN, not
        /// "allowed". Coord's ingest ignores this key until Phase 3 of plan
        /// `2026-10-29-launch-budget-is-a-coord-answer-not-a-per-box-script`
        /// stores it for the launch budget's `provider_rejected` HOLD.
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<&'a str>,
        /// Which account this machine's rotation has ACTUALLY selected right
        /// now — the same flag [`super::AccountInfo::is_active`] carries, from
        /// the same `ai_provider::get_account_statuses()` source. Without it
        /// the operator sees a roster with utilizations but no answer to "so
        /// which one runs?". At most one account is `true`; all-`false` is a
        /// legitimate state (no dir resolved yet on this machine).
        is_active: bool,
    }

    /// One prepaid provider balance — SHARED CONTRACT with coord's
    /// `claude_account_usage::WirePrepaid`. Money is integer MICROS (1e-6 of
    /// one `currency` unit) parsed exactly from the provider's decimal string
    /// (`PrepaidBalanceInfo::*_micros`), never converted through `f64`. Every
    /// measured field is `Option` and serializes `null` when unknown: coord
    /// stores `null` as NOT REPORTED and a real `0` as "out of credit", and
    /// those must not collapse. `error` is `Some` exactly when the probe
    /// failed.
    #[derive(Serialize)]
    struct WirePrepaid<'a> {
        provider: &'a str,
        label: &'a str,
        currency: Option<&'a str>,
        balance_micros: Option<i64>,
        granted_micros: Option<i64>,
        topped_up_micros: Option<i64>,
        is_available: Option<bool>,
        error: Option<&'a str>,
    }

    impl<'a> WirePrepaid<'a> {
        fn from_info(p: &'a crate::ai_provider::prepaid_balance::PrepaidBalanceInfo) -> Self {
            Self {
                provider: &p.provider,
                label: &p.label,
                currency: p.currency.as_deref(),
                balance_micros: p.balance_micros,
                granted_micros: p.granted_micros,
                topped_up_micros: p.topped_up_micros,
                is_available: p.is_available,
                error: p.error.as_deref(),
            }
        }
    }

    #[derive(Serialize)]
    struct WireBody<'a> {
        accounts: Vec<WireAccount<'a>>,
        /// Prepaid balances measured on the same refresh — a DIFFERENT object
        /// family from `accounts` (keyed by provider, money not utilization),
        /// stored by coord in its own `coord.prepaid_balances` table.
        prepaid: Vec<WirePrepaid<'a>>,
        /// The machine-global selection strategy, snake_case
        /// (`"manual"` | `"least_usage"` | `"highest_expected_usage"` |
        /// `"weighted_random"` | `"alternating_handoff"` — see
        /// [`crate::settings::AccountSelectionMode::as_str`]). Per-REPORT, not
        /// per-account: the roster file holds one mode for the whole box.
        ///
        /// This is what makes the operator string *"this machine will use:
        /// &lt;mode&gt;"* answerable from the feed alone.
        account_selection_mode: &'static str,
    }

    /// The coord this runner reports to, or `None` when it reports nothing.
    ///
    /// Connected-set: on a hosted (`qontinui_account`-tier) runner the
    /// production coordinator IS this account's backend, so the usage rollup
    /// must reach it even when profiles.json carries no explicit `coord_url`.
    /// An isolated runner reports nothing, and the
    /// `QONTINUI_ACCOUNT_USAGE_REPORT_DISABLED` kill switch opts a connected
    /// one out.
    pub(super) fn report_base() -> Option<String> {
        if std::env::var_os("QONTINUI_ACCOUNT_USAGE_REPORT_DISABLED").is_some() {
            return None;
        }
        qontinui_runner_lib::profiles::connected_coord_base()
    }

    /// The label as sent, or `None` for a label that is not a bare name: the
    /// wire never carries a local path ([`super::account_label`] keeps one for
    /// a dir with no final component).
    fn wire_label(label: &str) -> Option<&str> {
        let bare = !label.is_empty()
            && label != "."
            && label != ".."
            && !label.contains(['/', '\\'])
            && !std::path::Path::new(label).has_root();
        bare.then_some(label)
    }

    /// Post one report. `active_dirs` names the probed accounts in use, keyed
    /// by `config_dir` (not `label`: two roster dirs can share a basename, and
    /// the probe results carry the full dir).
    pub(super) async fn report_to_coord(
        base: &str,
        results: &[super::AccountUsageInfo],
        active_dirs: &std::collections::HashSet<String>,
        prepaid: &[crate::ai_provider::prepaid_balance::PrepaidBalanceInfo],
    ) {
        let url = format!("{}/coord/claude-accounts/usage", base.trim_end_matches('/'));
        let body = WireBody {
            accounts: results
                .iter()
                .filter_map(|r| {
                    let label = wire_label(&r.label);
                    if label.is_none() {
                        warn!("account usage row with no bare-name label not reported to coord");
                    }
                    Some((r, label?))
                })
                .map(|(r, label)| WireAccount {
                    label,
                    weekly_utilization: r.utilization,
                    weekly_resets_at: r.resets_at,
                    session_utilization: r.session_utilization,
                    session_resets_at: r.session_resets_at,
                    model_limits: &r.model_limits,
                    exhausted: super::probe_result_exhausted(r),
                    source: r.source.as_deref(),
                    error: r.error.is_some(),
                    status: r.status.as_deref(),
                    is_active: active_dirs.contains(&r.config_dir),
                })
                .collect(),
            prepaid: prepaid.iter().map(WirePrepaid::from_info).collect(),
            account_selection_mode: crate::claude_accounts::effective_selection_mode().as_str(),
        };
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        // coord-tenant-scope(device): the payload is this machine's whole Claude-account roster (:1540-1561) -- a device property, with no session and no artifact in scope.
        let req = qontinui_runner_lib::auth::attach_device_auth(client.post(&url));
        match req.json(&body).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let success = resp.status().is_success();
                let bytes = resp.bytes().await.unwrap_or_default();
                // coord's FleetPrincipal verifies the bearer on this route, so
                // a 2xx is a real Accepted (resets the dark streak); coord's
                // codeless 403 classifies Indeterminate and records nothing.
                crate::mcp::device_jwt_refresher::note_coord_upstream_verdict(
                    None, true, status, &bytes,
                );
                if success {
                    *last_refusal().lock().unwrap_or_else(|e| e.into_inner()) = None;
                    debug!(
                        accounts = results.len(),
                        prepaid = prepaid.len(),
                        "account usage mirrored to coord twin"
                    );
                } else {
                    let body = String::from_utf8_lossy(&bytes);
                    let posture = crate::mcp::device_jwt_refresher::coord_credential_posture()
                        .map(|s| s.posture);
                    if let Some(line) = refusal_warning(last_refusal(), status, &body, posture) {
                        warn!("{line}");
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "coord account-usage report failed — skipped");
            }
        }
    }

    /// The (status, refusal token, posture) of the last refusal WARNed. The report runs on
    /// every usage refresh, so an unchanged refusal must not re-WARN each tick
    /// (147 identical lines in one day on 2026-10-07). Cleared on a 2xx so the
    /// next refusal after a recovery is announced again.
    type RefusalKey = (u16, String, String);

    fn last_refusal() -> &'static std::sync::Mutex<Option<RefusalKey>> {
        static LAST: std::sync::Mutex<Option<RefusalKey>> = std::sync::Mutex::new(None);
        &LAST
    }

    /// The WARN line for a non-2xx ingest answer, or `None` when it repeats
    /// the last one.
    ///
    /// 401/403 is a CREDENTIAL refusal, not route-absence: coord's ingest
    /// answers every device-verification failure with a uniform
    /// `403 {"error":"auth_required"}` and no typed code
    /// (qontinui-coord `fleet_principal.rs`), so the cause cannot be read off
    /// the response — it is named from the runner's own credential posture.
    /// Every other status keeps the older-coord wording.
    fn refusal_warning(
        last: &std::sync::Mutex<Option<RefusalKey>>,
        status: u16,
        body: &str,
        posture: Option<crate::mcp::device_jwt_refresher::CoordCredentialPosture>,
    ) -> Option<String> {
        use crate::mcp::device_jwt_refresher::upstream_refusal_code;
        let credential = matches!(status, 401 | 403);
        let posture_label = match posture {
            Some(p) => match p.cause() {
                Some(cause) => format!("{} ({cause})", p.as_str()),
                None => p.as_str().to_string(),
            },
            None => "unknown (no refresher pass yet)".to_string(),
        };
        let code = if credential {
            upstream_refusal_code(body)
                .or_else(|| {
                    serde_json::from_str::<serde_json::Value>(body)
                        .ok()?
                        .get("error")?
                        .as_str()
                        .map(str::to_string)
                })
                .unwrap_or_else(|| "none".to_string())
        } else {
            String::new()
        };
        let key = (
            status,
            code.clone(),
            if credential {
                posture_label.clone()
            } else {
                String::new()
            },
        );
        {
            let mut guard = last.lock().unwrap_or_else(|e| e.into_inner());
            if guard.as_ref() == Some(&key) {
                return None;
            }
            *guard = Some(key);
        }
        if !credential {
            return Some(format!(
                "coord account-usage ingest non-2xx ({status}) (older coord?) — skipped"
            ));
        }
        let action = if posture.is_some_and(|p| p.can_answer()) {
            "credential looks live locally; coord does not say why"
        } else {
            "re-pair"
        };
        Some(format!(
            "coord account-usage ingest refused ({status} {code}) — credential rejected; \
             runner coord credential posture: {posture_label} — device feed not updating; {action}"
        ))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The wire KEYS are the shared contract with coord's
        /// `claude_account_usage.rs`; a rename on either side is a silent
        /// data loss (serde drops unknown keys on the ingest side). Pin them.
        #[test]
        fn wire_body_key_names_are_the_shared_contract() {
            let limits: Vec<super::super::ModelLimitInfo> = Vec::new();
            let body = WireBody {
                accounts: vec![WireAccount {
                    label: ".claude-hotmail",
                    weekly_utilization: 0.25,
                    weekly_resets_at: Some(1_700_000_000),
                    session_utilization: Some(0.5),
                    session_resets_at: None,
                    model_limits: &limits,
                    exhausted: false,
                    source: Some("oauth_usage"),
                    error: false,
                    status: None,
                    is_active: true,
                }],
                prepaid: Vec::new(),
                account_selection_mode: "least_usage",
            };
            let v = serde_json::to_value(&body).expect("serializes");

            // Per-report field, snake_case, NOT nested under an account.
            assert_eq!(
                v["account_selection_mode"],
                serde_json::json!("least_usage")
            );
            let acct = &v["accounts"][0];
            assert_eq!(acct["is_active"], serde_json::json!(true));
            // Identity stays the config-dir basename — never a full local path.
            assert_eq!(acct["label"], serde_json::json!(".claude-hotmail"));
            assert!(
                acct.get("config_dir").is_none(),
                "the wire must never carry a local path"
            );
        }

        /// An expired credential against coord's uniform
        /// `403 {"error":"auth_required"}` yields ONE credential-rejected WARN
        /// across repeated reports — not one per refresh — and a recovery
        /// re-arms it.
        #[test]
        fn expired_credential_refusal_warns_once_per_state_change() {
            use crate::mcp::device_jwt_refresher::CoordCredentialPosture;
            let last = std::sync::Mutex::new(None);
            let body = r#"{"error":"auth_required"}"#;
            let expired = Some(CoordCredentialPosture::Expired);
            let lines: Vec<String> = (0..5)
                .filter_map(|_| refusal_warning(&last, 403, body, expired))
                .collect();
            assert_eq!(lines.len(), 1, "one WARN, not one per tick: {lines:?}");
            let line = &lines[0];
            assert!(line.contains("refused (403 auth_required)"), "{line}");
            assert!(line.contains("credential rejected"), "{line}");
            assert!(line.contains("posture: expired"), "{line}");
            assert!(line.ends_with("re-pair"), "{line}");
            assert!(!line.contains("older coord"), "{line}");

            // A posture change is new information and WARNs again.
            let absent = refusal_warning(&last, 403, body, Some(CoordCredentialPosture::Absent));
            assert!(absent.is_some_and(|l| l.contains("posture: absent")));

            // A 2xx clears the state (what `report_to_coord` does), so the
            // next refusal is announced again.
            *last.lock().unwrap() = None;
            assert!(refusal_warning(&last, 403, body, expired).is_some());
        }

        /// A 404 is route-absence and keeps the older-coord wording.
        #[test]
        fn not_found_keeps_older_coord_wording() {
            let last = std::sync::Mutex::new(None);
            let line = refusal_warning(&last, 404, "", None).expect("first warns");
            assert!(line.contains("older coord?"), "{line}");
            assert!(!line.contains("credential"), "{line}");
            assert!(refusal_warning(&last, 404, "", None).is_none());
        }

        /// What `report_to_coord` itself decides: a reading with no status
        /// carries no `status` key (an absent status is UNKNOWN, so it must not
        /// arrive as a value), and a row whose label is not a bare name is not
        /// sent at all, so a local path never reaches coord.
        #[tokio::test]
        async fn report_to_coord_omits_absent_status_and_path_labels() {
            let _amb = crate::test_env::isolated_ambient();
            let coord = super::super::usage_report_scope_tests::FakeCoord::start().await;
            let row = |label: &str| crate::commands::ai_settings::AccountUsageInfo {
                config_dir: format!("/x/{label}"),
                label: label.into(),
                ..Default::default()
            };
            let rows = [
                row(".claude-silent"),
                row("/home/someone/.claude"),
                row("/"),
                row(".."),
                row(r"C:\Users\someone\.claude"),
            ];

            report_to_coord(&coord.base, &rows, &Default::default(), &[]).await;

            let bodies = coord.bodies();
            assert_eq!(bodies.len(), 1, "one report, one POST: {bodies:?}");
            let accounts = bodies[0]["accounts"].as_array().expect("accounts");
            assert_eq!(
                accounts.len(),
                1,
                "only the bare-name row is sent: {accounts:?}"
            );
            assert_eq!(accounts[0]["label"], serde_json::json!(".claude-silent"));
            assert!(
                accounts[0].get("status").is_none(),
                "no status means no key: {}",
                accounts[0]
            );
        }

        /// `is_active` is a per-account flag; a report where nothing has been
        /// resolved yet is all-`false`, not a missing key.
        #[test]
        fn inactive_accounts_serialize_is_active_false() {
            let limits: Vec<super::super::ModelLimitInfo> = Vec::new();
            let body = WireBody {
                accounts: vec![WireAccount {
                    label: ".claude-work",
                    weekly_utilization: 0.0,
                    weekly_resets_at: None,
                    session_utilization: None,
                    session_resets_at: None,
                    model_limits: &limits,
                    exhausted: false,
                    source: None,
                    error: true,
                    status: None,
                    is_active: false,
                }],
                prepaid: Vec::new(),
                account_selection_mode: "manual",
            };
            let v = serde_json::to_value(&body).expect("serializes");
            assert_eq!(v["accounts"][0]["is_active"], serde_json::json!(false));
            assert_eq!(v["account_selection_mode"], serde_json::json!("manual"));
        }

        fn prepaid_info(
            balance_micros: Option<i64>,
            error: Option<&str>,
        ) -> crate::ai_provider::prepaid_balance::PrepaidBalanceInfo {
            crate::ai_provider::prepaid_balance::PrepaidBalanceInfo {
                provider: "deepseek".into(),
                label: "DeepSeek".into(),
                currency: balance_micros.map(|_| "USD".to_string()),
                balance: balance_micros.map(|m| m as f64 / 1e6),
                granted_balance: None,
                topped_up_balance: balance_micros.map(|m| m as f64 / 1e6),
                is_available: balance_micros.map(|m| m > 0),
                error: error.map(str::to_string),
                balance_micros,
                granted_micros: None,
                topped_up_micros: balance_micros,
            }
        }

        /// The prepaid wire KEYS are the same shared contract, one struct
        /// over: coord's `WirePrepaid` deserializes exactly these names, and a
        /// rename on either side is silently dropped by serde. Pin every key.
        #[test]
        fn prepaid_wire_key_names_are_the_shared_contract() {
            let info = prepaid_info(Some(19_280_000), None);
            let body = WireBody {
                accounts: Vec::new(),
                prepaid: vec![WirePrepaid::from_info(&info)],
                account_selection_mode: "manual",
            };
            let v = serde_json::to_value(&body).expect("serializes");
            let p = &v["prepaid"][0];
            let mut keys: Vec<&str> = p
                .as_object()
                .expect("object")
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                vec![
                    "balance_micros",
                    "currency",
                    "error",
                    "granted_micros",
                    "is_available",
                    "label",
                    "provider",
                    "topped_up_micros",
                ]
            );
            assert_eq!(p["provider"], serde_json::json!("deepseek"));
            // Integer micros — never a float on the coord wire.
            assert_eq!(p["balance_micros"], serde_json::json!(19_280_000));
            assert!(p["balance_micros"].is_i64());
            // Unknown is an explicit null, never 0.
            assert!(p["granted_micros"].is_null());
        }

        /// An errored probe ships `error` and nulls — never zeros, which coord
        /// would store as "out of credit".
        #[test]
        fn errored_prepaid_probe_ships_nulls_not_zeros() {
            // The REAL error constructor, so a regression that reintroduces
            // best-effort zeros there is caught here too.
            let info = crate::ai_provider::prepaid_balance::PrepaidBalanceInfo::error(
                "deepseek",
                "DeepSeek",
                "API error (401)".to_string(),
            );
            let v = serde_json::to_value(WirePrepaid::from_info(&info)).expect("serializes");
            assert_eq!(v["error"], serde_json::json!("API error (401)"));
            for k in [
                "balance_micros",
                "granted_micros",
                "topped_up_micros",
                "is_available",
                "currency",
            ] {
                assert!(
                    v[k].is_null(),
                    "{k} must be null on an errored probe, got {}",
                    v[k]
                );
            }
        }
    }
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

/// The live roster status as [`AccountInfo`] rows.
///
/// Extracted so the Tauri command and the coord usage feed
/// (`usage_twin_report`) read ONE source: the feed's `is_active` must mean
/// exactly what the Settings UI's does, and two independent derivations of
/// "which account is active" is how those two answers drift apart.
pub(crate) fn account_infos() -> Vec<AccountInfo> {
    crate::ai_provider::get_account_statuses()
        .into_iter()
        .map(|(dir, label, active, cooled)| AccountInfo {
            config_dir: dir,
            label,
            is_active: active,
            is_rate_limited: cooled,
        })
        .collect()
}

fn get_claude_accounts_impl() -> Result<CommandResponse, AppError> {
    let accounts = account_infos();

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

/// Claude Code's own default config dir, `~/.claude`. It belongs to the user's
/// Claude login rather than to the runner, so it stays machine-global even
/// under an instance root. This is the file's one door to it (the census in
/// `ambient::tests::USER_GLOBAL_DIRS_ALLOWLIST` counts exactly one).
fn default_claude_config_dir() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|home| home.join(".claude"))
}

/// Find the Claude CLI credentials file, respecting config_dir settings.
pub(crate) fn find_claude_credentials_path() -> Option<std::path::PathBuf> {
    // 1. Check effective config_dir from runner AI settings (respects least-usage mode)
    let ai_settings = settings::get_ai_settings();
    let (effective_dir, _config_dir_source) =
        crate::ai_provider::get_effective_config_dir(&ai_settings.claude_cli);
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
    if let Some(dir) = default_claude_config_dir() {
        let path = dir.join(".credentials.json");
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

    let (effective_dir, _config_dir_source) =
        crate::ai_provider::get_effective_config_dir(&ai_settings.claude_cli);
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

        // console-ok: the auth flow WANTS a visible terminal for the user to log in in.
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
            // console-ok: the auth flow WANTS a visible terminal for the user to log in in.
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
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

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
mod save_selection_mode_tests {
    use super::*;

    #[test]
    fn absent_mode_keeps_the_stored_mode() {
        for stored in [
            AccountSelectionMode::Manual,
            AccountSelectionMode::LeastUsage,
            AccountSelectionMode::HighestExpectedUsage,
            AccountSelectionMode::WeightedRandom,
            AccountSelectionMode::AlternatingHandoff,
        ] {
            assert_eq!(resolve_saved_selection_mode(None, stored), stored);
        }
    }

    #[test]
    fn sent_mode_replaces_the_stored_mode() {
        let stored = AccountSelectionMode::Manual;
        assert_eq!(
            resolve_saved_selection_mode(Some("least_usage"), stored),
            AccountSelectionMode::LeastUsage
        );
        assert_eq!(
            resolve_saved_selection_mode(Some("highest_expected_usage"), stored),
            AccountSelectionMode::HighestExpectedUsage
        );
        assert_eq!(
            resolve_saved_selection_mode(Some("manual"), AccountSelectionMode::LeastUsage),
            AccountSelectionMode::Manual
        );
        assert_eq!(
            resolve_saved_selection_mode(Some("weighted_random"), stored),
            AccountSelectionMode::WeightedRandom
        );
        assert_eq!(
            resolve_saved_selection_mode(Some("alternating_handoff"), stored),
            AccountSelectionMode::AlternatingHandoff
        );
        // An unknown spelling falls back to the default.
        assert_eq!(
            resolve_saved_selection_mode(Some("bogus"), stored),
            AccountSelectionMode::default()
        );
    }
}

#[cfg(test)]
mod usage_report_scope_tests {
    use super::{
        plan_usage_refresh, refresh_account_usage_snapshot, refresh_fleet_usage_report,
        AccountUsageInfo, UsageRefreshCaller, UsageRefreshPlan, USAGE_PROBE_CACHE,
    };
    use crate::settings::AccountSelectionMode;
    use crate::test_env::{EnvVarRestore, IsolatedAmbient};
    use std::sync::{Arc, Mutex};

    /// A coord that records every usage report it is sent, so these tests read
    /// what actually went out on the wire.
    pub(super) struct FakeCoord {
        pub(super) base: String,
        received: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    impl FakeCoord {
        pub(super) async fn start() -> Self {
            let received = Arc::new(Mutex::new(Vec::new()));
            let sink = received.clone();
            let app = axum::Router::new().route(
                "/coord/claude-accounts/usage",
                axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                    let sink = sink.clone();
                    async move {
                        sink.lock().unwrap().push(body);
                        axum::http::StatusCode::OK
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.ok();
            });
            Self {
                base: format!("http://{addr}"),
                received,
            }
        }

        pub(super) fn bodies(&self) -> Vec<serde_json::Value> {
            self.received.lock().unwrap().clone()
        }

        /// Every account row coord received, in order.
        fn rows(&self) -> Vec<serde_json::Value> {
            self.bodies()
                .iter()
                .flat_map(|b| b["accounts"].as_array().cloned().unwrap_or_default())
                .collect()
        }

        /// The labels of every account row coord received, in order.
        fn reported_labels(&self) -> Vec<String> {
            self.rows()
                .iter()
                .map(|a| a["label"].as_str().unwrap_or_default().to_string())
                .collect()
        }

        /// Make this the coord the runner under test is connected to.
        /// `isolated_ambient` captured `COORD_HTTP_URL` and restores it.
        fn connect(&self) {
            std::env::set_var("COORD_HTTP_URL", &self.base);
        }
    }

    /// Env keys these tests set that `isolated_ambient` does not restore.
    /// `XDG_CONFIG_HOME` matters most: an inherited value would put a roster
    /// write in the operator's real `claude-accounts.json`.
    fn extra_env() -> EnvVarRestore {
        EnvVarRestore::capture(&["XDG_CONFIG_HOME", "QONTINUI_ACCOUNT_USAGE_REPORT_DISABLED"])
    }

    /// Write the machine-global roster inside the fixture: `dirs` in `mode`,
    /// with `config_dir` as the manual pin. The path is checked before the
    /// write.
    fn write_roster(
        amb: &IsolatedAmbient,
        dirs: Vec<String>,
        mode: AccountSelectionMode,
        config_dir: Option<String>,
    ) {
        std::env::set_var("XDG_CONFIG_HOME", amb.dir().join("xdg-config"));
        let path = crate::claude_accounts::claude_accounts_file_path().expect("roster path");
        assert!(
            path.starts_with(amb.dir()),
            "the roster escaped the fixture: {}",
            path.display()
        );
        crate::claude_accounts::update(move |r| {
            r.claude_config_dirs = dirs;
            r.account_selection_mode = mode;
            r.account_selection_pinned = true;
            r.config_dir = config_dir;
        })
        .expect("write roster");
    }

    /// A config dir with no credentials: the probe answers from the filesystem
    /// with an error row, so it never reaches a provider.
    fn logged_out_dir(amb: &IsolatedAmbient, name: &str) -> String {
        let dir = amb.dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    /// A config dir with unexpired credentials, so the spawn resolver accepts
    /// it as `CLAUDE_CONFIG_DIR`. Its probe result is seeded into the probe
    /// cache (fresh for the cache's TTL), so the report reads `reading`
    /// instead of calling the provider.
    async fn logged_in_dir(amb: &IsolatedAmbient, name: &str, status: Option<&str>) -> String {
        let dir = logged_out_dir(amb, name);
        let expires_ms = chrono::Utc::now().timestamp_millis() + 86_400_000;
        std::fs::write(
            std::path::Path::new(&dir).join(".credentials.json"),
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"sk-ant-test","refreshToken":"rt","expiresAt":{expires_ms}}}}}"#
            ),
        )
        .unwrap();
        let reading = AccountUsageInfo {
            config_dir: dir.clone(),
            label: name.to_string(),
            utilization: 0.25,
            status: status.map(str::to_string),
            source: Some("oauth_usage".into()),
            ..Default::default()
        };
        USAGE_PROBE_CACHE
            .get_or_fetch(&dir, move || async move { reading })
            .await;
        dir
    }

    fn plan(probe_configured: bool, probe_spent: bool, record_snapshot: bool) -> UsageRefreshPlan {
        UsageRefreshPlan {
            probe_configured,
            probe_spent,
            record_snapshot,
        }
    }

    /// The picker path (startup, account migration) is unchanged by the fleet
    /// report: it probes and records only from two configured accounts up, so
    /// a single-account runner pays no probe in front of its first pick.
    #[test]
    fn the_selection_path_needs_two_accounts() {
        let c = UsageRefreshCaller::SelectionSnapshot;
        for lone in [true, false] {
            assert_eq!(plan_usage_refresh(0, c, lone), plan(false, false, false));
            assert_eq!(plan_usage_refresh(1, c, lone), plan(false, false, false));
            assert_eq!(plan_usage_refresh(2, c, lone), plan(true, false, true));
        }
    }

    /// The fleet report covers a runner with fewer than two accounts by
    /// probing the account it spends, only when it reports lone accounts; from
    /// two up it is the roster, exactly as the picker sees it.
    #[test]
    fn the_fleet_report_covers_single_and_unlisted_accounts() {
        let c = UsageRefreshCaller::FleetReport;
        assert_eq!(plan_usage_refresh(0, c, true), plan(false, true, false));
        assert_eq!(plan_usage_refresh(1, c, true), plan(false, true, false));
        assert_eq!(plan_usage_refresh(3, c, true), plan(true, false, true));
        assert_eq!(plan_usage_refresh(0, c, false), plan(false, false, false));
        assert_eq!(plan_usage_refresh(1, c, false), plan(false, false, false));
        assert_eq!(plan_usage_refresh(3, c, false), plan(true, false, true));
    }

    /// The population this phase exists for, entered where the 10-minute timer
    /// enters: a one-account runner's account reaches coord, marked in use, and
    /// the provider's status rides along. The picker path over the same roster
    /// stays silent, because that guard is the picker's.
    #[tokio::test]
    async fn the_fleet_report_sends_a_single_configured_account_to_coord() {
        let amb = crate::test_env::isolated_ambient();
        let _env = extra_env();
        let coord = FakeCoord::start().await;
        coord.connect();
        let solo = logged_in_dir(&amb, ".claude-solo", Some("rejected")).await;
        write_roster(
            &amb,
            vec![solo.clone()],
            AccountSelectionMode::Manual,
            Some(solo),
        );

        let picker = refresh_account_usage_snapshot().await;
        assert!(picker.is_empty(), "the picker path probed: {picker:?}");
        assert!(coord.bodies().is_empty(), "the picker path reported");

        let results = refresh_fleet_usage_report().await;
        assert_eq!(results.len(), 1, "{results:?}");
        let rows = coord.rows();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["label"], serde_json::json!(".claude-solo"));
        assert_eq!(rows[0]["is_active"], serde_json::json!(true), "{}", rows[0]);
        assert_eq!(
            rows[0]["status"],
            serde_json::json!("rejected"),
            "{}",
            rows[0]
        );
    }

    /// A one-entry roster is not the account in use when `Manual` mode pins a
    /// different `config_dir`: spawns run on the pin, so the pin is reported.
    #[tokio::test]
    async fn the_fleet_report_sends_the_pinned_account_not_the_lone_roster_entry() {
        let amb = crate::test_env::isolated_ambient();
        let _env = extra_env();
        let coord = FakeCoord::start().await;
        coord.connect();
        let listed = logged_in_dir(&amb, ".claude-listed", None).await;
        let pinned = logged_in_dir(&amb, ".claude-pinned", None).await;
        write_roster(
            &amb,
            vec![listed],
            AccountSelectionMode::Manual,
            Some(pinned),
        );

        refresh_fleet_usage_report().await;

        assert_eq!(coord.reported_labels(), vec![".claude-pinned".to_string()]);
    }

    /// No list and no pin: a spawn inherits the runner's own environment, so
    /// an inherited `CLAUDE_CONFIG_DIR` is the account in use, ahead of
    /// `~/.claude`. Its label is the dir's real name even when the path ends
    /// in `..`.
    #[tokio::test]
    async fn the_fleet_report_sends_an_inherited_claude_config_dir_to_coord() {
        let amb = crate::test_env::isolated_ambient();
        let _env = extra_env();
        let coord = FakeCoord::start().await;
        coord.connect();
        write_roster(&amb, Vec::new(), AccountSelectionMode::Manual, None);
        logged_out_dir(&amb, ".claude");
        let inherited = logged_out_dir(&amb, ".claude-env");
        std::fs::create_dir_all(std::path::Path::new(&inherited).join("sub")).unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", format!("{inherited}/sub/.."));

        refresh_fleet_usage_report().await;

        assert_eq!(coord.reported_labels(), vec![".claude-env".to_string()]);
        let row = &coord.rows()[0];
        assert_eq!(row["error"], serde_json::json!(true), "logged out: {row}");
        assert_eq!(row["is_active"], serde_json::json!(true), "{row}");
    }

    /// A secondary (named or temp runner, or a subject under an instance
    /// root) reports no lone account: it resolves the primary's roster and
    /// environment, so its row would be the primary's written twice.
    #[tokio::test]
    async fn a_secondary_runner_reports_no_lone_account() {
        let amb = crate::test_env::isolated_ambient();
        let _env = extra_env();
        let coord = FakeCoord::start().await;
        coord.connect();
        write_roster(&amb, Vec::new(), AccountSelectionMode::Manual, None);
        std::env::set_var("CLAUDE_CONFIG_DIR", logged_out_dir(&amb, ".claude-env"));

        std::env::set_var("QONTINUI_INSTANCE_NAME", "temp-runner");
        let named = refresh_fleet_usage_report().await;
        std::env::remove_var("QONTINUI_INSTANCE_NAME");
        assert!(named.is_empty(), "a named secondary reported: {named:?}");

        std::env::set_var("QONTINUI_INSTANCE_ROOT", amb.root());
        let subject = refresh_fleet_usage_report().await;
        std::env::remove_var("QONTINUI_INSTANCE_ROOT");
        assert!(subject.is_empty(), "a subject reported: {subject:?}");
        assert!(coord.reported_labels().is_empty());

        refresh_fleet_usage_report().await;
        assert_eq!(coord.reported_labels(), vec![".claude-env".to_string()]);
    }

    /// No list, no pin, nothing inherited: the default `~/.claude`. Unix only:
    /// `HOME` steers `dirs::home_dir` there, and the Windows lookup does not
    /// promise to follow the fixture.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_fleet_report_sends_the_default_dir_to_coord() {
        let amb = crate::test_env::isolated_ambient();
        let _env = extra_env();
        let coord = FakeCoord::start().await;
        coord.connect();
        write_roster(&amb, Vec::new(), AccountSelectionMode::Manual, None);
        logged_out_dir(&amb, ".claude");

        refresh_fleet_usage_report().await;

        assert_eq!(coord.reported_labels(), vec![".claude".to_string()]);
    }

    /// An isolated runner has nobody to report to, so a lone account is not
    /// probed: below two accounts the probe's only consumer is the report.
    #[tokio::test]
    async fn an_isolated_runner_does_not_probe_a_lone_account() {
        let amb = crate::test_env::isolated_ambient();
        let _env = extra_env();
        write_roster(&amb, Vec::new(), AccountSelectionMode::Manual, None);
        std::env::set_var("CLAUDE_CONFIG_DIR", logged_out_dir(&amb, ".claude-env"));

        let results = refresh_fleet_usage_report().await;

        assert!(results.is_empty(), "probed with no consumer: {results:?}");
    }

    /// The kill switch silences a connected runner's report, and so its
    /// lone-account probe too.
    #[tokio::test]
    async fn the_kill_switch_stops_the_lone_account_report() {
        let amb = crate::test_env::isolated_ambient();
        let _env = extra_env();
        let coord = FakeCoord::start().await;
        coord.connect();
        write_roster(&amb, Vec::new(), AccountSelectionMode::Manual, None);
        std::env::set_var("CLAUDE_CONFIG_DIR", logged_out_dir(&amb, ".claude-env"));
        std::env::set_var("QONTINUI_ACCOUNT_USAGE_REPORT_DISABLED", "1");

        let results = refresh_fleet_usage_report().await;

        assert!(
            results.is_empty(),
            "probed under the kill switch: {results:?}"
        );
        assert!(coord.bodies().is_empty(), "reported under the kill switch");
    }
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
            status: status.map(|s| s.to_string()),
            error: error.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn session_window_exhaustion_confirms() {
        // A hit 5-hour window must read as exhausted even while the weekly
        // number is low — this is the gate that used to be blind to
        // "5-hour limit reached" migrations.
        let mut info = usage(0.20, None, None);
        info.session_utilization = Some(1.0);
        assert!(probe_result_exhausted(&info));
        info.session_utilization = Some(0.5);
        assert!(!probe_result_exhausted(&info));
        info.session_utilization = None;
        assert!(!probe_result_exhausted(&info));
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
