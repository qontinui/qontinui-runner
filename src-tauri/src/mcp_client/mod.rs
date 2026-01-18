//! MCP Client Module
//!
//! Provides MCP client capabilities for calling external MCP servers from workflows.
//! Supports both stdio (subprocess) and HTTP transports.

pub mod types;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::database::CheckpointDb;
pub use types::*;

/// MCP Client Manager
///
/// Manages connections to MCP servers and provides tool calling capabilities.
pub struct McpClientManager {
    /// Active connections (server_id -> connection state)
    connections: RwLock<HashMap<String, McpConnection>>,
    /// Database for loading server configs
    db: Arc<CheckpointDb>,
}

/// Connection state for an MCP server
struct McpConnection {
    config: McpServerConfig,
    connected: bool,
    tools: Vec<McpToolInfo>,
    last_connect_attempt: Option<String>,
    last_connected: Option<String>,
    last_error: Option<String>,
}

impl McpClientManager {
    /// Create a new MCP client manager
    pub fn new(db: Arc<CheckpointDb>) -> Self {
        Self {
            connections: RwLock::new(HashMap::new()),
            db,
        }
    }

    /// Get all configured MCP servers
    pub async fn list_servers(&self) -> Result<Vec<McpServerConfig>, String> {
        self.db.list_mcp_servers()
    }

    /// Get a specific MCP server by ID
    pub async fn get_server(&self, server_id: &str) -> Result<Option<McpServerConfig>, String> {
        self.db.get_mcp_server(server_id)
    }

    /// Create a new MCP server configuration
    pub async fn create_server(&self, input: CreateMcpServerInput) -> Result<McpServerConfig, String> {
        self.db.create_mcp_server(input)
    }

    /// Update an MCP server configuration
    pub async fn update_server(&self, server_id: &str, input: UpdateMcpServerInput) -> Result<McpServerConfig, String> {
        // Disconnect if currently connected
        let mut connections = self.connections.write().await;
        connections.remove(server_id);
        drop(connections);

        self.db.update_mcp_server(server_id, input)
    }

    /// Delete an MCP server configuration
    pub async fn delete_server(&self, server_id: &str) -> Result<(), String> {
        // Disconnect if currently connected
        let mut connections = self.connections.write().await;
        connections.remove(server_id);
        drop(connections);

        self.db.delete_mcp_server(server_id)
    }

    /// Get the status of all servers
    pub async fn get_all_status(&self) -> Vec<McpServerStatus> {
        let connections = self.connections.read().await;
        let servers = self.db.list_mcp_servers().unwrap_or_default();

        servers
            .into_iter()
            .map(|config| {
                if let Some(conn) = connections.get(&config.id) {
                    McpServerStatus {
                        server_id: config.id,
                        connected: conn.connected,
                        error: conn.last_error.clone(),
                        tools: if conn.connected { Some(conn.tools.clone()) } else { None },
                        last_connect_attempt: conn.last_connect_attempt.clone(),
                        last_connected: conn.last_connected.clone(),
                    }
                } else {
                    McpServerStatus {
                        server_id: config.id,
                        connected: false,
                        error: None,
                        tools: None,
                        last_connect_attempt: None,
                        last_connected: None,
                    }
                }
            })
            .collect()
    }

    /// Get the status of a specific server
    pub async fn get_server_status(&self, server_id: &str) -> Result<McpServerStatus, String> {
        let config = self.db.get_mcp_server(server_id)?
            .ok_or_else(|| format!("MCP server not found: {}", server_id))?;

        let connections = self.connections.read().await;

        if let Some(conn) = connections.get(server_id) {
            Ok(McpServerStatus {
                server_id: config.id,
                connected: conn.connected,
                error: conn.last_error.clone(),
                tools: if conn.connected { Some(conn.tools.clone()) } else { None },
                last_connect_attempt: conn.last_connect_attempt.clone(),
                last_connected: conn.last_connected.clone(),
            })
        } else {
            Ok(McpServerStatus {
                server_id: config.id,
                connected: false,
                error: None,
                tools: None,
                last_connect_attempt: None,
                last_connected: None,
            })
        }
    }

