//! AI Generation Commands for Builder Tabs
//!
//! This module contains Tauri commands for AI-powered content generation
//! across various builder tabs (Context, API Request, Task, Exploration).

use crate::commands::{AppState, CommandResponse};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;
use tracing::info;

// ============================================================================
// Context Generation
// ============================================================================

/// Input for generating a context with AI
#[derive(Debug, Deserialize)]
pub struct GenerateContextInput {
    pub user_prompt: String,
}

/// Generate a knowledge base context using AI.
///
/// Creates well-structured documentation for AI task automation based on a topic description.
#[tauri::command]
pub async fn generate_context_with_ai(
    input: GenerateContextInput,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    use crate::settings;

    info!(
        "Generating context with AI: {}",
        input.user_prompt.chars().take(50).collect::<String>()
    );

    // Get AI settings
    let ai_settings = settings::get_ai_settings();

    // Map provider to Python format
    let ai_provider = match ai_settings.provider {
        settings::AiProvider::ClaudeCli => "claude_cli",
        settings::AiProvider::ClaudeApi => "claude_api",
        settings::AiProvider::GeminiCli => "gemini_cli",
        settings::AiProvider::GeminiApi => "gemini_api",
    };

    // Build provider-specific settings
    let provider_settings = build_provider_settings(&ai_settings);

    // Clone state for use in spawn_blocking
    let app_state = state.inner().clone();
    let user_prompt = input.user_prompt;

    // Execute via spawn_blocking since PythonBridge uses block_on internally
    let result = tokio::task::spawn_blocking(move || {
        let mut guard = match app_state.python_bridge.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                tracing::warn!("Python bridge mutex was poisoned, recovering...");
                poisoned.into_inner()
            }
        };

        if let Some(ref mut bridge) = *guard {
            let params = serde_json::json!({
                "user_prompt": user_prompt,
                "ai_provider": ai_provider,
                "ai_settings": provider_settings,
            });

            match bridge.send_command_and_wait(
                "generate_context_with_ai",
                Some(params),
                std::time::Duration::from_secs(120),
            ) {
                Ok(response) => {
                    if response.success {
                        Ok(CommandResponse {
                            success: true,
                            message: Some("Context generated successfully".to_string()),
                            data: response.data,
                        })
                    } else {
                        Ok(CommandResponse {
                            success: false,
                            message: Some(
                                response
                                    .error
                                    .unwrap_or_else(|| "Generation failed".to_string()),
                            ),
                            data: None,
                        })
                    }
                }
                Err(e) => Ok(CommandResponse {
                    success: false,
                    message: Some(format!("Command failed: {}", e)),
                    data: None,
                }),
            }
        } else {
            Ok(CommandResponse {
                success: false,
                message: Some("Python executor not initialized".to_string()),
                data: None,
            })
        }
    })
    .await
    .map_err(|e| format!("spawn_blocking error: {}", e))?;

    result
}

// ============================================================================
// API Request Generation
// ============================================================================

/// Input for generating an API request with AI
#[derive(Debug, Deserialize)]
pub struct GenerateApiRequestInput {
    pub user_prompt: String,
    pub base_url: Option<String>,
}

/// Generate an API request template using AI.
///
/// Creates complete HTTP request templates with method, URL, headers, and body.
#[tauri::command]
pub async fn generate_api_request_with_ai(
    input: GenerateApiRequestInput,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    use crate::settings;

    info!(
        "Generating API request with AI: {}",
        input.user_prompt.chars().take(50).collect::<String>()
    );

    let ai_settings = settings::get_ai_settings();
    let ai_provider = match ai_settings.provider {
        settings::AiProvider::ClaudeCli => "claude_cli",
        settings::AiProvider::ClaudeApi => "claude_api",
        settings::AiProvider::GeminiCli => "gemini_cli",
        settings::AiProvider::GeminiApi => "gemini_api",
    };

    let provider_settings = build_provider_settings(&ai_settings);
    let app_state = state.inner().clone();
    let user_prompt = input.user_prompt;
    let base_url = input.base_url;

    let result = tokio::task::spawn_blocking(move || {
        let mut guard = match app_state.python_bridge.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                tracing::warn!("Python bridge mutex was poisoned, recovering...");
                poisoned.into_inner()
            }
        };

        if let Some(ref mut bridge) = *guard {
            let params = serde_json::json!({
                "user_prompt": user_prompt,
                "base_url": base_url,
                "ai_provider": ai_provider,
                "ai_settings": provider_settings,
            });

            match bridge.send_command_and_wait(
                "generate_api_request_with_ai",
                Some(params),
                std::time::Duration::from_secs(120),
            ) {
                Ok(response) => {
                    if response.success {
                        Ok(CommandResponse {
                            success: true,
                            message: Some("API request generated successfully".to_string()),
                            data: response.data,
                        })
                    } else {
                        Ok(CommandResponse {
                            success: false,
                            message: Some(
                                response
                                    .error
                                    .unwrap_or_else(|| "Generation failed".to_string()),
                            ),
                            data: None,
                        })
                    }
                }
                Err(e) => Ok(CommandResponse {
                    success: false,
                    message: Some(format!("Command failed: {}", e)),
                    data: None,
                }),
            }
        } else {
            Ok(CommandResponse {
                success: false,
                message: Some("Python executor not initialized".to_string()),
                data: None,
            })
        }
    })
    .await
    .map_err(|e| format!("spawn_blocking error: {}", e))?;

    result
}

