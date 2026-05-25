//! Shared types and structs for MCP API
//!
//! Contains all request/response types used across the MCP API modules.
//! This is the authoritative location for ApiState and core API types.

// These types are used for JSON serialization via serde - the compiler doesn't
// see the fields as "used" but they are accessed at runtime during serialization.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::action_service::UnifiedActionService;
use crate::commands::rag::RAGState;
use crate::commands::AppState;
use crate::config_storage::ConfigStorage;
use crate::doctor::DoctorHandle;
use crate::reflection::graph_engine::KnowledgeGraph;

/// Cached result of the last successful UI Bridge discover call.
///
/// The React-side element registry does not persist the discover result
/// across subsequent `get_elements` / `get_snapshot` calls: elements that
/// existed at discover time may have unmounted or been pruned from the
/// registry by the time the next HTTP call arrives. This cache lets agents
/// retrieve the raw discover payload via
/// `GET /ui-bridge/control/elements/last-discovered` so they can act on the
/// element set they just observed.
pub struct CachedDiscoverResult {
    /// Raw JSON payload returned by the frontend for the discover request.
    pub data: serde_json::Value,
    /// When the discover completed.
    pub captured_at: std::time::Instant,
    /// Number of elements in the discover result at capture time (for convenience).
    pub element_count: usize,
}

/// Cached knowledge graph with TTL + generation-based invalidation.
/// The graph is wrapped in `Arc` so it can be shared without cloning the
/// entire petgraph structure. The `generation` field tracks the cache
/// generation at build time — when `graph_cache_generation` on `ApiState`
/// is bumped (e.g. after a finding/fix/rule write), the cache is
/// considered stale even if the TTL hasn't expired.
pub struct CachedKnowledgeGraph {
    pub graph: Arc<KnowledgeGraph>,
    pub built_at: std::time::Instant,
    /// Generation counter snapshot when this cache entry was built.
    pub generation: u64,
}

/// Default port for the MCP API server
pub const MCP_API_PORT: u16 = 9876;

/// Get the MCP API port, checking `QONTINUI_PORT` env var first.
///
/// ⚠️ Only safe to use for **bootstrap** — i.e. binding the HTTP listener
/// before `AppState` is fully initialized. Once `app_state.api_port` is
/// populated with the actually-bound port, prefer
/// [`runner_api_port`] (or [`get_self_base_url`] / [`runner_ui_bridge_base`])
/// so secondary/temp runners on non-default ports produce correct URLs.
/// Env-var-only lookup silently falls back to `MCP_API_PORT` (9876), which
/// is wrong for any runner not bound to the default port.
pub fn get_mcp_api_port() -> u16 {
    std::env::var("QONTINUI_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(MCP_API_PORT)
}

/// Returns the base URL for this runner instance (e.g., "http://localhost:9877").
/// Reads the actual bound port from AppState.
pub fn get_self_base_url(app_state: &crate::commands::AppState) -> String {
    let port = app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);
    format!("http://localhost:{}", port)
}

/// Returns just the actually-bound API port for this runner instance.
///
/// Convenience wrapper around `app_state.api_port` for callers that only
/// need the port number (e.g. prompt substitution / templating) and don't
/// want to assemble a full base URL.
pub fn runner_api_port(app_state: &crate::commands::AppState) -> u16 {
    app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed)
}

/// Identifies which UI Bridge endpoint family a URL targets.
///
/// The runner exposes two distinct UI Bridge endpoint families that must
/// not be conflated:
///
/// - [`UiBridgeFamily::Control`] (`/ui-bridge/control/...`) — drives the
///   runner's own React UI (Tauri webview). Used by agents to inspect and
///   interact with the runner itself.
/// - [`UiBridgeFamily::Sdk`] (`/ui-bridge/sdk/...`) — drives external apps
///   that have embedded the UI Bridge SDK (e.g. qontinui-web, mobile
///   browser sessions, customer apps).
///
/// Using the wrong family silently produces responses from the wrong
/// target, so helpers that build UI Bridge URLs must take this enum
/// rather than hardcoding the path segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiBridgeFamily {
    /// Runner's own UI (`/ui-bridge/control/...`).
    Control,
    /// External apps embedding the UI Bridge SDK (`/ui-bridge/sdk/...`).
    Sdk,
}

