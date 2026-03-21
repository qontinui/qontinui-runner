//! Prompt library CRUD handlers for MCP API
//!
//! Provides HTTP handlers for managing saved prompts:
//! list, get, create, update, delete, search, import/export, duplicate.
//!
//! Note: The `run_prompt` handler remains in mcp_api.rs due to its deep
//! coupling with AI session management, database task runs, and ApiState fields.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Deserialize;
use std::sync::Arc;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::prompts;

// ============================================================================
// Request Types
// ============================================================================

/// Request to create a new prompt
#[derive(Debug, Deserialize)]
pub struct CreatePromptRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub content: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Maximum number of sessions (null = unlimited)
    #[serde(default)]
    pub max_sessions: Option<u32>,
    /// AI provider (e.g., "anthropic", "openai")
    #[serde(default)]
    pub provider: Option<String>,
    /// AI model to use
    #[serde(default)]
    pub model: Option<String>,
    /// Whether this prompt requires the orchestrator for planning and verification
    /// This is a system-level configuration set at prompt creation time
    #[serde(default)]
    pub requires_orchestrator: bool,
    /// Goal description for the orchestrator (used for planning)
    #[serde(default)]
    pub orchestrator_goal: Option<String>,
    /// Maximum iterations for the orchestrator (default: 10)
    #[serde(default)]
    pub orchestrator_max_iterations: Option<u32>,
}

/// Request to update an existing prompt
#[derive(Debug, Deserialize)]
pub struct UpdatePromptRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Maximum number of sessions (null = unlimited)
    #[serde(default)]
    pub max_sessions: Option<Option<u32>>,
    /// AI provider (e.g., "anthropic", "openai")
    #[serde(default)]
    pub provider: Option<Option<String>>,
    /// AI model to use
    #[serde(default)]
    pub model: Option<Option<String>>,
    /// Whether this prompt requires the orchestrator for planning and verification
    #[serde(default)]
    pub requires_orchestrator: Option<bool>,
    /// Goal description for the orchestrator (used for planning)
    #[serde(default)]
    pub orchestrator_goal: Option<Option<String>>,
    /// Maximum iterations for the orchestrator
    #[serde(default)]
    pub orchestrator_max_iterations: Option<Option<u32>>,
}

/// Request to import prompts
#[derive(Debug, Deserialize)]
pub struct ImportPromptsRequest {
    /// JSON array of prompts to import
    pub prompts_json: String,
}

/// Request to duplicate a prompt
#[derive(Debug, Deserialize)]
pub struct DuplicatePromptRequest {
    /// Optional new name (defaults to "Original Name (Copy)")
    #[serde(default)]
    pub new_name: Option<String>,
}

// ============================================================================
// Handlers
// ============================================================================

/// List all prompts
pub async fn list_prompts(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<prompts::SavedPrompt>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let prompts = prompts::get_all_prompts();
    Ok(Json(ApiResponse::success(prompts)))
}

/// Get a single prompt by ID
pub async fn get_prompt(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<prompts::SavedPrompt>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::get_prompt(&id) {
        Some(prompt) => Ok(Json(ApiResponse::success(prompt))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Prompt not found: {}", id))),
        )),
    }
}

/// Create a new prompt
pub async fn create_prompt(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<CreatePromptRequest>,
) -> Result<Json<ApiResponse<prompts::SavedPrompt>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::create_prompt(
        request.name,
        request.description,
        request.content,
        request.category,
        request.tags,
        request.max_sessions,
        request.provider,
        request.model,
        request.requires_orchestrator,
        request.orchestrator_goal,
        request.orchestrator_max_iterations,
    ) {
        Ok(prompt) => Ok(Json(ApiResponse::success(prompt))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Update an existing prompt
pub async fn update_prompt(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<UpdatePromptRequest>,
) -> Result<Json<ApiResponse<prompts::SavedPrompt>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::update_prompt(
        &id,
        request.name,
        request.description,
        request.content,
        request.category,
        request.tags,
        request.max_sessions,
        request.provider,
        request.model,
        request.requires_orchestrator,
        request.orchestrator_goal,
        request.orchestrator_max_iterations,
    ) {
        Ok(prompt) => Ok(Json(ApiResponse::success(prompt))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Delete a prompt
pub async fn delete_prompt(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::delete_prompt(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Get all categories
pub async fn get_prompt_categories(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let categories = prompts::get_categories();
    Ok(Json(ApiResponse::success(categories)))
}

/// Get all tags
pub async fn get_prompt_tags(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let tags = prompts::get_all_tags();
    Ok(Json(ApiResponse::success(tags)))
}

/// Import prompts from JSON
pub async fn import_prompts(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<ImportPromptsRequest>,
) -> Result<Json<ApiResponse<Vec<prompts::SavedPrompt>>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::import_prompts(&request.prompts_json) {
        Ok(imported) => Ok(Json(ApiResponse::success(imported))),
        Err(e) => Err((StatusCode::BAD_REQUEST, Json(api_error(e)))),
    }
}

/// Export all prompts as JSON
pub async fn export_prompts(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::export_prompts() {
        Ok(json) => Ok(Json(ApiResponse::success(json))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Duplicate a prompt
pub async fn duplicate_prompt(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<DuplicatePromptRequest>,
) -> Result<Json<ApiResponse<prompts::SavedPrompt>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::duplicate_prompt(&id, request.new_name) {
        Ok(prompt) => Ok(Json(ApiResponse::success(prompt))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Search prompts by query
pub async fn search_prompts(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<prompts::SavedPrompt>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let query = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let results = prompts::search_prompts(query);
    Ok(Json(ApiResponse::success(results)))
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{delete, get, post, put};
    axum::Router::new()
        .route("/prompts", get(list_prompts))
        .route("/prompts", post(create_prompt))
        .route("/prompts/search", get(search_prompts))
        .route("/prompts/categories", get(get_prompt_categories))
        .route("/prompts/tags", get(get_prompt_tags))
        .route("/prompts/import", post(import_prompts))
        .route("/prompts/export", get(export_prompts))
        .route("/prompts/{id}", get(get_prompt))
        .route("/prompts/{id}", put(update_prompt))
        .route("/prompts/{id}", delete(delete_prompt))
        .route("/prompts/{id}/duplicate", post(duplicate_prompt))
}
