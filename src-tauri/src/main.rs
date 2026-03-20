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
mod autoresearch;
mod ai_pricing;
mod ai_provider;
mod ai_router;
mod ai_workflows;
mod api_request;
mod auth;
mod backup;
mod check_executor;
mod check_generation;
mod claude_protocol;
mod claude_session;
mod commands;
mod config;
mod config_facade;
mod config_storage;
mod constraint_engine;
mod context;
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
mod follow_up;
mod meta_optimizer;
mod health_monitor;
mod heartbeat;
mod instance_manager;
mod iteration_bundle;
#[cfg(windows)]
mod job_object;
mod known_issues;
mod log_consolidation;
mod log_migration;
mod logging;
mod macros;
mod mcp;
mod mcp_api;
mod mcp_client;
mod mcp_embedded;
mod middleware;
mod orchestration_loop;
mod orchestration_loop_configs;
mod orchestrator;
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
mod scheduler;
mod scheduler_service;
mod secure_storage;
mod semantic_conventions;
mod settings;
mod skills;
mod spec_utils;
mod state_explorer;
mod state_machine_configs;
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
mod unified_ai_session;
mod unified_workflow_executor;
mod unified_workflows;
mod video_recorder;
mod comparison;
mod workflow_generation;
mod workflow_queue;
mod workflow_state;
mod worktree;
mod zombie_sweep;