    /// Connect to an MCP server
    pub async fn connect(&self, server_id: &str) -> Result<Vec<McpToolInfo>, String> {
        let config = self.db.get_mcp_server(server_id)?
            .ok_or_else(|| format!("MCP server not found: {}", server_id))?;

        if !config.enabled {
            return Err(format!("MCP server is disabled: {}", server_id));
        }

        info!("Connecting to MCP server: {} ({})", config.name, server_id);

        let now = chrono::Utc::now().to_rfc3339();
        let tools = match config.transport {
            McpTransport::Http => self.connect_http(&config).await,
            McpTransport::Stdio => self.connect_stdio(&config).await,
        };

        let mut connections = self.connections.write().await;

        match tools {
            Ok(tools) => {
                info!("Connected to MCP server: {} with {} tools", config.name, tools.len());

                // Cache tools in database
                if let Ok(tools_json) = serde_json::to_string(&tools) {
                    let _ = self.db.update_mcp_server_tools_cache(server_id, &tools_json, &now);
                }

                connections.insert(
                    server_id.to_string(),
                    McpConnection {
                        config,
                        connected: true,
                        tools: tools.clone(),
                        last_connect_attempt: Some(now.clone()),
                        last_connected: Some(now),
                        last_error: None,
                    },
                );

                Ok(tools)
            }
            Err(e) => {
                error!("Failed to connect to MCP server {}: {}", config.name, e);

                connections.insert(
                    server_id.to_string(),
                    McpConnection {
                        config,
                        connected: false,
                        tools: vec![],
                        last_connect_attempt: Some(now),
                        last_connected: None,
                        last_error: Some(e.clone()),
                    },
                );

                Err(e)
            }
        }
    }

    /// Disconnect from an MCP server
    pub async fn disconnect(&self, server_id: &str) -> Result<(), String> {
        let mut connections = self.connections.write().await;
        connections.remove(server_id);
        info!("Disconnected from MCP server: {}", server_id);
        Ok(())
    }

    /// Ensure connected to a server, connecting if necessary
    pub async fn ensure_connected(&self, server_id: &str) -> Result<(), String> {
        let connections = self.connections.read().await;
        if let Some(conn) = connections.get(server_id) {
            if conn.connected {
                return Ok(());
            }
        }
        drop(connections);

        self.connect(server_id).await?;
        Ok(())
    }

    /// Call a tool on an MCP server
    pub async fn call_tool(
        &self,
        server_id: &str,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<McpToolCallResult, String> {
        // Ensure connected
        self.ensure_connected(server_id).await?;

        let connections = self.connections.read().await;
        let conn = connections.get(server_id)
            .ok_or_else(|| format!("Not connected to server: {}", server_id))?;

        if !conn.connected {
            return Err(format!("Not connected to server: {}", server_id));
        }

        let config = &conn.config;
        debug!("Calling tool {} on server {}", tool_name, config.name);

        let start = Instant::now();
        let result = match config.transport {
            McpTransport::Http => self.call_tool_http(config, tool_name, &arguments).await,
            McpTransport::Stdio => self.call_tool_stdio(config, tool_name, &arguments).await,
        };
        let duration_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(content) => {
                let response_type = if content.is_object() || content.is_array() {
                    "json"
                } else {
                    "text"
                };

                Ok(McpToolCallResult {
                    success: true,
                    content: Some(content),
                    error: None,
                    response_type: response_type.to_string(),
                    duration_ms,
                })
            }
            Err(e) => {
                Ok(McpToolCallResult {
                    success: false,
                    content: None,
                    error: Some(e),
                    response_type: "error".to_string(),
                    duration_ms,
                })
            }
        }
    }

    /// List tools on a connected server
    pub async fn list_tools(&self, server_id: &str) -> Result<Vec<McpToolInfo>, String> {
        // Check cache first
        let connections = self.connections.read().await;
        if let Some(conn) = connections.get(server_id) {
            if conn.connected && !conn.tools.is_empty() {
                return Ok(conn.tools.clone());
            }
        }
        drop(connections);

        // Connect and get fresh tool list
        self.connect(server_id).await
    }

    // =========================================================================
    // HTTP Transport Implementation
    // =========================================================================

