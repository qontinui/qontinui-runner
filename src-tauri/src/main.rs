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
mod asset_headers;
mod auth;
mod auto_commit;
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
mod launch_env;
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
mod sdk_features;
mod secure_storage;
mod security;
mod semantic_conventions;
mod server_mode;
mod settings;
// `startup_panic` is a minimal, dep-free panic-hook installer called from
// the very top of `main()` so early-init crashes (DB connect, Tauri builder,
// axum router construction) write a `runner-panic.log` the supervisor can
// pick up. Must come before any module that panics during static init.
mod skills;
mod slash_commands;
mod spawn_placement;
mod spec_api;
mod spec_experimentation;
mod spec_utils;
mod startup_panic;
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
mod trace_api;
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
mod wake_handler; // Phase F.1 — qontinui:// custom-URL deep-link wake handler
mod window_manager;
mod workflow;
mod workflow_event_bus;
mod workflow_generation;
mod workflow_queue;
mod workflow_state;
mod worktree;
mod wrappers;
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

    // Install the startup-panic hook FIRST, before any other setup. Panics
    // during early init (database connection, Tauri builder, axum router
    // construction) would otherwise vanish — the process exits with code 1
    // and the supervisor sees only that. This hook writes a one-file-per-boot
    // `runner-panic.log` the supervisor reads after a non-zero exit.
    startup_panic::install_startup_panic_hook();

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
    // Read every QONTINUI_* / WEBVIEW2_* env var that influences startup
    // exactly once. All downstream code reads from this snapshot via the
    // Tauri-managed `Arc<RunnerLaunchEnv>` instead of re-parsing env vars.
    // See `launch_env.rs` for the full list and rationale.
    let launch_env: launch_env::SharedLaunchEnv = Arc::new(launch_env::RunnerLaunchEnv::read());

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

    // Initialize PostgreSQL connection.
    //
    // Connection settings come from `~/.qontinui/profiles.json` per the
    // canonical-DB topology (see `tmp_canonical_db_topology_plan.md` §3 and
    // `crate::profiles`). Selection order: `QONTINUI_ENV` env var → file's
    // `active` field → `dev`. Falls back to legacy `RUNNER_DATABASE_URL`
    // env var (and ultimately a localhost default) when profiles.json is
    // missing, so unmigrated machines remain bootable.
    //
    // Uses a dedicated tokio runtime for the one-shot async connection —
    // cannot use tauri::async_runtime::block_on here because the Tauri
    // runtime hasn't started yet. PG pool connections are tied to their
    // creating runtime, so this same runtime stays alive for the rest of
    // the bootstrap path.
    let pg_db: Arc<crate::database::pg::PgDb> = {
        let profile = qontinui_runner_lib::profiles::load();
        info!(
            "Connecting to canonical PG via profile '{}'",
            profile.source
        );
        let pg_url = profile.database_url;
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
    let server_mode_is_on = launch_env.server_mode;
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
        upcoming_file_registry: Arc::new(
            crate::executor::upcoming_file_registry::UpcomingFileRegistry::new(),
        ),
        file_lock_manager: Arc::new(crate::executor::FileLockManager::new()),
        ui_bridge_failure_tracker:
            crate::step_executor::handlers::ui_bridge::UiBridgeFailureTracker::new(),
        process_capture_manager: TokioMutex::new(None), // Initialized in setup()
        api_ready: AtomicBool::new(false),              // Set when MCP API server binds
        frontend_ready: AtomicBool::new(false), // Set on first successful UI Bridge IPC response
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
        app_registry: Arc::new(tokio::sync::OnceCell::new()),
        app_dispatcher: Arc::new(tokio::sync::OnceCell::new()),
        wrapper_state: Arc::new(tokio::sync::OnceCell::new()),
    });
    let mcp_app_state = shared_app_state.clone();
    let mcp_rag_state = rag_state.clone();
    let heartbeat_app_state = shared_app_state.clone();
    let crash_dump_app_state = shared_app_state.clone();

    // Workstream C (Move 2B): compartment wrappers around Arc<AppState> so
    // per-module plugins can migrate from `State<Arc<AppState>>` to a scoped
    // `State<<Compartment>>`. Both forms coexist during the gradual migration.
    let bridge_compartment = commands::compartments::BridgeCompartment(shared_app_state.clone());
    let execution_compartment =
        commands::compartments::ExecutionCompartment(shared_app_state.clone());
    let integration_compartment =
        commands::compartments::IntegrationCompartment(shared_app_state.clone());
    let health_compartment = commands::compartments::HealthCompartment(shared_app_state.clone());
    let storage_compartment = commands::compartments::StorageCompartment(shared_app_state.clone());

    // Frontend log-sync store. Holds the most recent batches of logs the React
    // UI mirrors back into Rust state for HTTP API consumption. Created here
    // so it can be both `.manage()`d for the Tauri commands in
    // `commands::log_api` and exposed to the MCP HTTP server later.
    let log_api_store = commands::log_api::create_log_store();

    // Create error monitor config for later initialization
    let error_monitor_pg = shared_app_state.pg_db.clone();

    // Secondary instances (spawned by InstanceManager) must NOT use single-instance
    // plugin — it would prevent them from starting since they share the same binary.
    let is_secondary_instance = instance::is_secondary();

    let mut builder = tauri::Builder::default();

    if !is_secondary_instance {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // When a second instance is launched, focus the existing window
            if let Some(window) =
                app.get_webview_window(qontinui_runner_lib::get_main_window_label())
            {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
            // Phase F.1 — single-instance plugin's `deep-link` feature flag
            // forwards `qontinui://...` URLs from the secondary launch's argv
            // to the running primary so the wake handler runs in the live
            // process instead of starting a duplicate runner.
            wake_handler::handle_args_from_secondary_instance(app.clone(), &args);
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
        // Phase F.1 — Wake-from-web custom URL scheme (`qontinui://wake?...`)
        // and launch-on-system-startup toggle. Deep-link plugin must come
        // before `.setup(...)` so `app.deep_link().on_open_url(...)` is
        // available in the setup closure. Autostart's MacosLauncher kind is
        // platform-conditional but the API is identical on Windows/Linux.
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(ui_bridge_plugin::init())
        // All other in-app Tauri commands are registered via the central
        // `generate_handler!` below rather than per-module plugins.
        //
        // Rationale: the 90-module plugin split (Workstream B of
        // REFACTOR_WAVE_3_PLAN) compiled clean but broke runtime IPC. Tauri 2
        // plugin commands are invokable only as `plugin:<name>|<cmd>` — the
        // frontend's hundreds of bare `invoke("<cmd>")` calls therefore failed
        // with "Command not found", stranding the app on "Starting API
        // server...". Tauri 2 identifier rules also reject the `qontinui_*`
        // plugin names outright (underscores disallowed in capability
        // prefixes), so the plugin shape can't be salvaged without a
        // cross-cutting rename.
        //
        // Keeping everything app-level lets the existing frontend surface work
        // unchanged, and Tauri skips the ACL check on bare commands when no
        // AppManifest is defined (`tauri-2.10.3/src/webview/mod.rs:1801-1806`).
        //
        // `ui_bridge_plugin` remains the one exception — its handler fns are
        // non-`pub` and the frontend talks to them only via HTTP, not IPC.
        //
        // The per-module `plugin()` fns are left in place for future reuse
        // even though nothing invokes them now.
        .invoke_handler(tauri::generate_handler![
            commands::accessibility::a11y_ai_context,
            commands::accessibility::a11y_capture,
            commands::accessibility::a11y_click,
            commands::accessibility::a11y_connect,
            commands::accessibility::a11y_disconnect,
            commands::accessibility::a11y_focus,
            commands::accessibility::a11y_query,
            commands::accessibility::a11y_type_text,
            commands::accessibility::check_chrome_available,
            commands::accessibility::get_accessibility_settings,
            commands::accessibility::launch_chrome_debug,
            commands::accessibility::save_accessibility_settings,
            commands::activity_timeline::delete_activity_timeline_entry,
            commands::activity_timeline::get_activity_timeline_entry,
            commands::activity_timeline::get_activity_timeline_for_task_run,
            commands::activity_timeline::get_activity_timeline_range,
            commands::activity_timeline::get_activity_timeline_stats,
            commands::activity_timeline::get_scripted_output_stats,
            commands::activity_timeline::insert_activity_entry,
            commands::activity_timeline::search_activity_timeline,
            commands::activity_timeline::search_activity_timeline_filtered,
            commands::adaptive_learning::delete_curated_example,
            commands::adaptive_learning::delete_playbook_entry,
            commands::adaptive_learning::get_adaptive_learning_stats,
            commands::adaptive_learning::get_curated_examples,
            commands::adaptive_learning::get_gepa_run_detail,
            commands::adaptive_learning::get_gepa_runs,
            commands::adaptive_learning::get_learning_trends,
            commands::adaptive_learning::get_playbook_entries,
            commands::adaptive_learning::get_playbook_entry_detail,
            commands::adaptive_learning::get_template_lifecycle_history,
            commands::adaptive_learning::get_template_performance,
            commands::adaptive_learning::update_playbook_entry_status,
            commands::agentic_metrics::get_agentic_metric_aggregates,
            commands::agentic_metrics::get_agentic_scores,
            commands::agentic_metrics::get_composite_score_trend,
            commands::agentic_metrics::push_agentic_scores_to_backend,
            commands::agentic_metrics::push_latest_agentic_scores,
            commands::agentic_metrics::recompute_agentic_baselines,
            commands::ai_data::get_ai_prompts_for_viewer,
            commands::ai_data::get_consolidated_ai_output,
            commands::ai_data::get_contexts_for_viewer,
            commands::ai_data::get_jsonl_logs_summary,
            commands::ai_data::get_loaded_config_for_viewer,
            commands::ai_data::get_screenshots_for_viewer,
            commands::ai_data::get_task_run_api_requests_from_db,
            commands::ai_data::get_task_run_awas_steps_from_db,
            commands::ai_data::get_task_run_context,
            commands::ai_data::get_task_run_events_from_db,
            commands::ai_data::get_task_run_for_viewer,
            commands::ai_data::get_task_run_migrated_logs_summary,
            commands::ai_data::get_task_run_playwright_results_from_db,
            commands::ai_data::get_task_run_screenshots_from_db,
            commands::ai_data::get_task_run_verification_results_from_db,
            commands::ai_data::get_task_runs_for_viewer,
            commands::ai_data::get_text_logs_summary,
            commands::ai_data::read_jsonl_logs_for_task_run,
            commands::ai_data::read_jsonl_logs_for_viewer,
            commands::ai_data::read_text_logs_for_viewer,
            commands::ai_data::reopen_task_run,
            commands::ai_generation::explore_flow_step,
            commands::ai_generation::generate_api_request_with_ai,
            commands::ai_generation::generate_context_with_ai,
            commands::ai_generation::generate_element_ai_description,
            commands::ai_generation::generate_task_prompt_with_ai,
            commands::ai_generation::generate_test_and_agentic_step,
            commands::ai_generation::suggest_exploration_strategy_with_ai,
            commands::ai_session::close_ai_session,
            commands::ai_session::commit_session_progress,
            commands::ai_session::create_ai_session,
            commands::ai_session::generate_workflow_from_session,
            commands::ai_session::get_ai_output,
            commands::ai_session::get_ai_session_state,
            commands::ai_session::interrupt_ai_session,
            commands::ai_session::list_ai_sessions,
            commands::ai_session::promote_session_to_worktree,
            commands::ai_session::rename_ai_session,
            commands::ai_session::send_user_message,
            commands::ai_settings::check_accounts_usage,
            commands::ai_settings::check_claude_cli_auth,
            commands::ai_settings::delete_ai_api_key_command,
            commands::ai_settings::get_agentic_settings,
            commands::ai_settings::get_ai_settings,
            commands::ai_settings::get_claude_accounts,
            commands::ai_settings::get_provider_circuit_states,
            commands::ai_settings::get_wsv_settings,
            commands::ai_settings::has_ai_api_key,
            commands::ai_settings::list_wsv_disagreements,
            commands::ai_settings::refresh_claude_cli_auth,
            commands::ai_settings::reset_provider_circuit,
            commands::ai_settings::save_agentic_settings,
            commands::ai_settings::save_ai_api_key_command,
            commands::ai_settings::save_ai_settings,
            commands::ai_settings::save_gemini_settings,
            commands::ai_settings::save_wsv_settings,
            commands::ai_settings::switch_claude_account,
            commands::ai_settings::test_ai_connection,
            commands::ai_settings::test_wsv_connection,
            commands::auth::check_auth_status,
            commands::auth::get_access_token_for_websocket,
            commands::auth::get_api_port,
            commands::auth::get_device_info,
            commands::auth::get_test_auto_login,
            commands::auth::get_user_projects,
            commands::auth::is_api_ready,
            commands::auth::login,
            commands::auth::logout,
            commands::auth::refresh_token,
            commands::autostart::get_autostart_enabled,
            commands::autostart::set_autostart_enabled,
            commands::backup::export_all_data,
            commands::backup::get_export_summary,
            commands::backup::get_import_preview,
            commands::backup::import_all_data,
            commands::build_id::get_build_id,
            commands::checkpoint_browser::add_sample_checkpoints,
            commands::checkpoint_browser::clear_all_checkpoints,
            commands::checkpoint_browser::compare_orchestrator_checkpoints,
            commands::checkpoint_browser::complete_replay_session,
            commands::checkpoint_browser::create_orchestrator_checkpoint,
            commands::checkpoint_browser::delete_orchestrator_checkpoint,
            commands::checkpoint_browser::fail_replay_session,
            commands::checkpoint_browser::find_checkpoints_by_tag,
            commands::checkpoint_browser::get_checkpoint_count,
            commands::checkpoint_browser::get_checkpoint_stats,
            commands::checkpoint_browser::get_checkpoint_task_ids,
            commands::checkpoint_browser::get_checkpoints_count,
            commands::checkpoint_browser::get_checkpoints_filtered,
            commands::checkpoint_browser::get_checkpoints_paginated,
            commands::checkpoint_browser::get_latest_checkpoint,
            commands::checkpoint_browser::get_orchestrator_checkpoint,
            commands::checkpoint_browser::get_replay_lineage,
            commands::checkpoint_browser::get_task_lineage_info,
            commands::checkpoint_browser::list_active_replay_sessions,
            commands::checkpoint_browser::list_orchestrator_checkpoints,
            commands::checkpoint_browser::register_task_for_lineage,
            commands::checkpoint_browser::replay_from_checkpoint,
            commands::checkpoint_browser::start_replay_session,
            commands::checkpoints::checkpoint_delete,
            commands::checkpoints::checkpoint_get,
            commands::checkpoints::checkpoint_history,
            commands::checkpoints::checkpoint_list_active,
            commands::checkpoints::checkpoint_save,
            commands::checkpoints::checkpoint_status,
            commands::checkpoints::session_create,
            commands::checkpoints::session_update_status,
            commands::checkpoints::setting_get,
            commands::checkpoints::setting_set,
            commands::checkpoints::settings_get_all,
            commands::checks::create_check,
            commands::checks::create_check_group,
            commands::checks::delete_check,
            commands::checks::delete_check_group,
            commands::checks::detect_project_check_suggestions,
            commands::checks::execute_check_by_id,
            commands::checks::execute_check_group,
            commands::checks::execute_code_check,
            commands::checks::execute_code_check_suite,
            commands::checks::get_check,
            commands::checks::get_check_group,
            commands::checks::get_check_results,
            commands::checks::get_check_tool_info,
            commands::checks::get_checks_in_group,
            commands::checks::list_check_groups,
            commands::checks::list_checks,
            commands::checks::repair_check_group_associations,
            commands::checks::set_checks_in_group,
            commands::checks::update_check,
            commands::checks::update_check_group,
            commands::chunk_labels::delete_chunk_label,
            commands::chunk_labels::list_chunk_labels,
            commands::chunk_labels::upsert_chunk_label,
            commands::clipboard::share_file_to_mobile,
            commands::clipboard::share_to_mobile,
            commands::comparison::get_comparison_status,
            commands::comparison::list_comparisons,
            commands::comparison::start_comparison,
            commands::config::get_auto_load_last_config,
            commands::config::get_claude_account_launch_commands,
            commands::config::get_claude_config_dirs,
            commands::config::get_current_configuration,
            commands::config::get_include_summary_step_by_default,
            commands::config::get_last_config_path,
            commands::config::get_workspace_paths,
            commands::config::load_configuration,
            commands::config::save_auto_load_last_config,
            commands::config::save_claude_account_launch_commands,
            commands::config::save_claude_config_dirs,
            commands::config::save_include_summary_step_by_default,
            commands::config::save_last_monitor_index,
            commands::config::save_last_monitor_indices,
            commands::config::save_last_workflow_id,
            commands::container_settings::check_docker_status,
            commands::container_settings::get_container_settings,
            commands::container_settings::update_container_settings,
            commands::context::create_context,
            commands::context::delete_context,
            commands::context::evaluate_auto_include,
            commands::context::get_all_contexts,
            commands::context::get_builtin_contexts_cmd,
            commands::context::get_context,
            commands::context::get_context_categories,
            commands::context::record_context_usage,
            commands::context::search_contexts,
            commands::context::set_context_enabled,
            commands::context::update_context,
            commands::cost_dashboard::get_active_budget_status,
            commands::cost_dashboard::get_cost_dashboard,
            crash_dumps::dismiss_recent_crash,
            commands::dag_workflows::export_dag_workflow,
            commands::dag_workflows::import_dag_workflow,
            commands::dag_workflows::import_dag_workflows_from_project,
            commands::dag_workflows::respond_dag_approval,
            commands::dag_workflows::validate_dag_workflow,
            commands::database::explain_query_plan,
            commands::database::get_database_stats,
            commands::database::optimize_database,
            commands::dataset::package_dataset,
            commands::dataset::scan_local_images,
            commands::debug::get_debug_settings,
            commands::debug::set_debug_settings,
            commands::dev_findings::dev_seed_finding,
            commands::dev_findings::is_dev_endpoints_enabled,
            commands::discoveries::clear_discovery,
            commands::discoveries::clear_failed_discoveries,
            commands::discoveries::get_discovery_summary,
            commands::discoveries::get_discovery_sync_status,
            commands::discoveries::get_pending_discoveries_cmd,
            commands::discoveries::sync_discoveries,
            doctor::commands::doctor_get_status,
            doctor::commands::stop_process_by_pid,
            commands::durable_execution::get_iteration_commits,
            commands::durable_execution::get_iteration_diffs,
            commands::durable_execution::get_phase_results,
            commands::durable_execution::list_replay_points,
            commands::durable_execution::replay_workflow,
            commands::durable_execution::rollback_workflow_to_iteration,
            error_monitor::commands::acknowledge_all_errors,
            error_monitor::commands::acknowledge_error,
            error_monitor::commands::get_debug_context,
            error_monitor::commands::get_debug_context_for_ai,
            error_monitor::commands::get_error_event,
            error_monitor::commands::get_error_recurrence_history,
            error_monitor::commands::get_error_summary,
            error_monitor::commands::get_recent_errors,
            error_monitor::commands::get_unresolved_errors,
            error_monitor::commands::has_actionable_errors,
            error_monitor::commands::ignore_error,
            error_monitor::commands::link_error_to_finding,
            error_monitor::commands::open_error_in_editor,
            error_monitor::commands::query_error_events,
            error_monitor::commands::resolve_error,
            error_monitor::commands::search_errors,
            error_monitor::commands::update_error_status,
            error_monitor::workflow::check_fixable_errors,
            commands::event_search::search_events,
            commands::execution::bridge_execution::get_bridge_info,
            commands::execution::bridge_execution::list_bridges,
            commands::execution::bridge_execution::run_workflow_on_bridge,
            commands::execution::bridge_execution::transfer_gui_lock,
            commands::execution::executor_status::get_executor_status,
            commands::execution::executor_status::get_input_validation_status,
            commands::execution::executor_status::get_monitors,
            commands::execution::executor_status::set_input_capture_enabled,
            commands::execution::python_executor::start_python_executor,
            commands::execution::python_executor::stop_python_executor,
            commands::execution::python_executor::update_capture_settings,
            commands::execution::system_ops::check_for_updates,
            commands::execution::system_ops::handle_error,
            commands::execution::system_ops::install_update,
            commands::execution::system_ops::open_folder,
            commands::execution::workflow_execution::get_resolved_initial_states,
            commands::execution::workflow_execution::get_workflow_required_screens,
            commands::execution::workflow_execution::pause_execution,
            commands::execution::workflow_execution::resume_execution,
            commands::execution::workflow_execution::start_execution,
            commands::execution::workflow_execution::stop_execution,
            commands::execution_reporting::complete_execution_run,
            commands::execution_reporting::create_execution_run,
            commands::execution_reporting::report_action_executions,
            commands::execution_reporting::report_execution_issues,
            commands::execution_reporting::upload_execution_screenshot,
            commands::execution_variables::get_execution_variables_settings,
            commands::execution_variables::get_resolved_execution_context,
            commands::execution_variables::save_execution_variables_settings,
            commands::execution_variables::test_env_var,
            commands::extraction::create_extraction_session,
            commands::extraction::export_state_structure,
            commands::extraction::export_training_data,
            commands::extraction::get_extraction_status,
            commands::extraction::get_project_extractions,
            commands::extraction::list_extractions,
            commands::extraction::request_extraction_screenshot,
            commands::extraction::start_vision_extraction,
            commands::extraction::start_web_extraction,
            commands::extraction::stop_web_extraction,
            commands::extraction::update_extraction_session,
            commands::extraction::upload_extraction_annotations,
            commands::extraction::upload_state_structure,
            commands::file_browser::browse_directory,
            commands::file_browser::get_safe_browse_roots,
            commands::file_browser::read_file_content,
            commands::findings::get_finding_by_id,
            commands::findings::get_findings_by_status_cmd,
            commands::findings::get_findings_summary,
            commands::findings::get_task_findings,
            commands::findings::list_task_knowledge_cmd,
            commands::findings::provide_finding_response,
            commands::findings::resolve_finding,
            commands::findings::update_finding,
            commands::flow::add_sample_flow,
            commands::flow::cancel_flow_execution,
            commands::flow::compare_flow_versions,
            commands::flow::create_flow_version,
            commands::flow::create_sample_flow,
            commands::flow::delete_flow,
            commands::flow::delete_flow_version,
            commands::flow::export_flow_json,
            commands::flow::export_flow_yaml,
            commands::flow::export_flows_bulk,
            commands::flow::get_flow,
            commands::flow::get_flow_execution,
            commands::flow::get_flow_executions_count,
            commands::flow::get_flow_executions_filtered,
            commands::flow::get_flow_executions_paginated,
            commands::flow::get_flow_version,
            commands::flow::get_flows_by_tag,
            commands::flow::get_latest_flow_version,
            commands::flow::import_flow_json,
            commands::flow::import_flow_yaml,
            commands::flow::import_flows_bulk,
            commands::flow::list_flow_executions,
            commands::flow::list_flow_versions,
            commands::flow::list_flows,
            commands::flow::pause_flow_execution,
            commands::flow::provide_flow_input,
            commands::flow::restore_flow_version,
            commands::flow::resume_flow_execution,
            commands::flow::run_flow_execution,
            commands::flow::save_flow,
            commands::flow::start_flow_execution,
            commands::flow::step_flow_execution,
            commands::flow::step_into_flow,
            commands::flow::validate_flow,
            commands::global_log_sources::add_global_log_source,
            commands::global_log_sources::create_global_log_source_profile,
            commands::global_log_sources::delete_global_log_source,
            commands::global_log_sources::delete_global_log_source_profile,
            commands::global_log_sources::find_log_sources_with_ai,
            commands::global_log_sources::get_global_log_sources,
            commands::global_log_sources::migrate_project_sources_to_global,
            commands::global_log_sources::read_global_log_sources,
            commands::global_log_sources::read_log_sources_by_profile,
            commands::global_log_sources::save_global_log_sources,
            commands::global_log_sources::select_log_sources_for_context,
            commands::global_log_sources::set_default_log_source_profile,
            commands::global_log_sources::set_log_source_ai_selection_mode,
            commands::global_log_sources::update_global_log_source,
            commands::global_log_sources::update_global_log_source_profile,
            commands::hooks::create_hook,
            commands::hooks::delete_hook,
            commands::hooks::get_all_hooks,
            commands::hooks::get_hook,
            commands::hooks::reorder_hooks,
            commands::hooks::set_hook_enabled,
            commands::hooks::test_hook,
            commands::hooks::update_hook,
            commands::instances::delete_runner_instance,
            commands::instances::get_runner_identity,
            commands::instances::get_runner_instances,
            commands::instances::get_temp_spawn_placements,
            commands::instances::launch_runner_instance,
            commands::instances::list_monitors_for_placement,
            commands::instances::preview_spawn_placement,
            commands::instances::save_runner_instance,
            commands::instances::set_temp_spawn_placements,
            commands::instances::stop_runner_instance,
            commands::interaction::get_interaction_recording_status,
            commands::interaction::start_interaction_recording,
            commands::interaction::stop_interaction_recording,
            commands::issues::sync_issues_to_backend,
            commands::known_issues::create_known_issue,
            commands::known_issues::create_pattern_template,
            commands::known_issues::delete_known_issue,
            commands::known_issues::export_known_issues,
            commands::known_issues::find_issues_for_spec,
            commands::known_issues::import_known_issues,
            commands::known_issues::list_known_issues,
            commands::known_issues::list_pattern_templates,
            commands::known_issues::resolve_known_issue,
            commands::known_issues::update_known_issue,
            commands::learning::add_sample_learning_data,
            commands::learning::analyze_learning_data,
            commands::learning::clear_learning_data,
            commands::learning::export_learning_data,
            commands::learning::get_best_strategy,
            commands::learning::get_current_running_task,
            commands::learning::get_feedback_for_context,
            commands::learning::get_learning_dashboard_data,
            commands::learning::get_learning_insights,
            commands::learning::get_learning_outcomes_count,
            commands::learning::get_learning_outcomes_filtered,
            commands::learning::get_learning_outcomes_paginated,
            commands::learning::get_learning_patterns,
            commands::learning::get_learning_stats_by_date_range,
            commands::learning::get_learning_stats_summary,
            commands::learning::get_learning_summary,
            commands::learning::get_most_recent_task_with_checkpoints,
            commands::learning::get_recent_tasks_with_outcomes,
            commands::learning::import_learning_data,
            commands::learning::record_task_outcome,
            commands::library_sync::sync_api_requests_to_backend,
            commands::library_sync::sync_check_groups_to_backend,
            commands::library_sync::sync_checks_to_backend,
            commands::library_sync::sync_contexts_to_backend,
            commands::library_sync::sync_library_to_backend,
            commands::library_sync::sync_macros_to_backend,
            commands::library_sync::sync_prompt_snippets_to_backend,
            commands::library_sync::sync_shell_commands_to_backend,
            commands::logging::append_ai_output_log,
            commands::logging::append_render_log,
            commands::logging::clear_ai_output_log,
            commands::logging::clear_all_run_history,
            commands::logging::clear_render_log,
            commands::logging::delete_session_checkpoints,
            commands::logging::get_ai_output_log_path_cmd,
            commands::logging::get_render_log_path_cmd,
            commands::logging::list_session_checkpoints,
            commands::logging::load_ai_output_log,
            commands::logging::load_render_log,
            commands::log_api::clear_log_api_store,
            commands::log_api::sync_action_logs,
            commands::log_api::sync_ai_output_logs,
            commands::log_api::sync_all_logs,
            commands::log_api::sync_general_logs,
            commands::log_api::sync_image_logs,
            commands::log_api::sync_issues,
            commands::log_api::sync_project_logs,
            commands::log_api::sync_rag_logs,
            commands::mcp::call_mcp_tool,
            commands::mcp::connect_mcp_server,
            commands::mcp::create_mcp_server,
            commands::mcp::delete_mcp_server,
            commands::mcp::disconnect_mcp_server,
            commands::mcp::get_mcp_server,
            commands::mcp::get_mcp_server_status,
            commands::mcp::get_mcp_servers_status,
            commands::mcp::get_task_run_mcp_calls,
            commands::mcp::list_mcp_server_tools,
            commands::mcp::list_mcp_servers,
            commands::mcp::update_mcp_server,
            commands::meta_optimizer::activate_prompt_variant,
            commands::meta_optimizer::apply_meta_optimizer_recommendation,
            commands::meta_optimizer::build_golden_dataset,
            commands::meta_optimizer::capture_meta_optimizer_baseline,
            commands::meta_optimizer::convert_comparison_to_recommendation,
            commands::meta_optimizer::create_eval_spec,
            commands::meta_optimizer::create_prompt_canary,
            commands::meta_optimizer::delete_eval_spec,
            commands::meta_optimizer::evaluate_with_io,
            commands::meta_optimizer::generate_default_eval_spec,
            commands::meta_optimizer::get_agent_cascade_effect,
            commands::meta_optimizer::get_agent_cost_effectiveness,
            commands::meta_optimizer::get_agent_effectiveness,
            commands::meta_optimizer::get_agent_interaction_matrix,
            commands::meta_optimizer::get_canary_rollouts,
            commands::meta_optimizer::get_eval_results,
            commands::meta_optimizer::get_eval_specs,
            commands::meta_optimizer::get_golden_datasets,
            commands::meta_optimizer::get_meta_optimizer_failure_analysis,
            commands::meta_optimizer::get_meta_optimizer_progress,
            commands::meta_optimizer::get_meta_optimizer_recommendations,
            commands::meta_optimizer::get_meta_optimizer_runs,
            commands::meta_optimizer::get_meta_optimizer_snapshots,
            commands::meta_optimizer::get_model_profiles,
            commands::meta_optimizer::get_model_recommendations,
            commands::meta_optimizer::get_prompt_canary_status,
            commands::meta_optimizer::get_prompt_evolution_diff,
            commands::meta_optimizer::get_prompt_evolution_history,
            commands::meta_optimizer::get_prompt_group_metrics,
            commands::meta_optimizer::get_prompt_optimization_evidence,
            commands::meta_optimizer::get_prompt_optimization_status,
            commands::meta_optimizer::get_prompt_variant_content,
            commands::meta_optimizer::get_prompt_variants,
            commands::meta_optimizer::get_recommendation_outcomes,
            commands::meta_optimizer::get_robustness_reports,
            commands::meta_optimizer::promote_canary_rollout,
            commands::meta_optimizer::reevaluate_recommendation_outcome,
            commands::meta_optimizer::refresh_model_profiles,
            commands::meta_optimizer::reject_meta_optimizer_recommendation,
            commands::meta_optimizer::rollback_canary_rollout,
            commands::meta_optimizer::rollback_meta_optimizer_recommendation,
            commands::meta_optimizer::run_recommendation_eval,
            commands::meta_optimizer::run_robustness_test,
            commands::meta_optimizer::start_canary_rollout,
            commands::meta_optimizer::trigger_meta_optimizer,
            commands::mobile::capture_mobile_feedback,
            commands::mobile::capture_mobile_logcat,
            commands::mobile::capture_mobile_screenshot,
            commands::mobile::create_mobile_log,
            commands::mobile::create_mobile_state,
            commands::mobile::delete_mobile_data,
            commands::mobile::get_latest_mobile_state,
            commands::mobile::get_mobile_errors,
            commands::mobile::get_mobile_logs,
            commands::mobile::get_mobile_states,
            commands::mobile::list_mobile_devices,
            commands::mobile_settings::get_mobile_settings,
            commands::mobile_settings::save_mobile_settings,
            orchestration_loop::commands::get_multi_orchestration_loop_status,
            orchestration_loop::commands::get_orchestration_loop_status,
            orchestration_loop::commands::signal_orchestration_restart,
            orchestration_loop::commands::signal_orchestration_restart_by_id,
            orchestration_loop::commands::start_multi_orchestration_loop,
            orchestration_loop::commands::start_orchestration_loop,
            orchestration_loop::commands::stop_all_orchestration_loops,
            orchestration_loop::commands::stop_orchestration_loop,
            orchestration_loop::commands::stop_orchestration_loop_by_id,
            commands::orchestration_loop_configs::ol_delete_config,
            commands::orchestration_loop_configs::ol_get_config,
            commands::orchestration_loop_configs::ol_list_configs,
            commands::orchestration_loop_configs::ol_save_config,
            commands::orchestration_loop_configs::ol_toggle_favorite,
            commands::orchestration_loop_configs::ol_update_config,
            commands::otel_settings::get_otel_settings,
            commands::otel_settings::update_otel_settings,
            commands::performance_metrics::get_action_performance,
            commands::performance_metrics::get_element_resolution_metrics,
            commands::performance_metrics::get_performance_dashboard,
            commands::performance_metrics::get_success_rate_trend,
            commands::performance_metrics::get_transition_reliability,
            commands::playwright_settings::delete_playwright_test_password,
            commands::playwright_settings::get_playwright_settings,
            commands::playwright_settings::has_playwright_test_password,
            commands::playwright_settings::save_playwright_settings,
            process_capture::commands::delete_process_config,
            process_capture::commands::get_managed_processes,
            process_capture::commands::get_process_configs,
            process_capture::commands::get_process_log_context,
            process_capture::commands::get_process_output,
            process_capture::commands::get_process_session_output_from_db,
            process_capture::commands::get_process_sessions_from_db,
            process_capture::commands::rebuild_and_restart_process,
            process_capture::commands::restart_managed_process,
            process_capture::commands::save_process_config,
            process_capture::commands::search_process_logs,
            process_capture::commands::start_all_managed_processes,
            process_capture::commands::start_managed_process,
            process_capture::commands::stop_all_managed_processes,
            process_capture::commands::stop_managed_process,
            commands::productivity::acknowledge_advisory,
            commands::productivity::approve_recommendation,
            commands::productivity::archive_plan,
            commands::productivity::check_path_claims,
            commands::productivity::get_coordinator_decisions,
            commands::productivity::get_escalations,
            commands::productivity::get_plan_recommendations,
            commands::productivity::get_plan_tasks,
            commands::productivity::get_recommendations,
            commands::productivity::get_reflection,
            commands::productivity::get_task_detail,
            commands::productivity::get_upcoming_claims,
            commands::productivity::list_plans,
            commands::productivity::list_plans_filtered,
            commands::productivity::reject_recommendation,
            commands::productivity::resolve_escalation,
            commands::productivity::search_knowledge,
            commands::productivity::unarchive_plan,
            commands::project_logs::append_project_log,
            commands::project_logs::delete_project_config,
            commands::project_logs::get_project_directories,
            commands::project_logs::get_project_log_config,
            commands::project_logs::list_project_configs,
            commands::project_logs::read_log_source,
            commands::project_logs::read_project_logs,
            commands::project_logs::save_project_log_config,
            commands::rag::delete_rag_config,
            commands::rag::get_rag_config,
            commands::rag::get_rag_embedding_status,
            commands::rag::get_rag_storage_usage,
            commands::rag::import_rag_config,
            commands::rag::list_rag_configs,
            commands::rag::search_rag_elements,
            commands::rag::search_rag_elements_semantic,
            commands::rag::start_rag_processing,
            commands::recap::get_task_run_recap,
            commands::saved_projects::add_saved_project,
            commands::saved_projects::list_saved_projects,
            commands::saved_projects::remove_saved_project,
            commands::saved_projects::save_saved_projects,
            commands::screenshot::capture_and_upload_screenshot,
            commands::screenshot::capture_screenshot,
            commands::screenshot::capture_screenshot_via_python,
            commands::screenshot::get_screenshot_monitors,
            commands::screenshots::list_screenshots,
            commands::script_emitter::emit_extraction_script,
            commands::script_emitter::emit_scripted_output_event,
            commands::scripted_output_settings::get_scripted_output_settings,
            commands::scripted_output_settings::save_scripted_output_settings,
            commands::security_settings::get_security_profiles,
            commands::security_settings::get_security_settings,
            commands::security_settings::update_security_settings,
            commands::self_healing_settings::delete_self_healing_api_key,
            commands::self_healing_settings::get_self_healing_settings,
            commands::self_healing_settings::has_self_healing_api_key,
            commands::self_healing_settings::save_self_healing_api_key,
            commands::self_healing_settings::save_self_healing_settings,
            commands::setup_wizard::check_setup_completed,
            commands::setup_wizard::complete_setup,
            commands::setup_wizard::detect_project_framework_for_setup,
            commands::setup_wizard::discover_claude_config_dirs,
            commands::setup_wizard::save_ai_provider_from_setup,
            commands::setup_wizard::save_dev_services_from_setup,
            commands::setup_wizard::save_log_sources_from_setup,
            commands::setup_wizard::scan_workspace_for_setup,
            commands::setup_wizard::suggest_dev_services_for_setup,
            commands::setup_wizard::suggest_log_sources_for_setup,
            commands::setup_wizard::suggest_process_configs_for_setup,
            commands::setup_wizard::suggest_workspace_sources_for_setup,
            commands::shell_commands::create_shell_command,
            commands::shell_commands::delete_shell_command,
            commands::shell_commands::execute_shell_command,
            commands::shell_commands::generate_shell_command_with_ai,
            commands::shell_commands::get_shell_command,
            commands::shell_commands::get_shell_command_categories,
            commands::shell_commands::get_shell_command_results,
            commands::shell_commands::list_shell_commands,
            commands::shell_commands::set_shell_command_enabled,
            commands::shell_commands::update_shell_command,
            commands::page_spec_store::delete_page_spec,
            commands::page_spec_store::load_user_specs,
            commands::page_spec_store::save_page_spec,
            commands::spec_drift::scan_spec_drift,
            spec_experimentation::commands::analyze_cross_page_consistency,
            spec_experimentation::commands::analyze_spec_element_coverage,
            spec_experimentation::commands::analyze_spec_freshness,
            spec_experimentation::commands::detect_broken_spec_assertions,
            spec_experimentation::commands::diff_spec_json,
            spec_experimentation::commands::diff_spec_versions,
            spec_experimentation::commands::extract_spec_compliance,
            spec_experimentation::commands::get_spec_accuracy_results,
            spec_experimentation::commands::get_spec_compliance_history,
            spec_experimentation::commands::get_spec_compliance_summary,
            spec_experimentation::commands::get_spec_version_history,
            spec_experimentation::commands::get_specs_needing_attention,
            spec_experimentation::commands::run_spec_mutation_test,
            spec_experimentation::commands::snapshot_current_spec,
            commands::spec_sync_state::push_spec_sync_state,
            commands::state_explorer::clear_exploration_history,
            commands::state_explorer::get_exploration_analysis_prompt,
            commands::state_explorer::get_exploration_history,
            commands::state_explorer::get_exploration_report,
            commands::state_explorer::get_exploration_strategies,
            commands::state_explorer::preview_exploration_plan,
            commands::state_explorer::start_exploration,
            commands::state_machine::clear_action_log,
            commands::state_machine::execute_transition,
            commands::state_machine::get_action_log_view,
            commands::state_machine::get_active_states,
            commands::state_machine::get_available_transitions,
            commands::state_machine::navigate_to_multiple_states,
            commands::state_machine::navigate_to_state,
            commands::state_machine_configs::sm_audit_capture_screenshot_bounds,
            commands::state_machine_configs::sm_backfill_capture_screenshot_dimensions,
            commands::state_machine_configs::sm_create_config,
            commands::state_machine_configs::sm_create_state,
            commands::state_machine_configs::sm_create_transition,
            commands::state_machine_configs::sm_delete_capture_screenshots,
            commands::state_machine_configs::sm_delete_config,
            commands::state_machine_configs::sm_delete_state,
            commands::state_machine_configs::sm_delete_transition,
            commands::state_machine_configs::sm_generate_static,
            commands::state_machine_configs::sm_get_capture_screenshot_image,
            commands::state_machine_configs::sm_get_capture_screenshots,
            commands::state_machine_configs::sm_get_config,
            commands::state_machine_configs::sm_get_thumbnails,
            commands::state_machine_configs::sm_import_config,
            commands::state_machine_configs::sm_list_configs,
            commands::state_machine_configs::sm_move_pending_screenshots,
            commands::state_machine_configs::sm_save_capture_screenshots,
            commands::state_machine_configs::sm_save_thumbnails,
            commands::state_machine_configs::sm_update_config,
            commands::state_machine_configs::sm_update_state,
            commands::state_machine_configs::sm_update_transition,
            commands::step_outputs::collect_step_outputs,
            commands::step_outputs::get_step_outputs_for_test_builder,
            commands::storage::clear_all_storage,
            commands::storage::delete_old_sessions,
            commands::storage::get_local_storage_usage,
            commands::storage::get_storage_paths,
            commands::storage::load_findings_data,
            commands::storage::read_image_as_base64,
            commands::storage::save_findings_data,
            commands::storage::save_screenshot_to_disk,
            commands::storage::save_video_to_disk,
            commands::task_sync::full_sync_ai_task,
            commands::task_sync::sync_ai_findings,
            commands::task_sync::sync_ai_session_ended,
            commands::task_sync::sync_ai_session_started,
            commands::task_sync::sync_ai_task_completed,
            commands::task_sync::sync_ai_task_created,
            commands::task_sync::sync_all_pending_ai_tasks,
            commands::task_sync::sync_deferred_questions,
            commands::terminal::terminal_ack,
            commands::terminal::terminal_cleanup_scrollback,
            commands::terminal::terminal_close,
            commands::terminal::terminal_collect_session_metadata,
            commands::terminal::terminal_create,
            commands::terminal::terminal_get_buffer,
            commands::terminal::terminal_get_saved_scrollback,
            commands::terminal::terminal_list,
            commands::terminal::terminal_resize,
            commands::terminal::terminal_save_scrollback,
            commands::terminal::terminal_write,
            commands::terminal_analysis::analyze_architecture,
            commands::terminal_analysis::analyze_change_impact,
            commands::terminal_analysis::analyze_cross_tab,
            commands::terminal_analysis::analyze_page_architecture,
            commands::terminal_analysis::analyze_plan_progress,
            commands::terminal_analysis::analyze_session_summary,
            commands::terminal_analysis::get_latest_plan_content,
            commands::test_orchestrator::delete_orchestration_plan,
            commands::test_orchestrator::execute_test_orchestration,
            commands::test_orchestrator::generate_test_from_orchestration,
            commands::test_orchestrator::get_saved_requests_for_orchestration,
            commands::test_orchestrator::list_orchestration_plans,
            commands::test_orchestrator::plan_test_orchestration,
            commands::test_orchestrator::save_orchestration_plan,
            commands::testing::analyze_page_playwright,
            commands::testing::analyze_page_playwright_script,
            commands::testing::analyze_page_vision,
            commands::testing::create_test_association,
            commands::testing::create_verification_test,
            commands::testing::delete_test_association,
            commands::testing::delete_verification_test,
            commands::testing::execute_test_by_id,
            commands::testing::execute_tests_by_ids,
            commands::testing::execute_verification_test,
            commands::testing::execute_verification_test_suite,
            commands::testing::export_all_tests_to_file,
            commands::testing::export_tests_to_file,
            commands::testing::generate_test_metadata,
            commands::testing::generate_test_with_ai,
            commands::testing::get_config_test_associations,
            commands::testing::get_task_run_test_results,
            commands::testing::get_test_results,
            commands::testing::get_test_type_info,
            commands::testing::get_verification_test,
            commands::testing::get_workflow_run_context,
            commands::testing::import_tests_from_file,
            commands::testing::list_recent_task_runs,
            commands::testing::list_verification_tests,
            commands::testing::update_verification_test,
            commands::testing::validate_test_definition,
            commands::tiered_info::cleanup_old_runs,
            commands::tiered_info::delete_ai_session,
            commands::tiered_info::get_ai_session_history,
            commands::tiered_info::get_config_statistics,
            commands::tiered_info::get_debugging_context,
            commands::tiered_info::get_debugging_context_prompt,
            commands::tiered_info::get_execution_options,
            commands::tiered_info::get_failed_runs,
            commands::tiered_info::get_flakiness_summary,
            commands::tiered_info::get_flaky_templates,
            commands::tiered_info::get_flaky_transitions,
            commands::tiered_info::get_recent_runs,
            commands::tiered_info::get_run_details,
            commands::tiered_info::record_run,
            commands::token_analytics::get_cost_by_model,
            commands::token_analytics::get_cost_by_phase,
            commands::token_analytics::get_cost_by_target_app,
            commands::token_analytics::get_daily_cost,
            commands::token_analytics::get_provider_latency,
            commands::token_analytics::get_task_run_costs,
            commands::token_analytics::get_token_usage_summary,
            commands::transcript::generate_workflow_standalone,
            commands::transcript::transcript_find_external_processes,
            commands::transcript::transcript_get_latest,
            commands::transcript::transcript_list_sessions,
            commands::transcript::transcript_read_session,
            commands::transcript::transcript_session_digests,
            commands::ui_bridge::ui_bridge_discover,
            commands::ui_bridge::ui_bridge_discover_states_from_fingerprints,
            commands::ui_bridge::ui_bridge_discover_states_native,
            commands::ui_bridge::ui_bridge_execute_action,
            commands::ui_bridge::ui_bridge_execute_component_action,
            commands::ui_bridge::ui_bridge_get_component,
            commands::ui_bridge::ui_bridge_get_components,
            commands::ui_bridge::ui_bridge_get_element,
            commands::ui_bridge::ui_bridge_get_elements,
            commands::ui_bridge::ui_bridge_get_snapshot,
            commands::ui_bridge::ui_bridge_reload_webview,
            commands::ui_bridge::ui_bridge_run_exploration,
            commands::ui_bridge::ui_bridge_run_exploration_native,
            commands::ui_bridge::ui_bridge_stop_exploration,
            commands::ui_bridge::ui_bridge_stop_exploration_native,
            commands::ui_bridge_baselines::sm_delete_baseline,
            commands::ui_bridge_baselines::sm_get_baseline,
            commands::ui_bridge_baselines::sm_list_baselines,
            commands::ui_bridge_baselines::sm_save_baseline,
            ui_error::clear_ui_error,
            ui_error::get_ui_error,
            ui_error::report_ui_error,
            commands::verification::clear_pending_verification,
            commands::verification::load_pending_verification,
            commands::verification::save_pending_verification,
            commands::verification::update_verification_status,
            commands::video::get_video_recording_status,
            commands::video::start_video_recording,
            commands::video::stop_video_recording,
            commands::watchers::create_watcher,
            commands::watchers::delete_watcher,
            commands::watchers::get_watcher,
            commands::watchers::list_watchers,
            commands::watchers::set_watcher_enabled,
            commands::watchers::update_watcher,
            commands::web_integration::get_web_integration_status,
            commands::web_integration::save_web_integration_settings,
            commands::web_integration::start_web_token_flow,
            commands::web_integration::test_web_integration_connection,
            commands::window_manager::activate_system_window,
            commands::window_manager::list_system_windows,
            commands::workflow_events::emit_workflow_event,
            commands::worktrees::merge_worktree,
            commands::worktrees::merge_worktree_force
        ])
        .manage(shared_app_state)
        .manage(launch_env.clone()) // Item 3: typed startup env snapshot, read once in run_app()
        .manage(bridge_compartment)
        .manage(execution_compartment)
        .manage(integration_compartment)
        .manage(health_compartment)
        .manage(storage_compartment)
        .manage(log_api_store)
        .manage(rag_state)
        .manage(instance_manager) // For multi-instance management (dev feature)
        .manage(session_manager) // For interactive AI session commands
        .manage(terminal_manager) // For embedded PTY terminal sessions
        .manage(tokio::sync::Mutex::new(
            qontinui_runner_lib::accessibility::AccessibilityManager::default(),
        )) // Native cross-platform accessibility API
        .manage(std::sync::Arc::new(
            commands::spec_sync_state::SpecSyncStateBus::new(),
        )) // P2 SSE remediation: useSpecSync progress mirror, consumed by /ui-bridge/sdk/spec-sync/{status,stream}
        // Tauri command handlers are registered via the central
        // `tauri::generate_handler![...]` block above. Each command module
        // (`commands/*.rs` and a few subsystem `commands.rs` files) ALSO
        // exposes a `pub fn plugin<R: Runtime>() -> TauriPlugin<R>` for
        // future plugin-based registration, but those are not currently
        // wired — Tauri 2's plugin path requires `plugin:<name>|<cmd>`
        // invoke prefixes and the frontend invokes commands bare, so the
        // plugin form was rolled back to the central handler at commit
        // `1f1d807f6`. The `plugin()` fns are kept as ready-to-go scaffolding
        // for any future migration that updates the frontend invoke sites.
        .setup(|app| {
            info!("Tauri application setup starting");

            // Phase F.1 — register the deep-link `on_open_url` callback so
            // `qontinui://wake?intent=...` URLs delivered by the OS (cold
            // start) or forwarded by the single-instance plugin (warm start)
            // route into the wake handler, which fires an immediate scheduler
            // tick.
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let app_handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    let urls: Vec<String> =
                        event.urls().into_iter().map(|u| u.to_string()).collect();
                    wake_handler::handle_deep_link_urls(app_handle.clone(), urls);
                });
            }

            // Pull the typed startup-env snapshot (managed onto state above)
            // so this closure can read launch env vars without re-parsing.
            let setup_launch_env: tauri::State<launch_env::SharedLaunchEnv> = app.state();
            let setup_launch_env = setup_launch_env.inner().clone();

            let server_mode = setup_launch_env.server_mode;
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

                let data_dir: Option<String> = setup_launch_env
                    .webview2_user_data_dir
                    .as_ref()
                    .and_then(|p| p.to_str().map(|s| s.to_string()));
                let is_secondary = instance::is_secondary();

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
                    let window_hints = &setup_launch_env.window;
                    let env_pos = match (window_hints.x, window_hints.y) {
                        (Some(x), Some(y)) => Some((x as f64, y as f64)),
                        _ => None,
                    };
                    let env_size = match (window_hints.width, window_hints.height) {
                        (Some(w), Some(h)) => Some((w as f64, h as f64)),
                        _ => None,
                    };
                    let initial_size = env_size.unwrap_or((1400.0, 800.0));
                    // QONTINUI_WINDOW_DECORATIONS=0 forces a borderless
                    // window; default is true (Tauri's default chrome).
                    // Borderless lets a placement land flush with the
                    // monitor's edge — the few-pixel right-of-edge inset
                    // people see with chrome on is the OS window border.
                    let env_decorations = window_hints.decorations;

                    let mut builder = tauri::WebviewWindowBuilder::new(app, "main", url)
                        .title("Qontinui Runner")
                        .inner_size(initial_size.0, initial_size.1)
                        .min_inner_size(1200.0, 700.0)
                        .fullscreen(false)
                        .resizable(true)
                        .decorations(env_decorations.unwrap_or(true))
                        // Phase P2.2 of `tmp_plans/sw-cache-invalidation.md`:
                        // mark the embedded index.html as `no-store` so a
                        // webview that survives a binary swap can't serve a
                        // stale shell whose <script src> tags point at
                        // hashed asset filenames the new bundle no longer
                        // contains. Hashed `/assets/*` responses pass
                        // through with their default headers.
                        .on_web_resource_request(asset_headers::stamp_no_store_on_index);

                    if let Some(ref dir) = data_dir {
                        builder = builder.data_directory(std::path::PathBuf::from(dir));
                    }

                    if let Some((x, y)) = env_pos {
                        info!(
                            "Positioning runner window via env vars at ({}, {}) size ({}, {})",
                            x, y, initial_size.0, initial_size.1
                        );
                        builder = builder.position(x, y);
                    } else if is_secondary {
                        builder = builder.position(100.0, 100.0);
                    } else {
                        builder = builder.maximized(true);
                    }

                    match builder.build() {
                        Ok(win) => {
                            // tauri-plugin-window-state restores saved size/position
                            // on window creation, which overrides .position()/.inner_size()
                            // from the builder. When the supervisor explicitly tells us
                            // where to land via env vars, force-apply those after build
                            // to win against the plugin's restore.
                            if let Some((x, y)) = env_pos {
                                let pos = tauri::PhysicalPosition::new(x as i32, y as i32);
                                if let Err(e) = win.set_position(pos) {
                                    warn!("Failed to apply env-var window position: {}", e);
                                }
                                let size = tauri::PhysicalSize::new(
                                    initial_size.0 as u32,
                                    initial_size.1 as u32,
                                );
                                if let Err(e) = win.set_size(size) {
                                    warn!("Failed to apply env-var window size: {}", e);
                                }
                            }
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

            // Rehydrate the productivity-stack upcoming-file registry from
            // PG. The registry is in-memory, so without this any Coordinator
            // / dispatcher lookups in the first few seconds after restart
            // would miss claims attached to non-terminal tasks. See
            // productivity-stack plan §3 "Persistence".
            {
                let upcoming_state: Arc<AppState> =
                    app.state::<Arc<AppState>>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    upcoming_state
                        .upcoming_file_registry
                        .rehydrate_from_pg(&upcoming_state.pg_db)
                        .await;
                });
            }

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

            // Phase 3 — web-backend integration is now driven entirely by
            // the unified WebSocket relay (`crate::mcp::backend_relay`).
            // The relay is launched from `mcp_api::start_server` once
            // `Arc<ApiState>` is available; it reads `WebIntegrationSettings`
            // on every reconnect and observes `ServerModeState::shutdown()`
            // for hot-reload. There is no separate HTTP register/heartbeat
            // loop in this phase.

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
            if instance::is_secondary() {
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
                let is_secondary = instance::is_secondary();
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

            // Start the productivity-stack session-file-snapshot pruner.
            // 24h interval; 7-day retention. See
            // `database/pg/session_file_snapshots.rs::SNAPSHOT_RETENTION_DAYS`.
            info!("Starting session-file-snapshot pruner");
            let snapshot_pruner_pg = app.state::<Arc<AppState>>().inner().pg_db.clone();
            crate::database::pg::session_file_snapshots::start_session_snapshot_pruner(
                snapshot_pruner_pg,
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
            if !instance::is_secondary() {
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

                            match im
                                .launch_instance_with_app(config, Some(&app_handle_for_restore))
                                .await
                            {
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
