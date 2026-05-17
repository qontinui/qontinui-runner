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

pub mod accessibility;
pub mod action_plan_cache;
pub mod adb_helper;
pub mod agent_worktrees;
pub mod ai_generation;
pub mod ai_network_probe;
pub mod ai_session;
pub mod ai_wait_for;
pub mod api_requests;
pub mod api_spec_verify;
pub mod api_surface;
pub mod api_surface_diff;
pub mod app_discovery;
pub mod app_dispatch;
pub mod app_registry;
pub mod auth_callback;
pub mod auto_continue;
pub mod automation_runs;
pub mod awas;
pub mod awas_bridge;
pub mod backend_relay;
pub mod backup_restore;
pub mod blind_spots_api;
pub mod bridges;
pub mod canvas;
pub mod cascade;
pub mod checks;
pub mod command_relay;
pub mod comparison_api;
pub mod completion_reports;
pub mod completion_sources;
pub mod configs;
pub mod constraints_api;
pub mod container_status;
pub mod contexts;
pub mod coordinator;
pub mod debug_builder_prompt;
pub mod decision_trail_api;
pub mod development_intelligence;
pub mod discovery;
pub mod dom_capture;
pub mod entity_profiles_api;
pub mod error_monitor;
pub mod executor;
pub mod extraction;
pub mod file_browser;
pub mod file_registry;
pub mod findings_api;
pub mod generation_rules_api;
pub mod generator_eval;
pub mod git_supervision_api;
pub mod goals;
pub mod graph_api;
pub mod gui_config;
pub mod gui_execution;
pub mod headless_browser;
pub mod hitl;
pub mod hooks;
pub mod image_quality_tests;
pub mod inngest;
pub mod interaction_recording;
pub mod knowledge;
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
pub mod online_learning_api;
pub mod orchestration_loop_api;
pub mod otel_status;
pub mod physical_device;
pub mod physical_device_api;
pub mod plans;
pub mod playwright;
pub mod playwright_collection;
pub mod prm_export;
pub mod processes;
pub mod prompt_home;
pub mod prompt_snippets;
pub mod prompts;
pub mod provider_health;
pub mod query_memory_tool;
pub mod query_tool;
pub mod queue;
pub mod rag;
pub mod recordings;
pub mod reflection;
pub mod reflection_api;
pub mod restate_api;
pub mod reviews;
pub mod saved_api_requests;
pub mod scheduler;
pub mod sdk_client;
pub mod sdk_terminal_buffer;
pub mod security_audit;
pub mod server;
pub mod session_recap;
pub mod sessions;
pub mod settings;
pub mod shared;
pub mod shell_commands;
pub mod skills;
pub mod snapshots;
pub mod state_explorer;
pub mod state_machine;
pub mod step_evaluation_api;
pub mod step_type_knowledge_api;
pub mod step_type_metadata_api;
pub mod streaming;
pub mod task_run_inspection;
pub mod task_run_queries;
pub mod task_run_structured_output;
pub mod task_run_workflow_state;
pub mod task_runs;
pub mod tauri_proxy;
pub mod terminals;
pub mod testing;
// Phase 5.1 of the UI Bridge discoverability/effectiveness plan:
// debug-only `/ui-bridge/test/inject-session` + `/ui-bridge/test/clear-sessions`
// for manual SessionCard-rendering tests. The cfg gate keeps the module
// (and its routes) out of production release builds unless `test-fixtures`
// is explicitly enabled.
#[cfg(any(debug_assertions, feature = "test-fixtures"))]
pub mod test_fixtures;
pub mod token_analytics;
pub mod trace_verification;
pub mod transport;
pub mod triggers;
pub mod tunnel_api;
pub mod types;
pub mod ui_bridge;
pub mod ui_bridge_integration;
pub mod ui_bridge_invoke_handlers;
pub mod unified_workflows;
pub mod verification_tests;
pub mod web_backend_workflows;
pub mod websocket;
pub mod window_manager;
pub mod worktrees;
pub mod ws_bridge_dispatch;
pub mod ws_relay;
