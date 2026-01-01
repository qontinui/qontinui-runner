//! RAG (Retrieval Augmented Generation) handlers for MCP API
//!
//! Provides handlers for RAG operations including import, list, and status.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use std::sync::Arc;
use tracing::{error, info};

use super::types::{api_error, ApiResponse, ApiState};

/// Import RAG configuration
pub async fn import_rag(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    crate::mcp_api::import_rag_handler(state, request).await
}

/// List RAG configurations
pub async fn list_rag_configs(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, (StatusCode, Json<ApiResponse<()>>)> {
    crate::mcp_api::list_rag_configs_handler(state).await
}

/// Get RAG availability
pub async fn get_rag_availability(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    crate::mcp_api::get_rag_availability_handler(state).await
}

/// Segment a screenshot using SAM
pub async fn segment_screenshot(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    crate::mcp_api::segment_screenshot_handler(state, request).await
}

/// Get RAG status for a project
pub async fn get_rag_status(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    crate::mcp_api::get_rag_status_handler(state, project_id).await
}

/// Load a RAG project
pub async fn load_rag_project(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    crate::mcp_api::load_rag_project_handler(state, project_id).await
}

/// Delete a RAG configuration
pub async fn delete_rag_config(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    crate::mcp_api::delete_rag_config_handler(state, project_id).await
}