impl UiBridgeFamily {
    /// The path segment used after `/ui-bridge/` for this family.
    pub fn as_path_segment(&self) -> &'static str {
        match self {
            UiBridgeFamily::Control => "control",
            UiBridgeFamily::Sdk => "sdk",
        }
    }
}

/// Returns the base URL for a UI Bridge endpoint family on this runner
/// instance, e.g. `"http://localhost:9877/ui-bridge/control"` or
/// `"http://localhost:9877/ui-bridge/sdk"`.
///
/// Reads the actually-bound port from `app_state.api_port` (same source
/// as [`get_self_base_url`]), so secondary/temp runners on non-default
/// ports produce correct URLs.
pub fn runner_ui_bridge_base(
    app_state: &crate::commands::AppState,
    family: UiBridgeFamily,
) -> String {
    let port = app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);
    format!(
        "http://localhost:{}/ui-bridge/{}",
        port,
        family.as_path_segment()
    )
}

/// Returns the base URL using the configured port (env var or default).
/// Use this in code paths that don't have access to AppState.
/// For code paths that do have AppState, prefer `get_self_base_url` which
/// uses the actual bound port and works correctly with multi-instance runners.
pub fn get_self_base_url_from_env() -> String {
    format!("http://localhost:{}", get_mcp_api_port())
}

