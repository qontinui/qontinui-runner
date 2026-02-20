//! Prompt snippet CRUD handlers for MCP API
//!
//! Provides HTTP handlers for managing prompt snippets (reusable text snippets):
//! list, get, create, update, delete, categories, search.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Deserialize;
use std::sync::Arc;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::prompt_snippets;

// ============================================================================
// Request Types
// ============================================================================

/// Request to create a new prompt snippet
#[derive(Debug, Deserialize)]
pub struct CreatePromptSnippetRequest {
    pub name: String,
    pub content: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub source_log_ids: Option<Vec<String>>,
}

/// Request to update an existing prompt snippet
#[derive(Debug, Deserialize)]
pub struct UpdatePromptSnippetRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

// ============================================================================
// Handlers
// ============================================================================

/// List all prompt snippets
pub async fn list_prompt_snippets(
    State(_state): State<Arc<ApiState>>,
) -> Result<
    Json<ApiResponse<Vec<prompt_snippets::PromptSnippet>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let snippets = prompt_snippets::get_all_prompt_snippets();
    Ok(Json(ApiResponse::success(snippets)))
}

/// Get a single prompt snippet by ID
pub async fn get_prompt_snippet(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<prompt_snippets::PromptSnippet>>, (StatusCode, Json<ApiResponse<()>>)>
{
    match prompt_snippets::get_prompt_snippet(&id) {
        Some(snippet) => Ok(Json(ApiResponse::success(snippet))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Prompt snippet not found: {}", id))),
        )),
    }
}

/// Create a new prompt snippet
pub async fn create_prompt_snippet(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<CreatePromptSnippetRequest>,
) -> Result<Json<ApiResponse<prompt_snippets::PromptSnippet>>, (StatusCode, Json<ApiResponse<()>>)>
{
    match prompt_snippets::create_prompt_snippet(
        request.name,
        request.content,
        request.category,
        request.tags,
        request.source_log_ids,
    ) {
        Ok(snippet) => Ok(Json(ApiResponse::success(snippet))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Update an existing prompt snippet
pub async fn update_prompt_snippet(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<UpdatePromptSnippetRequest>,
) -> Result<Json<ApiResponse<prompt_snippets::PromptSnippet>>, (StatusCode, Json<ApiResponse<()>>)>
{
    match prompt_snippets::update_prompt_snippet(
        &id,
        request.name,
        request.content,
        request.category,
        request.tags,
    ) {
        Ok(snippet) => Ok(Json(ApiResponse::success(snippet))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Delete a prompt snippet
pub async fn delete_prompt_snippet(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompt_snippets::delete_prompt_snippet(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Get all prompt snippet categories
pub async fn get_prompt_snippet_categories(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let categories = prompt_snippets::get_categories();
    Ok(Json(ApiResponse::success(categories)))
}

/// Search prompt snippets
pub async fn search_prompt_snippets(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<
    Json<ApiResponse<Vec<prompt_snippets::PromptSnippet>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let query = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let results = prompt_snippets::search_prompt_snippets(query);
    Ok(Json(ApiResponse::success(results)))
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{delete, get, post, put};
    axum::Router::new()
        .route("/prompt-snippets", get(list_prompt_snippets))
        .route("/prompt-snippets", post(create_prompt_snippet))
        .route("/prompt-snippets/search", get(search_prompt_snippets))
        .route(
            "/prompt-snippets/categories",
            get(get_prompt_snippet_categories),
        )
        .route("/prompt-snippets/:id", get(get_prompt_snippet))
        .route("/prompt-snippets/:id", put(update_prompt_snippet))
        .route("/prompt-snippets/:id", delete(delete_prompt_snippet))
}
