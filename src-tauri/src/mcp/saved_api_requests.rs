//! Saved API Request library CRUD handlers for MCP API
//!
//! Provides HTTP handlers for managing saved API requests:
//! list, get, create, update, delete, search, categories, tags, duplicate.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use std::sync::Arc;
use tracing::{error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Handlers
// ============================================================================

/// List all saved API requests
pub async fn list_saved_api_requests(
    State(state): State<Arc<ApiState>>,
) -> Result<
    Json<ApiResponse<Vec<crate::saved_api_requests::SavedApiRequest>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state.app_state.pg_db.list_saved_api_requests().await
        .map(|v| serde_json::from_value::<Vec<crate::saved_api_requests::SavedApiRequest>>(serde_json::Value::Array(v)).unwrap_or_default())
    {
        Ok(requests) => Ok(Json(ApiResponse::success(requests))),
        Err(e) => {
            error!("Failed to list saved API requests: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to list saved API requests: {}",
                    e
                ))),
            ))
        }
    }
}

/// Get a single saved API request by ID
pub async fn get_saved_api_request(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<
    Json<ApiResponse<crate::saved_api_requests::SavedApiRequest>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state.app_state.pg_db.get_saved_api_request(&id).await
        .map(|opt| opt.and_then(|v| serde_json::from_value::<crate::saved_api_requests::SavedApiRequest>(v).ok()))
    {
        Ok(Some(request)) => Ok(Json(ApiResponse::success(request))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Saved API request not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to get saved API request: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get saved API request: {}", e))),
            ))
        }
    }
}

/// Create a new saved API request
pub async fn create_saved_api_request(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<crate::saved_api_requests::CreateSavedApiRequestRequest>,
) -> Result<
    Json<ApiResponse<crate::saved_api_requests::SavedApiRequest>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!(
        "Creating saved API request: {} {}",
        request.method, request.url
    );
    match state
        .app_state
        .checkpoint_db
        .create_saved_api_request(&request)
    {
        Ok(created) => {
            info!(
                "Created saved API request: {} ({})",
                created.name, created.id
            );
            Ok(Json(ApiResponse::success(created)))
        }
        Err(e) => {
            error!("Failed to create saved API request: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to create saved API request: {}",
                    e
                ))),
            ))
        }
    }
}

/// Update a saved API request
pub async fn update_saved_api_request(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<crate::saved_api_requests::UpdateSavedApiRequestRequest>,
) -> Result<
    Json<ApiResponse<crate::saved_api_requests::SavedApiRequest>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!("Updating saved API request: {}", id);
    match state
        .app_state
        .checkpoint_db
        .update_saved_api_request(&id, &request)
    {
        Ok(updated) => {
            info!(
                "Updated saved API request: {} ({})",
                updated.name, updated.id
            );
            Ok(Json(ApiResponse::success(updated)))
        }
        Err(e) if e.contains("not found") => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Saved API request not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to update saved API request: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to update saved API request: {}",
                    e
                ))),
            ))
        }
    }
}

/// Delete a saved API request
pub async fn delete_saved_api_request(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Deleting saved API request: {}", id);
    match state.app_state.pg_db.delete_saved_api_request(&id).await {
        Ok(true) => Ok(Json(ApiResponse::success(serde_json::json!({
            "deleted": true,
            "id": id
        })))),
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Saved API request not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to delete saved API request: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to delete saved API request: {}",
                    e
                ))),
            ))
        }
    }
}

/// Search saved API requests
pub async fn search_saved_api_requests(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<crate::saved_api_requests::SearchSavedApiRequestsQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::saved_api_requests::SavedApiRequest>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state
        .app_state
        .checkpoint_db
        .search_saved_api_requests(&query)
    {
        Ok(requests) => Ok(Json(ApiResponse::success(requests))),
        Err(e) => {
            error!("Failed to search saved API requests: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to search saved API requests: {}",
                    e
                ))),
            ))
        }
    }
}

/// Get all categories from saved API requests
pub async fn get_saved_api_request_categories(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state
        .app_state
        .checkpoint_db
        .get_saved_api_request_categories()
    {
        Ok(categories) => Ok(Json(ApiResponse::success(categories))),
        Err(e) => {
            error!("Failed to get saved API request categories: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get categories: {}", e))),
            ))
        }
    }
}

/// Get all tags from saved API requests
pub async fn get_saved_api_request_tags(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state.app_state.pg_db.get_saved_api_request_tags().await {
        Ok(tags) => Ok(Json(ApiResponse::success(tags))),
        Err(e) => {
            error!("Failed to get saved API request tags: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get tags: {}", e))),
            ))
        }
    }
}

/// Duplicate a saved API request
pub async fn duplicate_saved_api_request(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<
    Json<ApiResponse<crate::saved_api_requests::SavedApiRequest>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!("Duplicating saved API request: {}", id);
    match state
        .app_state
        .checkpoint_db
        .duplicate_saved_api_request(&id)
    {
        Ok(duplicated) => {
            info!("Duplicated saved API request: {} -> {}", id, duplicated.id);
            Ok(Json(ApiResponse::success(duplicated)))
        }
        Err(e) if e.contains("not found") => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Saved API request not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to duplicate saved API request: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to duplicate saved API request: {}",
                    e
                ))),
            ))
        }
    }
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/saved-api-requests",
            get(list_saved_api_requests).post(create_saved_api_request),
        )
        .route("/saved-api-requests/search", get(search_saved_api_requests))
        .route(
            "/saved-api-requests/categories",
            get(get_saved_api_request_categories),
        )
        .route("/saved-api-requests/tags", get(get_saved_api_request_tags))
        .route(
            "/saved-api-requests/{id}",
            get(get_saved_api_request)
                .put(update_saved_api_request)
                .delete(delete_saved_api_request),
        )
        .route(
            "/saved-api-requests/{id}/duplicate",
            post(duplicate_saved_api_request),
        )
}