/// Shared state for the API server (authoritative definition)
pub struct ApiState {
    pub app_state: Arc<AppState>,
    pub rag_state: Arc<RAGState>,
    pub app_handle: tauri::AppHandle,
    /// Currently loaded config ID (for tracking which config is active)
    pub current_config_id: std::sync::Mutex<Option<String>>,
    /// Persistent storage for configurations
    pub config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
    /// Unified action service for deterministic execution
    pub action_service: Arc<UnifiedActionService>,
    /// Currently running AI process PIDs (for stopping)
    pub current_ai_pids: Arc<std::sync::Mutex<Vec<u32>>>,
    /// Server start time for uptime calculation
    pub started_at: std::time::Instant,
    /// Pending UI Bridge requests waiting for frontend response
    pub ui_bridge_pending:
        Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>>>,
    /// Lock-free pending request counter for the health endpoint.
    /// Incremented when a request is inserted, decremented when removed or timed out.
    pub ui_bridge_pending_count: Arc<std::sync::atomic::AtomicUsize>,
    /// UI Bridge circuit breaker state
    pub ui_bridge_circuit_breaker: Arc<crate::mcp::ui_bridge::UiBridgeCircuitBreaker>,
    /// UI Bridge concurrency semaphore (max 6 concurrent requests)
    pub ui_bridge_semaphore: Arc<tokio::sync::Semaphore>,
    /// Last frontend pong timestamp (epoch ms)
    pub ui_bridge_last_pong: Arc<std::sync::atomic::AtomicU64>,
    /// Readiness gate — notified on first pong. Requests wait on this during startup.
    pub ui_bridge_ready: Arc<tokio::sync::Notify>,
    /// Pending dedup channels for read-only UI Bridge requests
    pub ui_bridge_dedup: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<
                String,
                tokio::sync::broadcast::Sender<Result<serde_json::Value, String>>,
            >,
        >,
    >,
    /// Last known console error count (updated by console-errors handler and health probes)
    pub ui_bridge_console_error_count: Arc<std::sync::atomic::AtomicU64>,
    /// Render log entries for diagnostics (in-memory)
    pub ui_bridge_render_log: Arc<tokio::sync::Mutex<Vec<serde_json::Value>>>,
    /// Web extraction state tracking
    pub extraction_state: Arc<crate::mcp::extraction::ExtractionState>,
    /// SDK app connections manager (supports multiple simultaneous connections)
    pub sdk_connection: Arc<tokio::sync::Mutex<crate::mcp::sdk_client::SdkConnectionManager>>,
    /// Doctor health monitoring handle for AI process health tracking.
    pub doctor_handle: Option<DoctorHandle>,
    /// Instance manager for spawning/stopping secondary runner processes.
    pub instance_manager: Arc<crate::instance_manager::InstanceManager>,
    /// UI Bridge event sequence counter (monotonically increasing per runner lifetime)
    pub ui_bridge_event_sequence: std::sync::atomic::AtomicI64,
    /// Last successful UI Bridge discover result, cached in-process so
    /// agents can fetch the element set they just observed without racing
    /// the React registry. See `CachedDiscoverResult` for rationale.
    /// Populated by `ui_bridge_discover_handler`, read by
    /// `ui_bridge_get_last_discovered_handler`.
    pub ui_bridge_last_discovered: Arc<tokio::sync::RwLock<Option<CachedDiscoverResult>>>,
    /// Cached knowledge graph with TTL-based invalidation (60s).
    /// Avoids rebuilding the expensive petgraph on every request.
    pub knowledge_graph_cache: Arc<tokio::sync::RwLock<Option<CachedKnowledgeGraph>>>,
    /// Generation counter for event-driven graph cache invalidation.
    /// Bumped by write handlers (findings, fixes, rules, observations, patterns)
    /// so the graph rebuilds on next access even if the TTL hasn't expired.
    pub graph_cache_generation: Arc<std::sync::atomic::AtomicU64>,
    /// Shared accessibility manager for native a11y tree capture and interaction.
    /// Used by MCP API accessibility endpoints and Tauri commands.
    pub accessibility_manager:
        Arc<tokio::sync::Mutex<qontinui_runner_lib::accessibility::AccessibilityManager>>,
    /// Registry of physical mobile devices across all transports
    pub physical_device_registry: Arc<crate::mcp::physical_device::PhysicalDeviceRegistry>,
    /// Phone-home registry for apps that have registered via POST /ui-bridge/apps/register.
    pub app_registry: Arc<crate::mcp::app_registry::AppRegistry>,
    /// Phase 1 wrapper framework: tracks WebSocket connections keyed by
    /// monotonic `conn_id`, with a reverse `app_id → conn_id` map for
    /// last-tab-wins routing. Populated by `/ui-bridge/ws` upgrades.
    pub ws_connection_manager: Arc<crate::mcp::ws_relay::WsConnectionManager>,
    /// Phase 1 wrapper framework: pending-command state machine that
    /// dispatches to WebSocket-transport apps and awaits their response
    /// frames. Used by `AppDispatcher`.
    pub ws_command_relay: Arc<crate::mcp::command_relay::CommandRelay>,
    /// Unified dispatcher that picks HTTP or WebSocket transport based on
    /// each registered app's declared transport. Handlers that want to
    /// proxy to a specific `app_id` should use this instead of reqwest
    /// directly.
    pub app_dispatcher: Arc<crate::mcp::app_dispatch::AppDispatcher>,
    /// Pairing manager for LAN and Cloud device authentication
    pub pairing_manager: Arc<crate::mcp::transport::pairing::PairingManager>,
    /// Rathole tunnel client (Plan 1B). Singleton — only one tunnel active.
    pub tunnel_client: Arc<crate::tunnel::RatholeClient>,
    /// iOS transport sidecar (Plan 2). Started lazily on first iOS request.
    pub ios_transport: Arc<crate::mcp::transport::ios::IosTransport>,
    /// Pending UI Bridge invoke-proxy requests (Phase 3I.1).
    /// Keyed by request_id; HTTP handler awaits the oneshot receiver, the
    /// `ui-bridge:invoke-response` Tauri listener in `mcp_api.rs` fires the sender.
    pub ui_bridge_invoke_store: Arc<crate::ui_bridge_invoke::InvokeRequestStore>,
    /// Pending UI Bridge page/evaluate requests (Plan item D, post-Phase-3J).
    /// Mirrors `ui_bridge_invoke_store` but for the `/page/evaluate` HTTP
    /// route — keyed by request_id so concurrent callers never interleave
    /// responses. The `ui-bridge:evaluate-response` Tauri listener in
    /// `mcp_api.rs` fires the sender on arrival.
    pub ui_bridge_evaluate_store: Arc<crate::ui_bridge_evaluate::EvaluateRequestStore>,
    /// D5 Phase 1 Git Supervision Channel — bounded in-process ring buffer
    /// of recent `GitSupervisionEvent`s appended by the trigger system's
    /// `SupervisionProposal` dispatch path. Exposed via
    /// `GET /git-supervision/recent-events` for diagnostics and via the
    /// `git-supervision` Tauri event channel for the frontend hook.
    pub supervision_state: crate::git_supervision::SupervisionState,
    /// D4+D6 Blind-Spot Recommender (Phase 2) — read-through facade over
    /// the per-sub-space observer surfaces (SDK connections, file/git
    /// watchers). Holds clones of the same shared Arcs `ApiState` holds.
    pub observer_registry: crate::observer_registry::ObserverRegistry,
    /// Phase 3 vision pipeline: content-addressed disk cache for encoded
    /// capture bytes. Key = SHA-256 of (window_handle, scale, mutation_id,
    /// pipeline_hash); value = the verified bytes from `Pipeline::run`.
    /// Persisted under `tmp_vision_cache/` (scratch-file whitelisted).
    pub vision_cache: Arc<qontinui_vision_core::VisionCache>,
    /// Phase 3 vision pipeline: bounded permits around the xcap
    /// `spawn_blocking` capture. xcap is GDI-bound on Windows and exhibits
    /// the "fits-2-parallel-then-thrashes" pattern from the supervisor
    /// build pool — 2 permits matches that empirical sweet spot.
    pub vision_capture_semaphore: Arc<tokio::sync::Semaphore>,
    /// Phase 3 vision pipeline: monotonic mutation counter. Bumped by
    /// control handlers that can change rendered pixels (click, type,
    /// navigate). Folded into the cache key so any state-changing action
    /// transparently busts cache entries — no clock-based TTL needed.
    pub vision_mutation_id: Arc<std::sync::atomic::AtomicU64>,
    /// Phase 6 vision baselines: name → snapshot bboxes + provenance.
    /// Populated by `POST /ui-bridge/vision/baseline`, read by the
    /// `NoLayoutShiftSince { baseline }` assertion. Lives in-process —
    /// baselines do not persist across runner restarts (deliberate;
    /// they're operational, not durable artifacts).
    pub vision_baselines: Arc<
        std::sync::Mutex<
            std::collections::HashMap<
                String,
                crate::mcp::ui_bridge::vision_routes::BaselineRegistryEntry,
            >,
        >,
    >,
}

