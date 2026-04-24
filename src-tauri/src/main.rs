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
mod agentic_verification;
mod ai_pricing;
mod ai_provider;
mod ai_router;
mod ai_workflows;
pub mod api_config;
mod api_request;
mod auth;
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
mod crash_dumps;
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
mod instance;
mod instance_health;
mod instance_manager;
mod iteration_bundle;
#[cfg(windows)]
mod job_object;
mod knowledge_acquisition;
mod known_issues;
mod log_consolidation;
mod logging;
mod macros;
mod mcp;
mod mcp_api;
mod mcp_client;
mod mcp_embedded;
mod memory;
mod meta_optimizer;
mod middleware;
mod online_learning;
mod orchestration_loop;
mod orchestration_loop_configs;
mod orchestrator;
mod otel;
mod paths;
mod planning_bridge;
mod playwright;
mod process_capture;
mod process_helpers;
mod prompt_snippets;
mod prompts;
mod rag;
mod recording;
mod reflection;
mod restate;
mod rework;
mod routing;
mod runtime_env;
mod safe_lock;
mod saved_api_requests;
mod scheduler;
mod scheduler_service;
mod schema_registry;
mod screen;
mod secure_storage;
mod security;
mod semantic_conventions;
mod server_mode;
mod settings;
mod skills;
mod slash_commands;
mod spec_experimentation;
mod spec_utils;
mod state_discovery;
mod state_explorer;
mod state_machine_configs;
mod stats;
mod step_event_builder;
mod step_executor;
mod step_injection;
mod step_metadata;
mod step_output;
mod step_registry;
mod step_types;
mod steps;
mod storage;
pub(crate) mod str_utils;
mod summary_generator;
mod terminal;
mod test_executor;
mod test_orchestrator;
mod ticket_system;
mod tiered_info;
mod timeout_config;
mod tracing_layers;
mod trigger_system;
mod tunnel;
mod ui_bridge_evaluate;
mod ui_bridge_invoke;
mod ui_bridge_invoke_probe;
mod ui_bridge_plugin;
mod ui_error;
mod unified_ai_session;
mod unified_workflow_executor;
mod unified_workflows;
mod validation;
mod verification;
mod vga;
mod video_recorder;
mod vision;
mod window_manager;
mod workflow;
mod workflow_event_bus;
mod workflow_generation;
mod workflow_queue;
mod workflow_state;
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

    // Per-monitor DPI awareness must be set before any screen capture
    // (Windows: PROCESS_PER_MONITOR_DPI_AWARE_V2). No-op on macOS/Linux.
    screen::ensure_dpi_awareness();

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
    // Read persisted OTel settings so the tracing pipeline uses saved config
    let otel_config = crate::settings::get_otel_settings();
    let logging_result = init_logging(LoggingConfig {
        otel: otel_config,
        ..LoggingConfig::default()
    })?;
    setup_panic_handler();

    // Start health monitoring to detect resource exhaustion
    health_monitor::start_health_monitor();

    // Initialize Windows Job Object so spawned AI processes are auto-killed
    // if the runner crashes (safety net for the explicit taskkill in shutdown).
    #[cfg(windows)]
    job_object::init_job_object();

    info!("Starting Qontinui Runner v{}", env!("CARGO_PKG_VERSION"));

    // Initialize Sentry for crash reporting (release builds only).
    // The guard must live for the entire application lifetime — when it drops, Sentry shuts down.
    #[cfg(not(debug_assertions))]
    let _sentry_guard = std::env::var("SENTRY_DSN").ok().map(|dsn| {
        let guard = sentry::init((
            dsn,
            sentry::ClientOptions {
                release: sentry::release_name!(),
                environment: Some("beta".into()),
                before_send: Some(std::sync::Arc::new(|event| {
                    info!("Sending error to Sentry: {:?}", event);
                    Some(event)
                })),
                ..Default::default()
            },
        ));
        info!("Sentry crash reporting initialized");
        guard
    });

    // Initialize DisplayProcessor with ActionLogProfile
    let mut display_processor = DisplayProcessor::new();
    display_processor.register_profile(ActionLogProfile::with_default_config());
    let display_processor = Arc::new(TokioMutex::new(display_processor));

    // Initialize LocalStorage
    let local_storage = Arc::new(Mutex::new(
        LocalStorage::with_default_config().expect("Failed to initialize local storage"),
    ));

    // Initialize VideoRecordingService
    let video_recorder = Arc::new(Mutex::new(VideoRecordingService::new()));

    // Initialize PostgreSQL connection (required — local docker-compose PG).
    // Uses RUNNER_DATABASE_URL env var, or defaults to the local docker-compose PostgreSQL.
    // Uses a dedicated tokio runtime for the one-shot async connection — cannot use
    // tauri::async_runtime::block_on here because the Tauri runtime hasn't started yet.
    // Initialize PG and run one-time data migration in the same tokio runtime.
    // Critical: PG pool connections are tied to their creating runtime.
    let pg_db: Arc<crate::database::pg::PgDb> = {
        let pg_url = std::env::var("RUNNER_DATABASE_URL").unwrap_or_else(|_| {
            "host=localhost port=5432 user=qontinui_user password=qontinui_dev_password dbname=qontinui_db".to_string()
        });
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime for PG initialization");

        match rt.block_on(crate::database::pg::PgDb::new(&pg_url)) {
            Ok(pg) => {
                info!("PostgreSQL connected successfully");
                let pg = Arc::new(pg);
                crate::database::pg::PgDb::set_global(pg.clone());
                pg
            }
            Err(e) => {
                error!(
                    "PostgreSQL connection failed: {}. Ensure docker-compose PG is running.",
                    e
                );
                panic!("PostgreSQL connection required — {}", e);
            }
        }
    };

    // SQLite span layer, queue recovery, JSON migration, seed rules, and
    // check-group repair removed — all persistence now via PgDb.

    // Initialize online learning singletons (model router bandit + drift monitor)
    {
        let ol_pg = pg_db.clone();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime for online learning init");
        rt.block_on(online_learning::initialize(&ol_pg));
    }

    // Migrate plaintext API keys to secure keychain storage
    if let Err(e) = config_facade::migrate_api_keys_to_keychain() {
        warn!("API key migration to keychain failed (non-fatal): {}", e);
    }

    // Architecture spec caching removed — all persistence now via PgDb.
    // Initialize RAGState (graceful degradation if dependencies missing)
    let rag_state = match commands::rag::RAGState::new() {
        Ok(state) => {
            info!("RAG state initialized successfully");
            Arc::new(state)
        }
        Err(e) => {
            warn!(
                "RAG initialization failed (non-fatal): {}. RAG features will be disabled.",
                e
            );
            // Create a degraded RAGState that will return errors on use
            Arc::new(commands::rag::RAGState::new_degraded())
        }
    };

    // Create broadcast channel for WebSocket event streaming
    // Capacity of 256 allows for burst events without dropping
    let (event_broadcast, _) = tokio::sync::broadcast::channel::<serde_json::Value>(256);

    // Create run recording handler for automatic workflow execution recording
    let run_recording_handler = Arc::new(RunRecordingHandler::new());

    // Create MCP client manager for calling external MCP servers
    let mcp_client_manager = mcp_client::McpClientManager::new();

    // Create instance manager for multi-instance dev workflows
    let instance_manager = Arc::new(instance_manager::InstanceManager::new(pg_db.clone()));

    // Create session manager for interactive Claude CLI sessions
    let session_manager = Arc::new(claude_session::SessionManager::new());

    // Create terminal manager for embedded PTY terminals
    let terminal_manager = Arc::new(terminal::TerminalManager::new());

    // Create shared AppState for both Tauri and MCP API
    // Create shared SDK connection for UI Bridge (shared between AppState and ApiState)
    let shared_sdk_connection = Arc::new(TokioMutex::new(
        crate::mcp::sdk_client::SdkConnectionManager::new(),
    ));

    // Headless window control — `QONTINUI_SERVER_MODE=1` now ONLY governs
    // whether the main window is created and whether Restate is auto-enabled.
    // Web-backend integration (Phase 3G) is driven entirely by the
    // persisted `WebIntegrationSettings` (with env-var overlay support in
    // `settings::load_settings`). A desktop runner can register with web
    // via the Settings UI; a headless deploy can set
    // `QONTINUI_WEB_BACKEND_URL` + `QONTINUI_RUNNER_TOKEN` and get the same
    // behavior without touching settings.
    let server_mode_is_on = std::env::var("QONTINUI_SERVER_MODE")
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false);
    let web_integration_settings = crate::settings::load_settings().web_integration.clone();
    let initial_server_mode_state: Option<crate::server_mode::ServerModeState> =
        crate::server_mode::ServerModeConfig::from_settings(&web_integration_settings).map(|cfg| {
            info!(
                "Web-backend integration enabled (backend={})",
                cfg.web_backend_url
            );
            crate::server_mode::ServerModeState::new(cfg)
        });
    if initial_server_mode_state.is_none()
        && (!web_integration_settings.backend_url.is_empty()
            || !web_integration_settings.runner_token.is_empty())
    {
        warn!(
            "Web-integration partially configured (enabled={}, backend_url_empty={}, runner_token_empty={}) — phase events and heartbeats will NOT be reported until all three are set",
            web_integration_settings.enabled,
            web_integration_settings.backend_url.is_empty(),
            web_integration_settings.runner_token.is_empty(),
        );
    }
    let server_mode_state: Arc<tokio::sync::RwLock<Option<crate::server_mode::ServerModeState>>> =
        Arc::new(tokio::sync::RwLock::new(initial_server_mode_state));

    let shared_app_state = Arc::new(AppState {
        bridge_manager: TokioMutex::new(None), // Initialized in setup() when app_handle is available
        extraction_executor: Mutex::new(None), // Initialized on-demand
        sdk_connection: shared_sdk_connection.clone(),
        exploration_cancel: Arc::new(TokioMutex::new(None)),
        current_config: Mutex::new(None),
        display_processor,
        local_storage,
        video_recorder,
        event_broadcast,
        pg_db,
        run_recording_handler,
        mcp_client_manager: tokio::sync::Mutex::new(mcp_client_manager),
        error_monitor_handle: TokioMutex::new(None), // Initialized in setup()
        doctor_handle: TokioMutex::new(None),        // Initialized in setup()
        url_lock_manager: Arc::new(crate::executor::UrlLockManager::new()),
        file_registry_manager: Arc::new(crate::executor::FileRegistryManager::new()),
        file_lock_manager: Arc::new(crate::executor::FileLockManager::new()),
        ui_bridge_failure_tracker:
            crate::step_executor::handlers::ui_bridge::UiBridgeFailureTracker::new(),
        process_capture_manager: TokioMutex::new(None), // Initialized in setup()
        api_ready: AtomicBool::new(false),              // Set when MCP API server binds
        api_port: AtomicU16::new(crate::mcp::types::get_mcp_api_port()), // Updated when server binds
        ai_pid_tracker: Arc::new(std::sync::Mutex::new(Vec::new())),
        canvas_state: Arc::new(tokio::sync::RwLock::new(
            crate::mcp::canvas::CanvasState::new(),
        )),
        orchestration_loops: std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::orchestration_loop::loop_engine::MultiLoopManager::new(),
        )),
        container_executor: TokioMutex::new(None), // Initialized via container settings when enabled
        run_cost_trackers: TokioMutex::new(std::collections::HashMap::new()),
        working_representation_cache: {
            let cache =
                Arc::new(crate::memory::working_representation::WorkingRepresentationCache::new());
            crate::memory::working_representation::WorkingRepresentationCache::set_global(
                cache.clone(),
            );
            cache
        },
        server_mode: server_mode_state.clone(),
        token_flow: Arc::new(crate::server_mode::TokenFlowStore::new()),
        ui_error: Arc::new(ui_error::UiErrorState::new()),
        crash_dumps: Arc::new(crash_dumps::CrashDumpState::new()),
        usb_transport: Arc::new(tokio::sync::OnceCell::new()),
    });
    let mcp_app_state = shared_app_state.clone();
    let mcp_rag_state = rag_state.clone();
    let heartbeat_app_state = shared_app_state.clone();
    let crash_dump_app_state = shared_app_state.clone();

    // Create error monitor config for later initialization
    let error_monitor_pg = shared_app_state.pg_db.clone();

    // Secondary instances (spawned by InstanceManager) must NOT use single-instance
    // plugin — it would prevent them from starting since they share the same binary.
    let is_secondary_instance = std::env::var("QONTINUI_INSTANCE_NAME").is_ok();

    let mut builder = tauri::Builder::default();

    if !is_secondary_instance {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // When a second instance is launched, focus the existing window
            if let Some(window) =
                app.get_webview_window(qontinui_runner_lib::get_main_window_label())
            {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }));
    }

    // Skip the window-state plugin for secondary instances. The plugin
    // persists `window-state.json` to a path derived from the bundle
    // identifier, which is shared by every spawned instance — so a saved
    // off-screen / minimized geometry from any prior session would be
    // restored for fresh test runners. WebView2 throttles JS execution for
    // off-screen / minimized windows, which is one of the failure modes
    // that made `spawn-test` produce runners whose UI Bridge snapshot
    // hung at "Frontend did not become ready within 10s".
    if !is_secondary_instance {
        builder = builder.plugin(tauri_plugin_window_state::Builder::default().build());
    }

    let app = builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(ui_bridge_plugin::init())
        // Per-module plugins (Phase 1 of commands/ → plugin migration).
        // See commands/<module>.rs's plugin() fn for the handler list. Modules
        // not yet migrated remain in generate_handler! below.
        .plugin(commands::clipboard::plugin())
        .plugin(commands::debug::plugin())
        .plugin(commands::dev_findings::plugin())
        .plugin(commands::file_browser::plugin())
        .plugin(commands::window_manager::plugin())
        .plugin(commands::checks::plugin())
        .plugin(commands::checkpoints::plugin())
        .plugin(commands::comparison::plugin())
        .plugin(commands::container_settings::plugin())
        .plugin(commands::dag_workflows::plugin())
        .plugin(commands::database::plugin())
        .plugin(commands::dataset::plugin())
        .plugin(commands::discoveries::plugin())
        .plugin(commands::event_search::plugin())
        .plugin(commands::findings::plugin())
        .plugin(commands::hooks::plugin())
        .plugin(commands::issues::plugin())
        .plugin(commands::known_issues::plugin())
        .plugin(commands::playwright_settings::plugin())
        .plugin(commands::self_healing_settings::plugin())
        .plugin(commands::execution_variables::plugin())
        .plugin(commands::mobile_settings::plugin())
        .plugin(commands::otel_settings::plugin())
        .plugin(commands::security_settings::plugin())
        .plugin(commands::ai_settings::plugin())
        .plugin(commands::accessibility::plugin())
        .plugin(commands::web_integration::plugin())
        .plugin(commands::activity_timeline::plugin())
        .plugin(commands::agentic_metrics::plugin())
        .plugin(commands::ai_data::plugin())
        .plugin(commands::cost_dashboard::plugin())
        .plugin(commands::learning::plugin())
        .plugin(commands::performance_metrics::plugin())
        .plugin(commands::recap::plugin())
        .plugin(commands::terminal_analysis::plugin())
        .plugin(commands::token_analytics::plugin())
        .plugin(commands::transcript::plugin())
        .plugin(commands::adaptive_learning::plugin())
        .plugin(commands::ai_generation::plugin())
        .plugin(commands::backup::plugin())
        .plugin(commands::checkpoint_browser::plugin())
        .plugin(commands::config::plugin())
        .plugin(commands::context::plugin())
        .plugin(commands::library_sync::plugin())
        .plugin(commands::logging::plugin())
        .plugin(commands::rag::plugin())
        .plugin(commands::ai_session::plugin())
        .plugin(commands::meta_optimizer::plugin())
        .plugin(commands::auth::plugin())
        .plugin(commands::state_machine::plugin())
        .plugin(commands::websocket::plugin())
        .plugin(commands::video::plugin())
        .plugin(commands::interaction::plugin())
        .plugin(commands::storage::plugin())
        .plugin(commands::extraction::plugin())
        .plugin(commands::screenshot::plugin())
        .plugin(commands::screenshots::plugin())
        .plugin(commands::script_emitter::plugin())
        .plugin(commands::verification::plugin())
        .plugin(commands::project_logs::plugin())
        .plugin(commands::global_log_sources::plugin())
        .plugin(commands::execution_reporting::plugin())
        .plugin(commands::workflow_events::plugin())
        .plugin(commands::state_machine_configs::plugin())
        .plugin(commands::spec_drift::plugin())
        .plugin(commands::ui_bridge_baselines::plugin())
        .plugin(commands::state_explorer::plugin())
        .plugin(commands::tiered_info::plugin())
        .plugin(commands::task_sync::plugin())
        .plugin(commands::step_outputs::plugin())
        .plugin(commands::testing::plugin())
        .plugin(commands::shell_commands::plugin())
        .plugin(commands::mcp::plugin())
        .plugin(commands::mobile::plugin())
        .plugin(commands::setup_wizard::plugin())
        .plugin(commands::saved_projects::plugin())
        .plugin(commands::test_orchestrator::plugin())
        .plugin(commands::orchestration_loop_configs::plugin())
        .plugin(commands::scripted_output_settings::plugin())
        .plugin(commands::watchers::plugin())
        .plugin(commands::durable_execution::plugin())
        .plugin(commands::flow::plugin())
        .plugin(commands::ui_bridge::plugin())
        .plugin(commands::terminal::plugin())
        .plugin(commands::instances::plugin())
        .plugin(commands::execution::plugin())
        .plugin(doctor::commands::plugin())
        .plugin(error_monitor::commands::plugin())
        .plugin(process_capture::commands::plugin())
        .plugin(orchestration_loop::commands::plugin())
        .manage(shared_app_state)
        .manage(rag_state)
        .manage(instance_manager) // For multi-instance management (dev feature)
        .manage(session_manager) // For interactive AI session commands
        .manage(terminal_manager) // For embedded PTY terminal sessions
        .manage(tokio::sync::Mutex::new(
            qontinui_runner_lib::accessibility::AccessibilityManager::default(),
        )) // Native cross-platform accessibility API
        .invoke_handler(tauri::generate_handler![
            // Interactive AI session commands (send messages, interrupt, query state)
            // NOTE: ai_session handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: auth handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: clipboard, dev_findings, and file_browser handlers moved to
            // per-module plugins (see .plugin() calls above).
            // Configuration commands
            // NOTE: config handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: dataset handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: execution handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: state_machine handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: debug handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: websocket, video, interaction, storage handlers moved to per-module plugins (see .plugin() calls above).
            // NOTE: extraction handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: screenshot handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: script_emitter handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: logging handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: verification handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: rag handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: project_logs handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: global_log_sources handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: issues handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: execution_reporting, workflow_events handlers moved to per-module plugins (see .plugin() calls above).
            // NOTE: dag_workflows, checkpoints, findings, known_issues handlers moved to
            // per-module plugins (see .plugin() calls above).
            // NOTE: state_machine_configs, spec_drift, ui_bridge_baselines, state_explorer, tiered_info handlers moved to per-module plugins (see .plugin() calls above).
            // NOTE: discoveries handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: context handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: ai_data handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: recap handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: task_sync handlers moved to per-module plugin (see .plugin() calls above).
            // Library Sync commands (sync library items to qontinui-web)
            // NOTE: library_sync handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: testing, step_outputs handlers moved to per-module plugins (see .plugin() calls above).
            // NOTE: checks handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: screenshots handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: hooks handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: learning handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: adaptive_learning handlers moved to per-module plugin (see .plugin() calls above).
            // Agentic metric commands
            // NOTE: agentic_metrics handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: flow handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: checkpoint_browser handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: durable_execution handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: database handlers moved to per-module plugin (see .plugin() calls above).
            // Comprehensive backup commands
            // NOTE: backup handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: shell_commands handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: ai_generation handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: mcp, mobile handlers moved to per-module plugins (see .plugin() calls above).
            // NOTE: global_log_sources handlers consolidated above.
            // NOTE: setup_wizard, saved_projects, test_orchestrator handlers moved to per-module plugins (see .plugin() calls above).
            // Performance Metrics Dashboard commands
            // NOTE: performance_metrics handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: ui_bridge handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: error_monitor handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: doctor handlers moved to per-module plugin (see .plugin() calls above).
            // Cloud relay commands (remote mobile access via backend WebSocket)
            mcp::backend_relay::commands::start_cloud_relay,
            mcp::backend_relay::commands::stop_cloud_relay,
            mcp::backend_relay::commands::get_cloud_relay_status,
            mcp::backend_relay::commands::save_cloud_relay_settings,
            mcp::backend_relay::commands::get_cloud_relay_settings,
            // NOTE: process_capture handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: orchestration_loop handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: orchestration_loop_configs handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: terminal, instances handlers moved to per-module plugins (see .plugin() calls above).
            // Claude Code transcript import commands
            // NOTE: transcript handlers moved to per-module plugin (see .plugin() calls above).
            // Terminal session analysis commands
            // NOTE: terminal_analysis handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: meta_optimizer handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: comparison handlers moved to per-module plugin (see .plugin() calls above).
            // Spec experimentation commands
            spec_experimentation::commands::get_spec_compliance_history,
            spec_experimentation::commands::get_spec_compliance_summary,
            spec_experimentation::commands::extract_spec_compliance,
            spec_experimentation::commands::analyze_spec_element_coverage,
            spec_experimentation::commands::analyze_cross_page_consistency,
            spec_experimentation::commands::run_spec_mutation_test,
            spec_experimentation::commands::analyze_spec_freshness,
            spec_experimentation::commands::get_spec_accuracy_results,
            // Spec attention & broken assertion commands
            spec_experimentation::commands::detect_broken_spec_assertions,
            spec_experimentation::commands::get_specs_needing_attention,
            // Spec versioning commands
            spec_experimentation::commands::snapshot_current_spec,
            spec_experimentation::commands::get_spec_version_history,
            spec_experimentation::commands::diff_spec_versions,
            spec_experimentation::commands::diff_spec_json,
            // Token analytics commands (LLM cost and usage tracking)
            // NOTE: token_analytics handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: activity_timeline handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: scripted_output_settings handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: event_search handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: watchers handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: container_settings handlers moved to per-module plugin (see .plugin() calls above).
            // Cost dashboard commands (unified token/cache/budget overview)
            // NOTE: cost_dashboard handlers moved to per-module plugin (see .plugin() calls above).
            // NOTE: window_manager handlers moved to per-module plugin (see .plugin() calls above).
            // Runner UI error reporting (Phase 3J.1/3J.2)
            ui_error::report_ui_error,
            ui_error::clear_ui_error,
            ui_error::get_ui_error,
            // Startup crash-dump surface (post-3J follow-up): ack a panic
            // the previous process aborted on, clearing /health.derived_status.
            crash_dumps::dismiss_recent_crash,
        ])
        .setup(|app| {
            info!("Tauri application setup starting");

            let server_mode = std::env::var("QONTINUI_SERVER_MODE")
                .map(|v| v == "1" || v.to_lowercase() == "true")
                .unwrap_or(false);
            if server_mode {
                info!("QONTINUI_SERVER_MODE is set - running as headless server (no window, Restate forced on)");
            }

            // ── Programmatic window creation ───────────────────────────
            //
            // The declarative `windows[]` in tauri.conf.json is empty.
            // We create the main window here so that:
            //   - Secondary instances (test runners) get an isolated
            //     WebView2 user-data folder via `.data_directory()`,
            //     preventing profile-lock contention with the primary's
            //     `%LOCALAPPDATA%\com.qontinui.runner\EBWebView`.
            //   - The primary instance gets the exact same config it had
            //     declaratively (maximized, 1400×800, min 1200×700).
            //
            // This replaces the old "create declarative window then
            // destroy-and-recreate for secondaries" approach, which
            // failed because Tauri's declarative window was already
            // holding the default profile lock before .setup() ran.
            {
                use tauri::Manager;

                let data_dir = std::env::var("WEBVIEW2_USER_DATA_FOLDER").ok();
                let is_secondary = std::env::var("QONTINUI_INSTANCE_NAME").is_ok();

                if let Some(ref dir) = data_dir {
                    let _ = std::fs::create_dir_all(dir);
                    info!(
                        "Creating window with isolated WebView2 profile (WEBVIEW2_USER_DATA_FOLDER={})",
                        dir
                    );
                }

                // Always create the window programmatically. tauri.conf.json
                // has "windows": [] so there's no declarative window.
                // This lets us set data_directory() for test runners
                // (isolated WebView2 profiles) using the same code path.
                if server_mode {
                    info!("Skipping main window creation (server mode)");
                    // If Tauri's Win32 event loop exits immediately without any window, fall back
                    // to a hidden 0x0 tool window here. See plan Phase 1.5 validation.
                } else {
                    let url = tauri::WebviewUrl::App("index.html".into());
                    let mut builder = tauri::WebviewWindowBuilder::new(app, "main", url)
                        .title("Qontinui Runner")
                        .inner_size(1400.0, 800.0)
                        .min_inner_size(1200.0, 700.0)
                        .fullscreen(false)
                        .resizable(true)
                        .decorations(true);

                    if let Some(ref dir) = data_dir {
                        builder = builder.data_directory(std::path::PathBuf::from(dir));
                    }

                    if is_secondary {
                        builder = builder.position(100.0, 100.0);
                    } else {
                        builder = builder.maximized(true);
                    }

                    match builder.build() {
                        Ok(win) => {
                            let _ = win.show();
                            let _ = win.set_focus();
                            info!(
                                "Main window created (secondary={}, isolated={})",
                                is_secondary, data_dir.is_some()
                            );
                        }
                        Err(e) => {
                            error!("Failed to create main window: {}", e);
                            return Err(Box::new(e));
                        }
                    }
                }
            }

            // SQLite→PG data migration removed (migration complete, PG is primary)

            // Start MCP API server in background BEFORE any synchronous bridge/seed
            // work below. Spawning early lets /health bind and respond during the
            // WebView2 cold-profile init that follows, preventing supervisor health
            // probes from timing out on temp runners. The server only needs the
            // captures made above (mcp_app_state, mcp_rag_state, api_port,
            // mcp_instance_manager), all of which are populated earlier in main().
            let api_port = crate::mcp::types::get_mcp_api_port();
            info!("Starting MCP API server on port {}", api_port);
            let mcp_app_handle = app.handle().clone();
            let mcp_instance_manager = app.state::<Arc<instance_manager::InstanceManager>>().inner().clone();
            tauri::async_runtime::spawn(async move {
                info!("MCP API server task starting...");
                match mcp_api::start_server(mcp_app_state, mcp_rag_state, mcp_app_handle, api_port, mcp_instance_manager).await {
                    Ok(_) => info!("MCP API server stopped normally"),
                    Err(e) => error!("MCP API server error: {}", e),
                }
            });

            // Scan for crash dumps written by the previous process. Run in
            // parallel with the MCP server spawn above — the scan is a
            // read-only disk walk + ~1 file parse and completes in ms, so we
            // don't bother awaiting it before accepting /health requests.
            // Worst case: a request that lands in the first few ms after
            // startup sees `recent_crash: null` and then the next one flips
            // to the populated value.
            tauri::async_runtime::spawn(async move {
                let dir = crate::logging::get_crash_dump_dir();
                crash_dump_app_state.crash_dumps.scan_on_startup(&dir).await;
            });

            // Clear runner log files from previous session
            executor::FileLogger::clear_logs();
            dom_capture::DomCaptureLogger::clear_captures();
            // Clear logging.rs-managed files that FileLogger doesn't cover
            let dev_logs = crate::paths::get_dev_logs_dir();
            for filename in ["runner-render.jsonl", "ai-output.jsonl"] {
                let path = dev_logs.join(filename);
                if path.exists() {
                    if let Err(e) = std::fs::remove_file(&path) {
                        warn!("Failed to clear {}: {}", filename, e);
                    }
                }
            }
            info!("Cleared previous runner log files");

            // Seed default log sources if none configured
            settings::seed_default_log_sources_if_empty();

            // Seed demo workflows on first launch (if no demo workflows exist)
            {
                let seed_pg = app
                    .state::<Arc<crate::commands::AppState>>()
                    .pg_db
                    .clone();
                demo_workflows::seed_demo_workflows_if_needed(&seed_pg);

                // Slash command sync and built-in issue pattern seeding removed — all persistence now via PgDb.
            }

            // Window starts maximized via programmatic builder above

            // Initialize bridge manager for multi-bridge support
            // This replaces the legacy single python_bridge with a manager that can handle
            // multiple concurrent bridges (GUI + headless modes)
            info!("Initializing bridge manager");
            let app_state = app.state::<Arc<AppState>>();
            let bridge_manager = Arc::new(executor::BridgeManager::new(app.handle().clone()));

            // Check for headless-only mode from environment variable
            // This is intended for server deployments where GUI is not available
            let headless_only_env = std::env::var("QONTINUI_HEADLESS_ONLY")
                .map(|v| v == "1" || v.to_lowercase() == "true")
                .unwrap_or(false);
            let headless_only = server_mode || headless_only_env;

            if headless_only {
                info!(
                    "Headless-only mode enabled (server_mode={}, QONTINUI_HEADLESS_ONLY={})",
                    server_mode, headless_only_env
                );
                bridge_manager.set_headless_only(true);
            }

            {
                let app_state_for_bridge = app.state::<Arc<AppState>>().inner().clone();
                tauri::async_runtime::block_on(async {
                    let mut guard = app_state_for_bridge.bridge_manager.lock().await;
                    *guard = Some(bridge_manager.clone());
                });
            }
            info!("Bridge manager initialized (headless_only={})", headless_only);

            // Auto-start Python executor for screenshot capture and other features
            // In headless-only mode, creates a headless bridge instead of GUI bridge
            // In normal mode, creates the default GUI bridge via bridge manager
            if headless_only {
                info!("Headless-only mode: skipping default bridge creation (create bridges on-demand via API)");
            } else {
                info!("Auto-starting Python executor via bridge manager");
                tauri::async_runtime::block_on(async {
                    match bridge_manager.get_or_create_default_bridge().await {
                        Ok(bridge_id) => {
                            info!("Python executor auto-started successfully (bridge_id={})", bridge_id);
                        }
                        Err(e) => {
                            error!("Failed to auto-start Python executor: {}", e);
                            error!("Screenshot capture and other features will not work until the executor is started");
                            // Don't fail app startup, just log the error
                        }
                    }
                });
            }

            // Initialize extraction executor (starts on-demand when first extraction is requested)
            info!("Initializing extraction executor (will start on-demand)");
            let mut extraction_lock = crate::safe_lock::safe_lock_or_recover(&app_state.extraction_executor, "extraction_executor");
            let extraction_executor = executor::ExtractionExecutor::new(app.handle().clone());
            *extraction_lock = Some(extraction_executor);
            drop(extraction_lock);

            // MCP API server already spawned earlier (before bridge/seed work) so
            // that /health binds during WebView2 cold-profile init.

            // Start heartbeat background task for fleet registration
            heartbeat::start_heartbeat(heartbeat_app_state);

            // Start web-backend registration + heartbeat loop when
            // WebIntegrationSettings are configured. Runs independently of
            // QONTINUI_SERVER_MODE — any runner (desktop, secondary, or
            // headless) that has web integration enabled participates.
            {
                let startup_app_state = app.state::<Arc<AppState>>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    let sm_state_opt = startup_app_state.current_server_mode().await;
                    if let Some(sm_state) = sm_state_opt {
                        let restate_enabled = crate::settings::load_settings().restate.enabled;
                        let ui_error_state = startup_app_state.ui_error.clone();
                        let crash_dump_state = startup_app_state.crash_dumps.clone();
                        crate::server_mode::spawn_background_tasks(
                            sm_state,
                            restate_enabled,
                            ui_error_state,
                            crash_dump_state,
                        );
                    }
                });
            }

            // Register this runner in the PostgreSQL instance registry.
            // Primary registers itself; secondaries register via heartbeat to the primary.
            if !instance::is_secondary() {
                let im = app.state::<Arc<instance_manager::InstanceManager>>().inner().clone();
                let startup_pg = app.state::<Arc<commands::AppState>>().pg_db.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    im.register_self_as_primary().await;
                    // Clean up stale entries from previous sessions
                    instance_health::startup_cleanup(&startup_pg).await;
                });

                // Start background health monitor for secondary instances
                let health_pg = app.state::<Arc<commands::AppState>>().pg_db.clone();
                let health_im = app.state::<Arc<instance_manager::InstanceManager>>().inner().clone();
                instance_health::start_instance_health_monitor(health_pg, health_im);
            }

            // Start scheduler service in background (skip for secondary instances to avoid duplicate executions)
            if std::env::var("QONTINUI_INSTANCE_NAME").is_ok() {
                info!("Secondary instance — skipping scheduler service");
            } else {
                info!("Starting scheduler service");
                let scheduler_app_state: Arc<commands::AppState> =
                    app.state::<Arc<commands::AppState>>().inner().clone();
                let scheduler_pg = scheduler_app_state.pg_db.clone();
                tauri::async_runtime::spawn(async move {
                    // Wait briefly for MCP API server to bind
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    scheduler_service::start_scheduler_service(scheduler_pg, scheduler_app_state)
                        .await;
                });
            }

            // Auto-load the default state machine into the Python bridge so
            // that `POST /state-machine/navigate` and other bridge-dispatched
            // endpoints work immediately after startup without requiring a
            // manual click on the State Machine page's "Load into Runtime"
            // button. Best-effort — never blocks startup, never panics.
            {
                let sm_app_state: Arc<commands::AppState> =
                    app.state::<Arc<commands::AppState>>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    crate::mcp::state_machine::auto_load_default_state_machine(&sm_app_state).await;
                });
            }

            // Start the state-discovery drift detector (Step 6). Scores recent
            // co-occurrence observations against the currently loaded compiled
            // SM every 10 min, persists to `state_discovery_drift_scores`, and
            // logs a WARN when fit_score has been below 0.9 for 3 windows in a
            // row. Fire-and-forget: a transient failure inside the loop is
            // logged and the next tick runs normally; process death kills the
            // task cleanly.
            {
                let drift_app_state: Arc<commands::AppState> =
                    app.state::<Arc<commands::AppState>>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    crate::state_discovery::run_drift_detector(drift_app_state).await;
                });
            }

            // Start error monitor service in background
            info!("Starting error monitor service");
            let error_monitor_config = ErrorMonitorConfig::default();

            // Start the service and store the handle in AppState (all within async context)
            let app_state_for_error_monitor = app.state::<Arc<AppState>>().inner().clone();
            tauri::async_runtime::spawn(async move {
                // Start the error monitor service (this spawns the service loop internally)
                let error_monitor_handle =
                    start_error_monitor_async(error_monitor_pg, error_monitor_config).await;

                // Store the handle
                let mut handle_lock = app_state_for_error_monitor.error_monitor_handle.lock().await;
                *handle_lock = Some(error_monitor_handle);
                info!("Error monitor service started and handle stored in AppState");
            });

            if server_mode {
                // Server mode forces Restate on; persist so the block below picks it up.
                let mut s = crate::settings::load_settings();
                if !s.restate.enabled {
                    s.restate.enabled = true;
                    if let Err(e) = crate::settings::save_settings(&s) {
                        warn!("Failed to persist restate.enabled=true for server mode: {}", e);
                    } else {
                        info!("Server mode: forced restate.enabled=true");
                    }
                }
            }

            // Initialize process capture manager
            info!("Initializing process capture manager");
            let app_state_for_pcm = app.state::<Arc<AppState>>().inner().clone();
            let app_handle_for_pcm = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Wait for error monitor handle to be ready
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                let error_monitor = {
                    let handle_lock = app_state_for_pcm.error_monitor_handle.lock().await;
                    handle_lock.clone()
                };
                let error_monitor_arc =
                    Arc::new(tokio::sync::RwLock::new(error_monitor));

                let manager = Arc::new(process_capture::ProcessCaptureManager::new(
                    error_monitor_arc,
                    app_handle_for_pcm,
                ));

                // Load configs from settings and register them
                let mut configs = settings::get_managed_process_configs();

                // Backfill build commands for configs that don't have them yet
                // (ProcessConfigExt trait provides backfill_build_command after
                // the DTO moved to qontinui_types::process_management).
                use crate::process_capture::types::ProcessConfigExt;
                for config in &mut configs {
                    config.backfill_build_command();
                }

                // In dev-mode, upgrade legacy configs, dedupe dev-service ports,
                // and inject missing dev services. Persist if anything changed so
                // the cleanup survives future boots (otherwise duplicates can
                // re-accumulate in settings.json across sessions).
                if dev_services::is_dev_mode() {
                    if let Some(workspace) = dev_services::find_workspace_root() {
                        let mutated =
                            dev_services::upgrade_and_dedupe_configs(&mut configs, &workspace);

                        let missing = dev_services::get_missing_dev_services(&workspace, &configs);
                        let injected = !missing.is_empty();
                        if injected {
                            info!(
                                "Dev-mode: injecting {} default dev services for workspace {}",
                                missing.len(),
                                workspace.display()
                            );
                            for svc in &missing {
                                info!("  -> {} (group {}, port {:?})", svc.name, svc.start_group, svc.health_port);
                            }
                            configs.extend(missing);
                        }

                        if mutated || injected {
                            if let Err(e) =
                                settings::replace_managed_process_configs(configs.clone())
                            {
                                warn!(
                                    "Failed to persist dev-service config cleanup: {}. \
                                     Runtime state is still correct but duplicates may \
                                     re-appear on next boot.",
                                    e
                                );
                            } else {
                                info!("Dev-mode: persisted upgraded/deduped/injected configs to settings");
                            }
                        }
                    }
                }

                // Inject Restate server if durable execution is enabled
                let restate_settings = settings::load_settings().restate.clone();
                if restate_settings.enabled {
                    if restate_settings.is_external() {
                        info!(
                            "Restate durable execution enabled — using external server at {} / {}; skipping local spawn",
                            restate_settings.admin_url(),
                            restate_settings.ingress_url()
                        );
                    } else {
                        info!("Restate durable execution enabled — preparing server");
                        let app_data_dir = dirs::data_dir()
                            .unwrap_or_else(|| std::path::PathBuf::from("."))
                            .join("qontinui-runner");
                        match restate::lifecycle::ensure_restate_binary(
                            &restate_settings,
                            &app_data_dir,
                        )
                        .await
                        {
                            Ok(binary_path) => {
                                let restate_config =
                                    restate::lifecycle::build_restate_process_config(
                                        &restate_settings,
                                        &binary_path,
                                        &app_data_dir,
                                    );
                                info!(
                                    "Restate server config: binary={}, health_port={:?}, group={}",
                                    restate_config.command,
                                    restate_config.health_port,
                                    restate_config.start_group
                                );
                                configs.push(restate_config);
                            }
                            Err(e) => {
                                error!("Failed to prepare Restate binary: {} — workflows will use legacy execution", e);
                            }
                        }
                    }
                }

                // Filter out dev_only services in production
                let is_dev = dev_services::is_dev_mode();
                for config in configs {
                    if config.enabled && (is_dev || !config.dev_only) {
                        manager.register(config).await;
                    }
                }

                // Auto-start processes (skip for secondary instances to avoid port conflicts)
                let is_secondary = std::env::var("QONTINUI_INSTANCE_NAME").is_ok();
                if is_secondary {
                    info!("Secondary instance — skipping managed process auto-start");
                } else {
                    manager.start_auto_processes().await;
                }

                // Store the manager
                let mut manager_lock = app_state_for_pcm.process_capture_manager.lock().await;
                *manager_lock = Some(manager.clone());
                info!("Process capture manager initialized");

                // After processes are started, start Restate HTTP endpoint and register with server
                if restate_settings.enabled {
                    // Start the Restate service HTTP endpoint (where Restate calls back into)
                    #[cfg(feature = "restate")]
                    {
                        let endpoint_port = restate_settings.service_endpoint_port;
                        let restate_app_state = app_state_for_pcm.clone();
                        let restate_config_storage = match crate::config_storage::ConfigStorage::new() {
                            Ok(cs) => Arc::new(tokio::sync::Mutex::new(cs)),
                            Err(e) => {
                                warn!("ConfigStorage init for Restate failed, using degraded: {}", e);
                                Arc::new(tokio::sync::Mutex::new(
                                    crate::config_storage::ConfigStorage::new_degraded()
                                ))
                            }
                        };
                        tokio::spawn(async move {
                            if let Err(e) = restate::http_endpoint::start_restate_endpoint(
                                endpoint_port,
                                restate_app_state,
                                restate_config_storage,
                            ).await {
                                error!("Restate HTTP endpoint failed: {}", e);
                            }
                        });
                    }

                    // Register our endpoint with the Restate server once both are ready
                    let rs = restate_settings.clone();
                    tokio::spawn(async move {
                        // When using an external Restate server we cannot probe a
                        // local port for it — trust that it's up and skip the wait.
                        let admin_ready = if rs.is_external() {
                            true
                        } else {
                            crate::process_capture::health::wait_for_port_ready(
                                rs.admin_port,
                                std::time::Duration::from_secs(60),
                            )
                            .await
                        };

                        if admin_ready {
                            // Also wait for our service endpoint to be ready
                            if crate::process_capture::health::wait_for_port_ready(
                                rs.service_endpoint_port,
                                std::time::Duration::from_secs(30),
                            )
                            .await
                            {
                                // Register the runner's service endpoint with Restate
                                if let Err(e) = restate::lifecycle::register_service_endpoint(
                                    &rs,
                                    rs.service_endpoint_port,
                                )
                                .await
                                {
                                    error!("Failed to register Restate service endpoint: {}", e);
                                }
                            } else {
                                error!("Restate service endpoint port {} not ready after 30s", rs.service_endpoint_port);
                            }
                        } else {
                            error!("Restate admin port {} not ready after 60s", rs.admin_port);
                        }
                    });

                    // Start health watchdog (no-op when external)
                    restate::lifecycle::spawn_restate_watchdog(
                        manager.clone(),
                        restate_settings.clone(),
                    );
                }
            });

            // Start Doctor health monitoring service in background
            info!("Starting Doctor health monitoring service");
            let app_state_for_doctor = app.state::<Arc<AppState>>().inner().clone();
            let app_handle_for_doctor = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let doctor_config = DoctorConfig::default();
                let (doctor_handle, mut doctor_event_rx) =
                    start_doctor_async(doctor_config).await;

                // Store the handle in AppState
                let mut handle_lock = app_state_for_doctor.doctor_handle.lock().await;
                *handle_lock = Some(doctor_handle);
                info!("Doctor service started and handle stored in AppState");

                // Bridge Doctor events to Tauri frontend events
                drop(handle_lock); // Release lock before entering event loop
                while let Some(event) = doctor_event_rx.recv().await {
                    let event_name = match &event {
                        doctor::DoctorEvent::Warning { .. } => "doctor-warning",
                        doctor::DoctorEvent::Stuck { .. } => "doctor-stuck",
                        doctor::DoctorEvent::Healthy { .. } => "doctor-healthy",
                    };
                    if let Err(e) = tauri::Emitter::emit(&app_handle_for_doctor, event_name, &event) {
                        tracing::warn!("Failed to emit Doctor event: {}", e);
                    }
                }
            });

            // Embedding backfill job removed (SQLite embeddings deprecated, PG handles this)

            // Start memory consolidation scheduler in background
            info!("Starting memory consolidation scheduler");
            let scheduler_pg = app.state::<Arc<AppState>>().inner().pg_db.clone();
            memory::scheduler::start_memory_scheduler(
                scheduler_pg,
                memory::scheduler::MemorySchedulerConfig::default(),
            );

            // Start dreamer (formal reasoning) scheduler in background
            info!("Starting dreamer scheduler");
            let dreamer_pg = app.state::<Arc<AppState>>().inner().pg_db.clone();
            memory::scheduler::start_dreamer_scheduler(
                dreamer_pg,
                memory::scheduler::MemorySchedulerConfig::default(),
            );

            // Auto-start MCP servers marked with auto_start in background
            let app_state_for_mcp_auto = app.state::<Arc<AppState>>().inner().clone();
            tauri::async_runtime::spawn(async move {
                let mcp_manager = app_state_for_mcp_auto.mcp_client_manager.lock().await;
                mcp_manager.start_auto_start_servers().await;
            });

            // Cloud relay auto-start is handled in mcp_api.rs where ApiState is available

            // Forward circuit breaker state-change events to the frontend
            let app_handle_cb = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut rx = crate::ai_provider::circuit_breaker::event_receiver();
                loop {
                    match rx.recv().await {
                        Ok(event) => {
                            let _ = tauri::Emitter::emit(&app_handle_cb, "circuit-breaker-change", &event);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Circuit breaker event listener lagged by {} events", n);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            // Check Claude CLI auth status on startup
            let app_handle_for_auth = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Small delay to let the frontend initialize
                tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

                let status = commands::ai_settings::get_cli_auth_status();
                if status.is_cli_provider && status.expired {
                    tracing::warn!(
                        "Claude CLI OAuth token is expired (expired {} minutes ago)",
                        status.minutes_until_expiry.unwrap_or(0).abs()
                    );
                    if let Err(e) =
                        tauri::Emitter::emit(&app_handle_for_auth, "cli-auth-expired", &status)
                    {
                        tracing::warn!("Failed to emit cli-auth-expired event: {}", e);
                    }
                } else if status.is_cli_provider && status.has_credentials {
                    info!(
                        "Claude CLI auth valid ({} minutes remaining)",
                        status.minutes_until_expiry.unwrap_or(0)
                    );
                }

                // If least-usage mode is enabled, resolve the best account
                let ai_settings = settings::get_ai_settings();
                if matches!(
                    ai_settings.claude_cli.account_selection_mode,
                    settings::AccountSelectionMode::LeastUsage
                ) {
                    info!("Least-usage account selection enabled, resolving best account...");
                    if let Some(dir) =
                        commands::ai_settings::resolve_active_config_dir().await
                    {
                        info!("Resolved least-usage account: {}", dir);
                        ai_provider::set_resolved_config_dir(Some(dir));
                    }
                }
            });

            // Bootstrap World State Verifier live config from persisted
            // settings. Must run before any agentic verification iteration
            // so the loop picks up the persisted mode/endpoint/model.
            // Falls back to env vars when no persisted settings exist.
            verification::WsvConfig::init_from_persisted();

            // Restore previously-running instances (primary instance only).
            // The session file only exists if the previous process was killed
            // (e.g. by a rebuild) rather than closed intentionally by the user.
            if std::env::var("QONTINUI_INSTANCE_NAME").is_err() {
                let restore_ids = instance_manager::load_and_clear_active_instances();
                if !restore_ids.is_empty() {
                    info!(
                        "Restoring {} previously-active instance(s): {:?}",
                        restore_ids.len(),
                        restore_ids
                    );
                    let im = app.state::<Arc<instance_manager::InstanceManager>>().inner().clone();
                    let app_handle_for_restore = app.handle().clone();
                    tauri::async_runtime::spawn(async move {
                        // Brief delay to let the primary instance finish initializing
                        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                        let configs = settings::get_runner_instances();
                        let mut restored = 0u32;
                        for id in &restore_ids {
                            let Some(config) = configs.iter().find(|c| &c.id == id) else {
                                tracing::warn!(
                                    "Instance '{}' was active but no longer in settings — skipping",
                                    id
                                );
                                continue;
                            };

                            // Wait for the port to become free (previous process may still be dying)
                            if crate::process_capture::health::is_port_in_use(config.port) {
                                info!(
                                    "Port {} still in use, waiting up to 5 s for instance '{}'",
                                    config.port, config.name
                                );
                                let free = tokio::task::spawn_blocking({
                                    let port = config.port;
                                    move || instance_manager::wait_for_port_free(
                                        port,
                                        std::time::Duration::from_secs(5),
                                    )
                                })
                                .await
                                .unwrap_or(false);
                                if !free {
                                    error!(
                                        "Port {} still occupied — skipping restore of instance '{}'",
                                        config.port, config.name
                                    );
                                    continue;
                                }
                            }

                            match im.launch_instance(config).await {
                                Ok(pid) => {
                                    info!(
                                        "Restored instance '{}' (PID: {}, port: {})",
                                        config.name, pid, config.port
                                    );
                                    restored += 1;
                                }
                                Err(e) => {
                                    error!("Failed to restore instance '{}': {}", config.name, e);
                                }
                            }
                        }

                        // Notify the frontend so the instances panel refreshes immediately
                        if restored > 0 {
                            let _ = tauri::Emitter::emit(
                                &app_handle_for_restore,
                                "runner-instances-restored",
                                &serde_json::json!({ "count": restored }),
                            );
                        }
                    });
                }
            }

            // Clean up old security audit events based on retention policy
            {
                let security_settings = crate::settings::get_security_settings();
                if security_settings.audit_enabled {
                    let pg_for_audit = app.state::<Arc<commands::AppState>>().pg_db.clone();
                    let retention_days = security_settings.audit_retention_days;
                    tauri::async_runtime::spawn(async move {
                        match pg_for_audit.cleanup_old_audit_events(retention_days).await {
                            Ok(deleted) if deleted > 0 => {
                                info!("Audit cleanup: deleted {} events older than {} days", deleted, retention_days);
                            }
                            Ok(_) => {}
                            Err(e) => {
                                warn!("Audit cleanup failed: {}", e);
                            }
                        }
                    });
                }
            }

            // Spawn the process log cleanup loop + orphaned-session cleanup.
            {
                let pg_for_logs = app.state::<Arc<commands::AppState>>().pg_db.clone();
                let pg_for_orphans = pg_for_logs.clone();

                // Capture the cutoff BEFORE spawning the cleanup task. Only
                // sessions started before this timestamp are considered
                // orphans, which protects new sessions created by the current
                // runner's event_loop from being clobbered.
                let orphan_cutoff = chrono::Utc::now().to_rfc3339();

                // Mark orphaned 'running' sessions as 'failed' on startup.
                // These can only exist if a previous runner died unexpectedly
                // (the event_loop normally updates state on natural exit).
                tauri::async_runtime::spawn(async move {
                    match pg_for_orphans
                        .mark_orphaned_running_sessions_as_failed(&orphan_cutoff)
                        .await
                    {
                        Ok(n) if n > 0 => {
                            info!(
                                "Marked {} orphaned 'running' process sessions as 'failed'",
                                n
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            warn!("Failed to clean up orphaned process sessions: {}", e);
                        }
                    }
                });

                tauri::async_runtime::spawn(async move {
                    process_capture::cleanup::run_process_log_cleanup_loop(pg_for_logs).await;
                });
            }

            info!("Tauri application setup complete");
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                info!("Window close requested");
                let app_state = window.state::<Arc<AppState>>();

                // Intentional close — clear the session file so instances are NOT
                // restored on the next normal startup.  (If the process is killed
                // by a rebuild, this handler doesn't run and the file persists,
                // which is exactly what we want.)
                instance_manager::clear_active_instances();

                // Deregister this instance from the runner_instances table.
                // For the primary: mark stopped so secondaries know it's gone.
                // For secondaries: remove the row entirely and notify the primary.
                {
                    let pg = app_state.pg_db.clone();
                    let own_port = app_state
                        .api_port
                        .load(std::sync::atomic::Ordering::Relaxed);
                    let is_secondary = instance::is_secondary();
                    let primary_port = instance::primary_port();
                    let id = format!(
                        "{}-{}",
                        if is_secondary { "ext" } else { "primary" },
                        own_port
                    );

                    // Best-effort: spawn a quick runtime to clean up DB + HTTP
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build();
                        if let Ok(rt) = rt {
                            rt.block_on(async {
                                if is_secondary {
                                    // Remove from DB
                                    let _ = pg.remove_runner_instance(&id).await;
                                    // Also try removing by port (covers different ID formats)
                                    let _ = pg
                                        .cleanup_dead_runner_instances(0)
                                        .await;
                                    info!(
                                        "Deregistered secondary instance (port={}) from DB",
                                        own_port
                                    );

                                    // Notify primary (best-effort)
                                    if let Some(pp) = primary_port {
                                        let client = reqwest::Client::builder()
                                            .timeout(std::time::Duration::from_secs(2))
                                            .build()
                                            .ok();
                                        if let Some(client) = client {
                                            let url = format!(
                                                "http://127.0.0.1:{}/instances/{}/stop",
                                                pp, id
                                            );
                                            let _ = client.post(&url).send().await;
                                        }
                                    }
                                } else {
                                    // Primary: mark as stopped (not delete — secondaries
                                    // may query it to detect primary is gone)
                                    let _ = pg
                                        .update_runner_instance_heartbeat(
                                            &id, Some(0), "stopped",
                                        )
                                        .await;
                                    info!("Marked primary instance as stopped in DB");
                                }
                            });
                        }
                    });
                }

                // ── Explicit shutdown ordering ──
                let app_state_clone = app_state.inner().clone();

                let shutdown_handle = std::thread::spawn(move || {
                    // Build a small current-thread runtime for the cleanup work
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build();

                    match rt {
                        Ok(rt) => {
                            // Stop all bridges via bridge manager
                            rt.block_on(async {
                                let manager_guard =
                                    app_state_clone.bridge_manager.lock().await;
                                if let Some(ref manager) = *manager_guard {
                                    info!("Stopping all bridges via bridge manager");
                                    manager.remove_all().await;
                                }
                            });

                            // Release ADB forwards installed by the USB scanner
                            // so they don't linger in `adb forward --list`
                            // across runner restarts. Graceful path only — the
                            // supervisor force-kills via taskkill /F and this
                            // code never runs for temp runners. See plan
                            // adb-forwarder-port.md §1.6a.
                            rt.block_on(async {
                                if let Some(usb) =
                                    app_state_clone.usb_transport.get()
                                {
                                    info!("Releasing ADB forwards on shutdown");
                                    usb.release_all().await;
                                }
                            });
                        }
                        Err(e) => {
                            error!("Failed to create shutdown runtime: {}", e);
                        }
                    }

                    // Stop the extraction executor. Safe to call here now that it no longer
                    // owns an Arc<tokio::runtime::Runtime> — stop_internal() is pure
                    // synchronous Python-subprocess teardown.
                    if let Ok(mut guard) = app_state_clone.extraction_executor.lock() {
                        if let Some(mut ee) = guard.take() {
                            info!("Stopping extraction executor");
                            let _ = ee.stop();
                        }
                    }
                });

                // Wait for the dedicated shutdown thread, but with a hard
                // upper bound so a hung bridge or extractor can't freeze the
                // close handler (and with it the whole window). Without this
                // cap users see the X button "do nothing" — the handler is
                // actually running, just blocked on a shutdown step.
                //
                // Any cleanup still running past the deadline continues in
                // the background thread after we return from the close
                // handler. The process then exits on its normal Tauri path;
                // if something is still holding it up, the explicit
                // `std::process::exit` below forces termination.
                const SHUTDOWN_JOIN_DEADLINE_MS: u64 = 3_000;
                let deadline = std::time::Instant::now()
                    + std::time::Duration::from_millis(SHUTDOWN_JOIN_DEADLINE_MS);
                // Poll instead of blocking `.join()` so we can bail out on
                // timeout. `JoinHandle::is_finished` is stable and doesn't
                // require `nightly`.
                while !shutdown_handle.is_finished() && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                if !shutdown_handle.is_finished() {
                    warn!(
                        "Shutdown thread did not finish within {}ms — continuing \
                         window close; any remaining cleanup will be killed by \
                         process exit",
                        SHUTDOWN_JOIN_DEADLINE_MS
                    );
                }

                // Close all interactive Claude sessions
                if let Some(sm) =
                    window.try_state::<Arc<claude_session::SessionManager>>()
                {
                    sm.close_all_sessions();
                }

                // Close all embedded terminal sessions
                if let Some(tm) =
                    window.try_state::<Arc<terminal::TerminalManager>>()
                {
                    tm.close_all();
                }

                // Kill any orphaned AI (Claude CLI) processes tracked by the PID tracker.
                // This catches processes that survived session close (e.g., cmd.exe /c claude).
                {
                    let pids_to_kill: Vec<u32> = {
                        if let Ok(mut pids) = app_state.ai_pid_tracker.lock() {
                            let copy = pids.clone();
                            pids.clear();
                            copy
                        } else {
                            Vec::new()
                        }
                    };
                    if !pids_to_kill.is_empty() {
                        info!(
                            "Killing {} orphaned AI process(es) on shutdown: {:?}",
                            pids_to_kill.len(),
                            pids_to_kill
                        );
                        for pid in &pids_to_kill {
                            let result = std::process::Command::new("taskkill")
                                .args(["/F", "/T", "/PID", &pid.to_string()])
                                .output();
                            match result {
                                Ok(output) => {
                                    if output.status.success() {
                                        info!("Killed AI process tree for PID {}", pid);
                                    } else {
                                        // Process may have already exited — not an error
                                        info!("AI process PID {} already exited", pid);
                                    }
                                }
                                Err(e) => {
                                    error!("Failed to taskkill PID {}: {}", pid, e);
                                }
                            }
                        }
                    }
                }

                // Stop all managed processes
                let app_state_pcm = app_state.inner().clone();
                tauri::async_runtime::spawn(async move {
                    let manager_lock = app_state_pcm.process_capture_manager.lock().await;
                    if let Some(ref manager) = *manager_lock {
                        info!("Stopping all managed processes");
                        manager.stop_all().await;
                        info!("All managed processes stopped");
                    }
                });

                // Stop error monitor service
                let app_state_clone2 = app_state.inner().clone();
                tauri::async_runtime::spawn(async move {
                    let handle_lock = app_state_clone2.error_monitor_handle.lock().await;
                    if let Some(ref handle) = *handle_lock {
                        info!("Stopping error monitor service");
                        if let Err(e) = handle.stop().await {
                            error!("Failed to stop error monitor service: {}", e);
                        } else {
                            info!("Error monitor service stopped");
                        }
                    }
                });

                // Stop Doctor health monitoring service
                let app_state_clone3 = app_state.inner().clone();
                tauri::async_runtime::spawn(async move {
                    let handle_lock = app_state_clone3.doctor_handle.lock().await;
                    if let Some(ref handle) = *handle_lock {
                        info!("Stopping Doctor service");
                        if let Err(e) = handle.shutdown().await {
                            error!("Failed to stop Doctor service: {}", e);
                        } else {
                            info!("Doctor service stopped");
                        }
                    }
                });

                // Stop trigger service
                tauri::async_runtime::spawn(async move {
                    crate::trigger_system::stop_trigger_service().await;
                });

                // ── Explicit exit request ──
                //
                // Tauri's automatic "last-window-closed → exit" chain breaks
                // in this app:
                //   - WebView2's destroy on Windows sometimes hangs (see the
                //     `Failed to unregister class Chrome_WidgetWin_0` error
                //     that surfaces when the process is force-killed later).
                //   - Long-lived `tauri::async_runtime::spawn` tasks (mDNS
                //     scanner, workflow event bus, instance manager, backend
                //     relay poll, AI-settings checker) never complete, so
                //     Tauri's runtime shutdown has no clean point to drain.
                //
                // Calling `app_handle.exit(0)` explicitly bypasses the
                // window-destruction path and fires `RunEvent::ExitRequested`
                // directly. Tauri then aborts the runtime and returns from
                // `app.run`, and the process exits as soon as `main()`
                // returns. This is the polite, ordered shutdown path.
                let exit_handle = window.app_handle().clone();
                tauri::async_runtime::spawn(async move {
                    // Let the other spawned cleanup tasks run their first
                    // tick before we pull the rug out — in practice they
                    // all complete within ~500ms of the handler returning.
                    tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
                    info!("Requesting Tauri app exit");
                    exit_handle.exit(0);
                });

                // ── Force-exit watchdog (safety net) ──
                //
                // Even with the explicit `app_handle.exit(0)` above, Tauri's
                // event loop can still stall if WebView2 or a tao internal
                // handler is blocked. This detached OS thread is the last
                // line of defense: after a short grace period it calls
                // `std::process::exit(0)` unconditionally. If Tauri exits
                // cleanly first, the thread is killed with the process and
                // the force-exit is never reached.
                //
                // Grace is short (3s) because our cleanup completes in
                // ~1.2s and `app_handle.exit(0)` is scheduled for 1.5s.
                // Anything past 3s is definitely stalled.
                const FORCE_EXIT_GRACE_SECS: u64 = 3;
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(
                        FORCE_EXIT_GRACE_SECS,
                    ));
                    warn!(
                        "Force-exit watchdog: Tauri did not terminate within \
                         {}s of the close handler returning — exiting process",
                        FORCE_EXIT_GRACE_SECS
                    );
                    std::process::exit(0);
                });
            }
        })
        .build(tauri::generate_context!())?;

    info!("Tauri application built successfully");
    app.run(|_, event| {
        if let tauri::RunEvent::ExitRequested { .. } = event {
            info!("Application exit requested");
        }
    });

    Ok(())
}
