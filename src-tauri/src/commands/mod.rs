//! Command handlers for the Tauri application
//!
//! This module organizes all Tauri commands by their domain responsibility,
//! following the Single Responsibility Principle (SRP).
//!
//! # Module Organization
//!
//! - `config` - Configuration file loading and management
//! - `execution` - Python executor lifecycle and workflow execution
//! - `state_machine` - State navigation and transition execution
//! - `debug` - Debug settings management
//! - `websocket` - WebSocket connectivity for remote monitoring
//! - `video` - Video recording operations
//! - `storage` - Local disk storage management
//!
//! # Shared Types
//!
//! Common types and state used across all command modules are defined here.

use crate::config::QontinuiConfig;
use crate::display::DisplayProcessor;
use crate::executor::PythonBridge;
use crate::storage::LocalStorage;
use crate::video_recorder::VideoRecordingService;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as TokioMutex;

// Command modules organized by domain
pub mod config;
pub mod debug;
pub mod execution;
pub mod state_machine;
pub mod storage;
pub mod video;
pub mod websocket;

// Re-export all command functions for easy access in main.rs
pub use config::{
    get_auto_load_last_config, get_current_configuration, get_last_config_path,
    load_configuration, save_auto_load_last_config, save_last_workflow_id,
};
pub use debug::{get_debug_settings, set_debug_settings};
pub use execution::{
    check_for_updates, get_executor_status, get_monitors, handle_error, open_folder,
    start_execution, start_python_executor, stop_execution, stop_python_executor,
    update_capture_settings,
};
pub use state_machine::{
    clear_action_log, execute_transition, get_action_log_view, get_active_states,
    get_available_transitions, navigate_to_multiple_states, navigate_to_state,
};
pub use storage::{
    clear_all_storage, delete_old_sessions, get_local_storage_usage, get_storage_paths,
    save_screenshot_to_disk, save_video_to_disk,
};
pub use video::{get_video_recording_status, start_video_recording, stop_video_recording};
pub use websocket::{configure_websocket, connect_websocket, disconnect_websocket, WebSocketConfig};

/// Application state shared across all commands.
///
/// This structure holds the core application state including:
/// - Python bridge for executor communication
/// - Current loaded configuration
/// - Display processor for UI views
/// - Local storage service
/// - Video recording service
pub struct AppState {
    pub python_bridge: Mutex<Option<PythonBridge>>,
    pub current_config: Mutex<Option<QontinuiConfig>>,
    pub display_processor: Arc<TokioMutex<DisplayProcessor>>,
    pub local_storage: Arc<Mutex<LocalStorage>>,
    pub video_recorder: Arc<Mutex<VideoRecordingService>>,
}

/// Standard response structure for command handlers.
///
/// Provides a consistent response format across all commands with:
/// - Success/failure status
/// - Optional message for user feedback
/// - Optional data payload as JSON
#[derive(Debug, Serialize, Deserialize)]
pub struct CommandResponse {
    pub success: bool,
    pub message: Option<String>,
    pub data: Option<serde_json::Value>,
}