/// Response for API endpoints
#[derive(Debug, Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Structured error detail with machine-readable code and recovery hint.
    /// Added alongside `error` for backward compatibility.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_detail: Option<crate::mcp::ui_bridge::UiBridgeError>,
    /// Optional closest-match / recovery hint payload. Sibling field to
    /// `error` (does NOT replace the error envelope shape). Used by the
    /// three high-friction UI Bridge error paths (element-not-found,
    /// action-not-allowed, eval-rejected) to surface typo-recovery
    /// suggestions or workaround guidance to callers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<serde_json::Value>,
    /// Canonical SCREAMING_SNAKE_CASE error code for transport-layer and
    /// handler-level rejections (e.g. `INVALID_JSON`, `INVALID_REQUEST`,
    /// `UNSUPPORTED_MEDIA_TYPE`, `PAYLOAD_TOO_LARGE`). Absent on success
    /// responses and on legacy error paths that predate the envelope plan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Optional human-readable recovery suggestions surfaced to callers
    /// alongside `code`. Absent when no actionable guidance is available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestions: Option<Vec<String>>,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            error_detail: None,
            hint: None,
            code: None,
            suggestions: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message.into()),
            error_detail: None,
            hint: None,
            code: None,
            suggestions: None,
        }
    }

    pub fn error_detailed(
        message: impl Into<String>,
        detail: crate::mcp::ui_bridge::UiBridgeError,
    ) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message.into()),
            error_detail: Some(detail),
            hint: None,
            code: None,
            suggestions: None,
        }
    }

    /// Build an error response with a canonical SCREAMING_SNAKE_CASE `code`.
    /// Use for transport-layer rejections (JSON parse errors, missing
    /// Content-Type, body-too-large) where callers need a machine-readable
    /// discriminator in addition to the human-readable `error` message.
    pub fn error_with_code(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message.into()),
            error_detail: None,
            hint: None,
            code: Some(code.into()),
            suggestions: None,
        }
    }

    /// Like `error_with_code` but also attaches a list of human-readable
    /// recovery suggestions surfaced to callers alongside the `code`.
    pub fn error_with_code_and_suggestions(
        message: impl Into<String>,
        code: impl Into<String>,
        suggestions: Vec<String>,
    ) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message.into()),
            error_detail: None,
            hint: None,
            code: Some(code.into()),
            suggestions: Some(suggestions),
        }
    }
}

