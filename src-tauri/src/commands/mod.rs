//! Command handlers for the Tauri application
//!
//! This module organizes all Tauri commands by their domain responsibility,
//! following the Single Responsibility Principle (SRP).
//!
//! # Module Organization
//!
//! - `state_explorer` - State explorer for AI-driven state machine exploration
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
use crate::error_monitor::ErrorMonitorHandle;
use crate::executor::{BridgeManager, ExtractionExecutor};
use crate::mcp_client::McpClientManager;
use crate::storage::LocalStorage;
use crate::tiered_info::RunRecordingHandler;
use crate::video_recorder::VideoRecordingService;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio::sync::Mutex as TokioMutex;

// Command modules organized by domain
pub mod accessibility;
pub mod ai_chat; // Interactive AI chat session commands (send message, interrupt, get state)
pub mod ai_data;
pub mod ai_generation; // AI generation for builder tabs (context, api request, task, exploration)
pub mod ai_settings;
pub mod auth;
pub mod backup; // Comprehensive backup and restore
pub mod checkpoint_browser; // Orchestrator checkpoint browser (time-travel debugging)
pub mod checkpoints;
pub mod checks; // Code quality checks (linting, formatting, type checking)
pub mod config;
pub mod context;
pub mod database; // Database maintenance and optimization
pub mod dataset;
pub mod debug;
pub mod discoveries;
pub mod execution;
pub mod execution_reporting;
pub mod execution_variables; // Execution variables (auth source, custom variables)
pub mod extraction;
pub mod findings;
pub mod flow; // Flow designer commands
pub mod global_log_sources; // Global log source management
pub mod hooks;
pub mod interaction;
pub mod issues;
pub mod learning; // Learning insights dashboard commands
pub mod logging;
pub mod mcp; // MCP client management and tool calling
pub mod mobile; // Mobile development feedback (ADB, screenshots, logcat)
pub mod mobile_settings; // Mobile settings (ADB path, device config)
pub mod performance_metrics; // Performance metrics dashboard
pub mod playwright_settings;
pub mod project_logs;
pub mod rag;
pub mod recap; // Session recap overview
pub mod screenshot;
pub mod screenshots;
pub mod self_healing_settings;
pub mod shell_commands; // Shell command management and execution
pub mod state_explorer; // State explorer for AI-driven state machine exploration
pub mod state_machine;
pub mod step_outputs; // Step output collection for test builder
pub mod storage;
pub mod task_sync; // renamed from ai_task_reporting
pub mod test_orchestrator; // AI-driven multi-step API test orchestration
pub mod testing;
pub mod tiered_info;
pub mod ui_bridge; // UI Bridge for AI-driven UI automation
pub mod verification;
pub mod video;
pub mod websocket;

// Re-export all command functions for easy access in main.rs

/// Application state shared across all commands.
///
/// This structure holds the core application state including:
/// - Bridge manager for multi-bridge support (handles all Python bridges)
/// - Extraction executor for parallel extraction operations
/// - Current loaded configuration
/// - Display processor for UI views
/// - Local storage service
/// - Video recording service
/// - Event broadcast channel for WebSocket clients
/// - Checkpoint database for persistent storage
/// - Run recording handler for automatic run recording
/// - Error monitor handle for application log monitoring
pub struct AppState {
    /// Bridge manager for handling multiple concurrent Python bridges.
    /// Supports GUI mode (exclusive) and headless mode (parallel).
    /// Initialized lazily in setup() when app_handle is available.
    ///
    /// Use the helper functions from `crate::executor::bridge_helpers` to access:
    /// - `with_default_bridge()` - Execute closure with default bridge
    /// - `is_default_bridge_running()` - Check if executor is running
    /// - `get_or_create_default_bridge()` - Ensure a bridge exists
    pub bridge_manager: TokioMutex<Option<Arc<BridgeManager>>>,
    /// Separate executor for extraction operations (runs in parallel with bridges).
    /// This allows extraction (which uses Playwright) to run concurrently with
    /// GUI automation workflows (which use HAL).
    pub extraction_executor: Mutex<Option<ExtractionExecutor>>,
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
    /// MCP client manager for calling external MCP servers from workflows.
    pub mcp_client_manager: TokioMutex<McpClientManager>,
    /// Error monitor handle for application log monitoring.
    /// Used to monitor log files for errors and integrate with debug context.
    pub error_monitor_handle: TokioMutex<Option<ErrorMonitorHandle>>,
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
