//! AI Generation HTTP API handlers
//!
//! Wraps the existing Tauri IPC AI generation commands as HTTP endpoints
//! for use by the web frontend. Each handler follows the same pattern:
//! get AI settings, build provider settings, call the Python bridge via
//! spawn_blocking, and return an ApiResponse.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info};

use crate::commands::ai_generation::build_provider_settings;
use crate::executor::with_default_bridge;
use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Request Types
// ============================================================================

/// Request body for POST /ai/generate-test
#[derive(Debug, Deserialize)]
pub struct GenerateTestRequest {
    pub user_prompt: String,
    pub test_type: String,
    #[serde(default)]
    pub page_analysis: Option<serde_json::Value>,
    #[serde(default)]
    pub multi_request_analysis: Option<serde_json::Value>,
    #[serde(default)]
    pub collected_analyses: Option<serde_json::Value>,
    #[serde(default)]
    pub reference_documents: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub workflow_run_context: Option<serde_json::Value>,
}

/// Request body for POST /ai/generate-shell-command
#[derive(Debug, Deserialize)]
pub struct GenerateShellCommandRequest {
    pub user_prompt: String,
    pub target_os: String,
    #[serde(default)]
    pub category: Option<String>,
}

/// Request body for POST /ai/generate-api-request
#[derive(Debug, Deserialize)]
pub struct GenerateApiRequestRequest {
    pub user_prompt: String,
    #[serde(default)]
    pub base_url: Option<String>,
}

/// Request body for POST /ai/generate-context
#[derive(Debug, Deserialize)]
pub struct GenerateContextRequest {
    pub user_prompt: String,
}

/// Request body for POST /ai/generate-prompt
#[derive(Debug, Deserialize)]
pub struct GeneratePromptRequest {
    pub user_prompt: String,
    /// Mode: "generate" or "improve"
    pub mode: String,
}

/// Request body for POST /ai/generate-macro
#[derive(Debug, Deserialize)]
pub struct GenerateMacroRequest {
    pub user_prompt: String,
    #[serde(default)]
    pub category: Option<String>,
}

/// Request body for POST /ai/generate-prompt-snippet
#[derive(Debug, Deserialize)]
pub struct GeneratePromptSnippetRequest {
    pub user_prompt: String,
    #[serde(default)]
    pub language: Option<String>,
}

/// Request body for POST /ai/suggest-check-groups
#[derive(Debug, Deserialize)]
pub struct SuggestCheckGroupsRequest {
    pub user_prompt: String,
    #[serde(default)]
    pub existing_checks: Vec<serde_json::Value>,
}

/// Request body for POST /ai/suggest-exploration-strategy
#[derive(Debug, Deserialize)]
pub struct SuggestExplorationStrategyRequest {
    pub user_goal: String,
    #[serde(default)]
    pub config_path: Option<String>,
}

// ============================================================================
// Helper: Map AI provider to string
// ============================================================================

fn ai_provider_str(provider: &crate::settings::AiProvider) -> &'static str {
    use crate::settings::AiProvider;
    match provider {
        AiProvider::ClaudeCli => "claude_cli",
        AiProvider::ClaudeApi => "claude_api",
        AiProvider::GeminiCli => "gemini_cli",
        AiProvider::GeminiApi => "gemini_api",
        AiProvider::Ollama => "ollama",
        AiProvider::OpenAiCompatible => "openai_compatible",
    }
}

// ============================================================================
// Handlers
// ============================================================================

