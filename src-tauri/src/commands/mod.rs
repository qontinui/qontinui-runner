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
//!
//! # Handler registration
//!
//! Modules are transitioning to Tauri plugin self-registration. Two patterns
//! coexist:
//!
//! **Plugin pattern (preferred)** — module exposes `pub fn plugin<R: Runtime>()
//! -> TauriPlugin<R>` holding its own `tauri::generate_handler![...]`. main.rs
//! adds one `.plugin(commands::foo::plugin())` call. Adding a command to a
//! migrated module is a single-file edit.
//!
//! **Central pattern (legacy)** — handlers listed in main.rs's central
//! `tauri::generate_handler![...]`. Requires editing main.rs for every new
//! command; tracked for migration.
//!
//! Migrated modules: clipboard, debug, dev_findings, file_browser,
//! window_manager, checks, checkpoints, comparison, container_settings,
//! dag_workflows, database, dataset, discoveries, event_search, findings,
//! hooks, issues, known_issues, playwright_settings, self_healing_settings,
//! execution_variables, mobile_settings, otel_settings, security_settings,
//! ai_settings, accessibility, web_integration, activity_timeline,
//! agentic_metrics, ai_data, cost_dashboard, learning, performance_metrics,
//! recap, terminal_analysis, token_analytics, transcript, adaptive_learning,
//! ai_generation, ai_session, backup, checkpoint_browser, config, context,
//! library_sync, logging, meta_optimizer, rag, auth, state_machine,
//! websocket, video, interaction, storage, extraction, screenshot,
//! screenshots, script_emitter, verification, project_logs,
//! global_log_sources, execution_reporting, workflow_events,
//! state_machine_configs, spec_drift, ui_bridge_baselines, state_explorer,
//! tiered_info, task_sync, step_outputs, testing, shell_commands, mcp,
//! mobile, setup_wizard, saved_projects, test_orchestrator,
//! orchestration_loop_configs, scripted_output_settings, watchers,
//! durable_execution, flow, ui_bridge, terminal, instances, execution
//! (as of this commit).
//!
//! All `commands/*` modules are now migrated to the plugin pattern. So are
//! the subsystem command modules living outside `commands/`: `error_monitor`,
//! `doctor`, `mcp::backend_relay`, `process_capture`, `orchestration_loop`,
//! `spec_experimentation`, plus the module-level handlers in `ui_error.rs`
//! and `crash_dumps.rs`. main.rs no longer holds a central
//! `tauri::generate_handler![...]` — every command self-registers through
//! its owning module's `plugin()` fn.
//!
//! Note: ai_session, meta_optimizer, rag, flow, ui_bridge, terminal, and
//! execution use a non-generic `plugin() -> TauriPlugin<tauri::Wry>` because
//! they accept concrete `tauri::AppHandle` parameters in some commands.
//! Modules without `AppHandle` parameters use the generic
//! `plugin<R: Runtime>() -> TauriPlugin<R>` form.
//!
//! To migrate a module `foo.rs`:
//! 1. Add `use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};` and
//!    `use tauri::Runtime;` at the top.
//! 2. Add `pub fn plugin<R: Runtime>() -> TauriPlugin<R>` at the bottom that
//!    returns `PluginBuilder::new("qontinui_foo").invoke_handler(...).build()`.
//! 3. In `main.rs`, remove the module's entries from the central
//!    `generate_handler!` list and add `.plugin(commands::foo::plugin())`
//!    right after the existing per-module plugin block.
//! 4. Plugin name convention: `qontinui_<module_name>` (lowercase snake_case).
//!
//! # Migrating a module to a compartment (Workstream C)
//!
//! [`compartments`] defines five scoped wrappers around `Arc<AppState>` —
//! `BridgeCompartment`, `ExecutionCompartment`, `IntegrationCompartment`,
//! `HealthCompartment`, `StorageCompartment` — each exposing only the fields
//! in its domain. To migrate a module `foo.rs`:
//! 1. Pick the compartment whose accessors cover every AppState field the
//!    module reads (see `compartments.rs` for the field partitioning).
//! 2. Replace `use super::AppState;` with
//!    `use super::compartments::<Compartment>;`.
//! 3. In handler signatures, change `State<'_, Arc<AppState>>` to
//!    `State<'_, <Compartment>>`. Drop the now-unused `std::sync::Arc` import.
//! 4. In handler bodies, change `state.<field>` to `state.<field>()` (each
//!    accessor returns the same `&Field` reference — lock/atomic semantics
//!    are unchanged).
//! 5. Add a `// Migrated to <Compartment> (Workstream C).` marker comment
//!    near the module's `use` block so future sessions can audit progress.
//! 6. The existing `.manage(shared_app_state)` stays — unmigrated modules
//!    continue to use it until they migrate individually.
//!
//! # Typed errors (Workstream D)
//!
//! [`crate::error::AppError`] is the canonical error enum for command
//! internals, with `impl From<AppError> for String` providing the Tauri ABI
//! shim. Two patterns coexist during retrofit:
//!
//! (a) `-> Result<T, AppError>` directly — cleanest for internal helpers;
//!     Tauri handlers using this form send AppError's structured `Serialize`
//!     payload to the frontend (variant + message + error_code).
//! (b) `-> Result<T, String>` at the Tauri boundary with an `*_impl` helper
//!     returning `Result<T, AppError>` and `.map_err(String::from)` at the
//!     call site — preserves the plain-string wire format frontends already
//!     parse. See `commands/container_settings.rs` for the reference impl.
//!
//! Common variants: `ConfigError` (settings/persistence), `JsonError` (auto
//! from `?` on `serde_json::Error`), `IoError` (auto from `?` on
//! `std::io::Error`), `NetworkError` (auto from `reqwest::Error`),
//! `DatabaseError`, `ValidationError`, `TimeoutError`, `StateError`. Full
//! enum and all `From` impls live in `src/error.rs`; add a new variant
//! there if no existing one fits, along with a matching `to_user_facing`
//! arm so the frontend error-code mapping stays consistent.

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
pub mod autostart; // Phase F.1 — launch-on-system-startup toggle (tauri-plugin-autostart wrapper)
pub mod backup; // Comprehensive backup and restore
pub mod build_id; // SW-cache-invalidation: exposes RUNNER_BUILD_ID baked at compile time
pub mod checkpoint_browser; // Orchestrator checkpoint browser (time-travel debugging)
pub mod checkpoints;
pub mod checks; // Code quality checks (linting, formatting, type checking)
pub mod chunk_labels; // Per-config user-chosen chunk label overrides for chunked state-machine graph view
pub mod claims; // Plan 2026-05-18-agent-spawn-coordination Phase 3 — Tauri wrappers around coord's /claims/* API
pub mod clipboard; // Clipboard sync: share text to mobile via backend relay
pub mod cloud_sync_settings; // Cloud session sync consent toggle (plan 2026-07-09 session-history cloud sync)
pub mod command_interpreter; // Phase 8 — Tier-3 free-text → registry action via local `claude` CLI
pub mod comparison; // Side-by-side architecture comparison runs
pub mod compartments; // Workstream C: scoped wrappers around Arc<AppState> for gradual migration
pub mod config;
pub mod container_settings;
pub mod context;
pub mod cost_dashboard; // Cost dashboard with cache efficiency and phase breakdowns
pub mod dag_workflows; // DAG workflow import, export, and validation
pub mod database; // Database maintenance and optimization
pub mod dataset;
pub mod debug;
pub mod dev_findings; // Dev-only: seed synthetic findings into the frontend tracker
pub mod devenv_enroll; // Phase 2 — in-app devenv enrollment (no terminal); wraps env_agent::enroll
pub mod discoveries;
pub mod durable_execution; // Conductor-inspired replay, rollback, iteration diffs
pub mod event_search; // Unified full-text search across activity_timeline, observations, deferred_questions, error_events
pub mod execution;
pub mod execution_reporting;
pub mod execution_variables; // Execution variables (auth source, custom variables)
pub mod extraction;
pub mod federation; // Memory federation reports from coord
pub mod file_browser; // Safe read-only filesystem browsing for mobile
pub mod findings;
pub mod flow; // Flow designer commands
pub mod global_log_sources; // Global log source management
pub mod helper_tasks; // Helper Task Queue settings + collected answers (plan 2026-06-29)
pub mod hooks;
pub mod instances; // Runner instance management (dev feature)
pub mod interaction;
pub mod issues;
pub mod known_issues; // Known issues registry CRUD
pub mod learning; // Learning insights dashboard commands
pub mod library_sync; // Sync library items (checks, macros, etc.) to web backend
pub mod lock_yield_policy_settings; // Auto-yield-on-idle file-lock policy (Open Q4 / lock-yield-protocol-plan)
pub mod log_api; // Frontend → Rust log sync (general/image/action/AI/issues/RAG/project)
pub mod logging;
pub mod mcp; // MCP client management and tool calling
pub mod meta_optimizer; // Meta-optimizer recommendations and prompt registry
pub mod mobile; // Mobile development feedback (ADB, screenshots, logcat)
pub mod mobile_settings; // Mobile settings (ADB path, device config)
pub mod orchestration_loop_configs; // Orchestration loop saved config CRUD
pub mod otel_settings; // OpenTelemetry settings (endpoint, sampling, enable/disable)
pub mod page_spec_store; // User-saved page specs (persist generated specs to app data dir)
pub mod performance_metrics; // Performance metrics dashboard
pub mod playwright_settings;
pub mod productivity; // Phase 1 productivity stack: plans/tasks/upcoming-claims
pub mod project_logs;
pub mod rag;
pub mod recap; // Session recap overview
pub mod regression; // UI Bridge regression suite + run + diagnosis + per-assertion exercise log persistence (Section 11 Phase A2)
pub mod saved_projects; // User-curated project registry (wizard-populated, consumed by UI Bridge panel)
pub mod screenshot;
pub mod screenshots;
pub mod script_emitter; // Scripted-output (think-in-code) extraction-script emitter (Phase A)
pub mod scripted_output_settings; // get/save the ScriptedOutputSettings (provider, endpoint, ...)
pub mod security_settings;
pub mod self_healing_settings;
pub mod session; // Plan 2026-05-22-coord-native-session-coordination Phase 2 — unified Session primitive Tauri commands
pub mod session_prs; // Per-session PR merged/unmerged status for the Terminal zone-header dropdown (coord proxy)
pub mod setup_discovery; // Native (pure-Rust) setup-wizard discovery — no Python interpreter needed
pub mod setup_wizard; // First-launch setup wizard commands
pub mod shell_commands; // Shell command management and execution
pub mod spec_drift; // Spec drift detection (useUIElement vs spec assertions)
pub mod spec_sync_state; // SSE-backed mirror for useSpecSync progress (P2 SSE remediation)
pub mod state_explorer; // State explorer for AI-driven state machine exploration
pub mod state_machine;
pub mod state_machine_configs; // State machine config builder CRUD
pub mod step_outputs; // Step output collection for test builder
pub mod storage;
pub mod task_sync; // renamed from ai_task_reporting
pub mod tenant; // Plan 2026-05-22-coord-native-session-coordination §D12 / Phase 4 — active tenant resolver + per-machine pin
pub mod terminal;
pub mod terminal_analysis; // Terminal session analysis (session summary, architecture, change impact, etc.)
pub mod terminal_windows; // Pop-out terminal windows: open/close/move-session + assignment events (Phase 1)
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
pub mod web_integration; // Phase 3G: runner↔web backend integration toggle
pub mod window_manager; // OS-level window enumeration and activation
pub mod workflow_events; // Workflow event emission to backend for mobile push notifications
pub mod worktrees; // Phase F: pre-merge guard + force-merge Tauri commands

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
    /// Pre-conflict registry of files that decomposed-plan tasks expect to
    /// touch in the future. Populated by `/decompose-plan` and queried by
    /// the Coordinator alongside `file_registry_manager`. Advisory only —
    /// claims do not block sessions, but overlapping claims surface as
    /// expand-and-warn events. See productivity-stack plan §3.
    pub upcoming_file_registry: Arc<crate::executor::upcoming_file_registry::UpcomingFileRegistry>,
    /// Exclusive per-file lock manager. When a session edits a file, other
    /// sessions trying to edit the same file block until the lock is released.
    /// This prevents concurrent edits deterministically (no AI judgment needed).
    pub file_lock_manager: Arc<FileLockManager>,
    /// Best-effort broadcast sender for the Rust deconflicter loop
    /// (§4.1 of plans/2026-05-13-coord-as-deconflicter-plan.md). Fires
    /// every time `claude_session::dispatcher::auto_register_file`
    /// UPSERTs a row into `coord.session_touched_files`. Drop-on-no-
    /// receiver is fine — the deconflicter is a soft advisor and missed
    /// touches degrade gracefully (the next touch on the same path
    /// re-triggers).
    pub touch_events_tx: broadcast::Sender<crate::coordinator::deconflicter::TouchEvent>,
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
    /// Flag indicating the React frontend has finished its loading screen and
    /// is processing UI Bridge IPC. Flips to `true` the first time
    /// `ui_bridge_request_sync` decodes a successful response from the
    /// frontend; one-way (never flipped back to false). Surfaced in `/health`
    /// as `frontendReady` so external pollers can distinguish "Tauri shell is
    /// responsive" from "the React app has actually mounted past `App.tsx`'s
    /// loading screen branch and is ready for UI Bridge calls".
    pub frontend_ready: AtomicBool,
    /// Actual port the HTTP API server bound to.
    /// Set by `mcp_api::start_server` after successful bind.
    pub api_port: AtomicU16,
    /// Whether the HTTP API server's actual bound address is LAN-reachable
    /// (i.e. it bound a non-loopback interface). Set by
    /// `mcp_api::start_server` from the listener's real local address at
    /// bind time — currently always `false` because `try_bind_port`
    /// intentionally binds `127.0.0.1` only — but DERIVED rather than
    /// hardcoded so a future non-loopback bind flips it automatically.
    /// Forwarded on backend heartbeats as `lan_reachable` so the fleet
    /// registry stops handing mobile clients an advertised LAN `ip:port`
    /// the runner does not actually serve (plan
    /// 2026-06-12-mobile-account-usage-error-recovery, runner P1).
    pub api_lan_bound: AtomicBool,
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
    /// Web-backend integration state (Phase 3G: settings-driven).
    ///
    /// `Some` when [`crate::settings::WebIntegrationSettings::enabled`] is
    /// true and both `backend_url` and `runner_token` are set. Holds the
    /// runner_id assigned by `/api/v1/runners/register` and the token needed
    /// for authenticated POSTs to the web backend.
    ///
    /// Wrapped in `Arc<RwLock<...>>` so the settings-save command can
    /// hot-reload the integration without restarting the runner: the command
    /// signals shutdown on the old state, builds a new one, and swaps it in.
    /// Consumers should use [`AppState::current_server_mode`] which clones
    /// the inner `Option<ServerModeState>` under a read lock.
    pub server_mode: Arc<tokio::sync::RwLock<Option<crate::server_mode::ServerModeState>>>,
    /// Nuanced runner-health: latest UI error reported by the React
    /// `ErrorBoundary` (Phase 3J.1/3J.2).
    ///
    /// Populated via the `report_ui_error` Tauri command, cleared via
    /// `clear_ui_error`. Surfaced on the `/health` endpoint
    /// (`derived_status` + `ui_error` fields) and on outgoing heartbeats
    /// so supervisors and the qontinui-web fleet view can flag runners
    /// whose backend is up but whose UI is broken.
    pub ui_error: Arc<crate::ui_error::UiErrorState>,
    /// Recent (within the freshness window) Rust crash dump discovered at
    /// startup. Closes the 3J gap for non-unwinding panics that abort the
    /// process before the React ErrorBoundary can report them — the
    /// restarted runner marks itself `errored` via `/health.derived_status`
    /// until the user dismisses the record via `dismiss_recent_crash`.
    pub crash_dumps: Arc<crate::crash_dumps::CrashDumpState>,
    /// USB/ADB transport, published here by the `mcp_api` physical-device
    /// scanner so the CloseRequested handler in `main.rs` can call
    /// `release_all` on shutdown and clean up any `adb forward` entries this
    /// process installed.
    ///
    /// Force-kill (supervisor `taskkill /F`) still leaks — this only helps
    /// the graceful path where Tauri's window close event fires. Tracked in
    /// the adb-forwarder-port plan §1.6a.
    pub usb_transport: Arc<tokio::sync::OnceCell<crate::mcp::transport::usb::UsbTransport>>,
    /// Late-initialized handle to the shared `AppRegistry`. Populated by
    /// `mcp_api::start_server` immediately after it constructs the registry,
    /// so non-HTTP code paths (workflow generation, agentic prompt
    /// assembly) can introspect registered apps without taking a dep on
    /// `ApiState`. Reads via `.get()`; the cell is set exactly once.
    pub app_registry: Arc<tokio::sync::OnceCell<Arc<crate::mcp::app_registry::AppRegistry>>>,
    /// Late-initialized handle to the shared `AppDispatcher`. Same lifetime
    /// + access shape as `app_registry` above. Lets non-HTTP code dispatch
    /// commands to registered apps via the unified HTTP-or-WS transport
    /// without re-wiring ApiState.
    pub app_dispatcher: Arc<tokio::sync::OnceCell<Arc<crate::mcp::app_dispatch::AppDispatcher>>>,
    /// Late-initialized handle to the wrapper subsystem (registry +
    /// manager + dispatch). Populated during HTTP server bootstrap so
    /// `/wrappers/*` routes can resolve via the same OnceCell pattern as
    /// `app_registry` / `app_dispatcher`.
    pub wrapper_state: Arc<tokio::sync::OnceCell<Arc<crate::wrappers::WrapperState>>>,
    /// Speculative MCP / link pre-hydration cache (Track C3), keyed by
    /// `execution_id`. Populated best-effort before the first agentic turn with
    /// a timestamped, prompt-ready summary of available MCP tools + the gist of
    /// any URLs/issue-refs in the task description. Read once to prime the
    /// first prompt; cleared on execution completion. Never on the task's
    /// critical path — a pre-fetch failure leaves this empty and the task
    /// proceeds unchanged.
    pub prehydration_cache: Arc<crate::unified_workflow_executor::prehydration::PrehydrationCache>,
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

    /// Snapshot the current web-integration state.
    ///
    /// Returns a cloned `Option<ServerModeState>` under a read lock, so the
    /// caller does not hold the lock while awaiting downstream HTTP work.
    /// Callers that need to inspect state fields (runner_id) should use
    /// this helper rather than locking directly — the inner
    /// `ServerModeState` already shares its data via `Arc<RwLock<...>>`, so
    /// the clone is cheap.
    pub async fn current_server_mode(&self) -> Option<crate::server_mode::ServerModeState> {
        self.server_mode.read().await.clone()
    }

    /// Install a `ServerModeState` from current settings if none is present,
    /// returning the live state (existing or freshly installed), or `None`
    /// when current settings don't form a valid config.
    ///
    /// Closes the "integration disabled at boot" gap — see
    /// [`crate::server_mode::install_if_absent`]. The relay calls this on a
    /// successful connect so it always has somewhere to publish connection
    /// state, even when boot left the slot empty and a later sign-in enabled
    /// integration without routing through `apply_web_integration_settings`.
    pub async fn install_server_mode_if_absent(
        &self,
    ) -> Option<crate::server_mode::ServerModeState> {
        crate::server_mode::install_if_absent(
            &self.server_mode,
            &crate::settings::load_settings().web_integration,
        )
        .await
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
