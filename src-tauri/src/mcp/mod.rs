//! MCP API Module
//!
//! This module provides the HTTP API for the qontinui-runner.
//! Handlers are being incrementally extracted from mcp_api.rs into focused submodules.
//!
//! ## Module structure
//!
//! - `types` - Authoritative ApiState, ApiResponse, and request/response types
//! - `shared` - Cross-cutting utilities (emit_ai_output, FindingContext, etc.)
//! - `goals` - Goal verification logic
//! - `server` - HTTP routing and server initialization (delegates to mcp_api)
//! - `awas` - AWAS (Application Web Automation Specification) handlers
//! - `awas_bridge` - Bridge between AWAS and ui-bridge systems

pub mod ai_generation;
pub mod api_requests;
pub mod app_discovery;
pub mod automation_runs;
pub mod awas;
pub mod awas_bridge;
pub mod checkpoints;
pub mod checks;
pub mod configs;
pub mod contexts;
pub mod dom_capture;
pub mod error_monitor;
pub mod extension;
pub mod extraction;
pub mod findings_api;
pub mod goals;
pub mod hooks;
pub mod interaction_recording;
pub mod log_sources;
pub mod macros;
pub mod mcp_servers;
pub mod misc;
pub mod models;
pub mod monitors;
pub mod playwright;
pub mod prompts;
pub mod rag;
pub mod recordings;
pub mod saved_api_requests;
pub mod scriptlets;
pub mod sdk_client;
pub mod server;
pub mod settings;
pub mod shared;
pub mod shell_commands;
pub mod state_explorer;
pub mod task_runs;
pub mod testing;
pub mod types;
pub mod ui_bridge;
pub mod unified_workflows;
pub mod verification_tests;
pub mod websocket;
