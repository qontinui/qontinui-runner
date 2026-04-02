// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// Increase recursion limit for large serde_json::json! macros in database export
#![recursion_limit = "256"]
// Allow dead code: many modules are in active development with planned integrations
#![allow(dead_code)]
// Allow complex types: API response types are intentionally detailed
#![allow(clippy::type_complexity)]
// Allow many arguments: refactoring to structs is tracked separately
#![allow(clippy::too_many_arguments)]

mod action_service;
mod ai_pricing;
mod ai_provider;
mod ai_router;
mod ai_workflows;
mod api_request;
mod auth;
mod autoresearch;
mod backup;
mod check_executor;
mod check_generation;
mod claude_protocol;
mod claude_session;
mod click_overlay;
mod commands;
mod comparison;
mod config;
mod config_facade;
mod config_storage;
mod constraint_engine;
mod container;
mod context;
mod cost_management;
mod database;
mod debug_lifecycle;
mod demo_workflows;
mod dev_services;
mod discoveries;
mod display;
mod doctor;
mod dom_capture;
mod error;
mod error_monitor; // Must be declared before error (error re-exports ErrorSeverity from error_monitor)
mod event_system;
mod execution_context;
mod execution_core;
mod executor;
mod exploration;
mod findings;
mod fixer;
mod flow_control;
mod follow_up;
mod graphql;
mod health_monitor;
mod heartbeat;
mod instance_manager;
mod iteration_bundle;
#[cfg(windows)]
mod job_object;
mod knowledge_acquisition;
mod known_issues;
mod log_consolidation;
mod log_migration;
mod logging;
mod macros;
mod mcp;
mod mcp_api;
mod memory;
mod mcp_client;
mod mcp_embedded;
mod meta_optimizer;
mod middleware;
mod online_learning;
mod orchestration_loop;
mod orchestration_loop_configs;
mod orchestrator;
mod otel;
mod paths;
mod playwright;
mod process_capture;
mod process_helpers;
mod prompt_snippets;
mod prompts;
mod rag;
mod recording;
mod reflection;
mod runtime_env;
mod safe_lock;
mod saved_api_requests;
mod schema_registry;
mod scheduler;
mod scheduler_service;
mod secure_storage;
mod security;
mod semantic_conventions;
mod settings;
mod skills;
mod slash_commands;
mod spec_experimentation;
mod spec_utils;
mod state_explorer;
mod state_machine_configs;
mod stats;
mod step_event_builder;
mod step_executor;
mod step_injection;
mod step_metadata;
mod step_registry;
mod step_types;
mod steps;
mod storage;
pub(crate) mod str_utils;
mod summary_generator;
mod task_recorder;
mod terminal;
mod test_executor;
mod test_orchestrator;
mod tiered_info;
mod timeout_config;
mod tracing_layers;
mod trigger_system;
mod ui_bridge_plugin;
mod unified_ai_session;
mod unified_workflow_executor;
mod unified_workflows;
mod validation;
mod video_recorder;
mod workflow_event_bus;
mod workflow_generation;
mod workflow_queue;
mod workflow_state;
mod vision;
mod worktree;
mod zombie_sweep;

use commands::AppState;
use display::profiles::ActionLogProfile;
use display::DisplayProcessor;
use doctor::{start_doctor_async, DoctorConfig};
use error_monitor::{start_error_monitor_async, ErrorMonitorConfig};
use logging::{init_logging, setup_panic_handler, LoggingConfig};
use std::sync::atomic::{AtomicBool, AtomicU16};
use std::sync::{Arc, Mutex};
use storage::LocalStorage;
use tauri::Manager;
use tiered_info::RunRecordingHandler;
use tokio::sync::Mutex as TokioMutex;
use tracing::{error, info, warn};
use video_recorder::VideoRecordingService;

fn main() {
    // Enable backtraces in crash dumps for better diagnostics
    std::env::set_var("RUST_BACKTRACE", "1");

    // Initialize lifecycle debugging BEFORE anything else
    debug_lifecycle::init_lifecycle_debug();

    let result = std::panic::catch_unwind(run_app);

    match result {
        Ok(Ok(())) => {
            info!("Application exited successfully");
            debug_lifecycle::log_exit("Normal exit", 0);
        }
        Ok(Err(e)) => {
            error!("Application error: {}", e);
            debug_lifecycle::log_exit(&format!("Application error: {}", e), 1);
            std::process::exit(1);
        }
        Err(panic) => {
            error!("Application panicked: {:?}", panic);
            debug_lifecycle::log_exit(&format!("Panic in main: {:?}", panic), 2);
            std::process::exit(2);
        }
    }
}

fn run_app() -> Result<(), Box<dyn std::error::Error>> {
    Err("SQLite removed".to_string().into())
}