// ============================================================================
// Task Prompt Generation
// ============================================================================

/// Input for generating a task prompt with AI
#[derive(Debug, Deserialize)]
pub struct GenerateTaskPromptInput {
    pub user_prompt: String,
    pub mode: String, // "generate" or "improve"
}

/// Generate or improve an AI task prompt.
///
/// Creates well-structured prompts with clear instructions and expected outputs.
#[tauri::command]
pub async fn generate_task_prompt_with_ai(
    input: GenerateTaskPromptInput,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    use crate::settings;

    info!(
        "Generating task prompt with AI (mode={}): {}",
        input.mode,
        input.user_prompt.chars().take(50).collect::<String>()
    );

    let ai_settings = settings::get_ai_settings();
    let ai_provider = match ai_settings.provider {
        settings::AiProvider::ClaudeCli => "claude_cli",
        settings::AiProvider::ClaudeApi => "claude_api",
        settings::AiProvider::GeminiCli => "gemini_cli",
        settings::AiProvider::GeminiApi => "gemini_api",
    };

    let provider_settings = build_provider_settings(&ai_settings);
    let app_state = state.inner().clone();
    let user_prompt = input.user_prompt;
    let mode = input.mode;

    let result = tokio::task::spawn_blocking(move || {
        let mut guard = match app_state.python_bridge.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                tracing::warn!("Python bridge mutex was poisoned, recovering...");
                poisoned.into_inner()
            }
        };

        if let Some(ref mut bridge) = *guard {
            let params = serde_json::json!({
                "user_prompt": user_prompt,
                "mode": mode,
                "ai_provider": ai_provider,
                "ai_settings": provider_settings,
            });

            match bridge.send_command_and_wait(
                "generate_task_prompt_with_ai",
                Some(params),
                std::time::Duration::from_secs(120),
            ) {
                Ok(response) => {
                    if response.success {
                        Ok(CommandResponse {
                            success: true,
                            message: Some("Task prompt generated successfully".to_string()),
                            data: response.data,
                        })
                    } else {
                        Ok(CommandResponse {
                            success: false,
                            message: Some(
                                response
                                    .error
                                    .unwrap_or_else(|| "Generation failed".to_string()),
                            ),
                            data: None,
                        })
                    }
                }
                Err(e) => Ok(CommandResponse {
                    success: false,
                    message: Some(format!("Command failed: {}", e)),
                    data: None,
                }),
            }
        } else {
            Ok(CommandResponse {
                success: false,
                message: Some("Python executor not initialized".to_string()),
                data: None,
            })
        }
    })
    .await
    .map_err(|e| format!("spawn_blocking error: {}", e))?;

    result
}

// ============================================================================
// Exploration Strategy Suggestion
// ============================================================================

/// State information for exploration suggestion
#[derive(Debug, Deserialize, Serialize)]
pub struct StateInfo {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub is_initial: bool,
    pub is_final: bool,
}

/// Transition information for exploration suggestion
#[derive(Debug, Deserialize, Serialize)]
pub struct TransitionInfo {
    pub id: String,
    pub name: String,
    pub from_state: Option<String>,
    pub to_state: Option<String>,
}

/// Input for suggesting exploration strategy with AI
#[derive(Debug, Deserialize)]
pub struct SuggestExplorationInput {
    pub user_goal: String,
    pub available_states: Vec<StateInfo>,
    pub available_transitions: Vec<TransitionInfo>,
}

