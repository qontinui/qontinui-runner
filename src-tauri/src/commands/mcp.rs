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

use crate::bounded_read::{decode_cursor, keyset_page, row_position, ReadLimit};
use crate::commands::compartments::{IntegrationCompartment, StorageCompartment};
use crate::mcp_client::{
    CreateMcpServerInput, McpCallRecord, McpServerConfig, McpServerStatus, McpToolCallResult,
    McpToolInfo, UpdateMcpServerInput,
};
use qontinui_types::page::{BoundedReadMeta, CursorScope, SortKey};
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

/// One keyset page of a run's MCP calls: the rows, the run's success/failure
/// counts (FIRST page only — absent, not zero, on later pages), and the
/// shared `BoundedReadMeta` keys. Pass `next_cursor` back as `cursor` for the
/// next page.
#[derive(Debug, Serialize)]
pub struct TaskRunMcpCallsPage {
    pub task_run_id: String,
    pub calls: Vec<McpCallRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_count: Option<i64>,
    #[serde(flatten)]
    pub page: BoundedReadMeta,
}

/// An MCP-calls page: 200 rows by default, never more than 1000. Shared by the
/// Tauri command and the agent-facing `GET /task-runs/{id}/mcp-calls`.
pub const MCP_CALLS_PAGE: ReadLimit = ReadLimit::new(200, 1000);

/// The keyset sequence of `task_run_mcp_calls`: `(created_at, id)` ascending.
/// Both are written once by the INSERT and never updated (pinned by
/// `bounded_read::tests::keyset_keys_have_no_update_site`).
struct McpCallsWalk;
impl SortKey for McpCallsWalk {
    const ID: &'static str = "runner.task_run_mcp_calls:created_at,id:asc";
}

/// Why an MCP-calls page could not be served.
#[derive(Debug)]
pub enum McpCallsPageError {
    /// The `cursor` was not minted by this read (wrong run, wrong filter,
    /// garbage): `(code, refusal)` — the caller restarts without `cursor`.
    Cursor(&'static str, String),
    /// The store did not answer.
    Store(String),
}

/// One keyset page of a run's MCP calls — the single implementation behind
/// the Tauri command and the :9876 door. `limit` is resolved through
/// [`MCP_CALLS_PAGE`]; the cursor is fingerprinted by the run and the success
/// filter, so it cannot be replayed under another.
pub async fn task_run_mcp_calls_page(
    pg: &crate::database::pg::PgDb,
    task_run_id: &str,
    success_filter: Option<bool>,
    limit: Option<i64>,
    cursor: Option<&str>,
    surface: &str,
) -> Result<TaskRunMcpCallsPage, McpCallsPageError> {
    let limit = MCP_CALLS_PAGE.resolve(limit);
    let scope = CursorScope::<McpCallsWalk>::new()
        .opt_str("task_run_id", Some(task_run_id))
        .opt_str(
            "success",
            success_filter.map(|s| if s { "true" } else { "false" }),
        )
        .finish();
    let after = decode_cursor(&scope, cursor)
        .map_err(|e| McpCallsPageError::Cursor(e.code(), e.refusal(surface)))?;
    let fetched = pg
        .get_task_run_mcp_calls_page(task_run_id, success_filter, after, limit)
        .await
        .map_err(McpCallsPageError::Store)?;
    let page = keyset_page(
        fetched.rows,
        limit,
        fetched.counts.map(|c| c.total),
        &scope,
        |(call, at)| row_position(&call.id, *at),
    )
    .map_err(McpCallsPageError::Store)?;
    let meta = page.meta();
    Ok(TaskRunMcpCallsPage {
        task_run_id: task_run_id.to_string(),
        calls: page.into_rows().into_iter().map(|(call, _)| call).collect(),
        success_count: fetched.counts.map(|c| c.succeeded),
        failed_count: fetched.counts.map(|c| c.failed),
        page: meta,
    })
}

/// Get one keyset page of a run's MCP calls. Omit `cursor` for the first page
/// (which also carries the exact `total` and the success/failure counts);
/// pass the previous page's `next_cursor` for the next.
#[tauri::command]
pub async fn get_task_run_mcp_calls(
    storage: State<'_, StorageCompartment>,
    task_run_id: String,
    success_filter: Option<bool>,
    limit: Option<i64>,
    cursor: Option<String>,
) -> Result<McpResponse<TaskRunMcpCallsPage>, String> {
    match task_run_mcp_calls_page(
        storage.pg_db(),
        &task_run_id,
        success_filter,
        limit,
        cursor.as_deref(),
        "get_task_run_mcp_calls",
    )
    .await
    {
        Ok(page) => Ok(McpResponse::ok(page)),
        Err(McpCallsPageError::Cursor(code, refusal)) => {
            Ok(McpResponse::err(format!("{code}: {refusal}")))
        }
        Err(McpCallsPageError::Store(e)) => {
            error!("Failed to get task run MCP calls: {}", e);
            Ok(McpResponse::err(e))
        }
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
