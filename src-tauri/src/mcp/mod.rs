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

pub mod action_plan_cache;
pub mod ai_generation;
pub mod api_surface;
pub mod api_surface_diff;
pub mod ai_session;
pub mod api_requests;
pub mod app_discovery;
pub mod api_spec_verify;
pub mod auto_continue;
pub mod automation_runs;
pub mod awas;
pub mod awas_bridge;
pub mod backend_relay;
pub mod backup_restore;
pub mod bridges;
pub mod canvas;
pub mod cascade;
pub mod container_status;
pub mod checkpoints;
pub mod checks;
pub mod comparison_api;
pub mod configs;
pub mod constraints_api;
pub mod contexts;
pub mod decision_trail_api;
pub mod development_intelligence;
pub mod dom_capture;
pub mod error_monitor;
pub mod extraction;
pub mod findings_api;
pub mod generation_rules_api;
pub mod generator_eval;
pub mod goals;
pub mod graph_api;
pub mod gui_config;
pub mod gui_execution;
pub mod headless_browser;
pub mod hooks;
pub mod image_quality_tests;
pub mod inngest;
pub mod interaction_recording;
pub mod knowledge_acquisition_api;
pub mod log_sources;
pub mod macros;
pub mod mcp_servers;
pub mod memory_consolidation_api;
pub mod meta_optimizer_api;
pub mod misc;
pub mod models;
pub mod monitors;
pub mod observations_api;
pub mod orchestration_loop_api;
pub mod otel_status;
pub mod playwright;
pub mod playwright_collection;
pub mod processes;
pub mod prompt_snippets;
pub mod prompts;
pub mod query_memory_tool;
pub mod query_tool;
pub mod queue;
pub mod rag;
pub mod recordings;
pub mod reflection_api;
pub mod saved_api_requests;
pub mod scheduler;
pub mod sdk_client;
pub mod session_recap;
pub mod server;
pub mod settings;
pub mod shared;
pub mod shell_commands;
pub mod skills;
pub mod state_explorer;
pub mod state_machine;
pub mod step_type_knowledge_api;
pub mod step_type_metadata_api;
pub mod task_run_queries;
pub mod task_run_workflow_state;
pub mod task_runs;
pub mod terminals;
pub mod testing;
pub mod token_analytics;
pub mod trace_verification;
pub mod triggers;
pub mod types;
pub mod ui_bridge;
pub mod ui_bridge_integration;
pub mod unified_workflows;
pub mod verification_tests;
pub mod web_backend_workflows;
pub mod websocket;
pub mod worktrees;
