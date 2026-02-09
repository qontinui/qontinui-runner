//! Scriptlet CRUD handlers for MCP API
//!
//! Provides HTTP handlers for managing scriptlets (reusable code snippets):
//! list, get, create, update, delete, categories, search.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Deserialize;
use std::sync::Arc;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::scriptlets;

// ============================================================================
// Request Types
// ============================================================================

/// Request to create a new scriptlet
#[derive(Debug, Deserialize)]
pub struct CreateScriptletRequest {
    pub name: String,
    pub content: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub source_log_ids: Option<Vec<String>>,
}

/// Request to update an existing scriptlet
#[derive(Debug, Deserialize)]
pub struct UpdateScriptletRequest {
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

/// List all scriptlets
pub async fn list_scriptlets(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<scriptlets::Scriptlet>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let scriptlets = scriptlets::get_all_scriptlets();
    Ok(Json(ApiResponse::success(scriptlets)))
}

/// Get a single scriptlet by ID
pub async fn get_scriptlet(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<scriptlets::Scriptlet>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scriptlets::get_scriptlet(&id) {
        Some(scriptlet) => Ok(Json(ApiResponse::success(scriptlet))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Scriptlet not found: {}", id))),
        )),
    }
}

/// Create a new scriptlet
pub async fn create_scriptlet(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<CreateScriptletRequest>,
) -> Result<Json<ApiResponse<scriptlets::Scriptlet>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scriptlets::create_scriptlet(
        request.name,
        request.content,
        request.category,
        request.tags,
        request.source_log_ids,
    ) {
        Ok(scriptlet) => Ok(Json(ApiResponse::success(scriptlet))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Update an existing scriptlet
pub async fn update_scriptlet(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<UpdateScriptletRequest>,
) -> Result<Json<ApiResponse<scriptlets::Scriptlet>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scriptlets::update_scriptlet(
        &id,
        request.name,
        request.content,
        request.category,
        request.tags,
    ) {
        Ok(scriptlet) => Ok(Json(ApiResponse::success(scriptlet))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Delete a scriptlet
pub async fn delete_scriptlet(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scriptlets::delete_scriptlet(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Get all scriptlet categories
pub async fn get_scriptlet_categories(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let categories = scriptlets::get_categories();
    Ok(Json(ApiResponse::success(categories)))
}

/// Search scriptlets
pub async fn search_scriptlets(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<scriptlets::Scriptlet>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let query = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let results = scriptlets::search_scriptlets(query);
    Ok(Json(ApiResponse::success(results)))
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{delete, get, post, put};
    axum::Router::new()
        .route("/scriptlets", get(list_scriptlets))
        .route("/scriptlets", post(create_scriptlet))
        .route("/scriptlets/search", get(search_scriptlets))
        .route("/scriptlets/categories", get(get_scriptlet_categories))
        .route("/scriptlets/:id", get(get_scriptlet))
        .route("/scriptlets/:id", put(update_scriptlet))
        .route("/scriptlets/:id", delete(delete_scriptlet))
}