/// POST /ai/generate-test
///
/// Generate a verification test using AI via the Python bridge.
pub async fn generate_test_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<GenerateTestRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    use crate::settings;

    info!(
        "HTTP: Generating {} test with AI: {}",
        request.test_type,
        request.user_prompt.chars().take(50).collect::<String>()
    );

    let ai_settings = settings::get_ai_settings();
    let ai_provider = ai_provider_str(&ai_settings.provider);
    let provider_settings = build_provider_settings(&ai_settings);
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        let params = serde_json::json!({
            "user_prompt": request.user_prompt,
            "test_type": request.test_type,
            "page_analysis": request.page_analysis,
            "multi_request_analysis": request.multi_request_analysis,
            "collected_analyses": request.collected_analyses,
            "reference_documents": request.reference_documents,
            "workflow_run_context": request.workflow_run_context,
            "ai_provider": ai_provider,
            "ai_settings": provider_settings,
        });

        with_default_bridge(&app_state, |bridge| {
            bridge.send_command_and_wait(
                "generate_test_with_ai",
                Some(params),
                std::time::Duration::from_secs(180),
            )
        })
    })
    .await
    .map_err(|e| {
        error!("HTTP: spawn_blocking error for generate-test: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    handle_bridge_result(result, "Test generated successfully")
}

/// POST /ai/generate-shell-command
///
/// Generate a shell command using AI via the Python bridge.
pub async fn generate_shell_command_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<GenerateShellCommandRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    use crate::settings;

    info!(
        "HTTP: Generating shell command with AI: {}",
        request.user_prompt.chars().take(50).collect::<String>()
    );

    let ai_settings = settings::get_ai_settings();
    let ai_provider = ai_provider_str(&ai_settings.provider);
    let provider_settings = build_provider_settings(&ai_settings);
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        let params = serde_json::json!({
            "user_prompt": request.user_prompt,
            "target_os": request.target_os,
            "category": request.category.unwrap_or_else(|| "general".to_string()),
            "ai_provider": ai_provider,
            "ai_settings": provider_settings,
        });

        with_default_bridge(&app_state, |bridge| {
            bridge.send_command_and_wait(
                "generate_shell_command_with_ai",
                Some(params),
                std::time::Duration::from_secs(120),
            )
        })
    })
    .await
    .map_err(|e| {
        error!(
            "HTTP: spawn_blocking error for generate-shell-command: {}",
            e
        );
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    handle_bridge_result(result, "Shell command generated successfully")
}

/// POST /ai/generate-api-request
///
/// Generate an API request template using AI via the Python bridge.
pub async fn generate_api_request_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<GenerateApiRequestRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    use crate::settings;

    info!(
        "HTTP: Generating API request with AI: {}",
        request.user_prompt.chars().take(50).collect::<String>()
    );

    let ai_settings = settings::get_ai_settings();
    let ai_provider = ai_provider_str(&ai_settings.provider);
    let provider_settings = build_provider_settings(&ai_settings);
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        let params = serde_json::json!({
            "user_prompt": request.user_prompt,
            "base_url": request.base_url,
            "ai_provider": ai_provider,
            "ai_settings": provider_settings,
        });

        with_default_bridge(&app_state, |bridge| {
            bridge.send_command_and_wait(
                "generate_api_request_with_ai",
                Some(params),
                std::time::Duration::from_secs(120),
            )
        })
    })
    .await
    .map_err(|e| {
        error!("HTTP: spawn_blocking error for generate-api-request: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    handle_bridge_result(result, "API request generated successfully")
}

/// POST /ai/generate-context
///
/// Generate a knowledge base context using AI via the Python bridge.
pub async fn generate_context_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<GenerateContextRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    use crate::settings;

    info!(
        "HTTP: Generating context with AI: {}",
        request.user_prompt.chars().take(50).collect::<String>()
    );

    let ai_settings = settings::get_ai_settings();
    let ai_provider = ai_provider_str(&ai_settings.provider);
    let provider_settings = build_provider_settings(&ai_settings);
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        let params = serde_json::json!({
            "user_prompt": request.user_prompt,
            "ai_provider": ai_provider,
            "ai_settings": provider_settings,
        });

        with_default_bridge(&app_state, |bridge| {
            bridge.send_command_and_wait(
                "generate_context_with_ai",
                Some(params),
                std::time::Duration::from_secs(120),
            )
        })
    })
    .await
    .map_err(|e| {
        error!("HTTP: spawn_blocking error for generate-context: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    handle_bridge_result(result, "Context generated successfully")
}

/// POST /ai/generate-prompt
///
/// Generate or improve an AI task prompt via the Python bridge.
pub async fn generate_prompt_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<GeneratePromptRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    use crate::settings;

    info!(
        "HTTP: Generating task prompt with AI (mode={}): {}",
        request.mode,
        request.user_prompt.chars().take(50).collect::<String>()
    );

    let ai_settings = settings::get_ai_settings();
    let ai_provider = ai_provider_str(&ai_settings.provider);
    let provider_settings = build_provider_settings(&ai_settings);
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        let params = serde_json::json!({
            "user_prompt": request.user_prompt,
            "mode": request.mode,
            "ai_provider": ai_provider,
            "ai_settings": provider_settings,
        });

        with_default_bridge(&app_state, |bridge| {
            bridge.send_command_and_wait(
                "generate_task_prompt_with_ai",
                Some(params),
                std::time::Duration::from_secs(120),
            )
        })
    })
    .await
    .map_err(|e| {
        error!("HTTP: spawn_blocking error for generate-prompt: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    handle_bridge_result(result, "Task prompt generated successfully")
}

/// POST /ai/generate-macro
///
/// Generate a macro (action sequence) using AI via the Python bridge.
pub async fn generate_macro_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<GenerateMacroRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    use crate::settings;

    info!(
        "HTTP: Generating macro with AI: {}",
        request.user_prompt.chars().take(50).collect::<String>()
    );

    let ai_settings = settings::get_ai_settings();
    let ai_provider = ai_provider_str(&ai_settings.provider);
    let provider_settings = build_provider_settings(&ai_settings);
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        let params = serde_json::json!({
            "user_prompt": request.user_prompt,
            "category": request.category.unwrap_or_else(|| "general".to_string()),
            "ai_provider": ai_provider,
            "ai_settings": provider_settings,
        });

        with_default_bridge(&app_state, |bridge| {
            bridge.send_command_and_wait(
                "generate_macro_with_ai",
                Some(params),
                std::time::Duration::from_secs(120),
            )
        })
    })
    .await
    .map_err(|e| {
        error!("HTTP: spawn_blocking error for generate-macro: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    handle_bridge_result(result, "Macro generated successfully")
}

/// POST /ai/generate-prompt-snippet
///
/// Generate a prompt snippet using AI via the Python bridge.
pub async fn generate_prompt_snippet_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<GeneratePromptSnippetRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    use crate::settings;

    info!(
        "HTTP: Generating prompt snippet with AI: {}",
        request.user_prompt.chars().take(50).collect::<String>()
    );

    let ai_settings = settings::get_ai_settings();
    let ai_provider = ai_provider_str(&ai_settings.provider);
    let provider_settings = build_provider_settings(&ai_settings);
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        let params = serde_json::json!({
            "user_prompt": request.user_prompt,
            "language": request.language.unwrap_or_else(|| "python".to_string()),
            "ai_provider": ai_provider,
            "ai_settings": provider_settings,
        });

        with_default_bridge(&app_state, |bridge| {
            bridge.send_command_and_wait(
                "generate_prompt_snippet_with_ai",
                Some(params),
                std::time::Duration::from_secs(120),
            )
        })
    })
    .await
    .map_err(|e| {
        error!(
            "HTTP: spawn_blocking error for generate-prompt-snippet: {}",
            e
        );
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    handle_bridge_result(result, "Prompt snippet generated successfully")
}

/// POST /ai/suggest-check-groups
///
/// Suggest check groupings using AI via the Python bridge.
pub async fn suggest_check_groups_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<SuggestCheckGroupsRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    use crate::settings;

    info!(
        "HTTP: Suggesting check groups with AI: {}",
        request.user_prompt.chars().take(50).collect::<String>()
    );

    let ai_settings = settings::get_ai_settings();
    let ai_provider = ai_provider_str(&ai_settings.provider);
    let provider_settings = build_provider_settings(&ai_settings);
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        let params = serde_json::json!({
            "user_prompt": request.user_prompt,
            "existing_checks": request.existing_checks,
            "ai_provider": ai_provider,
            "ai_settings": provider_settings,
        });

        with_default_bridge(&app_state, |bridge| {
            bridge.send_command_and_wait(
                "suggest_check_groups_with_ai",
                Some(params),
                std::time::Duration::from_secs(120),
            )
        })
    })
    .await
    .map_err(|e| {
        error!("HTTP: spawn_blocking error for suggest-check-groups: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    handle_bridge_result(result, "Check groups suggested successfully")
}

/// POST /ai/suggest-exploration-strategy
///
/// Suggest an exploration strategy using AI via the Python bridge.
pub async fn suggest_exploration_strategy_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<SuggestExplorationStrategyRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    use crate::settings;

    info!(
        "HTTP: Suggesting exploration strategy with AI: {}",
        request.user_goal.chars().take(50).collect::<String>()
    );

    let ai_settings = settings::get_ai_settings();
    let ai_provider = ai_provider_str(&ai_settings.provider);
    let provider_settings = build_provider_settings(&ai_settings);
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        let params = serde_json::json!({
            "user_goal": request.user_goal,
            "config_path": request.config_path,
            "ai_provider": ai_provider,
            "ai_settings": provider_settings,
        });

        with_default_bridge(&app_state, |bridge| {
            bridge.send_command_and_wait(
                "suggest_exploration_strategy_with_ai",
                Some(params),
                std::time::Duration::from_secs(120),
            )
        })
    })
    .await
    .map_err(|e| {
        error!(
            "HTTP: spawn_blocking error for suggest-exploration-strategy: {}",
            e
        );
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    handle_bridge_result(result, "Exploration strategy suggested successfully")
}

// ============================================================================
// Helper: Process bridge result into ApiResponse
// ============================================================================

/// Converts the nested Result from spawn_blocking + with_default_bridge into
/// a proper HTTP response, handling all error cases consistently.
fn handle_bridge_result(
    result: Result<Result<crate::executor::lifecycle::CommandResponseResult, String>, String>,
    success_message: &str,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    match result {
        Ok(Ok(response)) => {
            if response.success {
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "message": success_message
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Generation failed".to_string());
                error!("HTTP: AI generation failed: {}", error_msg);
                Err((StatusCode::BAD_REQUEST, Json(api_error(error_msg))))
            }
        }
        Ok(Err(e)) => {
            error!("HTTP: Bridge command failed: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Command failed: {}", e))),
            ))
        }
        Err(e) => {
            error!("HTTP: Bridge error: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Routes
// ============================================================================

/// Create routes for this module.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::post;
    axum::Router::new()
        .route("/ai/generate-test", post(generate_test_handler))
        .route(
            "/ai/generate-shell-command",
            post(generate_shell_command_handler),
        )
        .route(
            "/ai/generate-api-request",
            post(generate_api_request_handler),
        )
        .route("/ai/generate-context", post(generate_context_handler))
        .route("/ai/generate-prompt", post(generate_prompt_handler))
        .route("/ai/generate-macro", post(generate_macro_handler))
        .route(
            "/ai/generate-prompt-snippet",
            post(generate_prompt_snippet_handler),
        )
        .route(
            "/ai/suggest-check-groups",
            post(suggest_check_groups_handler),
        )
        .route(
            "/ai/suggest-exploration-strategy",
            post(suggest_exploration_strategy_handler),
        )
}
