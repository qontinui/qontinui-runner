//! HTTP server and routing for MCP API
//!
//! This module provides the router configuration and server startup logic.
//! It delegates to the legacy mcp_api module for most handlers while the
//! refactoring is in progress.

use axum::Router;
use std::sync::Arc;

use crate::commands::rag::RAGState;
use crate::commands::AppState;

/// Create the API router with all routes configured
///
/// Note: This currently delegates to the mcp_api module's create_router.
/// Once refactoring is complete, this will directly configure the router
/// using handlers from the mcp submodules.
#[allow(dead_code)]
pub fn create_router(
    app_state: Arc<AppState>,
    rag_state: Arc<RAGState>,
    app_handle: tauri::AppHandle,
) -> Router {
    // Delegate to mcp_api for now - this router configuration is complex
    // and has many interdependencies that need careful migration
    crate::mcp_api::create_router(app_state, rag_state, app_handle)
}

/// Start the MCP API server
#[allow(dead_code)]
pub async fn start_server(
    app_state: Arc<AppState>,
    rag_state: Arc<RAGState>,
    app_handle: tauri::AppHandle,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Delegate to mcp_api for now
    crate::mcp_api::start_server(app_state, rag_state, app_handle, port).await
}