use commands::AppState;
use database::CheckpointDb;
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
    let logging_result = init_logging(LoggingConfig::default())?;
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

    // Initialize Checkpoint Database
    let checkpoint_db =
        Arc::new(CheckpointDb::new().expect("Failed to initialize checkpoint database"));
    info!(
        "Checkpoint database initialized at {:?}",
        checkpoint_db.path()
    );

    // Connect the SQLite span layer to the database pool
    if let Some(ref sqlite_layer) = logging_result.sqlite_span_layer {
        sqlite_layer.set_db_pool(checkpoint_db.get_pool());
        info!("SQLite span layer connected to database");
    }

    // Run one-time migration from JSON files (if any exist)
    match checkpoint_db.migrate_from_json_files() {
        Ok(result) => {
            if result.total_migrated() > 0 {
                info!(
                    "Migrated {} items from JSON files to database (settings: {}, prompts: {}, scheduler: {})",
                    result.total_migrated(),
                    result.settings_migrated,
                    result.prompts_migrated,
                    result.scheduler_tasks_migrated
                );
            }
            if !result.errors.is_empty() {
                for err in &result.errors {
                    warn!("Migration warning: {}", err);
                }
            }
        }
        Err(e) => {
            warn!("JSON migration failed (non-fatal): {}", e);
        }
    }

    // Ensure seed quality rules are present in the generation_rules table
    if let Ok(conn) = checkpoint_db.get_conn_string() {
        workflow_generation::rules::ensure_seed_rules(&conn);
    }

    // Repair check-group associations based on naming convention
    // This ensures checks named "project - tool" are properly linked to groups named "project"
    match checkpoint_db.repair_check_group_associations() {
        Ok(count) if count > 0 => {
            info!("Repaired {} check-group associations on startup", count);
        }
        Ok(_) => {
            // No repairs needed
        }
        Err(e) => {
            warn!("Check-group association repair failed (non-fatal): {}", e);
        }
    }

    // Migrate plaintext API keys to secure keychain storage
    if let Err(e) = config_facade::migrate_api_keys_to_keychain() {
        warn!("API key migration to keychain failed (non-fatal): {}", e);
    }

    // Auto-cache architecture specs from local .architecture.uibridge.json files
    {
        let db_for_arch = checkpoint_db.clone();
        std::thread::spawn(move || {
            let cwd = match std::env::current_dir() {
                Ok(p) => p,
                Err(e) => {
                    info!(
                        "Could not get working directory for architecture spec scan: {}",
                        e
                    );
                    return;
                }
            };

            let mut dirs_to_scan: Vec<std::path::PathBuf> = vec![cwd.clone()];

            // Also scan parent directory (Tauri runs from src-tauri/, spec may be in project root)
            if let Some(parent) = cwd.parent() {
                dirs_to_scan.push(parent.to_path_buf());

                // Also scan grandparent's immediate children (sibling projects)
                // e.g. cwd = qontinui-runner/src-tauri → grandparent = qontinui-root
                if let Some(grandparent) = parent.parent() {
                    if let Ok(entries) = std::fs::read_dir(grandparent) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.is_dir() && path != cwd && path != parent.to_path_buf() {
                                dirs_to_scan.push(path);
                            }
                        }
                    }
                }
            }

            let mut cached_count = 0;
            for dir in &dirs_to_scan {
                if let Ok(entries) = std::fs::read_dir(dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        if !file_name.ends_with(".architecture.uibridge.json") {
                            continue;
                        }

                        // Check file size before reading (10MB limit)
                        const MAX_SPEC_FILE_SIZE: u64 = 10 * 1024 * 1024;
                        match std::fs::metadata(&path) {
                            Ok(meta) => {
                                if meta.len() > MAX_SPEC_FILE_SIZE {
                                    warn!(
                                        "Skipping architecture spec file {} — size {} bytes exceeds 10MB limit",
                                        path.display(),
                                        meta.len()
                                    );
                                    continue;
                                }
                            }
                            Err(e) => {
                                warn!("Failed to read metadata for {}: {}", path.display(), e);
                                continue;
                            }
                        }

                        let content = match std::fs::read_to_string(&path) {
                            Ok(c) => c,
                            Err(e) => {
                                warn!(
                                    "Failed to read architecture spec file {}: {}",
                                    path.display(),
                                    e
                                );
                                continue;
                            }
                        };

                        // Validate it has techStack + features
                        let parsed: serde_json::Value = match serde_json::from_str(&content) {
                            Ok(v) => v,
                            Err(e) => {
                                warn!(
                                    "Failed to parse architecture spec file {}: {}",
                                    path.display(),
                                    e
                                );
                                continue;
                            }
                        };
                        if !crate::spec_utils::is_architecture_spec(&parsed) {
                            continue;
                        }

                        let dir_name = dir
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown");
                        let dir_path_str = dir.to_string_lossy().replace('\\', "/");
                        let app_url = format!("file://{}", dir_path_str);
                        let spec_id = format!("{}.architecture", dir_name);

                        if let Err(e) = db_for_arch
                            .upsert_cached_spec(&app_url, dir_name, &spec_id, &content, None)
                        {
                            warn!(
                                "Failed to cache architecture spec from {}: {}",
                                path.display(),
                                e
                            );
                        } else {
                            cached_count += 1;
                        }
                    }
                }
            }

            if cached_count > 0 {
                info!(
                    "Auto-cached {} architecture spec(s) from local files",
                    cached_count
                );
            }
        });
    }

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
    let run_recording_handler = Arc::new(RunRecordingHandler::new(checkpoint_db.clone()));

    // Create MCP client manager for calling external MCP servers
    let mcp_client_manager = mcp_client::McpClientManager::new(checkpoint_db.clone());

    // Create instance manager for multi-instance dev workflows
    let instance_manager = Arc::new(instance_manager::InstanceManager::new());

    // Create session manager for interactive Claude CLI sessions
    let session_manager = Arc::new(claude_session::SessionManager::new());

    // Create terminal manager for embedded PTY terminals
    let terminal_manager = Arc::new(terminal::TerminalManager::new());

    // Create shared AppState for both Tauri and MCP API
    // Create shared SDK connection for UI Bridge (shared between AppState and ApiState)
    let shared_sdk_connection = Arc::new(TokioMutex::new(
        crate::mcp::sdk_client::SdkConnectionManager::new(),
    ));

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
        checkpoint_db: checkpoint_db.clone(),
        run_recording_handler,
        mcp_client_manager: tokio::sync::Mutex::new(mcp_client_manager),
        error_monitor_handle: TokioMutex::new(None), // Initialized in setup()
        doctor_handle: TokioMutex::new(None),        // Initialized in setup()
        url_lock_manager: Arc::new(crate::executor::UrlLockManager::new()),
        ui_bridge_failure_tracker:
            crate::step_executor::handlers::ui_bridge::UiBridgeFailureTracker::new(),
        process_capture_manager: TokioMutex::new(None), // Initialized in setup()
        api_ready: AtomicBool::new(false),              // Set when MCP API server binds
        api_port: AtomicU16::new(crate::mcp::types::get_mcp_api_port()), // Updated when server binds
        ai_pid_tracker: Arc::new(std::sync::Mutex::new(Vec::new())),
        canvas_state: Arc::new(tokio::sync::RwLock::new(
            crate::mcp::canvas::CanvasState::new(),
        )),
        orchestration_loop: std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::orchestration_loop::loop_engine::LoopState::new(),
        )),
    });
    let mcp_app_state = shared_app_state.clone();
    let mcp_rag_state = rag_state.clone();
    let heartbeat_app_state = shared_app_state.clone();

    // Create error monitor config for later initialization
    let error_monitor_db = checkpoint_db.clone();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(shared_app_state)
        .manage(rag_state)
        .manage(instance_manager) // For multi-instance management (dev feature)
        .manage(session_manager) // For interactive AI session commands
        .manage(terminal_manager) // For embedded PTY terminal sessions
        .manage(checkpoint_db.clone()) // For error_monitor commands that need direct db access
        .manage(std::sync::Arc::new(
            tokio::sync::Mutex::new(autoresearch::engine::ResearchEngine::new()),
        ) as autoresearch::commands::SharedResearchEngine) // Autoresearch experiment engine
        .invoke_handler(tauri::generate_handler![
            // Interactive AI session commands (send messages, interrupt, query state)
            commands::ai_session::list_ai_sessions,
            commands::ai_session::send_user_message,
            commands::ai_session::interrupt_ai_session,
            commands::ai_session::get_ai_session_state,
            commands::ai_session::create_ai_session,
            commands::ai_session::close_ai_session,
            commands::ai_session::rename_ai_session,
            commands::ai_session::get_ai_output,
            commands::ai_session::generate_workflow_from_session,
            // Authentication commands
            commands::auth::login,
            commands::auth::logout,
            commands::auth::check_auth_status,
            commands::auth::get_device_info,
            commands::auth::get_connection_info,
            commands::auth::get_user_projects,
            commands::auth::refresh_token,
            commands::auth::get_access_token_for_websocket,
            commands::auth::send_device_heartbeat,
            commands::auth::is_api_ready,
            commands::auth::get_api_port,
            // Configuration commands
            commands::config::load_configuration,
            commands::config::get_current_configuration,
            commands::config::get_last_config_path,
            commands::config::save_last_workflow_id,
            commands::config::save_last_monitor_index,
            commands::config::save_last_monitor_indices,
            commands::config::get_auto_load_last_config,
            commands::config::save_auto_load_last_config,
            commands::config::get_include_summary_step_by_default,
            commands::config::save_include_summary_step_by_default,
            commands::config::get_workspace_paths,
            commands::config::get_claude_config_dirs,
            commands::config::save_claude_config_dirs,
            // Dataset commands
            commands::dataset::scan_local_images,
            commands::dataset::package_dataset,
            // Execution commands - python_executor
            commands::execution::python_executor::start_python_executor,
            commands::execution::python_executor::stop_python_executor,
            commands::execution::python_executor::update_capture_settings,
            // Execution commands - workflow_execution
            commands::execution::workflow_execution::start_execution,
            commands::execution::workflow_execution::stop_execution,
            commands::execution::workflow_execution::pause_execution,
            commands::execution::workflow_execution::resume_execution,
            commands::execution::workflow_execution::get_resolved_initial_states,
            commands::execution::workflow_execution::get_workflow_required_screens,
            // Execution commands - bridge_execution
            commands::execution::bridge_execution::run_workflow_on_bridge,
            commands::execution::bridge_execution::transfer_gui_lock,
            commands::execution::bridge_execution::list_bridges,
            commands::execution::bridge_execution::get_bridge_info,
            // Execution commands - executor_status
            commands::execution::executor_status::get_executor_status,
            commands::execution::executor_status::get_monitors,
            commands::execution::executor_status::set_input_capture_enabled,
            commands::execution::executor_status::get_input_validation_status,
            // Execution commands - system_ops
            commands::execution::system_ops::handle_error,
            commands::execution::system_ops::check_for_updates,
            commands::execution::system_ops::install_update,
            commands::execution::system_ops::open_folder,
            // State machine commands
            commands::state_machine::execute_transition,
            commands::state_machine::navigate_to_state,
            commands::state_machine::navigate_to_multiple_states,
            commands::state_machine::get_active_states,
            commands::state_machine::get_available_transitions,
            commands::state_machine::get_action_log_view,
            commands::state_machine::clear_action_log,
            // Debug commands
            commands::debug::get_debug_settings,
            commands::debug::set_debug_settings,
            // WebSocket commands
            commands::websocket::configure_websocket,
            commands::websocket::connect_websocket,
            commands::websocket::disconnect_websocket,
            // Video commands
            commands::video::start_video_recording,
            commands::video::stop_video_recording,
            commands::video::get_video_recording_status,
            // Interaction recording commands (video + input capture for State Machine creation)
            commands::interaction::start_interaction_recording,
            commands::interaction::stop_interaction_recording,
            commands::interaction::get_interaction_recording_status,
            // Storage commands
            commands::storage::save_screenshot_to_disk,
            commands::storage::save_video_to_disk,
            commands::storage::get_local_storage_usage,
            commands::storage::delete_old_sessions,
            commands::storage::clear_all_storage,
            commands::storage::get_storage_paths,
            commands::storage::read_image_as_base64,
            commands::storage::save_findings_data,
            commands::storage::load_findings_data,
            // Web extraction commands
            commands::extraction::start_web_extraction,
            commands::extraction::start_vision_extraction,
            commands::extraction::stop_web_extraction,
            commands::extraction::get_extraction_status,
            commands::extraction::request_extraction_screenshot,
            commands::extraction::export_training_data,
            commands::extraction::export_state_structure,
            commands::extraction::list_extractions,
            // Web extraction backend integration commands
            commands::extraction::create_extraction_session,
            commands::extraction::update_extraction_session,
            commands::extraction::upload_extraction_annotations,
            commands::extraction::upload_state_structure,
            commands::extraction::get_project_extractions,
            // Screenshot capture commands
            commands::screenshot::get_screenshot_monitors,
            commands::screenshot::capture_screenshot,
            commands::screenshot::capture_and_upload_screenshot,
            commands::screenshot::capture_screenshot_via_python,
            // Logging commands
            commands::logging::append_ai_output_log,
            commands::logging::clear_ai_output_log,
            commands::logging::get_ai_output_log_path_cmd,
            commands::logging::load_ai_output_log,
            commands::logging::list_session_checkpoints,
            commands::logging::delete_session_checkpoints,
            commands::logging::clear_all_run_history,
            // Render logging commands (for UI testing)
            commands::logging::append_render_log,
            commands::logging::clear_render_log,
            commands::logging::load_render_log,
            commands::logging::get_render_log_path_cmd,
            // Verification commands (AI self-healing)
            commands::verification::save_pending_verification,
            commands::verification::load_pending_verification,
            commands::verification::clear_pending_verification,
            commands::verification::update_verification_status,
            // RAG commands
            commands::rag::list_rag_configs,
            commands::rag::get_rag_config,
            commands::rag::delete_rag_config,
            commands::rag::import_rag_config,
            commands::rag::search_rag_elements,
            commands::rag::search_rag_elements_semantic,
            commands::rag::get_rag_embedding_status,
            commands::rag::get_rag_storage_usage,
            commands::rag::start_rag_processing,
            // Project logs commands
            commands::project_logs::get_project_log_config,
            commands::project_logs::save_project_log_config,
            commands::project_logs::list_project_configs,
            commands::project_logs::delete_project_config,
            commands::project_logs::read_log_source,
            commands::project_logs::read_project_logs,
            commands::project_logs::get_project_directories,
            commands::project_logs::append_project_log,
            // AI log source discovery (global)
            commands::global_log_sources::find_log_sources_with_ai,
            // AI settings commands
            commands::ai_settings::get_ai_settings,
            commands::ai_settings::save_ai_settings,
            commands::ai_settings::save_gemini_settings,
            commands::ai_settings::save_ai_api_key_command,
            commands::ai_settings::delete_ai_api_key_command,
            commands::ai_settings::has_ai_api_key,
            commands::ai_settings::test_ai_connection,
            commands::ai_settings::check_claude_cli_auth,
            commands::ai_settings::check_accounts_usage,
            commands::ai_settings::refresh_claude_cli_auth,
            commands::ai_settings::get_agentic_settings,
            commands::ai_settings::save_agentic_settings,
            // Accessibility commands
            commands::accessibility::get_accessibility_settings,
            commands::accessibility::save_accessibility_settings,
            commands::accessibility::launch_chrome_debug,
            commands::accessibility::check_chrome_available,
            // Playwright settings commands
            commands::playwright_settings::get_playwright_settings,
            commands::playwright_settings::save_playwright_settings,
            commands::playwright_settings::has_playwright_test_password,
            commands::playwright_settings::delete_playwright_test_password,
            // Self-healing settings commands
            commands::self_healing_settings::get_self_healing_settings,
            commands::self_healing_settings::save_self_healing_settings,
            commands::self_healing_settings::save_self_healing_api_key,
            commands::self_healing_settings::delete_self_healing_api_key,
            commands::self_healing_settings::has_self_healing_api_key,
            // Execution variables settings commands
            commands::execution_variables::get_execution_variables_settings,
            commands::execution_variables::save_execution_variables_settings,
            commands::execution_variables::get_resolved_execution_context,
            commands::execution_variables::test_env_var,
            // Issues sync commands
            commands::issues::sync_issues_to_backend,
            // Unified execution reporting commands
            commands::execution_reporting::create_execution_run,
            commands::execution_reporting::report_action_executions,
            commands::execution_reporting::upload_execution_screenshot,
            commands::execution_reporting::report_execution_issues,
            commands::execution_reporting::complete_execution_run,
            // Checkpoint/session commands (SQLite database)
            commands::checkpoints::checkpoint_get,
            commands::checkpoints::checkpoint_save,
            commands::checkpoints::checkpoint_delete,
            commands::checkpoints::checkpoint_list_active,
            commands::checkpoints::checkpoint_status,
            commands::checkpoints::checkpoint_history,
            commands::checkpoints::session_create,
            commands::checkpoints::session_update_status,
            commands::checkpoints::setting_get,
            commands::checkpoints::setting_set,
            commands::checkpoints::settings_get_all,
            // Findings commands (AI-detected issues)
            commands::findings::get_task_findings,
            commands::findings::get_findings_by_status_cmd,
            commands::findings::get_finding_by_id,
            commands::findings::update_finding,
            commands::findings::resolve_finding,
            commands::findings::provide_finding_response,
            commands::findings::get_findings_summary,
            commands::findings::list_task_knowledge_cmd,
            // Known Issues Registry commands
            commands::known_issues::list_known_issues,
            commands::known_issues::find_issues_for_spec,
            commands::known_issues::create_known_issue,
            commands::known_issues::update_known_issue,
            commands::known_issues::delete_known_issue,
            commands::known_issues::resolve_known_issue,
            commands::known_issues::list_pattern_templates,
            commands::known_issues::create_pattern_template,
            commands::known_issues::export_known_issues,
            commands::known_issues::import_known_issues,
            // State Machine Config Builder commands (CRUD for configs, states, transitions)
            commands::state_machine_configs::sm_list_configs,
            commands::state_machine_configs::sm_get_config,
            commands::state_machine_configs::sm_create_config,
            commands::state_machine_configs::sm_update_config,
            commands::state_machine_configs::sm_delete_config,
            commands::state_machine_configs::sm_create_state,
            commands::state_machine_configs::sm_update_state,
            commands::state_machine_configs::sm_delete_state,
            commands::state_machine_configs::sm_create_transition,
            commands::state_machine_configs::sm_update_transition,
            commands::state_machine_configs::sm_delete_transition,
            commands::state_machine_configs::sm_import_config,
            commands::state_machine_configs::sm_save_thumbnails,
            commands::state_machine_configs::sm_get_thumbnails,
            commands::state_machine_configs::sm_save_capture_screenshots,
            commands::state_machine_configs::sm_get_capture_screenshots,
            commands::state_machine_configs::sm_get_capture_screenshot_image,
            commands::state_machine_configs::sm_move_pending_screenshots,
            commands::state_machine_configs::sm_delete_capture_screenshots,
            // State Explorer commands
            commands::state_explorer::start_exploration,
            commands::state_explorer::get_exploration_strategies,
            commands::state_explorer::preview_exploration_plan,
            commands::state_explorer::get_exploration_history,
            commands::state_explorer::get_exploration_report,
            commands::state_explorer::get_exploration_analysis_prompt,
            commands::state_explorer::clear_exploration_history,
            // Tiered Information Model commands
            commands::tiered_info::get_config_statistics,
            commands::tiered_info::get_flaky_transitions,
            commands::tiered_info::get_flaky_templates,
            commands::tiered_info::get_debugging_context,
            commands::tiered_info::get_debugging_context_prompt,
            commands::tiered_info::get_run_details,
            commands::tiered_info::get_recent_runs,
            commands::tiered_info::get_failed_runs,
            commands::tiered_info::record_run,
            commands::tiered_info::cleanup_old_runs,
            commands::tiered_info::get_execution_options,
            commands::tiered_info::get_flakiness_summary,
            // AI Session History commands (for Runs page)
            commands::tiered_info::get_ai_session_history,
            commands::tiered_info::delete_ai_session,
            // Discovery Push commands
            commands::discoveries::get_pending_discoveries_cmd,
            commands::discoveries::get_discovery_summary,
            commands::discoveries::sync_discoveries,
            commands::discoveries::clear_discovery,
            commands::discoveries::clear_failed_discoveries,
            commands::discoveries::get_discovery_sync_status,
            // Context commands (AI knowledge snippets)
            commands::context::get_all_contexts,
            commands::context::get_context,
            commands::context::create_context,
            commands::context::update_context,
            commands::context::delete_context,
            commands::context::search_contexts,
            commands::context::get_context_categories,
            commands::context::set_context_enabled,
            commands::context::record_context_usage,
            commands::context::get_builtin_contexts_cmd,
            commands::context::evaluate_auto_include,
            // AI Data Viewer commands
            commands::ai_data::get_task_runs_for_viewer,
            commands::ai_data::get_task_run_for_viewer,
            commands::ai_data::read_jsonl_logs_for_viewer,
            commands::ai_data::read_jsonl_logs_for_task_run,
            commands::ai_data::get_consolidated_ai_output,
            commands::ai_data::get_jsonl_logs_summary,
            commands::ai_data::reopen_task_run,
            commands::ai_data::read_text_logs_for_viewer,
            commands::ai_data::get_text_logs_summary,
            commands::ai_data::get_screenshots_for_viewer,
            commands::ai_data::get_loaded_config_for_viewer,
            commands::ai_data::get_ai_prompts_for_viewer,
            commands::ai_data::get_contexts_for_viewer,
            // AI Data Viewer - SQLite queries (migrated logs)
            commands::ai_data::get_task_run_events_from_db,
            commands::ai_data::get_task_run_screenshots_from_db,
            commands::ai_data::get_task_run_playwright_results_from_db,
            commands::ai_data::get_task_run_migrated_logs_summary,
            commands::ai_data::get_task_run_api_requests_from_db,
            commands::ai_data::get_task_run_awas_steps_from_db,
            commands::ai_data::get_task_run_verification_results_from_db,
            commands::ai_data::get_task_run_context,
            // Recap commands (Session overview)
            commands::recap::get_task_run_recap,
            // Task Sync commands (sync to qontinui-web)
            commands::task_sync::sync_ai_task_created,
            commands::task_sync::sync_ai_session_started,
            commands::task_sync::sync_ai_session_ended,
            commands::task_sync::sync_ai_findings,
            commands::task_sync::sync_ai_task_completed,
            commands::task_sync::full_sync_ai_task,
            commands::task_sync::sync_all_pending_ai_tasks,
            // Library Sync commands (sync library items to qontinui-web)
            commands::library_sync::sync_library_to_backend,
            commands::library_sync::sync_checks_to_backend,
            commands::library_sync::sync_check_groups_to_backend,
            commands::library_sync::sync_shell_commands_to_backend,
            commands::library_sync::sync_api_requests_to_backend,
            commands::library_sync::sync_contexts_to_backend,
            commands::library_sync::sync_macros_to_backend,
            commands::library_sync::sync_prompt_snippets_to_backend,
            // Verification testing commands
            commands::testing::execute_verification_test,
            commands::testing::execute_verification_test_suite,
            commands::testing::get_test_type_info,
            commands::testing::validate_test_definition,
            // Verification test database CRUD commands
            commands::testing::list_verification_tests,
            commands::testing::get_verification_test,
            commands::testing::create_verification_test,
            commands::testing::update_verification_test,
            commands::testing::delete_verification_test,
            // Verification test execution with database integration
            commands::testing::execute_test_by_id,
            commands::testing::execute_tests_by_ids,
            // Test result commands
            commands::testing::get_test_results,
            commands::testing::get_task_run_test_results,
            // Test association commands
            commands::testing::create_test_association,
            commands::testing::get_config_test_associations,
            commands::testing::delete_test_association,
            // Test import/export commands
            commands::testing::export_tests_to_file,
            commands::testing::export_all_tests_to_file,
            commands::testing::import_tests_from_file,
            // Page analysis commands for AI test generation
            commands::testing::analyze_page_playwright,
            commands::testing::analyze_page_playwright_script,
            commands::testing::analyze_page_vision,
            // Step output collection commands (for test builder auto-population)
            commands::step_outputs::collect_step_outputs,
            commands::step_outputs::get_step_outputs_for_test_builder,
            commands::testing::generate_test_with_ai,
            commands::testing::generate_test_metadata,
            commands::testing::list_recent_task_runs,
            commands::testing::get_workflow_run_context,
            // Code quality check commands
            commands::checks::execute_code_check,
            commands::checks::execute_code_check_suite,
            commands::checks::execute_check_by_id,
            commands::checks::list_checks,
            commands::checks::get_check,
            commands::checks::create_check,
            commands::checks::update_check,
            commands::checks::delete_check,
            commands::checks::detect_project_check_suggestions,
            commands::checks::get_check_tool_info,
            commands::checks::get_check_results,
            // Check group commands
            commands::checks::list_check_groups,
            commands::checks::get_check_group,
            commands::checks::create_check_group,
            commands::checks::update_check_group,
            commands::checks::delete_check_group,
            commands::checks::get_checks_in_group,
            commands::checks::set_checks_in_group,
            commands::checks::execute_check_group,
            commands::checks::repair_check_group_associations,
            // Screenshot listing commands
            commands::screenshots::list_screenshots,
            // Lifecycle Hooks commands
            commands::hooks::get_all_hooks,
            commands::hooks::get_hook,
            commands::hooks::create_hook,
            commands::hooks::update_hook,
            commands::hooks::delete_hook,
            commands::hooks::set_hook_enabled,
            commands::hooks::reorder_hooks,
            commands::hooks::test_hook,
            // Learning insights dashboard commands
            commands::learning::get_learning_summary,
            commands::learning::get_learning_patterns,
            commands::learning::get_learning_insights,
            commands::learning::analyze_learning_data,
            commands::learning::get_feedback_for_context,
            commands::learning::get_learning_dashboard_data,
            commands::learning::record_task_outcome,
            commands::learning::get_best_strategy,
            commands::learning::export_learning_data,
            commands::learning::import_learning_data,
            commands::learning::clear_learning_data,
            commands::learning::add_sample_learning_data,
            // Enhanced learning queries (filtering, pagination, date ranges)
            commands::learning::get_learning_outcomes_filtered,
            commands::learning::get_learning_outcomes_paginated,
            commands::learning::get_learning_stats_by_date_range,
            commands::learning::get_learning_outcomes_count,
            // Task run integration (recent tasks with learning outcomes)
            commands::learning::get_recent_tasks_with_outcomes,
            commands::learning::get_current_running_task,
            commands::learning::get_most_recent_task_with_checkpoints,
            commands::learning::get_learning_stats_summary,
            // Flow designer commands
            commands::flow::list_flows,
            commands::flow::get_flow,
            commands::flow::save_flow,
            commands::flow::delete_flow,
            commands::flow::validate_flow,
            commands::flow::start_flow_execution,
            commands::flow::step_flow_execution,
            commands::flow::run_flow_execution,
            commands::flow::provide_flow_input,
            commands::flow::get_flow_execution,
            commands::flow::list_flow_executions,
            commands::flow::cancel_flow_execution,
            commands::flow::pause_flow_execution,
            commands::flow::resume_flow_execution,
            commands::flow::step_into_flow,
            commands::flow::create_sample_flow,
            commands::flow::add_sample_flow,
            // Enhanced flow queries (tag filtering, execution filtering, pagination)
            commands::flow::get_flows_by_tag,
            commands::flow::get_flow_executions_filtered,
            commands::flow::get_flow_executions_paginated,
            commands::flow::get_flow_executions_count,
            // Flow version history commands
            commands::flow::create_flow_version,
            commands::flow::list_flow_versions,
            commands::flow::get_flow_version,
            commands::flow::restore_flow_version,
            commands::flow::compare_flow_versions,
            commands::flow::delete_flow_version,
            commands::flow::get_latest_flow_version,
            // Flow import/export commands
            commands::flow::export_flow_json,
            commands::flow::export_flow_yaml,
            commands::flow::import_flow_json,
            commands::flow::import_flow_yaml,
            commands::flow::export_flows_bulk,
            commands::flow::import_flows_bulk,
            // Checkpoint browser commands (time-travel debugging)
            commands::checkpoint_browser::list_orchestrator_checkpoints,
            commands::checkpoint_browser::get_orchestrator_checkpoint,
            commands::checkpoint_browser::create_orchestrator_checkpoint,
            commands::checkpoint_browser::delete_orchestrator_checkpoint,
            commands::checkpoint_browser::find_checkpoints_by_tag,
            commands::checkpoint_browser::compare_orchestrator_checkpoints,
            commands::checkpoint_browser::get_latest_checkpoint,
            commands::checkpoint_browser::start_replay_session,
            commands::checkpoint_browser::get_checkpoint_count,
            commands::checkpoint_browser::get_checkpoint_task_ids,
            commands::checkpoint_browser::clear_all_checkpoints,
            commands::checkpoint_browser::add_sample_checkpoints,
            commands::checkpoint_browser::get_checkpoint_stats,
            // Replay commands (time-travel debugging)
            commands::checkpoint_browser::replay_from_checkpoint,
            commands::checkpoint_browser::get_replay_lineage,
            commands::checkpoint_browser::register_task_for_lineage,
            commands::checkpoint_browser::get_task_lineage_info,
            commands::checkpoint_browser::list_active_replay_sessions,
            commands::checkpoint_browser::complete_replay_session,
            commands::checkpoint_browser::fail_replay_session,
            // Enhanced checkpoint queries (filtering, pagination)
            commands::checkpoint_browser::get_checkpoints_filtered,
            commands::checkpoint_browser::get_checkpoints_paginated,
            commands::checkpoint_browser::get_checkpoints_count,
            // Database maintenance commands
            commands::database::optimize_database,
            commands::database::get_database_stats,
            commands::database::explain_query_plan,
            // Comprehensive backup commands
            commands::backup::get_export_summary,
            commands::backup::export_all_data,
            commands::backup::get_import_preview,
            commands::backup::import_all_data,
            // Shell command management commands
            commands::shell_commands::create_shell_command,
            commands::shell_commands::get_shell_command,
            commands::shell_commands::list_shell_commands,
            commands::shell_commands::update_shell_command,
            commands::shell_commands::delete_shell_command,
            commands::shell_commands::execute_shell_command,
            commands::shell_commands::get_shell_command_results,
            commands::shell_commands::get_shell_command_categories,
            commands::shell_commands::set_shell_command_enabled,
            commands::shell_commands::generate_shell_command_with_ai,
            // AI generation commands for builder tabs
            commands::ai_generation::generate_context_with_ai,
            commands::ai_generation::generate_api_request_with_ai,
            commands::ai_generation::generate_task_prompt_with_ai,
            commands::ai_generation::suggest_exploration_strategy_with_ai,
            commands::ai_generation::generate_test_and_agentic_step,
            commands::ai_generation::explore_flow_step,
            commands::ai_generation::generate_element_ai_description,
            // MCP client management commands
            commands::mcp::list_mcp_servers,
            commands::mcp::get_mcp_server,
            commands::mcp::create_mcp_server,
            commands::mcp::update_mcp_server,
            commands::mcp::delete_mcp_server,
            commands::mcp::connect_mcp_server,
            commands::mcp::disconnect_mcp_server,
            commands::mcp::get_mcp_servers_status,
            commands::mcp::get_mcp_server_status,
            commands::mcp::list_mcp_server_tools,
            commands::mcp::call_mcp_tool,
            commands::mcp::get_task_run_mcp_calls,
            // Mobile development feedback commands
            commands::mobile::list_mobile_devices,
            commands::mobile::capture_mobile_screenshot,
            commands::mobile::capture_mobile_logcat,
            commands::mobile::get_mobile_states,
            commands::mobile::get_latest_mobile_state,
            commands::mobile::create_mobile_state,
            commands::mobile::get_mobile_logs,
            commands::mobile::get_mobile_errors,
            commands::mobile::create_mobile_log,
            commands::mobile::capture_mobile_feedback,
            commands::mobile::delete_mobile_data,
            // Mobile settings commands
            commands::mobile_settings::get_mobile_settings,
            commands::mobile_settings::save_mobile_settings,
            // Global log source management commands
            commands::global_log_sources::get_global_log_sources,
            commands::global_log_sources::save_global_log_sources,
            commands::global_log_sources::add_global_log_source,
            commands::global_log_sources::update_global_log_source,
            commands::global_log_sources::delete_global_log_source,
            commands::global_log_sources::create_global_log_source_profile,
            commands::global_log_sources::update_global_log_source_profile,
            commands::global_log_sources::delete_global_log_source_profile,
            commands::global_log_sources::set_default_log_source_profile,
            commands::global_log_sources::set_log_source_ai_selection_mode,
            commands::global_log_sources::read_global_log_sources,
            commands::global_log_sources::read_log_sources_by_profile,
            commands::global_log_sources::migrate_project_sources_to_global,
            commands::global_log_sources::select_log_sources_for_context,
            // Setup wizard commands (first-launch setup)
            commands::setup_wizard::check_setup_completed,
            commands::setup_wizard::complete_setup,
            commands::setup_wizard::scan_workspace_for_setup,
            commands::setup_wizard::detect_project_framework_for_setup,
            commands::setup_wizard::suggest_log_sources_for_setup,
            commands::setup_wizard::suggest_workspace_sources_for_setup,
            commands::setup_wizard::save_log_sources_from_setup,
            commands::setup_wizard::save_ai_provider_from_setup,
            commands::setup_wizard::suggest_process_configs_for_setup,
            commands::setup_wizard::suggest_dev_services_for_setup,
            commands::setup_wizard::save_dev_services_from_setup,
            commands::setup_wizard::discover_claude_config_dirs,
            // Test Orchestrator commands (AI-driven multi-step API test creation)
            commands::test_orchestrator::plan_test_orchestration,
            commands::test_orchestrator::execute_test_orchestration,
            commands::test_orchestrator::generate_test_from_orchestration,
            commands::test_orchestrator::get_saved_requests_for_orchestration,
            commands::test_orchestrator::save_orchestration_plan,
            commands::test_orchestrator::list_orchestration_plans,
            commands::test_orchestrator::delete_orchestration_plan,
            // Performance Metrics Dashboard commands
            commands::performance_metrics::get_performance_dashboard,
            commands::performance_metrics::get_action_performance,
            commands::performance_metrics::get_transition_reliability,
            commands::performance_metrics::get_element_resolution_metrics,
            commands::performance_metrics::get_success_rate_trend,
            // UI Bridge commands (AI-driven UI automation)
            commands::ui_bridge::ui_bridge_get_elements,
            commands::ui_bridge::ui_bridge_get_element,
            commands::ui_bridge::ui_bridge_execute_action,
            commands::ui_bridge::ui_bridge_get_components,
            commands::ui_bridge::ui_bridge_get_component,
            commands::ui_bridge::ui_bridge_execute_component_action,
            commands::ui_bridge::ui_bridge_discover,
            commands::ui_bridge::ui_bridge_get_snapshot,
            commands::ui_bridge::ui_bridge_discover_states_from_fingerprints,
            commands::ui_bridge::ui_bridge_run_exploration,
            commands::ui_bridge::ui_bridge_stop_exploration,
            commands::ui_bridge::ui_bridge_reload_webview,
            commands::ui_bridge::ui_bridge_run_exploration_native,
            commands::ui_bridge::ui_bridge_stop_exploration_native,
            commands::ui_bridge::ui_bridge_discover_states_native,
            // Error Monitor commands (application log error detection)
            // Note: Log source CRUD is now managed through global_log_sources commands
            error_monitor::commands::query_error_events,
            error_monitor::commands::get_error_event,
            error_monitor::commands::get_unresolved_errors,
            error_monitor::commands::update_error_status,
            error_monitor::commands::acknowledge_error,
            error_monitor::commands::resolve_error,
            error_monitor::commands::ignore_error,
            error_monitor::commands::link_error_to_finding,
            error_monitor::commands::get_error_summary,
            error_monitor::commands::search_errors,
            error_monitor::commands::has_actionable_errors,
            error_monitor::commands::get_recent_errors,
            error_monitor::commands::acknowledge_all_errors,
            error_monitor::commands::get_debug_context,
            error_monitor::commands::get_debug_context_for_ai,
            error_monitor::workflow::generate_error_fix_workflow,
            error_monitor::workflow::generate_single_error_fix_workflow,
            error_monitor::workflow::check_fixable_errors,
            // Doctor health monitoring commands
            doctor::commands::doctor_get_status,
            doctor::commands::stop_process_by_pid,
            // Cloud relay commands (remote mobile access via backend WebSocket)
            mcp::backend_relay::commands::start_cloud_relay,
            mcp::backend_relay::commands::stop_cloud_relay,
            mcp::backend_relay::commands::get_cloud_relay_status,
            mcp::backend_relay::commands::save_cloud_relay_settings,
            mcp::backend_relay::commands::get_cloud_relay_settings,
            // Process capture commands
            process_capture::commands::start_managed_process,
            process_capture::commands::stop_managed_process,
            process_capture::commands::restart_managed_process,
            process_capture::commands::start_all_managed_processes,
            process_capture::commands::stop_all_managed_processes,
            process_capture::commands::get_managed_processes,
            process_capture::commands::get_process_output,
            process_capture::commands::get_process_configs,
            process_capture::commands::save_process_config,
            process_capture::commands::delete_process_config,
            process_capture::commands::get_process_sessions_from_db,
            process_capture::commands::get_process_session_output_from_db,
            // Orchestration loop commands (runner-side workflow loop)
            orchestration_loop::commands::start_orchestration_loop,
            orchestration_loop::commands::stop_orchestration_loop,
            orchestration_loop::commands::get_orchestration_loop_status,
            orchestration_loop::commands::signal_orchestration_restart,
            // Autoresearch commands (autonomous workflow optimization loop)
            autoresearch::commands::start_autoresearch,
            autoresearch::commands::stop_autoresearch,
            autoresearch::commands::get_autoresearch_status,
            autoresearch::commands::get_autoresearch_results,
            autoresearch::commands::get_autoresearch_results_tsv,
            autoresearch::commands::get_autoresearch_campaign_history,
            autoresearch::commands::get_autoresearch_campaign_experiments,
            autoresearch::commands::list_unified_workflows,
            autoresearch::commands::list_worktree_records,
            autoresearch::commands::get_worktree_diff,
            autoresearch::commands::merge_worktree_branch,
            autoresearch::commands::remove_worktree_branch,
            autoresearch::commands::compare_worktree_branches,
            autoresearch::commands::rerun_autoresearch_campaign,
            autoresearch::commands::compare_autoresearch_campaigns,
            // Orchestration loop saved config CRUD
            commands::orchestration_loop_configs::ol_list_configs,
            commands::orchestration_loop_configs::ol_get_config,
            commands::orchestration_loop_configs::ol_save_config,
            commands::orchestration_loop_configs::ol_update_config,
            commands::orchestration_loop_configs::ol_delete_config,
            commands::orchestration_loop_configs::ol_toggle_favorite,
            // Embedded terminal commands (PTY-backed shell sessions)
            commands::terminal::terminal_create,
            commands::terminal::terminal_write,
            commands::terminal::terminal_resize,
            commands::terminal::terminal_close,
            commands::terminal::terminal_list,
            commands::terminal::terminal_ack,
            commands::terminal::terminal_get_buffer,
            commands::terminal::terminal_save_scrollback,
            commands::terminal::terminal_get_saved_scrollback,
            commands::terminal::terminal_cleanup_scrollback,
            // Runner instance management commands (dev feature)
            commands::instances::get_runner_instances,
            commands::instances::save_runner_instance,
            commands::instances::delete_runner_instance,
            commands::instances::launch_runner_instance,
            commands::instances::stop_runner_instance,
            commands::instances::get_runner_identity,
            // Claude Code transcript import commands
            commands::transcript::transcript_list_sessions,
            commands::transcript::transcript_read_session,
            commands::transcript::transcript_get_latest,
            commands::transcript::generate_workflow_standalone,
            // Terminal session analysis commands
            commands::terminal_analysis::analyze_session_summary,
            commands::terminal_analysis::analyze_architecture,
            commands::terminal_analysis::analyze_change_impact,
            commands::terminal_analysis::analyze_plan_progress,
            commands::terminal_analysis::analyze_cross_tab,
            commands::terminal_analysis::analyze_page_architecture,
            commands::terminal_analysis::get_latest_plan_content,
            // Meta-optimizer commands (recommendation review, prompt registry, manual trigger)
            commands::meta_optimizer::get_meta_optimizer_recommendations,
            commands::meta_optimizer::apply_meta_optimizer_recommendation,
            commands::meta_optimizer::reject_meta_optimizer_recommendation,
            commands::meta_optimizer::rollback_meta_optimizer_recommendation,
            commands::meta_optimizer::get_prompt_variants,
            commands::meta_optimizer::activate_prompt_variant,
            commands::meta_optimizer::get_meta_optimizer_runs,
            commands::meta_optimizer::trigger_meta_optimizer,
            commands::meta_optimizer::get_meta_optimizer_progress,
            commands::meta_optimizer::capture_meta_optimizer_baseline,
            commands::meta_optimizer::get_meta_optimizer_snapshots,
            commands::meta_optimizer::get_agent_effectiveness,
            commands::meta_optimizer::get_meta_optimizer_failure_analysis,
            // Regression detection
            commands::meta_optimizer::get_recommendation_outcomes,
            commands::meta_optimizer::reevaluate_recommendation_outcome,
            // Cost-effectiveness
            commands::meta_optimizer::get_agent_cost_effectiveness,
            // Cross-agent interaction
            commands::meta_optimizer::get_agent_interaction_matrix,
            commands::meta_optimizer::get_agent_cascade_effect,
            // Canary rollout
            commands::meta_optimizer::start_canary_rollout,
            commands::meta_optimizer::get_canary_rollouts,
            commands::meta_optimizer::promote_canary_rollout,
            commands::meta_optimizer::rollback_canary_rollout,
            // Comparison commands
            commands::comparison::start_comparison,
            commands::comparison::get_comparison_status,
            commands::comparison::list_comparisons,
        ])
        .setup(|app| {
            info!("Tauri application setup starting");

            // Clear runner log files from previous session
            executor::FileLogger::clear_logs();
            dom_capture::DomCaptureLogger::clear_captures();
            info!("Cleared previous runner log files");

            // Seed default log sources if none configured
            settings::seed_default_log_sources_if_empty();

            // Seed demo workflows on first launch (if no demo workflows exist)
            {
                let seed_db = app
                    .state::<Arc<crate::database::CheckpointDb>>()
                    .inner()
                    .clone();
                demo_workflows::seed_demo_workflows_if_needed(&seed_db);

                // Seed built-in issue pattern templates
                if let Ok(conn) = seed_db.get_conn() {
                    commands::known_issues::seed_built_in_templates(&conn);
                }
            }

            // Window starts maximized via tauri.conf.json

            // Initialize bridge manager for multi-bridge support
            // This replaces the legacy single python_bridge with a manager that can handle
            // multiple concurrent bridges (GUI + headless modes)
            info!("Initializing bridge manager");
            let app_state = app.state::<Arc<AppState>>();
            let bridge_manager = Arc::new(executor::BridgeManager::new(app.handle().clone()));

            // Check for headless-only mode from environment variable
            // This is intended for server deployments where GUI is not available
            let headless_only = std::env::var("QONTINUI_HEADLESS_ONLY")
                .map(|v| v == "1" || v.to_lowercase() == "true")
                .unwrap_or(false);

            if headless_only {
                info!("QONTINUI_HEADLESS_ONLY is set - enabling headless-only mode for server deployment");
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

            // Start MCP API server in background using the shared AppState
            let api_port = crate::mcp::types::get_mcp_api_port();
            info!("Starting MCP API server on port {}", api_port);
            let mcp_app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                info!("MCP API server task starting...");
                match mcp_api::start_server(mcp_app_state, mcp_rag_state, mcp_app_handle, api_port).await {
                    Ok(_) => info!("MCP API server stopped normally"),
                    Err(e) => error!("MCP API server error: {}", e),
                }
            });

            // Start heartbeat background task for fleet registration
            heartbeat::start_heartbeat(heartbeat_app_state);

            // Start scheduler service in background (skip for secondary instances to avoid duplicate executions)
            if std::env::var("QONTINUI_INSTANCE_NAME").is_ok() {
                info!("Secondary instance — skipping scheduler service");
            } else {
                info!("Starting scheduler service");
                tauri::async_runtime::spawn(async move {
                    // Wait briefly for MCP API server to bind
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    scheduler_service::start_scheduler_service().await;
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
                    start_error_monitor_async(error_monitor_db, error_monitor_config).await;

                // Store the handle
                let mut handle_lock = app_state_for_error_monitor.error_monitor_handle.lock().await;
                *handle_lock = Some(error_monitor_handle);
                info!("Error monitor service started and handle stored in AppState");
            });

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

                let pcm_db = app_state_for_pcm.checkpoint_db.clone();
                let manager = Arc::new(process_capture::ProcessCaptureManager::new(
                    error_monitor_arc,
                    app_handle_for_pcm,
                    pcm_db,
                ));

                // Load configs from settings and register them
                let mut configs = settings::get_managed_process_configs();

                // In dev-mode, upgrade legacy configs and inject missing dev services
                if dev_services::is_dev_mode() {
                    if let Some(workspace) = dev_services::find_workspace_root() {
                        // Upgrade existing configs that match dev service ports but lack auto_start
                        dev_services::upgrade_legacy_configs(&mut configs, &workspace);

                        // Inject any dev services not yet covered
                        let missing = dev_services::get_missing_dev_services(&workspace, &configs);
                        if !missing.is_empty() {
                            info!(
                                "Dev-mode: injecting {} default dev services for workspace {}",
                                missing.len(),
                                workspace.display()
                            );
                            for svc in &missing {
                                info!("  → {} (group {}, port {:?})", svc.name, svc.start_group, svc.health_port);
                            }
                            configs.extend(missing);
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
                *manager_lock = Some(manager);
                info!("Process capture manager initialized");
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

            // Start embedding backfill job in background
            info!("Starting embedding backfill job");
            let embedding_db = app.state::<Arc<AppState>>().inner().checkpoint_db.clone();
            database::embedding_jobs::start_embedding_job(
                embedding_db,
                database::embedding_jobs::EmbeddingJobConfig::default(),
            );

            // Auto-start MCP servers marked with auto_start in background
            let app_state_for_mcp_auto = app.state::<Arc<AppState>>().inner().clone();
            tauri::async_runtime::spawn(async move {
                let mcp_manager = app_state_for_mcp_auto.mcp_client_manager.lock().await;
                mcp_manager.start_auto_start_servers().await;
            });

            // Cloud relay auto-start is handled in mcp_api.rs where ApiState is available

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

                // ── Explicit shutdown ordering ──
                // Take PythonBridge (via bridge manager) and ExtractionExecutor out
                // of AppState and clean them up on a dedicated thread with its own
                // runtime, so Drop never runs inside Tauri's async context.
                let extraction = {
                    if let Ok(mut guard) = app_state.extraction_executor.lock() {
                        guard.take()
                    } else {
                        None
                    }
                };

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
                        }
                        Err(e) => {
                            error!("Failed to create shutdown runtime: {}", e);
                        }
                    }

                    // Stop extraction executor (synchronous — owns its own runtime)
                    if let Some(mut ee) = extraction {
                        info!("Stopping extraction executor on shutdown thread");
                        let _ = ee.stop();
                    }
                });

                // Wait for the dedicated shutdown thread (bounded wait)
                let _ = shutdown_handle.join();

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
