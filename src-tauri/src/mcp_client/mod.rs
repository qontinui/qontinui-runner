//! MCP Client Module
//!
//! Provides MCP client capabilities for calling external MCP servers from workflows.
//! Supports both stdio (subprocess) and HTTP transports.

pub mod types;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

pub use types::*;

/// Handle for a stdio MCP server subprocess
struct StdioHandle {
    /// The child process (behind a Mutex so we can check/kill it)
    child: tokio::sync::Mutex<Child>,
    /// Stdin writer for sending JSON-RPC requests
    stdin: tokio::sync::Mutex<ChildStdin>,
    /// Stdout reader for receiving JSON-RPC responses
    stdout: tokio::sync::Mutex<BufReader<ChildStdout>>,
    /// Monotonically increasing request ID counter (starts at 1)
    next_id: AtomicU64,
}

impl StdioHandle {
    /// Get the next JSON-RPC request ID
    fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Check if the child process has exited.
    /// Returns Some(exit_status) if exited, None if still running.
    async fn try_wait(&self) -> Option<std::process::ExitStatus> {
        let mut child = self.child.lock().await;
        child.try_wait().unwrap_or_default()
    }

    /// Kill the child process.
    async fn kill(&self) -> Result<(), std::io::Error> {
        let mut child = self.child.lock().await;
        child.kill().await
    }
}

/// MCP Client Manager
///
/// Manages connections to MCP servers and provides tool calling capabilities.
pub struct McpClientManager {
    /// Active connections (server_id -> connection state)
    connections: RwLock<HashMap<String, McpConnection>>,
}

/// Connection state for an MCP server
struct McpConnection {
    config: McpServerConfig,
    connected: bool,
    tools: Vec<McpToolInfo>,
    last_connect_attempt: Option<String>,
    last_connected: Option<String>,
    last_error: Option<String>,
    /// Stdio process handle (only for stdio transport)
    stdio_handle: Option<Arc<StdioHandle>>,
}

impl McpClientManager {
    /// Create a new MCP client manager
    pub fn new() -> Self {
        Self {
            connections: RwLock::new(HashMap::new()),
        }
    }

    /// Helper to get the PgDb global instance.
    fn pg_db() -> Result<std::sync::Arc<crate::database::pg::PgDb>, String> {
        crate::database::pg::PgDb::try_global().ok_or_else(|| "PgDb not initialized".to_string())
    }

    /// Get all configured MCP servers
    pub async fn list_servers(&self) -> Result<Vec<McpServerConfig>, String> {
        Self::pg_db()?.list_mcp_servers().await
    }

    /// Get a specific MCP server by ID
    pub async fn get_server(&self, server_id: &str) -> Result<Option<McpServerConfig>, String> {
        Self::pg_db()?.get_mcp_server(server_id).await
    }

    /// Create a new MCP server configuration
    pub async fn create_server(
        &self,
        input: CreateMcpServerInput,
    ) -> Result<McpServerConfig, String> {
        Self::pg_db()?.create_mcp_server(input).await
    }

    /// Update an MCP server configuration.
    /// Disconnects any active connection before updating.
    pub async fn update_server(
        &self,
        server_id: &str,
        input: UpdateMcpServerInput,
    ) -> Result<McpServerConfig, String> {
        // Disconnect first to kill any active stdio subprocess
        let _ = self.disconnect(server_id).await;
        Self::pg_db()?.update_mcp_server(server_id, input).await
    }

    /// Delete an MCP server configuration.
    /// Disconnects any active connection before deleting.
    pub async fn delete_server(&self, server_id: &str) -> Result<(), String> {
        // Disconnect first to kill any active stdio subprocess
        let _ = self.disconnect(server_id).await;
        let deleted = Self::pg_db()?.delete_mcp_server(server_id).await?;
        if !deleted {
            return Err(format!("MCP server not found: {}", server_id));
        }
        Ok(())
    }

