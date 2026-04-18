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
use crate::container::isolated_executor::IsolatedExecutor;
use crate::database::pg::PgDb;

use crate::display::DisplayProcessor;
use crate::doctor::DoctorHandle;
use crate::error_monitor::ErrorMonitorHandle;
use crate::executor::{
    BridgeManager, ExtractionExecutor, FileLockManager, FileRegistryManager, UrlLockManager,
};
use crate::mcp_client::McpClientManager;
use crate::process_capture::ProcessCaptureManager;
use crate::step_executor::handlers::ui_bridge::UiBridgeFailureTracker;
use crate::storage::LocalStorage;
use crate::tiered_info::RunRecordingHandler;
use crate::video_recorder::VideoRecordingService;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU16};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio::sync::Mutex as TokioMutex;

// Command modules organized by domain
pub mod accessibility;
pub mod activity_timeline; // Screenpipe-inspired searchable capture history
pub mod adaptive_learning; // Adaptive learning: playbook, curated examples, GEPA, template lifecycle
pub mod agentic_metrics;
pub mod ai_data;
pub mod ai_generation; // AI generation for builder tabs (context, api request, task, exploration)
pub mod ai_session; // Interactive AI session commands (send message, interrupt, get state)
pub mod ai_settings;
pub mod auth;
pub mod backup; // Comprehensive backup and restore
pub mod checkpoint_browser; // Orchestrator checkpoint browser (time-travel debugging)
pub mod checkpoints;
pub mod checks; // Code quality checks (linting, formatting, type checking)
pub mod clipboard; // Clipboard sync: share text to mobile via backend relay
pub mod comparison; // Side-by-side architecture comparison runs
pub mod config;
pub mod container_settings;
pub mod context;
pub mod cost_dashboard; // Cost dashboard with cache efficiency and phase breakdowns
pub mod dag_workflows; // DAG workflow import, export, and validation
pub mod database; // Database maintenance and optimization
pub mod dataset;
pub mod debug;
pub mod dev_findings; // Dev-only: seed synthetic findings into the frontend tracker
pub mod discoveries;
pub mod durable_execution; // Conductor-inspired replay, rollback, iteration diffs
pub mod event_search; // Unified full-text search across activity_timeline, observations, deferred_questions, error_events
pub mod execution;
pub mod execution_reporting;
pub mod execution_variables; // Execution variables (auth source, custom variables)
pub mod extraction;
pub mod file_browser; // Safe read-only filesystem browsing for mobile
pub mod findings;
pub mod flow; // Flow designer commands
pub mod global_log_sources; // Global log source management
pub mod hooks;
pub mod instances; // Runner instance management (dev feature)
pub mod interaction;
pub mod issues;
pub mod known_issues; // Known issues registry CRUD
pub mod learning; // Learning insights dashboard commands
pub mod library_sync; // Sync library items (checks, macros, etc.) to web backend
pub mod logging;
pub mod mcp; // MCP client management and tool calling
pub mod meta_optimizer; // Meta-optimizer recommendations and prompt registry
pub mod mobile; // Mobile development feedback (ADB, screenshots, logcat)
pub mod mobile_settings; // Mobile settings (ADB path, device config)
pub mod orchestration_loop_configs; // Orchestration loop saved config CRUD
pub mod otel_settings; // OpenTelemetry settings (endpoint, sampling, enable/disable)
pub mod performance_metrics; // Performance metrics dashboard
pub mod playwright_settings;
pub mod project_logs;
pub mod rag;
pub mod recap; // Session recap overview
pub mod screenshot;
pub mod screenshots;
pub mod security_settings;
pub mod self_healing_settings;
pub mod setup_wizard; // First-launch setup wizard commands
pub mod shell_commands; // Shell command management and execution
pub mod state_explorer; // State explorer for AI-driven state machine exploration
pub mod state_machine;
pub mod state_machine_configs; // State machine config builder CRUD
pub mod step_outputs; // Step output collection for test builder
pub mod storage;
pub mod task_sync; // renamed from ai_task_reporting
pub mod terminal;
pub mod terminal_analysis; // Terminal session analysis (session summary, architecture, change impact, etc.)
pub mod test_orchestrator; // AI-driven multi-step API test orchestration
pub mod testing;
pub mod tiered_info;
pub mod token_analytics;
pub mod transcript; // Claude Code transcript import and standalone workflow generation
pub mod ui_bridge; // UI Bridge for AI-driven UI automation
pub mod ui_bridge_baselines; // UI Bridge visual regression baseline CRUD
pub mod verification;
pub mod video;
pub mod watchers; // Screenpipe-inspired scheduled reactive AI agents
pub mod websocket;
pub mod window_manager; // OS-level window enumeration and activation
pub mod workflow_events; // Workflow event emission to backend for mobile push notifications

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
    /// Shared SDK connection for UI Bridge (shared with ApiState)
    pub sdk_connection: Arc<TokioMutex<crate::mcp::sdk_client::SdkConnectionManager>>,
    /// Cancel token for native exploration (allows stopping from another command)
    pub exploration_cancel: Arc<TokioMutex<Option<tokio_util::sync::CancellationToken>>>,
    pub current_config: Mutex<Option<QontinuiConfig>>,
    pub display_processor: Arc<TokioMutex<DisplayProcessor>>,
    pub local_storage: Arc<Mutex<LocalStorage>>,
    pub video_recorder: Arc<Mutex<VideoRecordingService>>,
    /// Broadcast channel for streaming execution events to WebSocket clients.
    /// Events include image recognition results, tree events, and state changes.
    pub event_broadcast: broadcast::Sender<serde_json::Value>,
    /// PostgreSQL database (Clorinde-generated queries). Required — local docker-compose PG.
    pub pg_db: Arc<PgDb>,
    /// Run recording handler for automatic workflow execution recording.
    /// Records runs to the Tiered Information system.
    pub run_recording_handler: Arc<RunRecordingHandler>,
    /// MCP client manager for calling external MCP servers from workflows.
    pub mcp_client_manager: TokioMutex<McpClientManager>,
    /// Error monitor handle for application log monitoring.
    /// Used to monitor log files for errors and integrate with debug context.
    pub error_monitor_handle: TokioMutex<Option<ErrorMonitorHandle>>,
    /// Doctor health monitoring handle for AI process health tracking.
    /// The Doctor observes process health (CPU, memory, process tree, stdout activity)
    /// and emits events when processes appear stuck. It never kills processes.
    pub doctor_handle: TokioMutex<Option<DoctorHandle>>,
    /// Per-URL lock manager for UI Bridge operations.
    /// Ensures only one workflow at a time interacts with a given UI Bridge URL.
    /// Workflows targeting different URLs run concurrently.
    pub url_lock_manager: Arc<UrlLockManager>,
    /// Advisory file registry for tracking files under active development.
    /// Sessions register files they're working on so other sessions can detect
    /// potential conflicts. Non-blocking — multiple sessions can work on the
    /// same file but are warned about overlaps.
    pub file_registry_manager: Arc<FileRegistryManager>,
    /// Exclusive per-file lock manager. When a session edits a file, other
    /// sessions trying to edit the same file block until the lock is released.
    /// This prevents concurrent edits deterministically (no AI judgment needed).
    pub file_lock_manager: Arc<FileLockManager>,
    /// Tracks consecutive UI Bridge failures per URL.
    /// After 3+ consecutive failures to the same URL, triggers an AI diagnostic.
    pub ui_bridge_failure_tracker: UiBridgeFailureTracker,
    /// Process capture manager for spawning and managing child processes.
    /// Captures stdout/stderr and feeds errors into the Error Monitor.
    pub process_capture_manager: TokioMutex<Option<Arc<ProcessCaptureManager>>>,
    /// Flag indicating the HTTP API server has bound and is ready.
    /// Set by `mcp_api::start_server` after successful bind, checked by the
    /// `is_api_ready` Tauri command so the frontend can gate HTTP calls.
    pub api_ready: AtomicBool,
    /// Actual port the HTTP API server bound to.
    /// Set by `mcp_api::start_server` after successful bind.
    pub api_port: AtomicU16,
    /// Shared PID tracker for spawned AI (Claude CLI) processes.
    /// Shared between AppState (for shutdown cleanup) and ApiState (for stop endpoints).
    pub ai_pid_tracker: Arc<Mutex<Vec<u32>>>,
    /// In-memory canvas state for A2UI panels.
    /// Agent sends structured JSON panels via HTTP; frontend renders them.
    pub canvas_state: Arc<tokio::sync::RwLock<crate::mcp::canvas::CanvasState>>,
    /// Orchestration loop state for runner-side workflow loop management.
    /// Multi-loop manager: supports concurrent loops targeting different runners.
    pub orchestration_loops: crate::orchestration_loop::loop_engine::SharedLoopStates,
    /// Container isolation executor for running shell commands inside Docker containers.
    /// When `Some` and `is_available()`, shell commands are executed in isolated containers
    /// instead of on the host. Falls back to host execution when `None` or unavailable.
    pub container_executor: TokioMutex<Option<IsolatedExecutor>>,
    /// Per-run cost management trackers.
    /// Keyed by execution_id. Created at run start, removed on completion.
    pub run_cost_trackers:
        TokioMutex<HashMap<String, Arc<crate::cost_management::RunCostTrackers>>>,
    /// In-memory cache of pre-computed working representations, keyed by task_run_id.
    /// Avoids rebuilding expensive parallel PG queries on every prompt construction.
    pub working_representation_cache:
        Arc<crate::memory::working_representation::WorkingRepresentationCache>,
}

impl AppState {
    /// Register cost trackers for a new run.
    pub async fn register_cost_trackers(
        &self,
        execution_id: &str,
    ) -> Arc<crate::cost_management::RunCostTrackers> {
        let trackers = Arc::new(crate::cost_management::RunCostTrackers::new());
        self.run_cost_trackers
            .lock()
            .await
            .insert(execution_id.to_string(), trackers.clone());
        trackers
    }

    /// Get cost trackers for an active run.
    pub async fn get_cost_trackers(
        &self,
        execution_id: &str,
    ) -> Option<Arc<crate::cost_management::RunCostTrackers>> {
        self.run_cost_trackers
            .lock()
            .await
            .get(execution_id)
            .cloned()
    }

    /// Remove cost trackers after run completion.
    pub async fn remove_cost_trackers(&self, execution_id: &str) {
        self.run_cost_trackers.lock().await.remove(execution_id);
    }
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
