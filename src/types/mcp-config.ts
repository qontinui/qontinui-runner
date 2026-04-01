/**
 * MCP (Model Context Protocol) Configuration Types
 *
 * Types for configuring and managing MCP server connections.
 * MCP allows the runner to call external tools and services.
 */

// =============================================================================
// MCP Server Configuration
// =============================================================================

/**
 * Transport type for MCP server connections
 */
export type McpTransportType = "stdio" | "http";

/**
 * MCP server configuration
 */
export interface McpServerConfig {
  /** Unique identifier for this server */
  id: string;
  /** Display name */
  name: string;
  /** Description of what this server provides */
  description?: string;

  /** Transport type */
  transport: McpTransportType;

  // Stdio transport settings
  /** Command to execute (for stdio transport) */
  command?: string;
  /** Command arguments (for stdio transport) */
  args?: string[];
  /** Working directory (for stdio transport) */
  cwd?: string;
  /** Environment variables (for stdio transport) */
  env?: Record<string, string>;

  // HTTP transport settings
  /** Server URL (for http transport) */
  url?: string;
  /** HTTP headers to include (for http transport) */
  headers?: Record<string, string>;

  // Common settings
  /** Whether this server is enabled */
  enabled: boolean;
  /** Auto-start when runner launches */
  autoStart: boolean;
  /** Connection timeout in seconds */
  timeout_seconds: number;

  /** Creation timestamp */
  created_at: string;
  /** Last update timestamp */
  updated_at: string;
}

/**
 * MCP settings container
 */
export interface McpSettings {
  /** Configured MCP servers */
  servers: McpServerConfig[];
}

// =============================================================================
// MCP Tool Types
// =============================================================================

/**
 * MCP tool parameter schema (JSON Schema subset)
 */
export interface McpToolParameterSchema {
  type: string;
  description?: string;
  properties?: Record<string, McpToolParameterSchema>;
  required?: string[];
  items?: McpToolParameterSchema;
  enum?: string[];
  default?: unknown;
}

/**
 * MCP tool information
 */
export interface McpToolInfo {
  /** Tool name */
  name: string;
  /** Tool description */
  description?: string;
  /** Input schema (JSON Schema) */
  inputSchema: McpToolParameterSchema;
}

/**
 * MCP server connection status
 */
export interface McpServerStatus {
  /** Server ID */
  serverId: string;
  /** Whether connected */
  connected: boolean;
  /** Connection error if any */
  error?: string;
  /** Available tools (if connected) */
  tools?: McpToolInfo[];
  /** Last connection attempt */
  lastConnectAttempt?: string;
  /** Last successful connection */
  lastConnected?: string;
}

// =============================================================================
// MCP Call Result Types (for AI Data View)
// =============================================================================

/**
 * MCP call record from SQLite database.
 * Stores MCP tool call execution results.
 */
export interface TaskRunMcpCallDb {
  id: string;
  task_run_id: string;

  /** Step identification */
  step_id: string;
  step_name?: string | null;

  /** Server identification */
  server_id: string;
  server_name?: string | null;

  /** Tool call details */
  tool_name: string;
  /** JSON: tool arguments */
  arguments?: string | null;
  /** JSON: resolved arguments after variable substitution */
  resolved_arguments?: string | null;

  /** Response details */
  /** JSON: tool response content */
  response?: string | null;
  response_type: string; // 'text', 'json', 'error'
  duration_ms: number;

  /** Variable extractions (JSON array of extraction results) */
  extractions?: string | null;
  /** Assertion results (JSON array of assertion results) */
  assertions?: string | null;

  /** Overall result */
  success: boolean;
  error_message?: string | null;

  created_at: string;
}

/**
 * Result of querying MCP calls from SQLite.
 */
export interface TaskRunMcpCallsDbResult {
  task_run_id: string;
  calls: TaskRunMcpCallDb[];
  count: number;
  total_count?: number;
  success_count: number;
  failed_count: number;
  has_more?: boolean;
}

// =============================================================================
// Default Values
// =============================================================================

/**
 * Create a new MCP server configuration with defaults
 */
export function createDefaultMcpServerConfig(partial?: Partial<McpServerConfig>): McpServerConfig {
  const now = new Date().toISOString();
  return {
    id: crypto.randomUUID(),
    name: "New MCP Server",
    transport: "stdio",
    enabled: true,
    autoStart: false,
    timeout_seconds: 30,
    created_at: now,
    updated_at: now,
    ...partial,
  };
}

/**
 * Create default MCP settings
 */
export function createDefaultMcpSettings(): McpSettings {
  return {
    servers: [],
  };
}