    /// Get the status of all servers.
    /// Merges PgDb server configs with in-memory connection state.
    pub async fn get_all_status(&self) -> Vec<McpServerStatus> {
        let connections = self.connections.read().await;

        // If PgDb is available, fetch all servers and merge with connection state
        if let Ok(db) = Self::pg_db() {
            if let Ok(servers) = db.list_mcp_servers().await {
                let mut statuses = Vec::with_capacity(servers.len());
                for server in servers {
                    if let Some(conn) = connections.get(&server.id) {
                        statuses.push(McpServerStatus {
                            server_id: server.id,
                            connected: conn.connected,
                            error: conn.last_error.clone(),
                            tools: if conn.connected {
                                Some(conn.tools.clone())
                            } else {
                                None
                            },
                            last_connect_attempt: conn.last_connect_attempt.clone(),
                            last_connected: conn.last_connected.clone(),
                        });
                    } else {
                        statuses.push(McpServerStatus {
                            server_id: server.id,
                            connected: false,
                            error: None,
                            tools: None,
                            last_connect_attempt: None,
                            last_connected: None,
                        });
                    }
                }
                return statuses;
            }
        }

        // Fallback: return status for currently-connected servers only
        connections
            .iter()
            .map(|(id, conn)| McpServerStatus {
                server_id: id.clone(),
                connected: conn.connected,
                error: conn.last_error.clone(),
                tools: if conn.connected {
                    Some(conn.tools.clone())
                } else {
                    None
                },
                last_connect_attempt: conn.last_connect_attempt.clone(),
                last_connected: conn.last_connected.clone(),
            })
            .collect()
    }

    /// Get the status of a specific server.
    /// Returns connection state if connected, or a disconnected status if the
    /// server exists in PgDb but hasn't been connected yet.
    pub async fn get_server_status(&self, server_id: &str) -> Result<McpServerStatus, String> {
        let connections = self.connections.read().await;

        if let Some(conn) = connections.get(server_id) {
            Ok(McpServerStatus {
                server_id: server_id.to_string(),
                connected: conn.connected,
                error: conn.last_error.clone(),
                tools: if conn.connected {
                    Some(conn.tools.clone())
                } else {
                    None
                },
                last_connect_attempt: conn.last_connect_attempt.clone(),
                last_connected: conn.last_connected.clone(),
            })
        } else {
            // Check PgDb — server may exist but never been connected
            drop(connections);
            if let Ok(db) = Self::pg_db() {
                if let Ok(Some(_)) = db.get_mcp_server(server_id).await {
                    return Ok(McpServerStatus {
                        server_id: server_id.to_string(),
                        connected: false,
                        error: None,
                        tools: None,
                        last_connect_attempt: None,
                        last_connected: None,
                    });
                }
            }
            Err(format!("MCP server not found: {}", server_id))
        }
    }

    /// Connect to an MCP server by loading its config from PgDb.
    pub async fn connect(&self, server_id: &str) -> Result<Vec<McpToolInfo>, String> {
        let config = Self::pg_db()?
            .get_mcp_server(server_id)
            .await?
            .ok_or_else(|| format!("MCP server not found: {}", server_id))?;

        if !config.enabled {
            return Err(format!("MCP server is disabled: {}", server_id));
        }

        let now = chrono::Utc::now().to_rfc3339();

        let (tools, stdio_handle) = match config.transport {
            McpTransport::Http => {
                let tools = self.connect_http(&config).await?;
                (tools, None)
            }
            McpTransport::Stdio => self.connect_stdio(&config).await?,
        };

        // Cache the tool list in the database
        if let Ok(tools_json) = serde_json::to_string(&tools) {
            let _ = Self::pg_db().ok().map(|db| {
                let server_id = server_id.to_string();
                let now_clone = now.clone();
                tokio::spawn(async move {
                    let _ = db
                        .update_mcp_server_cached_tools(
                            &server_id,
                            Some(&tools_json),
                            Some(&now_clone),
                        )
                        .await;
                });
            });
        }

        // Store the connection state
        let mut connections = self.connections.write().await;
        connections.insert(
            server_id.to_string(),
            McpConnection {
                config,
                connected: true,
                tools: tools.clone(),
                last_connect_attempt: Some(now.clone()),
                last_connected: Some(now),
                last_error: None,
                stdio_handle,
            },
        );

        info!(
            "Connected to MCP server {} with {} tools",
            server_id,
            tools.len()
        );

        Ok(tools)
    }

    /// Disconnect from an MCP server
    pub async fn disconnect(&self, server_id: &str) -> Result<(), String> {
        let mut connections = self.connections.write().await;
        if let Some(mut conn) = connections.remove(server_id) {
            // Kill stdio subprocess if present
            if let Some(handle) = conn.stdio_handle.take() {
                if let Err(e) = handle.kill().await {
                    // The process may have already exited — only warn on unexpected errors
                    warn!(
                        "Failed to kill stdio process for server {}: {}",
                        server_id, e
                    );
                } else {
                    debug!("Killed stdio process for server {}", server_id);
                }
            }
        }
        info!("Disconnected from MCP server: {}", server_id);
        Ok(())
    }

