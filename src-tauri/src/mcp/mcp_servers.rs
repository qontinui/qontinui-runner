use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use std::sync::Arc;
use tracing::{error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::mcp_client::{CreateMcpServerInput, McpServerConfig, UpdateMcpServerInput};

/// List all MCP server configurations
pub async fn list_mcp_servers_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<McpServerConfig>>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state.app_state.pg_db.list_mcp_servers().await {
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
    match state.app_state.pg_db.get_mcp_server(&id).await {
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

/// Create a new MCP server configuration
pub async fn create_mcp_server_handler(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<CreateMcpServerInput>,
) -> Result<(StatusCode, Json<ApiResponse<McpServerConfig>>), (StatusCode, Json<ApiResponse<()>>)>
{
    info!("Creating MCP server via API: {}", input.name);
    match state.app_state.pg_db.create_mcp_server(input).await {
        Ok(server) => {
            info!("Created MCP server: {} ({})", server.name, server.id);
            Ok((StatusCode::CREATED, Json(ApiResponse::success(server))))
        }
        Err(e) => {
            error!("Failed to create MCP server: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to create MCP server: {}", e))),
            ))
        }
    }
}

/// Update an existing MCP server configuration.
/// Disconnects any active connection before updating.
pub async fn update_mcp_server_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(input): Json<UpdateMcpServerInput>,
) -> Result<Json<ApiResponse<McpServerConfig>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Updating MCP server via API: {}", id);

    // Disconnect active connection first
    {
        let mcp_manager = state.app_state.mcp_client_manager.lock().await;
        let _ = mcp_manager.disconnect(&id).await;
    }

    match state.app_state.pg_db.update_mcp_server(&id, input).await {
        Ok(server) => {
            info!("Updated MCP server: {} ({})", server.name, server.id);
            Ok(Json(ApiResponse::success(server)))
        }
        Err(e) => {
            error!("Failed to update MCP server: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to update MCP server: {}", e))),
            ))
        }
    }
}

/// Delete an MCP server configuration.
/// Disconnects any active connection before deleting.
pub async fn delete_mcp_server_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Deleting MCP server via API: {}", id);

    // Disconnect active connection first
    {
        let mcp_manager = state.app_state.mcp_client_manager.lock().await;
        let _ = mcp_manager.disconnect(&id).await;
    }

    match state.app_state.pg_db.delete_mcp_server(&id).await {
        Ok(true) => {
            info!("Deleted MCP server: {}", id);
            Ok(Json(ApiResponse::success(())))
        }
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("MCP server not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to delete MCP server: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to delete MCP server: {}", e))),
            ))
        }
    }
}

pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::get;
    axum::Router::new()
        .route(
            "/mcp-servers",
            get(list_mcp_servers_handler).post(create_mcp_server_handler),
        )
        .route(
            "/mcp-servers/{id}",
            get(get_mcp_server_handler)
                .put(update_mcp_server_handler)
                .delete(delete_mcp_server_handler),
        )
}
