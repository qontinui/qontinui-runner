//! Command handlers for the Tauri application
//!
//! This module organizes all Tauri commands by their domain responsibility,
//! following the Single Responsibility Principle (SRP).
//!
//! # Module Organization
//!
//! - `ai_verification` - AI verification agent for state machine exploration
//! - `auth` - Authentication and device management
//! - `config` - Configuration file loading and management
//! - `dataset` - Dataset packaging and YOLO format export
//! - `debug` - Debug settings management
//! - `execution` - Python executor lifecycle and workflow execution
//! - `extraction` - Web GUI extraction and training data export
//! - `discoveries` - Discovery Push mechanism for syncing detected patterns
//! - `findings` - AI-detected findings (issues, bugs, questions)
//! - `project_logs` - Project-specific log management and external log sources
//! - `screenshot` - Screenshot capture operations
//! - `state_machine` - State navigation and transition execution
//! - `storage` - Local disk storage management
//! - `video` - Video recording operations
//! - `websocket` - WebSocket connectivity for remote monitoring
//! - `interaction` - Interaction recording (video + input capture) for State Machine creation
//!
//! # Shared Types
//!
//! Common types and state used across all command modules are defined here.

use crate::config::QontinuiConfig;
use crate::database::CheckpointDb;
use crate::display::DisplayProcessor;
use crate::executor::PythonBridge;
use crate::storage::LocalStorage;
use crate::tiered_info::RunRecordingHandler;
use crate::video_recorder::VideoRecordingService;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio::sync::Mutex as TokioMutex;

// Command modules organized by domain
pub mod ai_data;
pub mod ai_settings;
pub mod ai_verification;
pub mod auth;
pub mod checkpoints;
pub mod config;
pub mod context;
pub mod dataset;
pub mod debug;
pub mod discoveries;
pub mod execution;
pub mod execution_reporting;
pub mod extraction;
pub mod findings;
pub mod interaction;
pub mod issues;
pub mod logging;
pub mod playwright_settings;
pub mod project_logs;
pub mod rag;
pub mod screenshot;
pub mod screenshots;
pub mod state_machine;
pub mod storage;
pub mod task_sync; // renamed from ai_task_reporting
pub mod testing;
pub mod tiered_info;
pub mod verification;
pub mod video;
pub mod websocket;

// Re-export all command functions for easy access in main.rs

/// Application state shared across all commands.
///
/// This structure holds the core application state including:
/// - Python bridge for executor communication
/// - Current loaded configuration
/// - Display processor for UI views
/// - Local storage service
/// - Video recording service
/// - Event broadcast channel for WebSocket clients
/// - Checkpoint database for persistent storage
/// - Run recording handler for automatic run recording
pub struct AppState {
    pub python_bridge: Mutex<Option<PythonBridge>>,
    pub current_config: Mutex<Option<QontinuiConfig>>,
    pub display_processor: Arc<TokioMutex<DisplayProcessor>>,
    pub local_storage: Arc<Mutex<LocalStorage>>,
    pub video_recorder: Arc<Mutex<VideoRecordingService>>,
    /// Broadcast channel for streaming execution events to WebSocket clients.
    /// Events include image recognition results, tree events, and state changes.
    pub event_broadcast: broadcast::Sender<serde_json::Value>,
    /// SQLite database for checkpoints, sessions, settings, and scheduler state.
    pub checkpoint_db: Arc<CheckpointDb>,
    /// Run recording handler for automatic workflow execution recording.
    /// Records runs to the Tiered Information system.
    pub run_recording_handler: Arc<RunRecordingHandler>,
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