/// Create an error response
pub fn api_error(message: impl Into<String>) -> ApiResponse<()> {
    ApiResponse {
        success: false,
        data: None,
        error: Some(message.into()),
        error_detail: None,
        hint: None,
        code: None,
        suggestions: None,
    }
}

/// Create an error response with structured error detail
pub fn api_error_detailed(
    message: impl Into<String>,
    detail: crate::mcp::ui_bridge::UiBridgeError,
) -> ApiResponse<()> {
    ApiResponse {
        success: false,
        data: None,
        error: Some(message.into()),
        error_detail: Some(detail),
        hint: None,
        code: None,
        suggestions: None,
    }
}

/// Create an error response with both a structured detail and a closest-match
/// / recovery `hint` payload. Used by the three high-friction UI Bridge
/// paths (element-not-found, action-not-allowed, eval-rejected) — see
/// `ApiResponse::hint` for the design rationale. The `hint` is a sibling
/// field to `error`, preserving the existing `success: false` / `error`
/// envelope shape.
pub fn api_error_with_hint(
    message: impl Into<String>,
    detail: crate::mcp::ui_bridge::UiBridgeError,
    hint: serde_json::Value,
) -> ApiResponse<()> {
    ApiResponse {
        success: false,
        data: None,
        error: Some(message.into()),
        error_detail: Some(detail),
        hint: Some(hint),
        code: None,
        suggestions: None,
    }
}

/// Load config request
#[derive(Debug, Deserialize)]
pub struct LoadConfigRequest {
    pub config_path: String,
}

