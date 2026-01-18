//! MCP Client Types
//!
//! Type definitions for MCP server configuration and tool calling.

use serde::{Deserialize, Serialize};

/// Transport type for MCP server connections
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum McpTransport {
    Stdio,
    Http,
}

impl Default for McpTransport {
    fn default() -> Self {
        Self::Stdio
    }
}

/// Stdio transport configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StdioConfig {
    /// Command to execute
    pub command: String,
    /// Command arguments
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory
    pub cwd: Option<String>,
    /// Environment variables
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

/// HTTP transport configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HttpConfig {
    /// Server URL (e.g., "http://localhost:8080/mcp")
    pub url: String,
    /// HTTP headers to include
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
}

/// MCP server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Unique identifier
    pub id: String,
    /// Display name
    pub name: String,
    /// Description
    pub description: Option<String>,

    /// Transport type
    pub transport: McpTransport,

    /// Stdio transport settings
    pub stdio_config: Option<StdioConfig>,

    /// HTTP transport settings
    pub http_config: Option<HttpConfig>,

    /// Whether this server is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Auto-start when runner launches
    #[serde(default)]
    pub auto_start: bool,

    /// Connection timeout in seconds
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,

    /// Cached tool definitions (JSON)
    pub cached_tools: Option<String>,

    /// When tools were cached
    pub tools_cached_at: Option<String>,

    /// Creation timestamp
    pub created_at: String,

    /// Last update timestamp
    pub updated_at: String,
}

fn default_true() -> bool {
    true
}

fn default_timeout() -> u64 {
    30
}

/// MCP tool parameter schema (subset of JSON Schema)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolInputSchema {
    #[serde(rename = "type")]
    pub schema_type: String,
    pub description: Option<String>,
    pub properties: Option<serde_json::Value>,
    pub required: Option<Vec<String>>,
}

/// MCP tool information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolInfo {
    /// Tool name
    pub name: String,
    /// Tool description
    pub description: Option<String>,
    /// Input schema
    #[serde(rename = "inputSchema")]
    pub input_schema: McpToolInputSchema,
}

/// MCP server connection status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerStatus {
    /// Server ID
    pub server_id: String,
    /// Whether connected
    pub connected: bool,
    /// Connection error if any
    pub error: Option<String>,
    /// Available tools (if connected)
    pub tools: Option<Vec<McpToolInfo>>,
    /// Last connection attempt
    pub last_connect_attempt: Option<String>,
    /// Last successful connection
    pub last_connected: Option<String>,
}

/// Input for creating an MCP server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateMcpServerInput {
    pub name: String,
    pub description: Option<String>,
    pub transport: McpTransport,
    pub stdio_config: Option<StdioConfig>,
    pub http_config: Option<HttpConfig>,
    pub enabled: Option<bool>,
    pub auto_start: Option<bool>,
    pub timeout_seconds: Option<u64>,
}

/// Input for updating an MCP server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateMcpServerInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub transport: Option<McpTransport>,
    pub stdio_config: Option<StdioConfig>,
    pub http_config: Option<HttpConfig>,
    pub enabled: Option<bool>,
    pub auto_start: Option<bool>,
    pub timeout_seconds: Option<u64>,
}

/// Result of an MCP tool call
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolCallResult {
    /// Whether the call succeeded
    pub success: bool,
    /// Response content (if success)
    pub content: Option<serde_json::Value>,
    /// Error message (if failed)
    pub error: Option<String>,
    /// Response type
    pub response_type: String,
    /// Duration in milliseconds
    pub duration_ms: u64,
}

/// Input for recording an MCP call to the database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateMcpCallInput {
    pub task_run_id: String,
    pub step_id: String,
    pub step_name: Option<String>,
    pub server_id: String,
    pub server_name: Option<String>,
    pub tool_name: String,
    pub arguments: Option<String>,
    pub resolved_arguments: Option<String>,
    pub response: Option<String>,
    pub response_type: String,
    pub duration_ms: i64,
    pub extractions: Option<String>,
    pub assertions: Option<String>,
    pub success: bool,
    pub error_message: Option<String>,
}

/// MCP call record from database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpCallRecord {
    pub id: String,
    pub task_run_id: String,
    pub step_id: String,
    pub step_name: Option<String>,
    pub server_id: String,
    pub server_name: Option<String>,
    pub tool_name: String,
    pub arguments: Option<String>,
    pub resolved_arguments: Option<String>,
    pub response: Option<String>,
    pub response_type: String,
    pub duration_ms: i64,
    pub extractions: Option<String>,
    pub assertions: Option<String>,
    pub success: bool,
    pub error_message: Option<String>,
    pub created_at: String,
}

/// Result of querying MCP calls
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpCallsResult {
    pub task_run_id: String,
    pub calls: Vec<McpCallRecord>,
    pub count: usize,
    pub success_count: usize,
    pub failed_count: usize,
}
