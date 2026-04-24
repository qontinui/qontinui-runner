//! MCP (Model Context Protocol) Commands
//!
//! Provides Tauri commands for managing MCP server configurations
//! and calling tools on connected MCP servers.
//!
//! # Features
//! - CRUD operations for MCP server configurations
//! - Connect/disconnect from MCP servers
//! - List available tools on connected servers
//! - Call tools with arguments
//! - Query MCP call history for task runs

use crate::commands::compartments::{IntegrationCompartment, StorageCompartment};
use crate::mcp_client::{
    CreateMcpServerInput, McpCallsResult, McpServerConfig, McpServerStatus, McpToolCallResult,
    McpToolInfo, UpdateMcpServerInput,
};
use serde::Serialize;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;
use tracing::{error, info};

// ============================================================================
// Response Types
// ============================================================================

/// Response wrapper for MCP operations
#[derive(Debug, Serialize)]
pub struct McpResponse<T> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<String>,
}

impl<T> McpResponse<T> {
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(error: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(error),
        }
    }
}

// ============================================================================
// MCP Server Management Commands
// ============================================================================

/// List all configured MCP servers
#[tauri::command]
pub async fn list_mcp_servers(
    storage: State<'_, StorageCompartment>,
) -> Result<McpResponse<Vec<McpServerConfig>>, String> {
    let result = storage.pg_db().list_mcp_servers().await;
    match result {
        Ok(servers) => Ok(McpResponse::ok(servers)),
        Err(e) => {
            error!("Failed to list MCP servers: {}", e);
            Ok(McpResponse::err(e))
        }
    }
}

/// Get a specific MCP server by ID
#[tauri::command]
pub async fn get_mcp_server(
    storage: State<'_, StorageCompartment>,
    server_id: String,
) -> Result<McpResponse<McpServerConfig>, String> {
    let result = storage.pg_db().get_mcp_server(&server_id).await;
    match result {
        Ok(Some(server)) => Ok(McpResponse::ok(server)),
        Ok(None) => Ok(McpResponse::err(format!(
            "MCP server not found: {}",
            server_id
        ))),
        Err(e) => {
            error!("Failed to get MCP server: {}", e);
            Ok(McpResponse::err(e))
        }
    }
}

/// Create a new MCP server configuration
#[tauri::command]
pub async fn create_mcp_server(
    storage: State<'_, StorageCompartment>,
    input: CreateMcpServerInput,
) -> Result<McpResponse<McpServerConfig>, String> {
    info!("Creating MCP server: {}", input.name);

    let result = storage.pg_db().create_mcp_server(input).await;
    match result {
        Ok(server) => {
            info!("Created MCP server: {} ({})", server.name, server.id);
            Ok(McpResponse::ok(server))
        }
        Err(e) => {
            error!("Failed to create MCP server: {}", e);
            Ok(McpResponse::err(e))
        }
    }
}

/// Update an existing MCP server configuration.
///
/// Goes through the MCP client manager to ensure any active stdio
/// subprocess is killed before updating the configuration.
#[tauri::command]
pub async fn update_mcp_server(
    integration: State<'_, IntegrationCompartment>,
    server_id: String,
    input: UpdateMcpServerInput,
) -> Result<McpResponse<McpServerConfig>, String> {
    info!("Updating MCP server: {}", server_id);

    let mcp_manager = integration.mcp_client_manager().lock().await;

    match mcp_manager.update_server(&server_id, input).await {
        Ok(server) => {
            info!("Updated MCP server: {} ({})", server.name, server.id);
            Ok(McpResponse::ok(server))
        }
        Err(e) => {
            error!("Failed to update MCP server: {}", e);
            Ok(McpResponse::err(e))
        }
    }
}

/// Delete an MCP server configuration.
///
/// Goes through the MCP client manager to ensure any active stdio
/// subprocess is killed before deleting the configuration.
#[tauri::command]
pub async fn delete_mcp_server(
    integration: State<'_, IntegrationCompartment>,
    server_id: String,
) -> Result<McpResponse<()>, String> {
    info!("Deleting MCP server: {}", server_id);

    let mcp_manager = integration.mcp_client_manager().lock().await;

    match mcp_manager.delete_server(&server_id).await {
        Ok(()) => {
            info!("Deleted MCP server: {}", server_id);
            Ok(McpResponse::ok(()))
        }
        Err(e) => {
            error!("Failed to delete MCP server: {}", e);
            Ok(McpResponse::err(e))
        }
    }
}

// ============================================================================
// MCP Connection Commands
// ============================================================================

/// Connect to an MCP server and list its tools
#[tauri::command]
pub async fn connect_mcp_server(
    integration: State<'_, IntegrationCompartment>,
    server_id: String,
) -> Result<McpResponse<Vec<McpToolInfo>>, String> {
    info!("Connecting to MCP server: {}", server_id);

    // Get the MCP client manager from state
    let mcp_manager = integration.mcp_client_manager().lock().await;

    match mcp_manager.connect(&server_id).await {
        Ok(tools) => {
            info!(
                "Connected to MCP server {} with {} tools",
                server_id,
                tools.len()
            );
            Ok(McpResponse::ok(tools))
        }
        Err(e) => {
            error!("Failed to connect to MCP server: {}", e);
            Ok(McpResponse::err(e))
        }
    }
}

