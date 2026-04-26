//! Compartment wrapper types for `AppState` (Workstream C, Move 2B).
//!
//! Each compartment is a newtype wrapper around `Arc<AppState>` that exposes
//! only the fields in its cohesive sub-domain via borrow-returning accessor
//! methods. This lets per-module Tauri plugins migrate from the god-object
//! `State<'_, Arc<AppState>>` to a scoped `State<'_, <Compartment>>` one
//! module at a time, without reshuffling `AppState`'s field layout.
//!
//! # Partitioning (matches `REFACTOR_WAVE_3_PLAN.md` Workstream C)
//!
//! - [`BridgeCompartment`] — bridge / URL-lock / extraction / file registry
//!   / file locks / UI-bridge failure tracker / exploration cancel.
//! - [`ExecutionCompartment`] — run cost trackers, orchestration loops,
//!   working-representation cache, process-capture, container executor,
//!   run-recording handler, current config, AI PID tracker, canvas state.
//! - [`IntegrationCompartment`] — SDK connection, MCP client manager,
//!   server mode, token flow, USB transport, event broadcast.
//! - [`HealthCompartment`] — doctor handle, error-monitor handle, crash
//!   dumps, UI error, api_ready, api_port.
//! - [`StorageCompartment`] — PostgreSQL DB, local storage, video recorder,
//!   display processor.
//!
//! # Design constraints
//!
//! - **No behavior change.** Each accessor returns a reference (`&Field`) to
//!   the existing `AppState` field, so locking/atomic semantics are
//!   identical to direct field access.
//! - **Both `State<'_, Arc<AppState>>` and `State<'_, <Compartment>>` are
//!   live simultaneously.** `main.rs` calls `.manage(shared_app_state)` plus
//!   `.manage(<each compartment>)`; unmigrated modules keep the old pattern
//!   and migrated modules use the new one.
//! - **AppState field layout is untouched this pass.** Compartments are
//!   thin read-only views over `Arc<AppState>`, not a restructuring.
//!
//! # Migration pattern
//!
//! See the module doc-comment in `commands/mod.rs` for the step-by-step
//! recipe to migrate a single Tauri command module onto a compartment.

use std::sync::Arc;

use super::AppState;

/// Bridge / URL-lock / extraction concerns.
///
/// Wraps `Arc<AppState>` and exposes only the bridge-domain fields. Future
/// sessions migrate plugins from `State<Arc<AppState>>` to
/// `State<BridgeCompartment>` per `REFACTOR_WAVE_3_PLAN.md` Workstream C.
#[derive(Clone)]
pub struct BridgeCompartment(pub(crate) Arc<AppState>);

impl BridgeCompartment {
    pub fn bridge_manager(
        &self,
    ) -> &tokio::sync::Mutex<Option<Arc<crate::executor::BridgeManager>>> {
        &self.0.bridge_manager
    }

    pub fn extraction_executor(
        &self,
    ) -> &std::sync::Mutex<Option<crate::executor::ExtractionExecutor>> {
        &self.0.extraction_executor
    }

    pub fn url_lock_manager(&self) -> &Arc<crate::executor::UrlLockManager> {
        &self.0.url_lock_manager
    }

    pub fn file_registry_manager(&self) -> &Arc<crate::executor::FileRegistryManager> {
        &self.0.file_registry_manager
    }

    pub fn file_lock_manager(&self) -> &Arc<crate::executor::FileLockManager> {
        &self.0.file_lock_manager
    }

    pub fn ui_bridge_failure_tracker(
        &self,
    ) -> &crate::step_executor::handlers::ui_bridge::UiBridgeFailureTracker {
        &self.0.ui_bridge_failure_tracker
    }

    pub fn exploration_cancel(
        &self,
    ) -> &Arc<tokio::sync::Mutex<Option<tokio_util::sync::CancellationToken>>> {
        &self.0.exploration_cancel
    }

    /// **Escape hatch — use sparingly.**
    ///
    /// Returns a clonable `Arc<AppState>` reference for handlers that must
    /// pass the full AppState to a downstream constructor (e.g. legacy types
    /// like `state_explorer::ExplorationTask::new`, `meta_optimizer::
    /// MetaOptimizerDeps`, or `workflow_generation::generate_workflow`) which
    /// pre-date the compartment split and have not been refactored to take
    /// only the fields they need.
    ///
    /// New code should NOT use this — pick the compartment whose accessor
    /// methods cover the fields you need, or split the call across multiple
    /// `State<_, <Compartment>>` parameters. Greppable at call sites as
    /// `.app_state()` so a future migration can find and remove every use.
    pub fn app_state(&self) -> &Arc<AppState> {
        &self.0
    }
}

/// Execution / orchestration / per-run tracking concerns.
#[derive(Clone)]
pub struct ExecutionCompartment(pub(crate) Arc<AppState>);

impl ExecutionCompartment {
    pub fn run_cost_trackers(
        &self,
    ) -> &tokio::sync::Mutex<
        std::collections::HashMap<String, Arc<crate::cost_management::RunCostTrackers>>,
    > {
        &self.0.run_cost_trackers
    }

    pub fn orchestration_loops(&self) -> &crate::orchestration_loop::loop_engine::SharedLoopStates {
        &self.0.orchestration_loops
    }

    pub fn working_representation_cache(
        &self,
    ) -> &Arc<crate::memory::working_representation::WorkingRepresentationCache> {
        &self.0.working_representation_cache
    }

    pub fn process_capture_manager(
        &self,
    ) -> &tokio::sync::Mutex<Option<Arc<crate::process_capture::ProcessCaptureManager>>> {
        &self.0.process_capture_manager
    }

