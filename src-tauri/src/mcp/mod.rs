//! MCP API Module
//!
//! This module provides the HTTP API for the qontinui-runner.
//! Currently delegates to mcp_api.rs while refactoring is in progress.
//!
//! The module structure is organized by responsibility:
//! - `types` - Shared types and structs
//! - `goals` - Goal verification logic
//! - `server` - HTTP routing and server initialization (delegates to mcp_api)
//!
//! Other modules are defined but not yet fully migrated from mcp_api.rs.

pub mod goals;
pub mod server;
pub mod types;
