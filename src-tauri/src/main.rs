// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
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
mod backup;
mod check_executor;
mod check_generation;
mod claude_protocol;
mod claude_session;
mod commands;
mod config;
mod config_facade;
mod config_storage;
mod context;
mod database;
mod debug_lifecycle;
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
mod findings;
mod health_monitor;
mod iteration_bundle;
mod log_consolidation;
mod log_migration;
mod logging;
mod macros;
mod mcp;
mod mcp_api;
mod mcp_client;
mod mcp_embedded;
mod orchestrator;
mod paths;
mod plan_executor;
mod playwright;
mod prompts;
mod rag;
mod recording;
mod runtime_env;
mod safe_lock;
mod saved_api_requests;
mod scheduler;
mod scheduler_service;
mod scriptlets;
mod secure_storage;
mod settings;
mod state_explorer;
mod step_event_builder;
mod step_executor;
mod step_metadata;
mod step_registry;
mod step_types;
mod steps;
mod storage;
mod summary_generator;
mod task_recorder;
mod test_executor;
mod test_orchestrator;
mod tiered_info;
mod timeout_config;
mod tracing_layers;
mod unified_ai_session;
mod unified_workflow_executor;
mod unified_workflows;
mod video_recorder;
mod workflow_generation;
mod workflow_state;

use commands::AppState;
use database::CheckpointDb;
use display::profiles::ActionLogProfile;
use display::DisplayProcessor;
use doctor::{start_doctor_async, DoctorConfig};
use error_monitor::{start_error_monitor_async, ErrorMonitorConfig};
use logging::{init_logging, setup_panic_handler, LoggingConfig};
use std::sync::{Arc, Mutex};
use storage::LocalStorage;
use tauri::Manager;
use tiered_info::RunRecordingHandler;
use tokio::sync::Mutex as TokioMutex;
use tracing::{error, info, warn};
use video_recorder::VideoRecordingService;

fn main() {
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

    info!("Starting Qontinui Runner v{}", env!("CARGO_PKG_VERSION"));

    // TODO: The Sentry guard is scoped to this block — when it drops, Sentry shuts down.
    // Move _guard to the outer scope so it lives for the entire application lifetime.
    // Also, the panic handler in logging.rs re-initializes Sentry redundantly.
    // Consolidate into a single initialization here and remove the duplicate in setup_panic_handler().
    #[cfg(not(debug_assertions))]
    {
        if let Ok(dsn) = std::env::var("SENTRY_DSN") {
            let _guard = sentry::init((
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
        }
    }

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

    // Create session manager for interactive Claude CLI sessions
    let session_manager = Arc::new(claude_session::SessionManager::new());

    // Create shared AppState for both Tauri and MCP API
    let shared_app_state = Arc::new(AppState {
        bridge_manager: TokioMutex::new(None), // Initialized in setup() when app_handle is available
        extraction_executor: Mutex::new(None), // Initialized on-demand
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
    });
    let mcp_app_state = shared_app_state.clone();
    let mcp_rag_state = rag_state.clone();

    // Create error monitor config for later initialization
    let error_monitor_db = checkpoint_db.clone();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(shared_app_state)
        .manage(rag_state)
        .manage(session_manager) // For interactive AI chat commands
        .manage(checkpoint_db.clone()) // For error_monitor commands that need direct db access
        .invoke_handler(tauri::generate_handler![
            // Interactive AI chat commands (send messages, interrupt, query state)
            commands::ai_chat::send_user_message,
            commands::ai_chat::interrupt_ai_session,
            commands::ai_chat::get_ai_session_state,
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
        ])
        .setup(|app| {
            info!("Tauri application setup starting");

            // Clear runner log files from previous session
            executor::FileLogger::clear_logs();
            dom_capture::DomCaptureLogger::clear_captures();
            info!("Cleared previous runner log files");

            // Seed default log sources if none configured
            settings::seed_default_log_sources_if_empty();

            // Position window at top-center of screen and maximize height
            if let Some(window) = app.get_webview_window("main") {
                if let Ok(monitor) = window.current_monitor() {
                    if let Some(monitor) = monitor {
                        let monitor_size = monitor.size();
                        let monitor_pos = monitor.position();
                        let scale_factor = monitor.scale_factor();

                        if let Ok(window_size) = window.outer_size() {
                            // Calculate optimal height: use most of screen with margins
                            // Top margin: 20px, Bottom margin: 60px (for taskbar)
                            let top_margin: u32 = 20;
                            let bottom_margin: u32 = 60;
                            let total_margin = top_margin + bottom_margin;

                            // Calculate new height (leave room for margins)
                            let new_height = if monitor_size.height > total_margin {
                                monitor_size.height - total_margin
                            } else {
                                monitor_size.height
                            };

                            // Keep width the same, maximize height
                            let new_width = window_size.width;

                            // Set the new window size
                            if let Err(e) = window.set_size(tauri::Size::Physical(
                                tauri::PhysicalSize {
                                    width: new_width,
                                    height: new_height,
                                },
                            )) {
                                error!("Failed to set window size: {}", e);
                            } else {
                                info!(
                                    "Window resized to {}x{} (monitor: {}x{}, scale: {:.2})",
                                    new_width, new_height, monitor_size.width, monitor_size.height, scale_factor
                                );
                            }

                            // Calculate center X position using new width
                            let x = monitor_pos.x
                                + ((monitor_size.width as i32 - new_width as i32) / 2);
                            // Position at top (with small margin)
                            let y = monitor_pos.y + top_margin as i32;

                            if let Err(e) = window.set_position(tauri::Position::Physical(
                                tauri::PhysicalPosition { x, y },
                            )) {
                                error!("Failed to set window position: {}", e);
                            } else {
                                info!("Window positioned at top-center: x={}, y={}", x, y);
                            }
                        }
                    }
                } else {
                    error!("Failed to get current monitor");
                }
            } else {
                error!("Failed to get main window");
            }

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
            info!("Starting MCP API server on port {}", crate::mcp::types::MCP_API_PORT);
            let mcp_app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Wait a bit for app to fully initialize
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

                info!("MCP API server task starting...");
                match mcp_api::start_server(mcp_app_state, mcp_rag_state, mcp_app_handle, crate::mcp::types::MCP_API_PORT).await {
                    Ok(_) => info!("MCP API server stopped normally"),
                    Err(e) => error!("MCP API server error: {}", e),
                }
            });

            // Start scheduler service in background
            info!("Starting scheduler service");
            tauri::async_runtime::spawn(async move {
                // Wait for MCP API server to be ready
                tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                scheduler_service::start_scheduler_service().await;
            });

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

            info!("Tauri application setup complete");
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                info!("Window close requested");
                let app_state = window.state::<Arc<AppState>>();

                // Stop all bridges via bridge manager
                let app_state_clone = app_state.inner().clone();
                tauri::async_runtime::block_on(async {
                    let manager_guard = app_state_clone.bridge_manager.lock().await;
                    if let Some(ref manager) = *manager_guard {
                        info!("Stopping all bridges via bridge manager");
                        manager.remove_all().await;
                    }
                });

                // Stop extraction executor
                if let Ok(mut executor) = app_state.extraction_executor.lock() {
                    if let Some(ref mut ee) = *executor {
                        let _ = ee.stop();
                    }
                };

                // Close all interactive Claude sessions
                if let Some(sm) =
                    window.try_state::<Arc<claude_session::SessionManager>>()
                {
                    sm.close_all_sessions();
                }

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