/// Disconnect from an MCP server
#[tauri::command]
pub async fn disconnect_mcp_server(
    integration: State<'_, IntegrationCompartment>,
    server_id: String,
) -> Result<McpResponse<()>, String> {
    info!("Disconnecting from MCP server: {}", server_id);

    let mcp_manager = integration.mcp_client_manager().lock().await;

    match mcp_manager.disconnect(&server_id).await {
        Ok(()) => {
            info!("Disconnected from MCP server: {}", server_id);
            Ok(McpResponse::ok(()))
        }
        Err(e) => {
            error!("Failed to disconnect from MCP server: {}", e);
            Ok(McpResponse::err(e))
        }
    }
}

/// Get the status of all MCP servers
#[tauri::command]
pub async fn get_mcp_servers_status(
    integration: State<'_, IntegrationCompartment>,
) -> Result<McpResponse<Vec<McpServerStatus>>, String> {
    let mcp_manager = integration.mcp_client_manager().lock().await;
    let status = mcp_manager.get_all_status().await;
    Ok(McpResponse::ok(status))
}

/// Get the status of a specific MCP server
#[tauri::command]
pub async fn get_mcp_server_status(
    integration: State<'_, IntegrationCompartment>,
    server_id: String,
) -> Result<McpResponse<McpServerStatus>, String> {
    let mcp_manager = integration.mcp_client_manager().lock().await;

    match mcp_manager.get_server_status(&server_id).await {
        Ok(status) => Ok(McpResponse::ok(status)),
        Err(e) => {
            error!("Failed to get MCP server status: {}", e);
            Ok(McpResponse::err(e))
        }
    }
}

/// List tools available on a connected MCP server
#[tauri::command]
pub async fn list_mcp_server_tools(
    integration: State<'_, IntegrationCompartment>,
    server_id: String,
) -> Result<McpResponse<Vec<McpToolInfo>>, String> {
    let mcp_manager = integration.mcp_client_manager().lock().await;

    match mcp_manager.list_tools(&server_id).await {
        Ok(tools) => Ok(McpResponse::ok(tools)),
        Err(e) => {
            error!("Failed to list MCP server tools: {}", e);
            Ok(McpResponse::err(e))
        }
    }
}

// ============================================================================
// MCP Tool Call Commands
// ============================================================================

/// Call a tool on an MCP server
#[tauri::command]
pub async fn call_mcp_tool(
    integration: State<'_, IntegrationCompartment>,
    server_id: String,
    tool_name: String,
    arguments: serde_json::Value,
) -> Result<McpResponse<McpToolCallResult>, String> {
    info!("Calling MCP tool: {}.{}", server_id, tool_name);

    let mcp_manager = integration.mcp_client_manager().lock().await;

    match mcp_manager
        .call_tool(&server_id, &tool_name, arguments)
        .await
    {
        Ok(result) => {
            if result.success {
                info!(
                    "MCP tool call succeeded: {}.{} ({}ms)",
                    server_id, tool_name, result.duration_ms
                );
            } else {
                error!(
                    "MCP tool call failed: {}.{}: {:?}",
                    server_id, tool_name, result.error
                );
            }
            Ok(McpResponse::ok(result))
        }
        Err(e) => {
            error!("Failed to call MCP tool: {}", e);
            Ok(McpResponse::err(e))
        }
    }
}

// ============================================================================
// MCP Call History Commands
// ============================================================================

/// Get MCP calls for a task run
#[tauri::command]
pub async fn get_task_run_mcp_calls(
    storage: State<'_, StorageCompartment>,
    task_run_id: String,
    success_filter: Option<bool>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<McpResponse<McpCallsResult>, String> {
    match storage
        .pg_db()
        .get_task_run_mcp_calls(&task_run_id, success_filter)
        .await
    {
        Ok(mut result) => {
            // Apply offset/limit pagination to prevent large IPC payloads
            let total_count = result.calls.len();
            let off = offset.unwrap_or(0);
            let lim = limit.unwrap_or(200);
            let paginated: Vec<_> = result.calls.into_iter().skip(off).take(lim).collect();
            let has_more = off + paginated.len() < total_count;
            result.calls = paginated;
            result.total_count = total_count;
            result.has_more = has_more;
            Ok(McpResponse::ok(result))
        }
        Err(e) => {
            error!("Failed to get task run MCP calls: {}", e);
            Ok(McpResponse::err(e))
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_response_ok() {
        let response: McpResponse<String> = McpResponse::ok("test".to_string());
        assert!(response.success);
        assert_eq!(response.data, Some("test".to_string()));
        assert!(response.error.is_none());
    }

    #[test]
    fn test_mcp_response_err() {
        let response: McpResponse<String> = McpResponse::err("error".to_string());
        assert!(!response.success);
        assert!(response.data.is_none());
        assert_eq!(response.error, Some("error".to_string()));
    }
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_mcp")
        .invoke_handler(tauri::generate_handler![
            list_mcp_servers,
            get_mcp_server,
            create_mcp_server,
            update_mcp_server,
            delete_mcp_server,
            connect_mcp_server,
            disconnect_mcp_server,
            get_mcp_servers_status,
            get_mcp_server_status,
            list_mcp_server_tools,
            call_mcp_tool,
            get_task_run_mcp_calls,
        ])
        .build()
}
