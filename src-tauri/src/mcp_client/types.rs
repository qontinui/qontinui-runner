//! MCP Client Types
//!
//! The wire-format DTOs (`McpTransport`, `StdioConfig`, `HttpConfig`,
//! `McpServerConfig`, `McpToolInputSchema`, `McpToolInfo`, `McpServerStatus`,
//! `CreateMcpServerInput`, `UpdateMcpServerInput`, `McpToolCallResult`,
//! `CreateMcpCallInput`, `McpCallRecord`, `McpCallsResult`) live in
//! `qontinui_types::mcp_config`; this module re-exports them so the rest of
//! the runner can continue to `use crate::mcp_client::types::*` (or the
//! `pub use types::*;` in `mod.rs`) without change.
//!
//! Runtime state (the `McpClientManager`, `StdioHandle`, `McpConnection`,
//! and JSON-RPC machinery) stays in `crate::mcp_client`. Nothing on the
//! extracted DTOs carried an inherent `impl` before the extraction, so no
//! extension traits are needed here.

pub use qontinui_types::mcp_config::*;
