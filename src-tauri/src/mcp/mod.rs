//! MCP API Module
//!
//! This module provides the HTTP API for the qontinui-runner.
//! Currently delegates to mcp_api.rs while refactoring is in progress.
//!
//! The module structure is organized by responsibility:
//! - `types` - Shared types and structs
//! - `goals` - Goal verification logic
//! - `server` - HTTP routing and server initialization (delegates to mcp_api)
//! - `awas` - AWAS (Application Web Automation Specification) handlers
//! - `awas_bridge` - Bridge between AWAS and ui-bridge systems
//!
//! Other modules are defined but not yet fully migrated from mcp_api.rs.

pub mod awas;
pub mod awas_bridge;
pub mod goals;
pub mod server;
pub mod types;