/// Suggest an exploration strategy using AI.
///
/// Recommends optimal exploration settings based on user goals and available states/transitions.
#[tauri::command]
pub async fn suggest_exploration_strategy_with_ai(
    input: SuggestExplorationInput,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    use crate::settings;

    info!(
        "Suggesting exploration strategy with AI: {}",
        input.user_goal.chars().take(50).collect::<String>()
    );

    let ai_settings = settings::get_ai_settings();
    let ai_provider = match ai_settings.provider {
        settings::AiProvider::ClaudeCli => "claude_cli",
        settings::AiProvider::ClaudeApi => "claude_api",
        settings::AiProvider::GeminiCli => "gemini_cli",
        settings::AiProvider::GeminiApi => "gemini_api",
    };

    let provider_settings = build_provider_settings(&ai_settings);
    let app_state = state.inner().clone();
    let user_goal = input.user_goal;
    let available_states = input.available_states;
    let available_transitions = input.available_transitions;

    let result = tokio::task::spawn_blocking(move || {
        let mut guard = match app_state.python_bridge.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                tracing::warn!("Python bridge mutex was poisoned, recovering...");
                poisoned.into_inner()
            }
        };

        if let Some(ref mut bridge) = *guard {
            let params = serde_json::json!({
                "user_goal": user_goal,
                "available_states": available_states,
                "available_transitions": available_transitions,
                "ai_provider": ai_provider,
                "ai_settings": provider_settings,
            });

            match bridge.send_command_and_wait(
                "suggest_exploration_strategy_with_ai",
                Some(params),
                std::time::Duration::from_secs(120),
            ) {
                Ok(response) => {
                    if response.success {
                        Ok(CommandResponse {
                            success: true,
                            message: Some(
                                "Exploration strategy suggested successfully".to_string(),
                            ),
                            data: response.data,
                        })
                    } else {
                        Ok(CommandResponse {
                            success: false,
                            message: Some(
                                response
                                    .error
                                    .unwrap_or_else(|| "Suggestion failed".to_string()),
                            ),
                            data: None,
                        })
                    }
                }
                Err(e) => Ok(CommandResponse {
                    success: false,
                    message: Some(format!("Command failed: {}", e)),
                    data: None,
                }),
            }
        } else {
            Ok(CommandResponse {
                success: false,
                message: Some("Python executor not initialized".to_string()),
                data: None,
            })
        }
    })
    .await
    .map_err(|e| format!("spawn_blocking error: {}", e))?;

    result
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Build provider-specific settings based on AI configuration
fn build_provider_settings(ai_settings: &crate::settings::AiSettings) -> serde_json::Value {
    use crate::settings;

    match ai_settings.provider {
        settings::AiProvider::ClaudeCli => {
            let execution_mode = match ai_settings.claude_cli.execution_mode {
                settings::CliExecutionMode::Auto => "auto",
                settings::CliExecutionMode::WindowsNative => "native",
                settings::CliExecutionMode::Wsl => "wsl",
                settings::CliExecutionMode::Native => "native",
            };
            serde_json::json!({
                "execution_mode": execution_mode,
                "custom_path": ai_settings.claude_cli.custom_path,
                "timeout_seconds": ai_settings.claude_cli.timeout_seconds,
                "config_dir": ai_settings.claude_cli.config_dir,
            })
        }
        settings::AiProvider::ClaudeApi => {
            serde_json::json!({
                "model": ai_settings.claude_api.model,
                "max_tokens": ai_settings.claude_api.max_tokens,
            })
        }
        settings::AiProvider::GeminiCli => {
            let execution_mode = match ai_settings.gemini_cli.execution_mode {
                settings::CliExecutionMode::Auto => "auto",
                settings::CliExecutionMode::WindowsNative => "native",
                settings::CliExecutionMode::Wsl => "wsl",
                settings::CliExecutionMode::Native => "native",
            };
            let auth_method = match ai_settings.gemini_cli.auth_method {
                settings::GeminiAuthMethod::OAuth => "oauth",
                settings::GeminiAuthMethod::ApiKey => "api_key",
            };
            serde_json::json!({
                "execution_mode": execution_mode,
                "custom_path": ai_settings.gemini_cli.custom_path,
                "timeout_seconds": ai_settings.gemini_cli.timeout_seconds,
                "auth_method": auth_method,
                "model": ai_settings.gemini_cli.model,
            })
        }
        settings::AiProvider::GeminiApi => {
            serde_json::json!({
                "model": ai_settings.gemini_api.model,
                "max_output_tokens": ai_settings.gemini_api.max_output_tokens,
                "temperature": ai_settings.gemini_api.temperature,
            })
        }
    }
}
