use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use std::sync::Arc;
use tracing::error;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::mcp_client::McpServerConfig;

/// List all MCP server configurations
pub async fn list_mcp_servers_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<McpServerConfig>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = if let Some(pg) = &state.app_state.pg_db {
        pg.list_mcp_servers().await
    } else {
        state.app_state.checkpoint_db.list_mcp_servers()
    };
    match result {
        Ok(servers) => Ok(Json(ApiResponse::success(servers))),
        Err(e) => {
            error!("Failed to list MCP servers: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to list MCP servers: {}", e))),
            ))
        }
    }
}

/// Get a single MCP server by ID
pub async fn get_mcp_server_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<McpServerConfig>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = if let Some(pg) = &state.app_state.pg_db {
        pg.get_mcp_server(&id).await
    } else {
        state.app_state.checkpoint_db.get_mcp_server(&id)
    };
    match result {
        Ok(Some(server)) => Ok(Json(ApiResponse::success(server))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("MCP server not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to get MCP server: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get MCP server: {}", e))),
            ))
        }
    }
}

pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::get;
    axum::Router::new()
        .route("/mcp-servers", get(list_mcp_servers_handler))
        .route("/mcp-servers/{id}", get(get_mcp_server_handler))
}