    pub fn container_executor(
        &self,
    ) -> &tokio::sync::Mutex<Option<crate::container::isolated_executor::IsolatedExecutor>> {
        &self.0.container_executor
    }

    pub fn run_recording_handler(&self) -> &Arc<crate::tiered_info::RunRecordingHandler> {
        &self.0.run_recording_handler
    }

    pub fn current_config(&self) -> &std::sync::Mutex<Option<crate::config::QontinuiConfig>> {
        &self.0.current_config
    }

    pub fn ai_pid_tracker(&self) -> &Arc<std::sync::Mutex<Vec<u32>>> {
        &self.0.ai_pid_tracker
    }

    pub fn canvas_state(&self) -> &Arc<tokio::sync::RwLock<crate::mcp::canvas::CanvasState>> {
        &self.0.canvas_state
    }

    /// Register cost trackers for a new run.
    ///
    /// Mirrors `AppState::register_cost_trackers` — the AppState version
    /// remains as a delegating wrapper so existing callers that hold a
    /// `Arc<AppState>` keep working unchanged.
    pub async fn register_cost_trackers(
        &self,
        execution_id: &str,
    ) -> Arc<crate::cost_management::RunCostTrackers> {
        self.0.register_cost_trackers(execution_id).await
    }

    /// Get cost trackers for an active run.
    pub async fn get_cost_trackers(
        &self,
        execution_id: &str,
    ) -> Option<Arc<crate::cost_management::RunCostTrackers>> {
        self.0.get_cost_trackers(execution_id).await
    }

    /// Remove cost trackers after run completion.
    pub async fn remove_cost_trackers(&self, execution_id: &str) {
        self.0.remove_cost_trackers(execution_id).await
    }

    /// **Escape hatch — use sparingly.** See [`BridgeCompartment::app_state`].
    pub fn app_state(&self) -> &Arc<AppState> {
        &self.0
    }
}

/// SDK / MCP / server-mode / USB transport / event broadcast concerns.
#[derive(Clone)]
pub struct IntegrationCompartment(pub(crate) Arc<AppState>);

impl IntegrationCompartment {
    pub fn sdk_connection(
        &self,
    ) -> &Arc<tokio::sync::Mutex<crate::mcp::sdk_client::SdkConnectionManager>> {
        &self.0.sdk_connection
    }

    pub fn mcp_client_manager(&self) -> &tokio::sync::Mutex<crate::mcp_client::McpClientManager> {
        &self.0.mcp_client_manager
    }

    pub fn server_mode(
        &self,
    ) -> &Arc<tokio::sync::RwLock<Option<crate::server_mode::ServerModeState>>> {
        &self.0.server_mode
    }

    pub fn token_flow(&self) -> &Arc<crate::server_mode::TokenFlowStore> {
        &self.0.token_flow
    }

    pub fn usb_transport(
        &self,
    ) -> &Arc<tokio::sync::OnceCell<crate::mcp::transport::usb::UsbTransport>> {
        &self.0.usb_transport
    }

    pub fn event_broadcast(&self) -> &tokio::sync::broadcast::Sender<serde_json::Value> {
        &self.0.event_broadcast
    }

    /// **Escape hatch — use sparingly.** See [`BridgeCompartment::app_state`].
    pub fn app_state(&self) -> &Arc<AppState> {
        &self.0
    }
}

/// Doctor / error-monitor / crash-dump / UI-error / API-ready state.
#[derive(Clone)]
pub struct HealthCompartment(pub(crate) Arc<AppState>);

impl HealthCompartment {
    pub fn doctor_handle(&self) -> &tokio::sync::Mutex<Option<crate::doctor::DoctorHandle>> {
        &self.0.doctor_handle
    }

    pub fn error_monitor_handle(
        &self,
    ) -> &tokio::sync::Mutex<Option<crate::error_monitor::ErrorMonitorHandle>> {
        &self.0.error_monitor_handle
    }

    pub fn crash_dumps(&self) -> &Arc<crate::crash_dumps::CrashDumpState> {
        &self.0.crash_dumps
    }

    pub fn ui_error(&self) -> &Arc<crate::ui_error::UiErrorState> {
        &self.0.ui_error
    }

    pub fn api_ready(&self) -> &std::sync::atomic::AtomicBool {
        &self.0.api_ready
    }

    pub fn api_port(&self) -> &std::sync::atomic::AtomicU16 {
        &self.0.api_port
    }

    /// **Escape hatch — use sparingly.** See [`BridgeCompartment::app_state`].
    pub fn app_state(&self) -> &Arc<AppState> {
        &self.0
    }
}

/// PostgreSQL / local-storage / video-recorder / display-processor concerns.
#[derive(Clone)]
pub struct StorageCompartment(pub(crate) Arc<AppState>);

impl StorageCompartment {
    pub fn pg_db(&self) -> &Arc<crate::database::pg::PgDb> {
        &self.0.pg_db
    }

    pub fn local_storage(&self) -> &Arc<std::sync::Mutex<crate::storage::LocalStorage>> {
        &self.0.local_storage
    }

    pub fn video_recorder(
        &self,
    ) -> &Arc<std::sync::Mutex<crate::video_recorder::VideoRecordingService>> {
        &self.0.video_recorder
    }

    pub fn display_processor(&self) -> &Arc<tokio::sync::Mutex<crate::display::DisplayProcessor>> {
        &self.0.display_processor
    }

    /// **Escape hatch — use sparingly.** See [`BridgeCompartment::app_state`].
    pub fn app_state(&self) -> &Arc<AppState> {
        &self.0
    }
}
