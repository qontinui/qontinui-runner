//! AI Generation Commands for Builder Tabs
//!
//! This module contains Tauri commands for AI-powered content generation
//! across various builder tabs (Context, API Request, Task, Exploration).
//!
//! Also includes direct AI API calls for lightweight generation tasks
//! like element descriptions that don't require the full Python bridge.

use crate::commands::{AppState, CommandResponse};
use crate::config_facade::ai_keychain;
use crate::executor::with_default_bridge;
use crate::str_utils::truncate_str;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;
use tracing::{error, info};

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
        let params = serde_json::json!({
            "user_prompt": user_prompt,
            "ai_provider": ai_provider,
            "ai_settings": provider_settings,
        });

        let bridge_result = with_default_bridge(&app_state, |bridge| {
            bridge.send_command_and_wait(
                "generate_context_with_ai",
                Some(params),
                std::time::Duration::from_secs(120),
            )
        });

        match bridge_result {
            Ok(Ok(response)) => {
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
            Ok(Err(e)) => Ok(CommandResponse {
                success: false,
                message: Some(format!("Command failed: {}", e)),
                data: None,
            }),
            Err(e) => Ok(CommandResponse {
                success: false,
                message: Some(e),
                data: None,
            }),
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
        let params = serde_json::json!({
            "user_prompt": user_prompt,
            "base_url": base_url,
            "ai_provider": ai_provider,
            "ai_settings": provider_settings,
        });

        let bridge_result = with_default_bridge(&app_state, |bridge| {
            bridge.send_command_and_wait(
                "generate_api_request_with_ai",
                Some(params),
                std::time::Duration::from_secs(120),
            )
        });

        match bridge_result {
            Ok(Ok(response)) => {
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
            Ok(Err(e)) => Ok(CommandResponse {
                success: false,
                message: Some(format!("Command failed: {}", e)),
                data: None,
            }),
            Err(e) => Ok(CommandResponse {
                success: false,
                message: Some(e),
                data: None,
            }),
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
        let params = serde_json::json!({
            "user_prompt": user_prompt,
            "mode": mode,
            "ai_provider": ai_provider,
            "ai_settings": provider_settings,
        });

        let bridge_result = with_default_bridge(&app_state, |bridge| {
            bridge.send_command_and_wait(
                "generate_task_prompt_with_ai",
                Some(params),
                std::time::Duration::from_secs(120),
            )
        });

        match bridge_result {
            Ok(Ok(response)) => {
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
            Ok(Err(e)) => Ok(CommandResponse {
                success: false,
                message: Some(format!("Command failed: {}", e)),
                data: None,
            }),
            Err(e) => Ok(CommandResponse {
                success: false,
                message: Some(e),
                data: None,
            }),
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
        let params = serde_json::json!({
            "user_goal": user_goal,
            "available_states": available_states,
            "available_transitions": available_transitions,
            "ai_provider": ai_provider,
            "ai_settings": provider_settings,
        });

        let bridge_result = with_default_bridge(&app_state, |bridge| {
            bridge.send_command_and_wait(
                "suggest_exploration_strategy_with_ai",
                Some(params),
                std::time::Duration::from_secs(120),
            )
        });

        match bridge_result {
            Ok(Ok(response)) => {
                if response.success {
                    Ok(CommandResponse {
                        success: true,
                        message: Some("Exploration strategy suggested successfully".to_string()),
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
            Ok(Err(e)) => Ok(CommandResponse {
                success: false,
                message: Some(format!("Command failed: {}", e)),
                data: None,
            }),
            Err(e) => Ok(CommandResponse {
                success: false,
                message: Some(e),
                data: None,
            }),
        }
    })
    .await
    .map_err(|e| format!("spawn_blocking error: {}", e))?;

    result
}

// ============================================================================
// Test and Agentic Step Generation
// ============================================================================

/// Single page info for multi-page context
#[derive(Debug, Deserialize, Serialize)]
pub struct PageInfo {
    pub url: Option<String>,
    pub title: Option<String>,
    pub elements: Option<Vec<ElementInfo>>,
}

/// Page context from UI Bridge for test generation
/// Supports both single-page context (url, title, elements) and
/// multi-page context (pages array)
#[derive(Debug, Deserialize, Serialize)]
pub struct PageContext {
    /// Single page context (legacy)
    pub url: Option<String>,
    pub title: Option<String>,
    pub elements: Option<Vec<ElementInfo>>,
    /// Multi-page context (for flow capture)
    pub pages: Option<Vec<PageInfo>>,
}

/// Element information from UI Bridge
#[derive(Debug, Deserialize, Serialize)]
pub struct ElementInfo {
    pub id: String,
    #[serde(rename = "tagName")]
    pub tag_name: String,
    #[serde(rename = "type")]
    pub element_type: String,
    pub text: Option<String>,
    pub label: Option<String>,
    pub visible: bool,
    pub enabled: bool,
}

/// Input for generating test and agentic step with AI
#[derive(Debug, Deserialize)]
pub struct GenerateTestAndAgenticInput {
    pub user_prompt: String,
    pub page_context: Option<PageContext>,
    /// Context IDs from the context library to inject into the AI prompt
    pub context_ids: Option<Vec<String>>,
}

/// Generate a verification test and agentic step using AI.
///
/// Creates both a Python verification test and an agentic prompt
/// based on user instructions and optional page context.
#[tauri::command]
pub async fn generate_test_and_agentic_step(
    input: GenerateTestAndAgenticInput,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    use crate::settings;

    info!(
        "Generating test and agentic step with AI: {}",
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
    let page_context = input.page_context;
    let context_ids = input.context_ids;

    let result = tokio::task::spawn_blocking(move || {
        let params = serde_json::json!({
            "user_prompt": user_prompt,
            "page_context": page_context,
            "context_ids": context_ids,
            "ai_provider": ai_provider,
            "ai_settings": provider_settings,
        });

        let bridge_result = with_default_bridge(&app_state, |bridge| {
            bridge.send_command_and_wait(
                "generate_test_and_agentic_step",
                Some(params),
                std::time::Duration::from_secs(180), // Longer timeout for complex generation
            )
        });

        match bridge_result {
            Ok(Ok(response)) => {
                if response.success {
                    Ok(CommandResponse {
                        success: true,
                        message: Some("Test and agentic step generated successfully".to_string()),
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
            Ok(Err(e)) => Ok(CommandResponse {
                success: false,
                message: Some(format!("Command failed: {}", e)),
                data: None,
            }),
            Err(e) => Ok(CommandResponse {
                success: false,
                message: Some(e),
                data: None,
            }),
        }
    })
    .await
    .map_err(|e| format!("spawn_blocking error: {}", e))?;

    result
}

// ============================================================================
// AI-Driven Flow Exploration
// ============================================================================

/// Information about a previously captured page during flow exploration
#[derive(Debug, Deserialize, Serialize)]
pub struct CapturedPageInfo {
    pub url: String,
    pub title: String,
    pub element_count: i32,
}

/// Input for a single AI-driven flow exploration step
#[derive(Debug, Deserialize)]
pub struct ExploreFlowStepInput {
    /// User's natural language description of the navigation flow
    pub user_prompt: String,
    /// Current page elements
    pub current_elements: Vec<ElementInfo>,
    /// Current page URL
    pub current_url: String,
    /// Current page title
    pub current_title: String,
    /// Pages already captured during this exploration
    pub captured_pages: Vec<CapturedPageInfo>,
    /// Current step number in the exploration
    pub step_number: i32,
}

/// Ask AI to decide the next action in a flow exploration.
///
/// The AI analyzes the current page elements and user's goal to determine
/// what action to take next (click, type, wait, or done).
#[tauri::command]
pub async fn explore_flow_step(
    input: ExploreFlowStepInput,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    use crate::settings;

    info!(
        "Explore flow step {}: {}",
        input.step_number,
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

    let result = tokio::task::spawn_blocking(move || {
        let params = serde_json::json!({
            "user_prompt": input.user_prompt,
            "current_elements": input.current_elements,
            "current_url": input.current_url,
            "current_title": input.current_title,
            "captured_pages": input.captured_pages,
            "step_number": input.step_number,
            "ai_provider": ai_provider,
            "ai_settings": provider_settings,
        });

        let bridge_result = with_default_bridge(&app_state, |bridge| {
            bridge.send_command_and_wait(
                "explore_flow_step",
                Some(params),
                std::time::Duration::from_secs(60), // Shorter timeout for single decision
            )
        });

        match bridge_result {
            Ok(Ok(response)) => {
                if response.success {
                    Ok(CommandResponse {
                        success: true,
                        message: Some("Flow step decided".to_string()),
                        data: response.data,
                    })
                } else {
                    Ok(CommandResponse {
                        success: false,
                        message: Some(
                            response
                                .error
                                .unwrap_or_else(|| "Flow exploration failed".to_string()),
                        ),
                        data: None,
                    })
                }
            }
            Ok(Err(e)) => Ok(CommandResponse {
                success: false,
                message: Some(format!("Command failed: {}", e)),
                data: None,
            }),
            Err(e) => Ok(CommandResponse {
                success: false,
                message: Some(e),
                data: None,
            }),
        }
    })
    .await
    .map_err(|e| format!("spawn_blocking error: {}", e))?;

    result
}

// ============================================================================
// Helper Functions
// ============================================================================

// ============================================================================
// Element AI Description Generation
// ============================================================================

/// Input for generating AI description for a UI element
#[derive(Debug, Deserialize)]
pub struct GenerateElementAiDescriptionInput {
    /// Element type (button, input, link, etc.)
    pub element_type: String,
    /// Element role (ARIA role)
    pub role: Option<String>,
    /// Element label (accessible name)
    pub label: Option<String>,
    /// Element text content
    pub text: Option<String>,
    /// Element value (for inputs)
    pub value: Option<String>,
    /// Placeholder text
    pub placeholder: Option<String>,
    /// Element states (enabled, disabled, focused, etc.)
    pub states: Vec<String>,
    /// Available actions on the element
    pub actions: Vec<String>,
    /// Parent element info
    pub parent_info: Option<ParentInfo>,
    /// Page context
    pub page_context: Option<ElementPageContext>,
}

/// Parent element information
#[derive(Debug, Deserialize, Serialize)]
pub struct ParentInfo {
    pub element_type: String,
    pub role: Option<String>,
    pub label: Option<String>,
}

/// Page context for element description
#[derive(Debug, Deserialize, Serialize)]
pub struct ElementPageContext {
    pub url: Option<String>,
    pub title: Option<String>,
}

/// AI-generated element description
#[derive(Debug, Serialize)]
pub struct ElementAiDescription {
    /// Natural language description of the element
    pub description: String,
    /// The element's likely purpose in the user flow
    pub purpose: String,
    /// How a user would interact with this element
    pub interaction: String,
    /// Accessibility notes and considerations
    pub accessibility_notes: Option<String>,
}

/// Generate an AI description for a UI element.
///
/// Uses the Claude API directly for fast, lightweight description generation.
/// Falls back to Gemini API if Claude is not configured.
#[tauri::command]
pub async fn generate_element_ai_description(
    input: GenerateElementAiDescriptionInput,
) -> Result<CommandResponse, String> {
    use crate::settings;

    info!(
        "Generating AI element description for: {} ({})",
        input.element_type,
        input.label.as_deref().unwrap_or("unlabeled")
    );

    let ai_settings = settings::get_ai_settings();

    // Build the prompt
    let prompt = build_element_description_prompt(&input);

    // Try Claude API first, then Gemini API
    let result = match ai_settings.provider {
        settings::AiProvider::ClaudeApi | settings::AiProvider::ClaudeCli => {
            call_claude_api_for_description(&ai_settings.claude_api, &prompt).await
        }
        settings::AiProvider::GeminiApi | settings::AiProvider::GeminiCli => {
            call_gemini_api_for_description(&ai_settings.gemini_api, &prompt).await
        }
    };

    match result {
        Ok(description) => Ok(CommandResponse {
            success: true,
            message: Some("AI description generated successfully".to_string()),
            data: Some(serde_json::to_value(&description).map_err(|e| e.to_string())?),
        }),
        Err(e) => {
            error!("Failed to generate AI description: {}", e);
            Ok(CommandResponse {
                success: false,
                message: Some(e),
                data: None,
            })
        }
    }
}

/// Build the prompt for element description
fn build_element_description_prompt(input: &GenerateElementAiDescriptionInput) -> String {
    let mut prompt = String::from(
        r#"Analyze this UI element and provide a structured description. Be concise but informative.

Element:
"#,
    );

    prompt.push_str(&format!("- Type: {}\n", input.element_type));

    if let Some(ref role) = input.role {
        prompt.push_str(&format!("- Role: {}\n", role));
    }

    if let Some(ref label) = input.label {
        prompt.push_str(&format!("- Label: {}\n", label));
    }

    if let Some(ref text) = input.text {
        if text.len() <= 100 {
            prompt.push_str(&format!("- Text: {}\n", text));
        } else {
            prompt.push_str(&format!("- Text: {}...\n", truncate_str(text, 100)));
        }
    }

    if let Some(ref value) = input.value {
        prompt.push_str(&format!("- Value: {}\n", value));
    }

    if let Some(ref placeholder) = input.placeholder {
        prompt.push_str(&format!("- Placeholder: {}\n", placeholder));
    }

    if !input.states.is_empty() {
        prompt.push_str(&format!("- States: {}\n", input.states.join(", ")));
    }

    if !input.actions.is_empty() {
        prompt.push_str(&format!("- Actions: {}\n", input.actions.join(", ")));
    }

    if let Some(ref page_context) = input.page_context {
        prompt.push_str("\nPage Context:\n");
        if let Some(ref title) = page_context.title {
            prompt.push_str(&format!("- Title: {}\n", title));
        }
        if let Some(ref url) = page_context.url {
            prompt.push_str(&format!("- URL: {}\n", url));
        }
    }

    if let Some(ref parent) = input.parent_info {
        prompt.push_str(&format!(
            "\nParent Element: {} ({})\n",
            parent.element_type,
            parent.label.as_deref().unwrap_or("unlabeled")
        ));
    }

    prompt.push_str(
        r#"
Respond with a JSON object containing:
{
  "description": "A natural language description of what this element is (1-2 sentences)",
  "purpose": "Its likely purpose in the user flow (1 sentence)",
  "interaction": "How a user would interact with it (1 sentence)",
  "accessibility_notes": "Any accessibility considerations (null if none, otherwise 1 sentence)"
}

Respond ONLY with the JSON object, no markdown or extra text."#,
    );

    prompt
}

/// Call Claude API for element description
async fn call_claude_api_for_description(
    settings: &crate::settings::ClaudeApiSettings,
    prompt: &str,
) -> Result<ElementAiDescription, String> {
    // Get API key from keychain
    let api_key = ai_keychain()
        .get("claude_api")
        .map_err(|e| format!("Failed to retrieve API key: {}", e))?
        .ok_or_else(|| {
            "No Claude API key configured. Please enter your API key in Settings.".to_string()
        })?;

    let client = reqwest::Client::new();
    let response = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "model": settings.model,
            "max_tokens": 500,
            "messages": [{"role": "user", "content": prompt}]
        }))
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let error_body = response.text().await.unwrap_or_default();

        if status.as_u16() == 401 {
            return Err("Invalid API key. Please check your Claude API key.".to_string());
        }
        return Err(format!("Claude API error ({}): {}", status, error_body));
    }

    let response_json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    // Extract the content text from Claude's response
    let content = response_json["content"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|obj| obj["text"].as_str())
        .ok_or_else(|| "Invalid response format from Claude API".to_string())?;

    parse_ai_description_response(content)
}

/// Call Gemini API for element description
async fn call_gemini_api_for_description(
    settings: &crate::settings::GeminiApiSettings,
    prompt: &str,
) -> Result<ElementAiDescription, String> {
    // Get API key from keychain
    let api_key = ai_keychain()
        .get("gemini_api")
        .map_err(|e| format!("Failed to retrieve API key: {}", e))?
        .ok_or_else(|| {
            "No Gemini API key configured. Please enter your API key in Settings.".to_string()
        })?;

    let client = reqwest::Client::new();
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        settings.model, api_key
    );

    let response = client
        .post(&url)
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "contents": [{"parts": [{"text": prompt}]}],
            "generationConfig": {
                "maxOutputTokens": 500,
                "temperature": 0.3
            }
        }))
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let error_body = response.text().await.unwrap_or_default();

        if error_body.contains("API_KEY_INVALID") {
            return Err("Invalid API key. Please check your Gemini API key.".to_string());
        }
        return Err(format!("Gemini API error ({}): {}", status, error_body));
    }

    let response_json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    // Extract the content text from Gemini's response
    let content = response_json["candidates"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|obj| obj["content"]["parts"].as_array())
        .and_then(|parts| parts.first())
        .and_then(|part| part["text"].as_str())
        .ok_or_else(|| "Invalid response format from Gemini API".to_string())?;

    parse_ai_description_response(content)
}

/// Parse the AI response into ElementAiDescription
fn parse_ai_description_response(content: &str) -> Result<ElementAiDescription, String> {
    // Try to parse as JSON directly
    let content_trimmed = content.trim();

    // Remove markdown code block if present
    let json_str = if content_trimmed.starts_with("```json") {
        content_trimmed
            .strip_prefix("```json")
            .and_then(|s| s.strip_suffix("```"))
            .unwrap_or(content_trimmed)
            .trim()
    } else if content_trimmed.starts_with("```") {
        content_trimmed
            .strip_prefix("```")
            .and_then(|s| s.strip_suffix("```"))
            .unwrap_or(content_trimmed)
            .trim()
    } else {
        content_trimmed
    };

    let parsed: serde_json::Value = serde_json::from_str(json_str).map_err(|e| {
        format!(
            "Failed to parse AI response as JSON: {}. Response: {}",
            e, content
        )
    })?;

    Ok(ElementAiDescription {
        description: parsed["description"]
            .as_str()
            .unwrap_or("No description available")
            .to_string(),
        purpose: parsed["purpose"]
            .as_str()
            .unwrap_or("Unknown purpose")
            .to_string(),
        interaction: parsed["interaction"]
            .as_str()
            .unwrap_or("Click to interact")
            .to_string(),
        accessibility_notes: parsed["accessibility_notes"]
            .as_str()
            .map(|s| s.to_string()),
    })
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Build provider-specific settings based on AI configuration
pub fn build_provider_settings(ai_settings: &crate::settings::AiSettings) -> serde_json::Value {
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
