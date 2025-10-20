// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod config;
mod error;
mod executor;
mod logging;

#[cfg(test)]
mod test;

use commands::AppState;
use logging::{init_logging, setup_panic_handler, LoggingConfig};
use std::sync::Mutex;
use tauri::Manager;
use tracing::{error, info};

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

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState {
            python_bridge: Mutex::new(None),
            current_config: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            commands::load_configuration,
            commands::start_python_executor,
            commands::start_python_executor_with_type,
            commands::stop_python_executor,
            commands::start_execution,
            commands::stop_execution,
            commands::get_executor_status,
            commands::get_current_configuration,
            commands::get_monitors,
            commands::handle_error,
            commands::check_for_updates,
            commands::start_recording,
            commands::stop_recording,
            commands::get_recording_status,
            commands::open_folder,
        ])
        .setup(|app| {
            info!("Tauri application setup starting");

            // Position window at top-center of screen
            if let Some(window) = app.get_webview_window("main") {
                if let Ok(monitor) = window.current_monitor() {
                    if let Some(monitor) = monitor {
                        let monitor_size = monitor.size();
                        let monitor_pos = monitor.position();

                        if let Ok(window_size) = window.outer_size() {
                            // Calculate center X position
                            let x = monitor_pos.x
                                + ((monitor_size.width as i32 - window_size.width as i32) / 2);
                            // Position at top (with small margin)
                            let y = monitor_pos.y + 20;

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

            info!("Tauri application setup complete");
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                info!("Window close requested");
                let app_state = window.state::<AppState>();
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