    /// Ensure connected to a server, connecting if necessary.
    ///
    /// For stdio servers, also checks whether the child process is still alive.
    /// If the process has exited, marks the connection as dead and reconnects.
    pub async fn ensure_connected(&self, server_id: &str) -> Result<(), String> {
        let needs_reconnect = {
            let connections = self.connections.read().await;
            if let Some(conn) = connections.get(server_id) {
                if conn.connected {
                    // For stdio connections, verify the process is still alive
                    if let Some(ref handle) = conn.stdio_handle {
                        if let Some(exit_status) = handle.try_wait().await {
                            warn!(
                                "MCP stdio process for server {} has exited ({}), will reconnect",
                                server_id, exit_status
                            );
                            true // needs reconnect
                        } else {
                            false // process is alive, we're good
                        }
                    } else {
                        // HTTP connection — connected flag is sufficient
                        false
                    }
                } else {
                    true // not connected
                }
            } else {
                true // no connection entry at all
            }
        };

        if needs_reconnect {
            // Clean up any dead connection state first
            {
                let mut connections = self.connections.write().await;
                if let Some(conn) = connections.get(server_id) {
                    if conn.connected {
                        // The process died — remove the stale connection
                        connections.remove(server_id);
                    }
                }
            }
            self.connect(server_id).await?;
        }

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

        // Extract what we need from the connection, then drop the lock before
        // making any async network/IPC calls. This prevents blocking other
        // operations (connect, disconnect, status) during tool execution.
        enum TransportInfo {
            Http {
                config: Box<McpServerConfig>,
            },
            Stdio {
                handle: Arc<StdioHandle>,
                timeout_secs: u64,
            },
        }

        let transport_info = {
            let connections = self.connections.read().await;
            let conn = connections
                .get(server_id)
                .ok_or_else(|| format!("Not connected to server: {}", server_id))?;

            if !conn.connected {
                return Err(format!("Not connected to server: {}", server_id));
            }

            debug!("Calling tool {} on server {}", tool_name, conn.config.name);

            match conn.config.transport {
                McpTransport::Http => TransportInfo::Http {
                    config: Box::new(conn.config.clone()),
                },
                McpTransport::Stdio => TransportInfo::Stdio {
                    handle: conn
                        .stdio_handle
                        .as_ref()
                        .ok_or("Stdio handle missing for connected stdio server")?
                        .clone(),
                    timeout_secs: conn.config.timeout_seconds,
                },
            }
        }; // connections lock dropped here

        let start = Instant::now();
        let result = match transport_info {
            TransportInfo::Http { ref config } => {
                self.call_tool_http(config, tool_name, &arguments).await
            }
            TransportInfo::Stdio {
                ref handle,
                timeout_secs,
            } => Self::call_tool_stdio_impl(handle, tool_name, &arguments, timeout_secs).await,
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
            Err(e) => Ok(McpToolCallResult {
                success: false,
                content: None,
                error: Some(e),
                response_type: "error".to_string(),
                duration_ms,
            }),
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
        let http_config = config
            .http_config
            .as_ref()
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

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        // Parse JSON-RPC response
        if let Some(error) = json.get("error") {
            return Err(format!("Server error: {}", error));
        }

        let result = json.get("result").ok_or("Missing result in response")?;

        let tools: Vec<McpToolInfo> = serde_json::from_value(
            result
                .get("tools")
                .cloned()
                .unwrap_or(serde_json::json!([])),
        )
        .map_err(|e| format!("Failed to parse tools: {}", e))?;

        Ok(tools)
    }

    async fn call_tool_http(
        &self,
        config: &McpServerConfig,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let http_config = config
            .http_config
            .as_ref()
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

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        // Parse JSON-RPC response
        if let Some(error) = json.get("error") {
            return Err(format!("Tool error: {}", error));
        }

        let result = json.get("result").ok_or("Missing result in response")?;

        // MCP tool results have a "content" array
        if let Some(content) = result.get("content") {
            if let Some(first) = content.as_array().and_then(|a| a.first()) {
                if let Some(text) = first.get("text") {
                    // Try to parse as JSON, fall back to string
                    if let Ok(parsed) =
                        serde_json::from_str::<serde_json::Value>(text.as_str().unwrap_or(""))
                    {
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
    // Stdio Transport Implementation
    // =========================================================================

    /// Connect to an MCP server via stdio transport.
    ///
    /// Spawns the configured command as a subprocess, performs the MCP initialize
    /// handshake over stdin/stdout using newline-delimited JSON-RPC 2.0, then
    /// fetches the tool list. Returns the tools and the stdio handle for
    /// subsequent tool calls.
    async fn connect_stdio(
        &self,
        config: &McpServerConfig,
    ) -> Result<(Vec<McpToolInfo>, Option<Arc<StdioHandle>>), String> {
        let stdio_config = config
            .stdio_config
            .as_ref()
            .ok_or("Stdio config missing for stdio transport")?;

        info!(
            "Spawning stdio MCP server: {} {}",
            stdio_config.command,
            stdio_config.args.join(" ")
        );

        // Build the subprocess command
        let mut cmd = tokio::process::Command::new(&stdio_config.command);
        cmd.args(&stdio_config.args);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        // Set working directory if configured
        if let Some(ref cwd) = stdio_config.cwd {
            cmd.current_dir(cwd);
        }

        // Set environment variables
        for (key, value) in &stdio_config.env {
            cmd.env(key, value);
        }

        // Spawn the process
        let mut child = cmd.spawn().map_err(|e| {
            format!(
                "Failed to spawn stdio process '{}': {}",
                stdio_config.command, e
            )
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or("Failed to capture stdin of stdio process")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("Failed to capture stdout of stdio process")?;

        // Spawn a task to drain stderr and log it
        if let Some(stderr) = child.stderr.take() {
            let server_name = config.name.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break, // EOF
                        Ok(_) => {
                            let trimmed = line.trim_end();
                            if !trimmed.is_empty() {
                                debug!("MCP server [{}] stderr: {}", server_name, trimmed);
                            }
                        }
                        Err(e) => {
                            warn!(
                                "Error reading stderr from MCP server [{}]: {}",
                                server_name, e
                            );
                            break;
                        }
                    }
                }
            });
        }

        let handle = Arc::new(StdioHandle {
            child: tokio::sync::Mutex::new(child),
            stdin: tokio::sync::Mutex::new(stdin),
            stdout: tokio::sync::Mutex::new(BufReader::new(stdout)),
            next_id: AtomicU64::new(1),
        });

        let timeout = std::time::Duration::from_secs(config.timeout_seconds);

        // Step 1: Send initialize request
        let init_id = handle.next_request_id();
        let init_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": init_id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "qontinui-runner",
                    "version": "1.0.0"
                }
            }
        });

        let init_response = Self::stdio_request(&handle, &init_request, timeout).await?;

        // Validate initialize response
        if let Some(error) = init_response.get("error") {
            return Err(format!("MCP initialize error: {}", error));
        }

        if let Some(result) = init_response.get("result") {
            if let Some(server_info) = result.get("serverInfo") {
                info!(
                    "MCP server initialized: {} v{}",
                    server_info
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown"),
                    server_info
                        .get("version")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                );
            }
        }

        // Step 2: Send initialized notification (no id, no response expected)
        let initialized_notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });

        Self::stdio_send(&handle, &initialized_notification).await?;

        // Step 3: Send tools/list request
        let tools_id = handle.next_request_id();
        let tools_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": tools_id,
            "method": "tools/list",
            "params": {}
        });

        let tools_response = Self::stdio_request(&handle, &tools_request, timeout).await?;

        if let Some(error) = tools_response.get("error") {
            return Err(format!("MCP tools/list error: {}", error));
        }

        let result = tools_response
            .get("result")
            .ok_or("Missing result in tools/list response")?;

        let tools: Vec<McpToolInfo> = serde_json::from_value(
            result
                .get("tools")
                .cloned()
                .unwrap_or(serde_json::json!([])),
        )
        .map_err(|e| format!("Failed to parse tools: {}", e))?;

        Ok((tools, Some(handle)))
    }

    /// Call a tool on a stdio-connected MCP server.
    async fn call_tool_stdio_impl(
        handle: &Arc<StdioHandle>,
        tool_name: &str,
        arguments: &serde_json::Value,
        timeout_secs: u64,
    ) -> Result<serde_json::Value, String> {
        let request_id = handle.next_request_id();
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments
            }
        });

        let timeout = std::time::Duration::from_secs(timeout_secs);
        let response = Self::stdio_request(handle, &request, timeout).await?;

        if let Some(error) = response.get("error") {
            return Err(format!("Tool error: {}", error));
        }

        let result = response
            .get("result")
            .ok_or("Missing result in tool call response")?;

        // MCP tool results have a "content" array
        if let Some(content) = result.get("content") {
            if let Some(first) = content.as_array().and_then(|a| a.first()) {
                if let Some(text) = first.get("text") {
                    // Try to parse as JSON, fall back to string
                    if let Ok(parsed) =
                        serde_json::from_str::<serde_json::Value>(text.as_str().unwrap_or(""))
                    {
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
    // Stdio JSON-RPC helpers
    // =========================================================================

    /// Send a JSON-RPC message (request or notification) over stdin.
    /// Does NOT read a response — use `stdio_request` for request-response.
    async fn stdio_send(
        handle: &Arc<StdioHandle>,
        message: &serde_json::Value,
    ) -> Result<(), String> {
        let mut line = serde_json::to_string(message)
            .map_err(|e| format!("Failed to serialize JSON-RPC message: {}", e))?;
        line.push('\n');

        let mut stdin = handle.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("Failed to write to stdin: {}", e))?;
        stdin
            .flush()
            .await
            .map_err(|e| format!("Failed to flush stdin: {}", e))?;

        Ok(())
    }

    /// Send a JSON-RPC request and wait for the response line with a timeout.
    ///
    /// Reads lines from stdout, skipping any that are not valid JSON-RPC
    /// responses matching the request ID (e.g., server log lines or notifications).
    async fn stdio_request(
        handle: &Arc<StdioHandle>,
        request: &serde_json::Value,
        timeout: std::time::Duration,
    ) -> Result<serde_json::Value, String> {
        let request_id = request.get("id").and_then(|v| v.as_u64());

        // Send the request
        Self::stdio_send(handle, request).await?;

        // Read response with timeout
        let read_fut = async {
            let mut stdout = handle.stdout.lock().await;
            let mut line = String::new();

            loop {
                line.clear();
                let bytes_read = stdout
                    .read_line(&mut line)
                    .await
                    .map_err(|e| format!("Failed to read from stdout: {}", e))?;

                if bytes_read == 0 {
                    return Err(
                        "MCP server process closed stdout (process may have exited)".to_string()
                    );
                }

                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                // Try to parse as JSON
                let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => {
                        // Not valid JSON — skip (could be a log line from the server)
                        debug!("Skipping non-JSON line from MCP server stdout: {}", trimmed);
                        continue;
                    }
                };

                // Check if this is a JSON-RPC response matching our request ID
                if let Some(expected_id) = request_id {
                    if let Some(response_id) = parsed.get("id").and_then(|v| v.as_u64()) {
                        if response_id == expected_id {
                            return Ok(parsed);
                        }
                        // Response for a different request ID — skip
                        debug!(
                            "Skipping JSON-RPC response with mismatched id: expected {}, got {}",
                            expected_id, response_id
                        );
                        continue;
                    }
                }

                // If the parsed message is a notification (no "id" field) or we don't
                // have a request ID to match against, check if it looks like a response
                if parsed.get("result").is_some() || parsed.get("error").is_some() {
                    // Looks like a response — return it if we had no specific ID to match
                    if request_id.is_none() {
                        return Ok(parsed);
                    }
                }

                // Otherwise it's a notification or other message — skip
                debug!("Skipping non-response JSON from MCP server: {}", trimmed);
            }
        };

        tokio::time::timeout(timeout, read_fut).await.map_err(|_| {
            format!(
                "Timeout waiting for MCP server response after {}s",
                timeout.as_secs()
            )
        })?
    }

    // =========================================================================
    // Auto-start servers
    // =========================================================================

    /// Start all servers marked for auto-start.
    pub async fn start_auto_start_servers(&self) {
        let servers = match Self::pg_db() {
            Ok(db) => match db.list_auto_start_mcp_servers().await {
                Ok(s) => s,
                Err(e) => {
                    warn!("Failed to list auto-start MCP servers: {}", e);
                    return;
                }
            },
            Err(e) => {
                warn!("Cannot auto-start MCP servers — {}", e);
                return;
            }
        };

        if servers.is_empty() {
            debug!("No MCP servers configured for auto-start");
            return;
        }

        info!(
            "Auto-starting {} MCP server(s): {}",
            servers.len(),
            servers
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );

        for server in &servers {
            match self.connect(&server.id).await {
                Ok(tools) => {
                    info!(
                        "Auto-started MCP server '{}' with {} tools",
                        server.name,
                        tools.len()
                    );
                }
                Err(e) => {
                    error!("Failed to auto-start MCP server '{}': {}", server.name, e);
                }
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
