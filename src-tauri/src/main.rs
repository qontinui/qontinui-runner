// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod config;
mod executor;

use commands::AppState;
use std::sync::Mutex;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