    async fn connect_http(&self, config: &McpServerConfig) -> Result<Vec<McpToolInfo>, String> {
        let http_config = config.http_config.as_ref()
            .ok_or("HTTP config missing for HTTP transport")?;

        let client = reqwest::Client::new();
        let url = format!("{}/tools/list", http_config.url.trim_end_matches('/'));

        let mut request = client.post(&url);

        // Add configured headers
        for (key, value) in &http_config.headers {
            request = request.header(key, value);
        }

        // MCP uses JSON-RPC 2.0
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        });

        let response = request
            .json(&body)
            .timeout(std::time::Duration::from_secs(config.timeout_seconds))
            .send()
            .await
            .map_err(|e| format!("Failed to connect: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Server returned error: {}", response.status()));
        }

        let json: serde_json::Value = response.json().await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        // Parse JSON-RPC response
        if let Some(error) = json.get("error") {
            return Err(format!("Server error: {}", error));
        }

        let result = json.get("result")
            .ok_or("Missing result in response")?;

        let tools: Vec<McpToolInfo> = serde_json::from_value(
            result.get("tools").cloned().unwrap_or(serde_json::json!([]))
        ).map_err(|e| format!("Failed to parse tools: {}", e))?;

        Ok(tools)
    }

    async fn call_tool_http(
        &self,
        config: &McpServerConfig,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let http_config = config.http_config.as_ref()
            .ok_or("HTTP config missing for HTTP transport")?;

        let client = reqwest::Client::new();
        let url = format!("{}/tools/call", http_config.url.trim_end_matches('/'));

        let mut request = client.post(&url);

        // Add configured headers
        for (key, value) in &http_config.headers {
            request = request.header(key, value);
        }

        // MCP uses JSON-RPC 2.0
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments
            }
        });

        let response = request
            .json(&body)
            .timeout(std::time::Duration::from_secs(config.timeout_seconds))
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Server returned error: {}", response.status()));
        }

        let json: serde_json::Value = response.json().await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        // Parse JSON-RPC response
        if let Some(error) = json.get("error") {
            return Err(format!("Tool error: {}", error));
        }

        let result = json.get("result")
            .ok_or("Missing result in response")?;

        // MCP tool results have a "content" array
        if let Some(content) = result.get("content") {
            if let Some(first) = content.as_array().and_then(|a| a.first()) {
                if let Some(text) = first.get("text") {
                    // Try to parse as JSON, fall back to string
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text.as_str().unwrap_or("")) {
                        return Ok(parsed);
                    }
                    return Ok(text.clone());
                }
            }
            return Ok(content.clone());
        }

        Ok(result.clone())
    }

    // =========================================================================
    // Stdio Transport Implementation (placeholder)
    // =========================================================================

    async fn connect_stdio(&self, config: &McpServerConfig) -> Result<Vec<McpToolInfo>, String> {
        let _stdio_config = config.stdio_config.as_ref()
            .ok_or("Stdio config missing for stdio transport")?;

        // TODO: Implement stdio transport using rmcp crate
        // For now, return an error indicating it's not yet implemented
        warn!("Stdio transport not yet fully implemented for MCP client");
        Err("Stdio transport is not yet implemented. Use HTTP transport instead.".to_string())
    }

    async fn call_tool_stdio(
        &self,
        config: &McpServerConfig,
        _tool_name: &str,
        _arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let _stdio_config = config.stdio_config.as_ref()
            .ok_or("Stdio config missing for stdio transport")?;

        // TODO: Implement stdio transport using rmcp crate
        Err("Stdio transport is not yet implemented. Use HTTP transport instead.".to_string())
    }

    // =========================================================================
    // Auto-start servers
    // =========================================================================

    /// Start all servers marked for auto-start
    pub async fn start_auto_start_servers(&self) {
        match self.db.list_mcp_servers() {
            Ok(servers) => {
                for server in servers {
                    if server.enabled && server.auto_start {
                        info!("Auto-starting MCP server: {}", server.name);
                        if let Err(e) = self.connect(&server.id).await {
                            warn!("Failed to auto-start MCP server {}: {}", server.name, e);
                        }
                    }
                }
            }
            Err(e) => {
                error!("Failed to load MCP servers for auto-start: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transport_default() {
        assert_eq!(McpTransport::default(), McpTransport::Stdio);
    }
}