/// Run workflow request
#[derive(Debug, Deserialize)]
pub struct RunWorkflowRequest {
    pub workflow_name: String,
    #[serde(default)]
    pub monitor_index: Option<i32>,
    /// Timeout in seconds for execution completion (default: 300)
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

/// Default timeout for workflow/state navigation operations.
/// Returns 0 to indicate no timeout (run until completion).
pub fn default_timeout() -> u64 {
    0 // No timeout - run until completion
}

/// Default timeout for single action operations.
/// Returns 0 to indicate no timeout (run until completion).
pub fn default_action_timeout() -> u64 {
    0 // No timeout - run until completion
}

/// Execute single action request
#[derive(Debug, Deserialize)]
pub struct ExecuteActionRequest {
    /// Action type: "click", "double_click", "right_click", etc.
    pub action_type: String,
    /// Image ID from the loaded config
    pub image_id: String,
    /// Optional monitor index
    #[serde(default)]
    pub monitor_index: Option<i32>,
    /// Timeout in seconds for action completion (default: 30)
    #[serde(default = "default_action_timeout")]
    pub timeout_seconds: u64,
}

/// Execute action result
#[derive(Debug, Serialize)]
pub struct ExecuteActionResult {
    pub success: bool,
    pub action_type: String,
    pub image_id: String,
    pub error: Option<String>,
}

/// Go to state request - navigate to a target state using pathfinding
#[derive(Debug, Deserialize)]
pub struct GoToStateRequest {
    /// The state ID or name to navigate to
    pub state_id: String,
    /// Optional monitor index
    #[serde(default)]
    pub monitor_index: Option<i32>,
    /// Timeout in seconds for navigation completion (default: 300)
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

/// Go to state result
#[derive(Debug, Serialize)]
pub struct GoToStateResult {
    pub success: bool,
    pub state_id: String,
    pub error: Option<String>,
}

/// Execution result
#[derive(Debug, Serialize)]
pub struct ExecutionResult {
    pub success: bool,
    pub workflow_name: String,
    pub error: Option<String>,
}

/// Capture screenshot request for AI Automation Builder
#[derive(Debug, Deserialize)]
pub struct CaptureScreenshotRequest {
    /// Monitor index (0-based), None for all monitors combined
    #[serde(default)]
    pub monitor: Option<i32>,
    /// Delay in seconds before capture (0-30)
    #[serde(default)]
    pub delay_seconds: Option<f64>,
    /// Task/run identifier for filename (e.g., "ai-task-abc123")
    #[serde(default)]
    pub task_id: Option<String>,
    /// Step index for filename ordering
    #[serde(default)]
    pub step_index: Option<u32>,
}

/// Capture screenshot response
#[derive(Debug, Serialize)]
pub struct CaptureScreenshotResponse {
    pub success: bool,
    /// Relative path to screenshot in .dev-logs/screenshots/
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
    /// Absolute path to screenshot
    #[serde(skip_serializing_if = "Option::is_none")]
    pub absolute_path: Option<String>,
    /// Screenshot width in pixels
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<i32>,
    /// Screenshot height in pixels
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<i32>,
    /// Monitor that was captured (None = all monitors)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor: Option<i32>,
    /// Error message if capture failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Monitor info for the API response.
/// Matches the Monitor type from qontinui-schemas/geometry.
#[derive(Debug, Serialize)]
pub struct MonitorInfoResponse {
    pub index: usize,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Spatial position: "left", "center", or "right"
    pub position: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_primary: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale_factor: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Human-readable description (runner-specific extension)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Monitors response
#[derive(Debug, Serialize)]
pub struct MonitorsResponse {
    pub count: usize,
    pub monitors: Vec<MonitorInfoResponse>,
    pub available_descriptors: Vec<String>,
}

// Re-export AiSessionContext from the canonical location

/// A single execution step to run before AI session
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExecutionStepConfig {
    /// Step type: "state", "action", "workflow", "screenshot", "playwright"
    #[serde(rename = "type")]
    pub step_type: String,
    /// Step name (for state navigation, workflow name, etc.)
    #[serde(default)]
    pub name: Option<String>,
    /// For action steps: click, double_click, etc.
    #[serde(rename = "actionType")]
    pub action_type: Option<String>,
    /// Target image ID for click actions
    #[serde(rename = "targetImageId")]
    pub target_image_id: Option<String>,
    /// Target image name for display
    #[serde(rename = "targetImageName")]
    pub target_image_name: Option<String>,
    /// Monitor index for action/state steps (0 = primary monitor)
    #[serde(rename = "monitorIndex", default)]
    pub monitor_index: Option<i32>,
    /// Whether to take screenshot after step
    #[serde(rename = "takeScreenshot", default)]
    pub take_screenshot: bool,
    /// Delay before screenshot in seconds
    #[serde(rename = "screenshotDelay", default)]
    pub screenshot_delay: u32,
    /// Monitor for screenshot
    #[serde(rename = "screenshotMonitor", default)]
    pub screenshot_monitor: Option<String>,
    /// Playwright script ID
    #[serde(rename = "playwrightScriptId")]
    pub playwright_script_id: Option<String>,
    /// Playwright script content (for combined/inline scripts)
    #[serde(rename = "playwrightScriptContent")]
    pub playwright_script_content: Option<String>,
    /// Playwright target URL (for combined scripts)
    #[serde(rename = "playwrightTargetUrl")]
    pub playwright_target_url: Option<String>,
    /// Whether this step is a setup step (brings app to target state) vs verification step.
    /// Setup steps typically run on the first iteration only.
    #[serde(rename = "isSetup", default)]
    pub is_setup: Option<bool>,
    /// Whether to run this step on subsequent iterations (after the first).
    /// For GUI automation: default false (setup usually only needed on first iteration)
    /// For Playwright: always true (Playwright closes environment, so setup is always needed)
    #[serde(rename = "runOnSubsequentIterations", default)]
    pub run_on_subsequent_iterations: Option<bool>,
}

/// Result of executing a single step
#[derive(Debug, Clone, Serialize)]
pub struct StepExecutionResult {
    pub step_index: usize,
    pub step_type: String,
    pub step_name: String,
    pub success: bool,
    pub error: Option<String>,
    pub screenshot_path: Option<String>,
    pub duration_ms: u64,
}

/// Request to start a new unified session
#[derive(Debug, Deserialize)]
pub struct StartSessionRequest {
    /// Session name (for display)
    pub name: String,
    /// Initial prompt content
    pub prompt: String,
    /// Prompt for continuation (if different)
    pub continuation_prompt: Option<String>,
    /// Total phases/iterations (0 = unlimited)
    #[serde(default)]
    pub total_phases: u32,
    /// Whether this session uses GUI automation
    #[serde(default)]
    pub uses_gui: bool,
    /// Timeout per phase in seconds (default 1800 = 30 min)
    #[serde(default = "default_session_timeout")]
    pub timeout_seconds: u64,

