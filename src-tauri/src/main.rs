// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod auth;
mod commands;
mod config;
mod display;
mod error;
mod executor;
mod logging;
mod mcp_api;
mod settings;
mod storage;
mod video_recorder;

use commands::AppState;
use display::profiles::ActionLogProfile;
use display::DisplayProcessor;
use logging::{init_logging, setup_panic_handler, LoggingConfig};
use std::sync::{Arc, Mutex};
use storage::LocalStorage;
use tauri::Manager;
use tokio::sync::Mutex as TokioMutex;
use tracing::{error, info};
use video_recorder::VideoRecordingService;

fn main() {
    let result = std::panic::catch_unwind(run_app);

    match result {
        Ok(Ok(())) => {
            info!("Application exited successfully");
        }
        Ok(Err(e)) => {
            error!("Application error: {}", e);
            std::process::exit(1);
        }
        Err(panic) => {
            error!("Application panicked: {:?}", panic);
            std::process::exit(2);
        }
    }
}

fn run_app() -> Result<(), Box<dyn std::error::Error>> {
    init_logging(LoggingConfig::default())?;
    setup_panic_handler();

    info!("Starting Qontinui Runner v{}", env!("CARGO_PKG_VERSION"));

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

    // Create shared AppState for both Tauri and MCP API
    let shared_app_state = Arc::new(AppState {
        python_bridge: Mutex::new(None),
        current_config: Mutex::new(None),
        display_processor,
        local_storage,
        video_recorder,
    });
    let mcp_app_state = shared_app_state.clone();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(shared_app_state)
        .invoke_handler(tauri::generate_handler![
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
            commands::config::get_auto_load_last_config,
            commands::config::save_auto_load_last_config,
            // Execution commands
            commands::execution::start_python_executor,
            commands::execution::stop_python_executor,
            commands::execution::start_execution,
            commands::execution::stop_execution,
            commands::execution::get_executor_status,
            commands::execution::get_monitors,
            commands::execution::handle_error,
            commands::execution::check_for_updates,
            commands::execution::open_folder,
            commands::execution::update_capture_settings,
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
            // Storage commands
            commands::storage::save_screenshot_to_disk,
            commands::storage::save_video_to_disk,
            commands::storage::get_local_storage_usage,
            commands::storage::delete_old_sessions,
            commands::storage::clear_all_storage,
            commands::storage::get_storage_paths,
            // Web extraction commands
            commands::extraction::start_web_extraction,
            commands::extraction::stop_web_extraction,
            commands::extraction::get_extraction_status,
            commands::extraction::request_extraction_screenshot,
            commands::extraction::export_training_data,
            commands::extraction::export_state_structure,
            commands::extraction::list_extractions,
            // Screenshot capture commands
            commands::screenshot::get_screenshot_monitors,
            commands::screenshot::capture_screenshot,
            commands::screenshot::capture_and_upload_screenshot,
            commands::screenshot::capture_screenshot_via_python,
        ])
        .setup(|app| {
            info!("Tauri application setup starting");

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

            // Auto-start Python executor for screenshot capture and other features
            info!("Auto-starting Python executor");
            let app_state = app.state::<Arc<AppState>>();
            let mut bridge_lock = app_state.python_bridge.lock().unwrap();
            let mut bridge = executor::PythonBridge::new(app.handle().clone());
            match bridge.start() {
                Ok(_) => {
                    info!("Python executor auto-started successfully");
                    *bridge_lock = Some(bridge);
                }
                Err(e) => {
                    error!("Failed to auto-start Python executor: {}", e);
                    error!("Screenshot capture and other features will not work until the executor is started");
                    // Don't fail app startup, just log the error
                }
            }
            drop(bridge_lock);

            // Start MCP API server in background using the shared AppState
            info!("Starting MCP API server on port {}", mcp_api::MCP_API_PORT);
            let mcp_app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Wait a bit for app to fully initialize
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

                info!("MCP API server task starting...");
                match mcp_api::start_server(mcp_app_state, mcp_app_handle, mcp_api::MCP_API_PORT).await {
                    Ok(_) => info!("MCP API server stopped normally"),
                    Err(e) => error!("MCP API server error: {}", e),
                }
            });

            info!("Tauri application setup complete");
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                info!("Window close requested");
                let app_state = window.state::<Arc<AppState>>();
                if let Ok(mut bridge) = app_state.python_bridge.lock() {
                    if let Some(ref mut pb) = *bridge {
                        let _ = pb.stop();
                    }
                }; // Add semicolon to drop the temporary earlier
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