    /// Execution steps to run BEFORE spawning AI (deterministic)
    #[serde(default)]
    pub execution_steps: Vec<ExecutionStepConfig>,

    /// Goal configuration - specifies how to verify task completion.
    /// If not provided, defaults to PromptBased (trusts [TASK_COMPLETE] marker).
    #[serde(default)]
    pub goal_config: Option<super::goals::GoalConfig>,

    // Legacy fields - kept for backward compatibility but no longer used
    // State is now tracked via task_runs table and [TASK_COMPLETE] marker
    #[serde(default)]
    #[allow(dead_code)]
    pub checkpoint_path: Option<String>,
    #[serde(default = "default_phase_field")]
    #[allow(dead_code)]
    pub phase_field: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub completion_value: Option<u32>,
}

pub fn default_phase_field() -> String {
    "current_phase".to_string()
}

/// Default session inactivity timeout.
/// Returns 0 to indicate no timeout (run until completion).
pub fn default_session_timeout() -> u64 {
    0 // No timeout - session runs until completion
}

/// Information about a single active session
#[derive(Debug, Clone, Serialize)]
pub struct ActiveSessionInfo {
    /// Unique session ID
    pub id: String,
    /// Display name
    pub name: String,
    /// Current status (running, waiting_for_continuation, etc.)
    pub status: String,
    /// When the session started
    pub started_at: String,
    /// Whether this session uses GUI automation (blocks other GUI sessions)
    pub uses_gui: bool,
}

/// Response for resumable workflow check
#[derive(Debug, Serialize)]
pub struct ResumableWorkflowInfo {
    /// Whether a resumable workflow exists
    pub has_resumable: bool,
    /// Whether an AI workflow is currently running (prevents Continue button)
    pub is_running: bool,
    /// Whether auto-continue on restart is enabled (global setting)
    pub auto_continue_enabled: bool,
    /// Workflow name (if resumable)
    pub name: Option<String>,
    /// Current phase/iteration
    pub current_phase: Option<u32>,
    /// Total phases (0 = unlimited)
    pub total_phases: Option<u32>,
    /// When the workflow was started
    pub started_at: Option<String>,
    /// Number of cross-session continuations
    pub cross_session_count: Option<u32>,
    /// Status from checkpoint
    pub status: Option<String>,
    /// All currently active sessions (for concurrent session display)
    #[serde(default)]
    pub active_sessions: Vec<ActiveSessionInfo>,
}

/// Response type for resume workflow
#[derive(Debug, Serialize)]
pub struct ResumeWorkflowResponse {
    pub message: String,
    pub name: String,
}

/// Request body for force continue
#[derive(Debug, Deserialize)]
pub struct ForceContinueRequest {
    /// Optional task run ID to continue (if not provided, continues most recent running task)
    #[serde(default)]
    pub task_run_id: Option<String>,
    /// Optional custom continuation prompt (if not provided, uses a default)
    #[serde(default)]
    pub prompt: Option<String>,
}

/// Response type for force continue
#[derive(Debug, Serialize)]
pub struct ForceContinueResponse {
    pub message: String,
    pub session_id: String,
}

/// Response for auto-continue setting
#[derive(Debug, Serialize)]
pub struct AutoContinueSettingResponse {
    pub enabled: bool,
}

/// Request body for setting auto-continue
#[derive(Debug, Deserialize)]
pub struct SetAutoContinueRequest {
    pub enabled: bool,
}

/// Response for per-workflow auto-continue setting
#[derive(Debug, Serialize)]
pub struct WorkflowAutoContinueResponse {
    pub enabled: bool,
    pub workflow_name: Option<String>,
}

/// Query params for checkpoint status.
#[derive(Debug, Deserialize)]
pub struct CheckpointStatusQuery {
    pub completion_value: Option<u32>,
}

/// Query params for checkpoint history.
#[derive(Debug, Deserialize)]
pub struct CheckpointHistoryQuery {
    pub workflow_name: Option<String>,
    pub limit: Option<u32>,
}

/// Request body for saving a checkpoint.
#[derive(Debug, Deserialize)]
pub struct SaveCheckpointRequest {
    pub workflow_name: String,
    pub current_phase: u32,
    #[serde(default)]
    pub total_phases: Option<u32>,
    #[serde(default)]
    pub completed: bool,
    #[serde(default)]
    pub restart_permitted: bool,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub repos_to_process: Option<Vec<String>>,
    #[serde(default)]
    pub work_completed: Option<serde_json::Value>,
    #[serde(default)]
    pub items_needing_user_input: Option<Vec<String>>,
    #[serde(default)]
    pub error_message: Option<String>,
}

/// Query params for listing task runs.
#[derive(Debug, Deserialize)]
pub struct ListTaskRunsQuery {
    /// Maximum number of task runs to return (default: 50)
    pub limit: Option<u32>,
}

/// Request body for creating a task run.
#[derive(Debug, Deserialize)]
pub struct CreateTaskRunRequest {
    /// Name/identifier for this task
    pub task_name: String,
    /// The prompt to run
    pub prompt: String,
    /// Maximum number of sessions before giving up (optional)
    #[serde(default)]
    pub max_sessions: Option<u32>,
    /// Per-run auto-continue setting (defaults to true if not specified)
    #[serde(default)]
    pub auto_continue: Option<bool>,
    /// Whether this is a follow-up comment (not shown in main runs dropdown)
    #[serde(default)]
    pub is_followup: Option<bool>,
}

/// Query params for getting task output.
#[derive(Debug, Deserialize)]
pub struct TaskOutputQuery {
    /// Number of characters from end of output to return (optional)
    pub tail_chars: Option<usize>,
}

/// Request body for setting auto-continue on a task run.
#[derive(Debug, Deserialize)]
pub struct SetTaskAutoContinueRequest {
    pub auto_continue: bool,
}
