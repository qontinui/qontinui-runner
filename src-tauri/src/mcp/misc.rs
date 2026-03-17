//! Miscellaneous handlers for MCP API
//!
//! Contains bridge management, debug/status, legacy execution,
//! screenshot/render log, playwright collection, AI session management,
//! backup/restore, auto-continue settings, and other utility handlers.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tracing::{error, info, warn};

use crate::config::ConfigLoader;
use crate::config_storage::ConfigStorage;
use crate::context;
use crate::database::{CheckpointDb, CreateTaskRunInput};
use crate::executor::{
    with_default_bridge, BridgeInfo, BridgeMode, CreateBridgeResult, GuiLockInfo,
};
use crate::findings::storage as finding_storage;
use crate::mcp::shared::spawn_python_with_console;
use crate::mcp::shared::{emit_ai_output, get_workspace_paths_internal, FINDING_INSTRUCTIONS};
use crate::mcp::types::{api_error, ApiResponse, ApiState, GoToStateRequest, GoToStateResult};
use crate::orchestrator::{DeterministicVerifier, WorkerSignal};
use crate::safe_eprintln;
use crate::safe_lock::safe_lock_or_recover;
use crate::settings;
use crate::task_recorder::{TaskConfig, TaskRecorder};
use crate::timeout_config::Timeouts;
use regex::Regex;

/// Generate a stable ID from a file path using a hash.
pub fn generate_id_from_path(path: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    format!("file-{:x}", hasher.finish())
}

/// Extract a human-readable name from a file path.
pub fn path_to_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Unnamed Config")
        .to_string()
}

/// Status response
#[derive(Debug, Serialize)]
pub struct StatusResponse {
    /// Name of this runner instance (None for the primary/default runner)
    pub instance_name: Option<String>,
    /// The HTTP API port this runner is listening on
    pub api_port: u16,
    pub executor_running: bool,
    pub executor_state: String,
    pub config_loaded: bool,
    pub config_path: Option<String>,
    /// Whether an AI analysis is currently in progress
    pub ai_analysis_running: bool,
}

/// Tool version response for MCP caching
#[derive(Debug, Serialize)]
pub struct ToolVersionResponse {
    /// Version hash for cache invalidation (based on config + test count)
    pub version: String,
    /// Number of base tools available
    pub tool_count: usize,
    /// Number of tests that can be executed
    pub test_count: usize,
    /// Last update timestamp
    pub last_updated: String,
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
    /// Timeout in seconds for execution completion (None = disabled, no timeout)
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

/// Execute single action request
#[derive(Debug, Deserialize)]
pub struct ExecuteActionRequest {
    /// Action type: "click", "double_click", "right_click", "type", "hotkey", etc.
    pub action_type: String,
    /// Image ID from the loaded config (required for click actions)
    #[serde(default)]
    pub image_id: String,
    /// Optional monitor index
    #[serde(default)]
    pub monitor_index: Option<i32>,
    /// Timeout in seconds for action completion (None = disabled, no timeout)
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    /// Text to type (for "type" action)
    #[serde(default)]
    pub text_input: Option<String>,
    /// Hotkey combination (for "hotkey" action), e.g., "ctrl+c"
    #[serde(default)]
    pub hotkey: Option<String>,
}

/// Execute action result
#[derive(Debug, Serialize)]
pub struct ExecuteActionResult {
    pub success: bool,
    pub action_type: String,
    pub image_id: String,
    pub error: Option<String>,
}

/// Execute Python command request (generic command forwarding to Python executor)
///
/// This is used by the accessibility service and other features that need
/// to send commands directly to the Python executor via HTTP.
#[derive(Debug, Deserialize)]
pub struct ExecutePythonCommandRequest {
    /// Command type (e.g., "capture_accessibility", "auto_connect_accessibility")
    pub cmd_type: String,
    /// Command parameters as JSON
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Execute Python command response
#[derive(Debug, Serialize)]
pub struct ExecutePythonCommandResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Workflow execution result (for /workflow/run endpoint)
#[derive(Debug, Serialize)]
pub struct WorkflowExecutionResult {
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

// ============================================================================
// Bridge Management Endpoints
// ============================================================================

/// Request body for creating a new bridge
#[derive(Debug, Deserialize)]
pub struct CreateBridgeRequest {
    /// Operating mode: "gui" or "headless"
    #[serde(default)]
    mode: BridgeMode,
    /// Optional task run ID to associate with this bridge
    run_id: Option<String>,
    /// Monitor indices for GUI mode (default: [0])
    #[serde(default)]
    monitor_indices: Vec<i32>,
    /// Force acquire GUI lock even if held by another bridge
    #[serde(default)]
    force_gui_lock: bool,
}

/// Request body for running a workflow on a specific bridge
#[derive(Debug, Deserialize)]
pub struct BridgeWorkflowRequest {
    /// Workflow name to run
    workflow_name: Option<String>,
    /// Config path to load (optional if already loaded)
    config_path: Option<String>,
    /// Workflow parameters
    params: Option<serde_json::Value>,
}

/// List all active bridges
pub async fn list_bridges(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<BridgeInfo>>> {
    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        let bridges = bridge_manager.list_bridges().await;
        Json(ApiResponse::success(bridges))
    } else {
        Json(ApiResponse::<Vec<BridgeInfo>>::error(
            "Bridge manager not initialized",
        ))
    }
}

/// Create a new bridge
pub async fn create_bridge(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateBridgeRequest>,
) -> Json<ApiResponse<CreateBridgeResult>> {
    info!(
        "Creating new bridge: mode={:?}, run_id={:?}",
        request.mode, request.run_id
    );

    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        let monitor_indices = if request.monitor_indices.is_empty() {
            vec![0]
        } else {
            request.monitor_indices
        };

        match bridge_manager
            .create_bridge(
                request.mode,
                request.run_id,
                monitor_indices,
                request.force_gui_lock,
            )
            .await
        {
            Ok(result) => Json(ApiResponse::success(result)),
            Err(e) => Json(ApiResponse::<CreateBridgeResult>::error(&e)),
        }
    } else {
        Json(ApiResponse::<CreateBridgeResult>::error(
            "Bridge manager not initialized",
        ))
    }
}

/// Get info for a specific bridge
pub async fn get_bridge(
    State(state): State<Arc<ApiState>>,
    Path(bridge_id): Path<String>,
) -> Json<ApiResponse<Option<BridgeInfo>>> {
    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        let info = bridge_manager.get_bridge_info(&bridge_id).await;
        Json(ApiResponse::success(info))
    } else {
        Json(ApiResponse::<Option<BridgeInfo>>::error(
            "Bridge manager not initialized",
        ))
    }
}

/// Remove a bridge
pub async fn remove_bridge(
    State(state): State<Arc<ApiState>>,
    Path(bridge_id): Path<String>,
) -> Json<ApiResponse<()>> {
    info!("Removing bridge: {}", bridge_id);

    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        match bridge_manager.remove_bridge(&bridge_id).await {
            Ok(()) => Json(ApiResponse::success(())),
            Err(e) => Json(ApiResponse::<()>::error(&e)),
        }
    } else {
        Json(ApiResponse::<()>::error("Bridge manager not initialized"))
    }
}

/// Run a workflow on a specific bridge
pub async fn run_bridge_workflow(
    State(state): State<Arc<ApiState>>,
    Path(bridge_id): Path<String>,
    Json(request): Json<BridgeWorkflowRequest>,
) -> Json<ApiResponse<serde_json::Value>> {
    info!(
        "Running workflow on bridge {}: {:?}",
        bridge_id, request.workflow_name
    );

    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        // Load config if provided
        if let Some(config_path) = request.config_path {
            let load_result = bridge_manager
                .with_bridge(&bridge_id, |bridge| bridge.load_configuration(&config_path));

            if let Err(e) = load_result {
                return Json(ApiResponse::<serde_json::Value>::error(format!(
                    "Failed to access bridge: {}",
                    e
                )));
            }

            if let Ok(Err(e)) = load_result {
                return Json(ApiResponse::<serde_json::Value>::error(format!(
                    "Failed to load config: {}",
                    e
                )));
            }
        }

        // Build execution params
        let params = if request.workflow_name.is_some() || request.params.is_some() {
            Some(serde_json::json!({
                "workflow_name": request.workflow_name,
                "params": request.params,
            }))
        } else {
            None
        };

        // Start execution
        let start_result = bridge_manager.with_bridge(&bridge_id, |bridge| {
            bridge.start_execution_with_params(params)
        });

        match start_result {
            Ok(Ok(())) => Json(ApiResponse::success(serde_json::json!({
                "message": "Workflow started",
                "bridge_id": bridge_id,
            }))),
            Ok(Err(e)) => Json(ApiResponse::<serde_json::Value>::error(format!(
                "Failed to start workflow: {}",
                e
            ))),
            Err(e) => Json(ApiResponse::<serde_json::Value>::error(e)),
        }
    } else {
        Json(ApiResponse::<serde_json::Value>::error(
            "Bridge manager not initialized",
        ))
    }
}

/// Get current GUI lock holder
pub async fn get_gui_lock(State(state): State<Arc<ApiState>>) -> Json<ApiResponse<GuiLockInfo>> {
    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        let info = bridge_manager.get_gui_lock_info().await;
        Json(ApiResponse::success(info))
    } else {
        Json(ApiResponse::<GuiLockInfo>::error(
            "Bridge manager not initialized",
        ))
    }
}

// ============================================================================
// Headless-Only Mode Endpoints
// ============================================================================

/// Response for headless-only mode status
#[derive(Debug, Serialize)]
pub struct HeadlessOnlyResponse {
    /// Whether headless-only mode is enabled
    enabled: bool,
    /// Description of what this mode does
    description: String,
}

/// Request body for setting headless-only mode
#[derive(Debug, Deserialize)]
pub struct SetHeadlessOnlyRequest {
    /// Whether to enable headless-only mode
    enabled: bool,
}

/// Get headless-only mode status
///
/// When headless-only mode is enabled, GUI bridges cannot be created.
/// This is intended for server deployments without GUI access.
pub async fn get_headless_only(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<HeadlessOnlyResponse>> {
    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        let enabled = bridge_manager.is_headless_only();
        Json(ApiResponse::success(HeadlessOnlyResponse {
            enabled,
            description: if enabled {
                "Headless-only mode is ENABLED. All bridges must be headless. \
                GUI mode bridges cannot be created. This is intended for server deployments."
                    .to_string()
            } else {
                "Headless-only mode is DISABLED. Both GUI and headless bridges can be created."
                    .to_string()
            },
        }))
    } else {
        Json(ApiResponse::<HeadlessOnlyResponse>::error(
            "Bridge manager not initialized",
        ))
    }
}

/// Set headless-only mode
///
/// When enabled, all bridges must be created in headless mode.
/// GUI mode requests will be rejected with an error.
pub async fn set_headless_only(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<SetHeadlessOnlyRequest>,
) -> Json<ApiResponse<HeadlessOnlyResponse>> {
    info!("Setting headless-only mode to: {}", request.enabled);

    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        bridge_manager.set_headless_only(request.enabled);

        Json(ApiResponse::success(HeadlessOnlyResponse {
            enabled: request.enabled,
            description: if request.enabled {
                "Headless-only mode is now ENABLED. All bridges must be headless. \
                GUI mode bridges cannot be created."
                    .to_string()
            } else {
                "Headless-only mode is now DISABLED. Both GUI and headless bridges can be created."
                    .to_string()
            },
        }))
    } else {
        Json(ApiResponse::<HeadlessOnlyResponse>::error(
            "Bridge manager not initialized",
        ))
    }
}

// ============================================================================
// Debug Endpoints
// ============================================================================

/// A parsed error entry from log files
#[derive(Debug, Clone, Serialize)]
pub struct DebugError {
    /// Timestamp of the error
    timestamp: String,
    /// Service that generated the error (backend, frontend, api, runner)
    service: String,
    /// Log level (error, warning)
    level: String,
    /// Error message
    message: String,
    /// Optional stack trace or additional context
    context: Option<String>,
}

/// Summary of errors by category
#[derive(Debug, Clone, Serialize)]
pub struct DebugErrorSummary {
    /// Total errors found
    total: usize,
    /// Errors by service
    by_service: std::collections::HashMap<String, usize>,
    /// Errors by level
    by_level: std::collections::HashMap<String, usize>,
}

/// Response from /debug/app/errors endpoint
#[derive(Debug, Clone, Serialize)]
pub struct DebugErrorsResponse {
    /// Summary statistics
    summary: DebugErrorSummary,
    /// Individual errors (most recent first)
    errors: Vec<DebugError>,
}

/// Query parameters for /debug/app/errors
#[derive(Debug, Deserialize)]
pub struct DebugErrorsQuery {
    /// Maximum number of errors to return (default: 50)
    limit: Option<usize>,
    /// Filter by service (backend, frontend, api, runner)
    service: Option<String>,
    /// Filter by level (error, warning)
    level: Option<String>,
}

/// Get application errors from dev-logs
///
/// Parses log files from .dev-logs/ and returns structured error information.
pub async fn get_debug_errors(
    axum::extract::Query(query): axum::extract::Query<DebugErrorsQuery>,
) -> Json<ApiResponse<DebugErrorsResponse>> {
    use std::io::{BufRead, BufReader};

    let dev_logs_path = crate::paths::get_dev_logs_dir();

    if !dev_logs_path.exists() {
        return Json(ApiResponse::success(DebugErrorsResponse {
            summary: DebugErrorSummary {
                total: 0,
                by_service: std::collections::HashMap::new(),
                by_level: std::collections::HashMap::new(),
            },
            errors: vec![],
        }));
    }

    let limit = query.limit.unwrap_or(50);
    let mut all_errors: Vec<DebugError> = Vec::new();

    // Build log file list from global settings
    let global_settings = crate::settings::get_global_log_source_settings();
    let log_files: Vec<(String, String)> = if global_settings.sources.is_empty() {
        // Fallback if no sources configured
        vec![
            ("backend.log".to_string(), "backend".to_string()),
            ("frontend.log".to_string(), "frontend".to_string()),
            ("runner-tauri.log".to_string(), "runner".to_string()),
            (
                "runner-actions.jsonl".to_string(),
                "runner-actions".to_string(),
            ),
        ]
    } else {
        global_settings
            .sources
            .iter()
            .filter(|s| s.enabled)
            .map(|s| {
                let filename = s.path.clone();
                let service = s.name.to_lowercase().replace(' ', "-");
                (filename, service)
            })
            .collect()
    };

    // Regex patterns for error detection (compiled once)
    static RE_ERROR_1: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_ERROR_2: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_WARNING: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_TIMESTAMP: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_DATE_PREFIX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();

    let re_error_1 =
        RE_ERROR_1.get_or_init(|| Regex::new(r"(?i)(error|exception|traceback|failed)").unwrap());
    let re_error_2 =
        RE_ERROR_2.get_or_init(|| Regex::new(r"(?i)(ERROR|error:|\[error\])").unwrap());
    let re_warning = RE_WARNING.get_or_init(|| Regex::new(r"(?i)(warning|warn|\[warn\])").unwrap());
    let re_timestamp = RE_TIMESTAMP
        .get_or_init(|| Regex::new(r"(\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2})").unwrap());
    let re_date_prefix = RE_DATE_PREFIX.get_or_init(|| Regex::new(r"^\d{4}-\d{2}-\d{2}").unwrap());

    let error_patterns: &[(&Regex, &str)] = &[
        // Python/FastAPI errors
        (re_error_1, "error"),
        // TypeScript/Next.js errors
        (re_error_2, "error"),
        // Warnings
        (re_warning, "warning"),
    ];

    for (filename, service) in &log_files {
        // Apply service filter if specified
        if let Some(ref svc_filter) = query.service {
            if !service.eq_ignore_ascii_case(svc_filter) {
                continue;
            }
        }

        // If the path is absolute, use it directly; otherwise join with dev_logs_path
        let source_path = std::path::Path::new(filename);
        let file_path = if source_path.is_absolute() {
            source_path.to_path_buf()
        } else {
            dev_logs_path.join(filename)
        };
        if !file_path.exists() {
            continue;
        }

        if let Ok(file) = std::fs::File::open(&file_path) {
            let reader = BufReader::new(file);
            let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();

            // Process from end (most recent) to beginning
            let mut i = lines.len();
            while i > 0 {
                i -= 1;
                let line = &lines[i];

                // Determine log level
                let mut level = None;
                for (re, lvl) in error_patterns {
                    if re.is_match(line) {
                        level = Some(*lvl);
                        break;
                    }
                }

                if let Some(lvl) = level {
                    // Apply level filter if specified
                    if let Some(ref lvl_filter) = query.level {
                        if !lvl.eq_ignore_ascii_case(lvl_filter) {
                            continue;
                        }
                    }

                    // Extract timestamp if present (various formats)
                    let timestamp = re_timestamp
                        .captures(line)
                        .and_then(|c| c.get(1))
                        .map(|m| m.as_str().to_string())
                        .unwrap_or_default();

                    // Collect context (surrounding lines for stack traces)
                    let mut context_lines = Vec::new();
                    let mut j = i + 1;
                    while j < lines.len() && j < i + 10 {
                        let ctx_line = &lines[j];
                        // Stop at next log entry (has timestamp or is empty)
                        if ctx_line.is_empty() || re_date_prefix.is_match(ctx_line) {
                            break;
                        }
                        context_lines.push(ctx_line.clone());
                        j += 1;
                    }

                    let context = if context_lines.is_empty() {
                        None
                    } else {
                        Some(context_lines.join("\n"))
                    };

                    all_errors.push(DebugError {
                        timestamp,
                        service: service.to_string(),
                        level: lvl.to_string(),
                        message: line.clone(),
                        context,
                    });
                }
            }
        }
    }

    // Sort by timestamp (most recent first)
    all_errors.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    // Build summary before truncating
    let mut by_service: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut by_level: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for err in &all_errors {
        *by_service.entry(err.service.clone()).or_insert(0) += 1;
        *by_level.entry(err.level.clone()).or_insert(0) += 1;
    }

    let total = all_errors.len();

    // Truncate to limit
    all_errors.truncate(limit);

    Json(ApiResponse::success(DebugErrorsResponse {
        summary: DebugErrorSummary {
            total,
            by_service,
            by_level,
        },
        errors: all_errors,
    }))
}

/// Get findings summary from database
///
/// Returns a summary of issues detected in previous sessions.
/// If task_run_id query parameter is provided, returns findings only for that task run.
/// Otherwise returns findings from the most recent task runs.
pub async fn get_findings_summary(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<ApiResponse<serde_json::Value>> {
    // Get optional task_run_id filter
    let task_run_id_filter = params.get("task_run_id").cloned();

    // Get the database path using the same pattern as context.rs
    let app_data_dir = match dirs::config_dir() {
        Some(config_dir) => config_dir.join("com.qontinui.runner"),
        None => {
            return Json(ApiResponse::success(serde_json::json!({
                "total_findings": 0,
                "code_related_findings": 0,
                "by_severity": {},
                "findings": [],
                "error": "Could not find config directory"
            })));
        }
    };
    let db_path = app_data_dir.join("runner.db");

    let db = match rusqlite::Connection::open(&db_path) {
        Ok(conn) => conn,
        Err(e) => {
            return Json(ApiResponse::success(serde_json::json!({
                "total_findings": 0,
                "code_related_findings": 0,
                "by_severity": {},
                "findings": [],
                "error": format!("Failed to open database: {}", e)
            })));
        }
    };

    // Get findings based on filter
    let mut all_findings = Vec::new();
    if let Some(task_run_id) = task_run_id_filter {
        // Filter to specific task run
        if let Ok(findings) = finding_storage::get_findings_for_task(&db, &task_run_id) {
            all_findings.extend(findings);
        }
    } else {
        // Get recent task run IDs (fallback for when no specific run is requested)
        let task_run_ids: Vec<String> =
            match db.prepare("SELECT id FROM task_runs ORDER BY created_at DESC LIMIT 5") {
                Ok(mut stmt) => stmt
                    .query_map([], |row| row.get(0))
                    .ok()
                    .map(|rows| rows.filter_map(|r| r.ok()).collect())
                    .unwrap_or_default(),
                Err(_) => vec![],
            };

        for task_run_id in &task_run_ids {
            if let Ok(findings) = finding_storage::get_findings_for_task(&db, task_run_id) {
                all_findings.extend(findings);
            }
        }
    }

    let total = all_findings.len();

    // Count by severity
    let mut by_severity: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut code_related = 0;

    for finding in &all_findings {
        *by_severity
            .entry(finding.severity.as_str().to_string())
            .or_insert(0) += 1;
        if finding
            .code_context
            .as_ref()
            .and_then(|c| c.file.as_ref())
            .is_some()
        {
            code_related += 1;
        }
    }

    let response = serde_json::json!({
        "total_findings": total,
        "code_related_findings": code_related,
        "by_severity": by_severity,
        "findings": all_findings.iter().take(20).collect::<Vec<_>>()
    });

    Json(ApiResponse::success(response))
}

/// Launch Chrome with remote debugging enabled
pub async fn launch_debug_chrome() -> Json<ApiResponse<String>> {
    // Common Chrome paths on Windows
    let chrome_paths = [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    ];

    let chrome_path = chrome_paths
        .iter()
        .find(|p| std::path::Path::new(p).exists());

    match chrome_path {
        Some(path) => {
            // First, kill all existing Chrome processes
            // The debug port only works on the FIRST Chrome instance
            info!("Killing existing Chrome processes...");
            let _ = crate::process_helpers::no_window("taskkill")
                .args(["/F", "/IM", "chrome.exe"])
                .output();

            // Wait a moment for processes to terminate
            std::thread::sleep(std::time::Duration::from_millis(1000));

            // Now launch Chrome with debug flag and separate profile
            // Using a separate user-data-dir ensures the debug port works
            // even if Chrome would normally restore a previous session
            match crate::process_helpers::no_window(path)
                .args([
                    "--remote-debugging-port=9222",
                    "--user-data-dir=C:\\temp\\chrome-debug-profile",
                ])
                .spawn()
            {
                Ok(_) => {
                    info!("Launched Chrome with remote debugging on port 9222");
                    Json(ApiResponse::success(
                        "Chrome launched with debugging enabled".to_string(),
                    ))
                }
                Err(e) => {
                    error!("Failed to launch Chrome: {}", e);
                    Json(ApiResponse {
                        success: false,
                        data: None,
                        error: Some(format!("Failed to launch Chrome: {}", e)),
                    })
                }
            }
        }
        None => {
            error!("Chrome not found at expected paths");
            Json(ApiResponse {
                success: false,
                data: None,
                error: Some("Chrome not found. Please close Chrome and launch it manually with: chrome.exe --remote-debugging-port=9222".to_string()),
            })
        }
    }
}

// Monitor handler moved to crate::mcp::monitors

/// Get executor status
pub async fn get_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<StatusResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Clone Arc for use in spawn_blocking
    let app_state = state.app_state.clone();

    // Run blocking operations in a separate thread to avoid blocking the async runtime
    let result = tokio::task::spawn_blocking(move || {
        // Use with_default_bridge helper for bridge access
        let (executor_running, executor_state) = match with_default_bridge(&app_state, |bridge| {
            (bridge.is_running(), bridge.get_state().name().to_string())
        }) {
            Ok(result) => result,
            Err(_) => (false, "not_started".to_string()),
        };

        // Use unwrap_or_else to recover from poisoned mutex
        let config_lock = app_state.current_config.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: current_config mutex was poisoned, recovering");
            poisoned.into_inner()
        });
        let config_loaded = config_lock.is_some();
        drop(config_lock);

        let config_path = crate::settings::get_last_config_path();

        (executor_running, executor_state, config_loaded, config_path)
    })
    .await
    .map_err(|e| {
        error!("Failed to get status: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    // Check AI analysis status using async version to avoid blocking
    let ai_running = has_running_ai_tasks_async(state.app_state.checkpoint_db.clone()).await;

    let instance_name = std::env::var("QONTINUI_INSTANCE_NAME").ok();
    let api_port = state
        .app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);

    Ok(Json(ApiResponse::success(StatusResponse {
        instance_name,
        api_port,
        executor_running: result.0,
        executor_state: result.1,
        config_loaded: result.2,
        config_path: result.3,
        ai_analysis_running: ai_running,
    })))
}

/// Response for a single runner instance in the discovery endpoint.
#[derive(Debug, Serialize)]
pub struct DiscoveredInstance {
    /// Instance name (None for the primary/default runner)
    name: Option<String>,
    /// HTTP API port
    port: u16,
    /// Whether this is the runner handling the current request
    is_self: bool,
    /// Whether the instance's HTTP API is reachable
    reachable: bool,
}

/// Discover all runner instances (this runner + configured secondary instances).
///
/// Returns the current runner plus all configured instances with reachability status.
/// This allows callers to find a runner by name and resolve it to a port.
pub async fn get_instances(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<DiscoveredInstance>>> {
    let self_port = state
        .app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);
    let self_name = std::env::var("QONTINUI_INSTANCE_NAME").ok();

    let mut instances = vec![DiscoveredInstance {
        name: self_name,
        port: self_port,
        is_self: true,
        reachable: true,
    }];

    // Add configured secondary instances (skip any that match our own port)
    let configs = crate::settings::get_runner_instances();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1))
        .build()
        .ok();

    for config in &configs {
        if config.port == self_port || config.port < 1024 {
            continue;
        }
        let reachable = if let Some(ref client) = client {
            let url = format!("http://localhost:{}/health", config.port);
            client.get(&url).send().await.is_ok()
        } else {
            false
        };
        instances.push(DiscoveredInstance {
            name: Some(config.name.clone()),
            port: config.port,
            is_self: false,
            reachable,
        });
    }

    Json(ApiResponse::success(instances))
}

/// Get tool version for MCP caching
///
/// Returns a version hash based on:
/// - Current config ID (if loaded)
/// - Number of tests in the database
///
/// MCP clients can use this to invalidate their tool cache when
/// the available tools change (e.g., new tests added).
pub async fn get_tool_version(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<ToolVersionResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    use sha2::{Digest, Sha256};

    // Get current config ID
    let config_id = state
        .current_config_id
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_else(|| "none".to_string());

    // Get test count from database using list_verification_tests
    let db = state.app_state.checkpoint_db.clone();
    let test_count = tokio::task::spawn_blocking(move || {
        db.list_verification_tests(false, None, None)
            .map(|tests| tests.len())
            .unwrap_or(0)
    })
    .await
    .unwrap_or(0);

    // Base tool count (from qontinui-mcp server.py TOOLS list)
    // This should be kept in sync with the actual tool count
    const BASE_TOOL_COUNT: usize = 35;

    // Create version hash from config_id and test_count
    let version_input = format!("{}:{}", config_id, test_count);
    let mut hasher = Sha256::new();
    hasher.update(version_input.as_bytes());
    let hash = hasher.finalize();
    let version = format!("{:x}", hash)[..8].to_string();

    let last_updated = chrono::Utc::now().to_rfc3339();

    Ok(Json(ApiResponse::success(ToolVersionResponse {
        version,
        tool_count: BASE_TOOL_COUNT,
        test_count,
        last_updated,
    })))
}

/// Internal helper to load a configuration file synchronously.
/// Used by resume_active_workflow_on_startup to load config before resuming.
///
/// This performs the core config loading logic:
/// 1. Loads and validates the configuration file
/// 2. Stores it in the app state (current_config)
/// 3. Sends debug settings to the Python executor
/// 4. Sends the configuration to the Python executor
pub fn load_config_internal(
    app_state: &Arc<crate::AppState>,
    config_path: &str,
) -> Result<String, String> {
    // Step 1: Load and validate the configuration file
    let config = ConfigLoader::load_from_file(config_path).map_err(|e| {
        error!(
            "load_config_internal: Failed to load configuration from {}: {}",
            config_path, e
        );
        format!("Failed to load configuration: {}", e)
    })?;

    let summary = config.summary();
    info!("load_config_internal: Configuration validated: {}", summary);

    // Set project context for runtime environment
    crate::runtime_env::set_project_context(crate::runtime_env::ProjectContext {
        project_id: config.metadata.project_id.clone(),
        workspace_id: None, // workspace_id intentionally omitted — configs are not workspace-scoped (single-tenant desktop app)
        name: Some(config.metadata.name.clone()),
        triggered_by: None, // Set dynamically when task is triggered via API
    });

    // Step 2: Store the configuration in app state
    *app_state.current_config.lock().unwrap_or_else(|poisoned| {
        warn!("load_config_internal: current_config mutex was poisoned, recovering");
        poisoned.into_inner()
    }) = Some(config);
    info!("load_config_internal: Configuration stored in app state");

    // Step 3: Send debug settings and configuration to Python bridge
    let config_path_owned = config_path.to_string();
    let summary_clone = summary.clone();
    match with_default_bridge(app_state, |bridge| {
        if !bridge.is_running() {
            warn!("load_config_internal: Python executor not running, config stored but not sent to executor");
            return Ok(summary_clone.clone());
        }

        // Send debug settings first (before config execution)
        let debug_settings = settings::get_debug_settings();
        if let Err(e) = bridge.set_debug_settings(
            debug_settings.enable_image_debug,
            debug_settings.top_matches_count,
        ) {
            warn!("load_config_internal: Failed to send debug settings: {}", e);
        } else {
            info!(
                "load_config_internal: Debug settings sent: enable={}, top_matches={}",
                debug_settings.enable_image_debug, debug_settings.top_matches_count
            );
        }

        // Send configuration to Python
        bridge.load_configuration(&config_path_owned).map_err(|e| {
            error!(
                "load_config_internal: Failed to send configuration to Python: {}",
                e
            );
            format!("Failed to send configuration to Python: {}", e)
        })?;

        info!("load_config_internal: Configuration sent to Python executor");
        Ok(summary_clone.clone())
    }) {
        Ok(result) => result,
        Err(_) => {
            warn!(
                "load_config_internal: Python executor not initialized, config stored but not sent"
            );
            Ok(summary)
        }
    }
}

/// Load a configuration file
///
/// This mirrors the behavior from commands/config.rs:
/// 1. Loads and validates the configuration file
/// 2. Stores it in the app state (current_config)
/// 3. Saves the path for auto-load functionality
/// 4. Sends debug settings to the Python executor
/// 5. Sends the configuration to the Python executor
#[tracing::instrument(
    name = "api.request.load_config",
    skip(state, request),
    fields(
        endpoint = "/load-config",
        method = "POST",
        config_path = %request.config_path
    )
)]
pub async fn load_config(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<LoadConfigRequest>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Loading config: {}", request.config_path);

    let app_state = state.app_state.clone();
    let config_path = request.config_path.clone();
    let config_path_for_event = request.config_path.clone();

    let result = tokio::task::spawn_blocking(move || {
        // Step 1: Load and validate the configuration file
        let config = ConfigLoader::load_from_file(&config_path).map_err(|e| {
            error!(
                "MCP API: Failed to load configuration from {}: {}",
                config_path, e
            );
            format!("Failed to load configuration: {}", e)
        })?;

        let summary = config.summary();
        info!("MCP API: Configuration validated: {}", summary);

        // Create config data for event emission (including metadata for projectId)
        let config_data = serde_json::json!({
            "metadata": config.metadata.clone(),
            "workflows": config.workflows.clone(),
            "states": config.states.clone(),
            "transitions": config.transitions.clone(),
            "images": config.images.clone()
        });

        // Step 2: Store the configuration in app state
        *app_state.current_config.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: current_config mutex was poisoned, recovering");
            poisoned.into_inner()
        }) = Some(config);
        info!("MCP API: Configuration stored in app state");

        // Step 3: Save the path as the last loaded config
        if let Err(e) = settings::save_last_config_path(&config_path) {
            warn!("MCP API: Failed to save last config path: {}", e);
        }

        // Step 4 & 5: Send debug settings and configuration to Python bridge
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Send debug settings first (before config execution)
            let debug_settings = settings::get_debug_settings();
            if let Err(e) = bridge.set_debug_settings(
                debug_settings.enable_image_debug,
                debug_settings.top_matches_count,
            ) {
                warn!("MCP API: Failed to send debug settings: {}", e);
            } else {
                info!(
                    "MCP API: Debug settings sent: enable={}, top_matches={}",
                    debug_settings.enable_image_debug, debug_settings.top_matches_count
                );
            }

            // Send configuration to Python
            bridge.load_configuration(&config_path).map_err(|e| {
                error!("MCP API: Failed to send configuration to Python: {}", e);
                format!("Failed to send configuration to Python: {}", e)
            })?;

            info!("MCP API: Configuration sent to Python executor");
            Ok((summary.clone(), config_data.clone()))
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok((summary, config_data)) => {
            info!("MCP API: Config loaded successfully");

            // Debug: Log metadata being sent
            if let Some(metadata) = config_data.get("metadata") {
                info!("MCP API: Config metadata being emitted: {:?}", metadata);
            } else {
                warn!("MCP API: No metadata in config_data!");
            }

            // Auto-add to ConfigStorage (database)
            // Generate ID from project_id in config metadata, or from file path
            let config_id = config_data
                .get("metadata")
                .and_then(|m| m.get("projectId"))
                .and_then(|p| p.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| generate_id_from_path(&config_path_for_event));

            let config_name = config_data
                .get("metadata")
                .and_then(|m| m.get("name"))
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| path_to_name(&config_path_for_event));

            // Upsert: update if exists, insert if new
            if let Err(e) = state.app_state.checkpoint_db.save_config_with_id(
                &config_id,
                config_data.clone(),
                &config_name,
                "file",
                Some(&config_path_for_event),
            ) {
                warn!(
                    "MCP API: Failed to auto-store config in ConfigStorage: {}",
                    e
                );
            } else {
                info!(
                    "MCP API: Auto-stored config '{}' with id '{}' in ConfigStorage",
                    config_name, config_id
                );
                // Store current config ID
                if let Ok(mut current_id) = state.current_config_id.lock() {
                    *current_id = Some(config_id);
                }
            }

            // Emit event to notify frontend of config load
            let event_payload = serde_json::json!({
                "event": "config_loaded",
                "data": {
                    "path": config_path_for_event,
                    "config": config_data
                }
            });

            if let Err(e) = state.app_handle.emit("executor-event", &event_payload) {
                warn!("MCP API: Failed to emit config_loaded event: {}", e);
            } else {
                info!("MCP API: Emitted config_loaded event to frontend");
            }

            Ok(Json(ApiResponse::success(summary)))
        }
        Err(e) => {
            error!("MCP API: Failed to load config: {}", e);
            Err((StatusCode::BAD_REQUEST, Json(api_error(e))))
        }
    }
}

/// Run a workflow by name and wait for completion
///
/// Uses the UnifiedActionService for deterministic execution, ensuring both
/// manual API calls and AI task execution use the same code path.
///
/// Creates a TaskRun record (task_type='automation') to ensure all automation
/// runs are tracked in the unified TaskRun system.
#[tracing::instrument(
    name = "api.request.run_workflow",
    skip(state, request),
    fields(
        endpoint = "/run-workflow",
        method = "POST",
        workflow_name = %request.workflow_name,
        monitor_index = ?request.monitor_index,
        timeout_seconds = ?request.timeout_seconds
    )
)]
pub async fn run_workflow(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RunWorkflowRequest>,
) -> Result<Json<ApiResponse<WorkflowExecutionResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Running workflow: {} (timeout: {:?})",
        request.workflow_name, request.timeout_seconds
    );
    safe_eprintln!(
        "[MCP_API] run_workflow received: workflow={}, monitor_index={:?}",
        request.workflow_name,
        request.monitor_index
    );

    // Get current config_id for linking automation to config
    let config_id = state
        .current_config_id
        .lock()
        .ok()
        .and_then(|guard| guard.clone());

    // Create a TaskRun for this automation execution
    // This ensures ALL automation runs go through the unified TaskRun system
    let task_recorder = TaskRecorder::new(state.app_state.checkpoint_db.clone());
    let task_config = TaskConfig::automation_task(
        &format!("Workflow: {}", request.workflow_name),
        config_id.as_deref().unwrap_or("unknown"),
        Some(&request.workflow_name),
    );

    let task_handle = match task_recorder.start_task(task_config) {
        Ok(handle) => {
            info!(
                "MCP API: Created TaskRun {} for workflow {}",
                handle.id(),
                request.workflow_name
            );
            handle
        }
        Err(e) => {
            error!("MCP API: Failed to create TaskRun: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to create task run: {}", e))),
            ));
        }
    };

    // Link the run_recording_handler to this task run
    // This ensures automation metrics go to task_run_automation table
    // session_num=1 for the initial run
    state
        .app_state
        .run_recording_handler
        .set_task_run(task_handle.id().to_string(), 1, 1)
        .await;

    // Use UnifiedActionService for deterministic execution
    let action_service = state.action_service.clone();

    let result = action_service
        .run_workflow(
            &request.workflow_name,
            None, // No additional config
            request.monitor_index,
            request.timeout_seconds,
            None, // No initial state override from MCP API
        )
        .await;

    // Clear the task_run link after execution
    state.app_state.run_recording_handler.clear_task_run().await;

    match result {
        Ok(workflow_result) => {
            info!(
                "MCP API: Workflow completed via UnifiedActionService: success={}, error={:?}",
                workflow_result.success, workflow_result.error
            );

            // Update task run status based on workflow result
            if workflow_result.success {
                if let Err(e) = task_handle.complete() {
                    warn!("MCP API: Failed to complete task run: {}", e);
                }
            } else {
                let error_msg = workflow_result
                    .error
                    .as_deref()
                    .unwrap_or("Workflow failed");
                if let Err(e) = task_handle.fail(error_msg) {
                    warn!("MCP API: Failed to mark task run as failed: {}", e);
                }
            }

            Ok(Json(ApiResponse::success(WorkflowExecutionResult {
                success: workflow_result.success,
                workflow_name: workflow_result.workflow_name,
                error: workflow_result.error,
            })))
        }
        Err(e) => {
            error!("MCP API: Workflow execution failed: {}", e);

            // Mark task run as failed
            if let Err(fail_err) = task_handle.fail(&e.to_string()) {
                warn!("MCP API: Failed to mark task run as failed: {}", fail_err);
            }

            match e {
                crate::action_service::ActionError::Timeout(seconds) => Err((
                    StatusCode::REQUEST_TIMEOUT,
                    Json(api_error(format!(
                        "Workflow execution timed out after {} seconds",
                        seconds
                    ))),
                )),
                crate::action_service::ActionError::ExecutorNotRunning => Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error("Python executor not running")),
                )),
                crate::action_service::ActionError::ExecutorNotInitialized => Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error("Python executor not initialized")),
                )),
                _ => Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(e.to_string())),
                )),
            }
        }
    }
}

// ============================================================================
// Execute Steps Endpoint - Unified Step Execution
// ============================================================================

/// Request to execute a list of steps
#[derive(Debug, Deserialize)]
pub struct ExecuteStepsRequest {
    /// Steps to execute
    steps: Vec<crate::step_executor::ExecutionStepConfig>,
    /// Optional execution ID (generated if not provided)
    #[serde(default)]
    execution_id: Option<String>,
    /// Log sources to capture during execution
    #[serde(default)]
    log_sources: Vec<crate::step_executor::LogSourceConfig>,
    /// Optional task run ID for database logging (AWAS steps, etc.)
    #[serde(default)]
    task_run_id: Option<String>,
}

/// Execute a list of steps and return results
///
/// This is the unified execution endpoint used by:
/// - Run page (single workflow step)
/// - AI Builder (multi-step before AI session)
/// - MCP API (direct step execution)
///
/// Running a single workflow from the Run page is just:
/// `{ "steps": [{ "type": "workflow", "name": "MyWorkflow" }] }`
#[tracing::instrument(
    name = "api.request.execute_steps",
    skip(state, request),
    fields(
        endpoint = "/execute-steps",
        method = "POST",
        step_count = %request.steps.len(),
        execution_id = ?request.execution_id
    )
)]
pub async fn execute_steps(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ExecuteStepsRequest>,
) -> Result<
    Json<ApiResponse<crate::step_executor::ExecutionResult>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let execution_id = request.execution_id.unwrap_or_else(|| {
        format!(
            "exec-{}-{}",
            chrono::Utc::now().format("%Y%m%d%H%M%S"),
            uuid::Uuid::new_v4()
                .to_string()
                .split('-')
                .next()
                .unwrap_or("0000")
        )
    });

    info!(
        "MCP API: Executing {} steps (execution_id: {})",
        request.steps.len(),
        execution_id
    );

    // Create step executor with app handle for frontend event emission
    let mut executor = crate::step_executor::StepExecutor::with_app_handle(
        state.app_state.clone(),
        state.config_storage.clone(),
        state.app_handle.clone(),
    );

    // Add task_run_id if provided (enables AWAS step result logging to database)
    if let Some(task_run_id) = request.task_run_id {
        executor = executor.with_task_run_id(task_run_id);
    }

    // Execute all steps with log source capture
    let result = executor
        .execute_steps_with_log_sources(&request.steps, &execution_id, &request.log_sources)
        .await;

    info!(
        "MCP API: Execution complete - {} of {} steps succeeded",
        result.successful_steps, result.total_steps
    );

    Ok(Json(ApiResponse::success(result)))
}

/// Response for load-last-config endpoint
#[derive(Debug, Serialize)]
pub struct LoadLastConfigResponse {
    pub config_path: String,
    pub workflow_id: Option<String>,
    pub monitor_index: Option<i32>,
    pub summary: String,
}

/// Load the last used configuration, workflow, and monitor from settings
///
/// This is useful after a runner restart to restore the previous state.
/// It reads saved settings and loads the configuration just like load_config.
pub async fn load_last_config(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<LoadLastConfigResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Loading last configuration from settings");

    // First, get the saved settings
    let config_path = match settings::get_last_config_path() {
        Some(path) => path,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error("No last configuration found")),
            ))
        }
    };

    let workflow_id = settings::get_last_workflow_id();
    let monitor_index = settings::get_last_monitor_index();

    info!(
        "MCP API: Found last config: path={}, workflow={:?}, monitor={:?}",
        config_path, workflow_id, monitor_index
    );

    // Check if the file still exists
    if !std::path::Path::new(&config_path).exists() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!(
                "Last configuration file no longer exists: {}",
                config_path
            ))),
        ));
    }

    let app_state = state.app_state.clone();
    let config_path_clone = config_path.clone();
    let config_path_for_event = config_path.clone();

    let result = tokio::task::spawn_blocking(move || {
        // Load and validate the configuration file
        let config = ConfigLoader::load_from_file(&config_path_clone).map_err(|e| {
            error!(
                "MCP API: Failed to load configuration from {}: {}",
                config_path_clone, e
            );
            format!("Failed to load configuration: {}", e)
        })?;

        let summary = config.summary();
        info!("MCP API: Configuration validated: {}", summary);

        // Create config data for event emission (including metadata for projectId)
        let config_data = serde_json::json!({
            "metadata": config.metadata.clone(),
            "workflows": config.workflows.clone(),
            "states": config.states.clone(),
            "transitions": config.transitions.clone(),
            "images": config.images.clone()
        });

        // Store the configuration in app state
        *app_state.current_config.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: current_config mutex was poisoned, recovering");
            poisoned.into_inner()
        }) = Some(config);
        info!("MCP API: Configuration stored in app state");

        // Send debug settings and configuration to Python bridge
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Send debug settings first
            let debug_settings = settings::get_debug_settings();
            if let Err(e) = bridge.set_debug_settings(
                debug_settings.enable_image_debug,
                debug_settings.top_matches_count,
            ) {
                warn!("MCP API: Failed to send debug settings: {}", e);
            }

            // Send configuration to Python
            bridge.load_configuration(&config_path_clone).map_err(|e| {
                error!("MCP API: Failed to send configuration to Python: {}", e);
                format!("Failed to send configuration to Python: {}", e)
            })?;

            info!("MCP API: Configuration sent to Python executor");
            Ok((summary.clone(), config_data.clone()))
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok((summary, config_data)) => {
            info!("MCP API: Last config loaded successfully");

            // Emit event to notify frontend of config load
            let event_payload = serde_json::json!({
                "event": "config_loaded",
                "data": {
                    "path": config_path_for_event,
                    "config": config_data,
                    "workflow_id": workflow_id,
                    "monitor_index": monitor_index
                }
            });

            if let Err(e) = state.app_handle.emit("executor-event", &event_payload) {
                warn!("MCP API: Failed to emit config_loaded event: {}", e);
            } else {
                info!("MCP API: Emitted config_loaded event to frontend");
            }

            Ok(Json(ApiResponse::success(LoadLastConfigResponse {
                config_path,
                workflow_id,
                monitor_index,
                summary,
            })))
        }
        Err(e) => {
            error!("MCP API: Failed to load last config: {}", e);
            Err((StatusCode::BAD_REQUEST, Json(api_error(e))))
        }
    }
}

/// Stop the current execution
pub async fn stop_execution(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stopping execution");

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| match bridge.stop_execution() {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("Failed to stop execution: {}", e)),
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(_) => {
            info!("MCP API: Execution stopped");
            Ok(Json(ApiResponse::success("Execution stopped".to_string())))
        }
        Err(e) => {
            error!("MCP API: Failed to stop execution: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute a Python command via the executor bridge
///
/// This endpoint forwards commands to the Python executor and returns the result.
/// Used by the accessibility service and other features that need to communicate
/// with the Python executor via HTTP (e.g., for frontend components that can't
/// use Tauri IPC directly).
pub async fn execute_python_command(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ExecutePythonCommandRequest>,
) -> Result<Json<ApiResponse<ExecutePythonCommandResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Executing Python command: {} with params: {:?}",
        request.cmd_type, request.params
    );

    let app_state = state.app_state.clone();
    let cmd_type = request.cmd_type.clone();
    let cmd_type_for_log = cmd_type.clone(); // Clone for logging after closure
    let params = request.params.clone();

    // Use spawn_blocking for the synchronous bridge operation
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Convert params to Option<Value> (None if params is null or empty object)
            let params_option = if params.is_null()
                || (params.is_object() && params.as_object().is_none_or(|o| o.is_empty()))
            {
                None
            } else {
                Some(params)
            };

            // Use configurable timeout (default: disabled)
            // Falls back to 1 hour to prevent infinite IPC hangs
            let timeout_duration =
                Timeouts::python_command().unwrap_or_else(|| std::time::Duration::from_secs(3600));
            bridge.send_command_and_wait(&cmd_type, params_option, timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            info!(
                "MCP API: Python command {} completed, success={}",
                cmd_type_for_log, response.success
            );
            Ok(Json(ApiResponse::success(ExecutePythonCommandResponse {
                success: response.success,
                error: response.error,
                data: response.data,
            })))
        }
        Err(e) => {
            error!("MCP API: Python command {} failed: {}", cmd_type_for_log, e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute a single action (e.g., click on an image, type text, press hotkey)
///
/// This endpoint allows executing individual GUI actions without running a full workflow.
/// Uses the UnifiedActionService for deterministic execution, ensuring both
/// manual API calls and AI task execution use the same code path.
#[tracing::instrument(
    name = "api.request.execute_action",
    skip(state, request),
    fields(
        endpoint = "/execute-action",
        method = "POST",
        action_type = %request.action_type,
        image_id = %request.image_id,
        monitor_index = ?request.monitor_index
    )
)]
pub async fn execute_action(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ExecuteActionRequest>,
) -> Result<Json<ApiResponse<ExecuteActionResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Executing action: {} (image: {}, text: {:?}, hotkey: {:?}, timeout: {:?})",
        request.action_type,
        request.image_id,
        request.text_input,
        request.hotkey,
        request.timeout_seconds
    );

    // Use UnifiedActionService for deterministic execution
    let action_service = state.action_service.clone();
    let action_type = request.action_type.clone();
    let image_id = request.image_id.clone();

    // Build config for TYPE and HOTKEY actions
    let config = if let Some(ref text) = request.text_input {
        Some(serde_json::json!({ "text": text }))
    } else {
        request
            .hotkey
            .as_ref()
            .map(|hotkey| serde_json::json!({ "hotkey": hotkey }))
    };

    match action_service
        .execute_action(
            &request.action_type,
            &request.image_id,
            config.as_ref(),
            request.monitor_index,
        )
        .await
    {
        Ok(result) => {
            let action_result = ExecuteActionResult {
                success: result.success,
                action_type: action_type.clone(),
                image_id: image_id.clone(),
                error: if result.success { None } else { result.message },
            };

            if action_result.success {
                info!(
                    "MCP API: Action {} on image {} succeeded via UnifiedActionService",
                    action_result.action_type, action_result.image_id
                );
            } else {
                warn!(
                    "MCP API: Action {} on image {} failed: {:?}",
                    action_result.action_type, action_result.image_id, action_result.error
                );
            }
            Ok(Json(ApiResponse::success(action_result)))
        }
        Err(e) => {
            error!("MCP API: Failed to execute action: {}", e);
            match e {
                crate::action_service::ActionError::ExecutorNotRunning => Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error("Python executor not running")),
                )),
                crate::action_service::ActionError::ExecutorNotInitialized => Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error("Python executor not initialized")),
                )),
                _ => Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(e.to_string())),
                )),
            }
        }
    }
}

// ============================================================================
// State Navigation API Endpoint
// ============================================================================

/// Navigate to a target state using pathfinding
///
/// This endpoint uses the state machine to find and execute the path
/// from the current state to the target state.
#[tracing::instrument(
    name = "api.request.go_to_state",
    skip(state, request),
    fields(
        endpoint = "/go-to-state",
        method = "POST",
        state_id = %request.state_id,
        timeout_seconds = %request.timeout_seconds
    )
)]
pub async fn go_to_state(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<GoToStateRequest>,
) -> Result<Json<ApiResponse<GoToStateResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Navigating to state: {} (timeout: {}s)",
        request.state_id, request.timeout_seconds
    );

    // Use UnifiedActionService for deterministic execution
    let action_service = state.action_service.clone();
    let state_id = request.state_id.clone();

    match action_service
        .go_to_state(
            &request.state_id,
            None, // No additional config
            request.monitor_index,
            Some(request.timeout_seconds),
        )
        .await
    {
        Ok(result) => {
            let nav_result = GoToStateResult {
                success: result.success,
                state_id: state_id.clone(),
                error: result.error,
            };

            if nav_result.success {
                info!(
                    "MCP API: Successfully navigated to state {} via UnifiedActionService",
                    nav_result.state_id
                );
            } else {
                warn!(
                    "MCP API: Failed to navigate to state {}: {:?}",
                    nav_result.state_id, nav_result.error
                );
            }
            Ok(Json(ApiResponse::success(nav_result)))
        }
        Err(e) => {
            error!("MCP API: Failed to navigate to state: {}", e);
            match e {
                crate::action_service::ActionError::ExecutorNotRunning => Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error("Python executor not running")),
                )),
                crate::action_service::ActionError::ExecutorNotInitialized => Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error("Python executor not initialized")),
                )),
                _ => Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(e.to_string())),
                )),
            }
        }
    }
}

// ============================================================================
// Screenshot Capture API Endpoint
// ============================================================================

/// Capture a screenshot and save it to .dev-logs/screenshots/ with task-identifiable naming.
/// Also logs the screenshot to ai-output.jsonl for AI analysis.
///
/// This endpoint is used by both:
/// 1. Dedicated screenshot actions in the AI Automation Builder
/// 2. Post-step screenshots (takeScreenshot toggle on other step types)
#[tracing::instrument(
    name = "api.request.capture_screenshot",
    skip(state, request),
    fields(
        endpoint = "/capture-screenshot",
        method = "POST",
        monitor = ?request.monitor,
        delay_seconds = ?request.delay_seconds
    )
)]
pub async fn capture_screenshot_step(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CaptureScreenshotRequest>,
) -> Result<Json<ApiResponse<CaptureScreenshotResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    use base64::Engine;
    use std::fs;
    use std::io::Write;

    info!(
        "MCP API: Capturing screenshot (monitor: {:?}, delay: {:?}s, task: {:?}, step: {:?})",
        request.monitor, request.delay_seconds, request.task_id, request.step_index
    );

    // Apply delay if specified (clamped to 0-30 seconds)
    if let Some(delay) = request.delay_seconds {
        if delay > 0.0 {
            let clamped_delay = delay.clamp(0.0, 30.0);
            info!(
                "MCP API: Waiting {}s before screenshot capture",
                clamped_delay
            );
            tokio::time::sleep(tokio::time::Duration::from_secs_f64(clamped_delay)).await;
        }
    }

    // Capture screenshot via Python IPC
    let capture_response =
        match capture_screenshot_ipc(state.app_state.clone(), request.monitor, "png").await {
            Ok(response) => response,
            Err(e) => {
                error!("MCP API: Failed to capture screenshot via IPC: {}", e);
                return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
                    success: false,
                    screenshot_path: None,
                    absolute_path: None,
                    width: None,
                    height: None,
                    monitor: request.monitor,
                    error: Some(format!("IPC error: {}", e)),
                })));
            }
        };

    let screenshot_base64 = match capture_response
        .get("screenshot_base64")
        .and_then(|s| s.as_str())
    {
        Some(s) => s.to_string(),
        None => {
            error!("MCP API: No screenshot_base64 in IPC response");
            return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
                success: false,
                screenshot_path: None,
                absolute_path: None,
                width: None,
                height: None,
                monitor: request.monitor,
                error: Some("No screenshot data in response".to_string()),
            })));
        }
    };

    let width = capture_response
        .get("width")
        .and_then(|w| w.as_i64())
        .map(|w| w as i32);
    let height = capture_response
        .get("height")
        .and_then(|h| h.as_i64())
        .map(|h| h as i32);

    // Decode base64 to bytes
    let image_bytes = match base64::engine::general_purpose::STANDARD.decode(&screenshot_base64) {
        Ok(bytes) => bytes,
        Err(e) => {
            error!("MCP API: Failed to decode screenshot base64: {}", e);
            return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
                success: false,
                screenshot_path: None,
                absolute_path: None,
                width: None,
                height: None,
                monitor: request.monitor,
                error: Some(format!("Failed to decode screenshot: {}", e)),
            })));
        }
    };

    // Generate filename with task/run identification
    let timestamp = chrono::Utc::now().timestamp_millis();
    let task_id = request.task_id.as_deref().unwrap_or("manual");
    let step_part = request
        .step_index
        .map(|i| format!("step{:02}", i))
        .unwrap_or_else(|| "step00".to_string());
    let monitor_part = request
        .monitor
        .map(|m| format!("m{}", m))
        .unwrap_or_else(|| "all".to_string());
    let filename = format!(
        "screenshot-{}-{}-{}-{}.png",
        task_id, step_part, timestamp, monitor_part
    );

    // Save to .dev-logs/screenshots/
    let screenshots_dir = crate::paths::get_screenshots_dir();
    if let Err(e) = fs::create_dir_all(&screenshots_dir) {
        error!("MCP API: Failed to create screenshots directory: {}", e);
        return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
            success: false,
            screenshot_path: None,
            absolute_path: None,
            width: None,
            height: None,
            monitor: request.monitor,
            error: Some(format!("Failed to create directory: {}", e)),
        })));
    }

    let screenshot_path = screenshots_dir.join(&filename);
    let mut file = match fs::File::create(&screenshot_path) {
        Ok(f) => f,
        Err(e) => {
            error!("MCP API: Failed to create screenshot file: {}", e);
            return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
                success: false,
                screenshot_path: None,
                absolute_path: None,
                width: None,
                height: None,
                monitor: request.monitor,
                error: Some(format!("Failed to create file: {}", e)),
            })));
        }
    };

    if let Err(e) = file.write_all(&image_bytes) {
        error!("MCP API: Failed to write screenshot file: {}", e);
        return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
            success: false,
            screenshot_path: None,
            absolute_path: None,
            width: None,
            height: None,
            monitor: request.monitor,
            error: Some(format!("Failed to write file: {}", e)),
        })));
    }

    let relative_path = format!("screenshots/{}", filename);
    let absolute_path_str = screenshot_path.to_string_lossy().to_string();

    info!(
        "MCP API: Screenshot saved: {} ({}x{})",
        relative_path,
        width.unwrap_or(0),
        height.unwrap_or(0)
    );

    // Log to ai-output.jsonl
    let ai_output_entry = crate::commands::logging::AiOutputEntry {
        id: format!("ss-{}-{}", timestamp, rand::random::<u32>()),
        timestamp,
        line: format!(
            "[SCREENSHOT] {} ({}x{})",
            filename,
            width.unwrap_or(0),
            height.unwrap_or(0)
        ),
        source: "runner".to_string(),
        action_id: Some(format!("screenshot-{}", step_part)),
        task_run_id: None,
        session_id: None,
        session_name: None,
        phase: None,
        phase_iteration: None,
        screenshot_path: Some(relative_path.clone()),
        screenshot_width: width,
        screenshot_height: height,
    };

    // Append to ai-output.jsonl
    let _ = crate::commands::logging::append_ai_output_log(ai_output_entry);

    // Also emit to frontend for real-time display
    emit_ai_output(
        &state.app_handle,
        &format!(
            "[SCREENSHOT] Captured: {} ({}x{})",
            filename,
            width.unwrap_or(0),
            height.unwrap_or(0)
        ),
        "runner",
        Some(&format!("screenshot-{}", step_part)),
        None,
    );

    Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
        success: true,
        screenshot_path: Some(relative_path),
        absolute_path: Some(absolute_path_str),
        width,
        height,
        monitor: request.monitor,
        error: None,
    })))
}

// ============================================================================
// Screenshot List API Endpoint
// ============================================================================

/// List all screenshots from dev-logs directories.
/// Returns screenshots from:
/// - `.dev-logs/screenshots/` - Annotated screenshots from image recognition
/// - `.dev-logs/playwright-screenshots/` - Screenshots from Playwright test failures
pub async fn list_screenshots_endpoint() -> Json<crate::commands::screenshots::ScreenshotsResponse>
{
    info!("MCP API: Listing screenshots from dev-logs directories");
    Json(crate::commands::screenshots::list_screenshots().await)
}

// ============================================================================
// Action Log View API Endpoint
// ============================================================================

/// Get the action log view data.
/// Returns the same data as the Tauri command `get_action_log_view`.
/// This provides a single source of truth for both the Actions page and GUI Automation widget.
pub async fn get_action_log_view_endpoint(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Getting action log view");

    let processor = state.app_state.display_processor.lock().await;

    match processor.get_view("action_log") {
        Ok(view_data) => {
            info!("MCP API: Action log view retrieved successfully");
            Ok(Json(view_data))
        }
        Err(e) => {
            error!("MCP API: Failed to get action log view: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get action log view: {}", e))),
            ))
        }
    }
}

// ============================================================================
// Render Logging API Endpoints (for UI Testing)
// ============================================================================

/// Get all render log entries
/// Used by Python tests to verify component rendering
pub async fn get_render_log() -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiResponse<()>>)>
{
    info!("MCP API: Getting render log");

    let result = crate::commands::logging::load_render_log();

    if result.success {
        Ok(Json(serde_json::json!({
            "success": true,
            "data": result.data
        })))
    } else {
        Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(
                result
                    .message
                    .unwrap_or_else(|| "Unknown error".to_string()),
            )),
        ))
    }
}

/// Clear the render log file
pub async fn clear_render_log_handler(
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Clearing render log");

    let result = crate::commands::logging::clear_render_log();

    if result.success {
        Ok(Json(ApiResponse::success(())))
    } else {
        Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(
                result
                    .message
                    .unwrap_or_else(|| "Unknown error".to_string()),
            )),
        ))
    }
}

/// Get the path to the render log file
pub async fn get_render_log_path(
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Getting render log path");

    let result = crate::commands::logging::get_render_log_path_cmd();

    if result.success {
        Ok(Json(serde_json::json!({
            "success": true,
            "data": result.data
        })))
    } else {
        Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(
                result
                    .message
                    .unwrap_or_else(|| "Unknown error".to_string()),
            )),
        ))
    }
}

// ============================================================================
// Navigation API Endpoints (for UI Testing)
// ============================================================================

/// Request to navigate to a page
#[derive(Debug, Deserialize)]
pub struct NavigateRequest {
    /// Target page/tab ID (e.g., "run-recap", "run", "active", "library")
    pub page: String,
    /// Optional: task run ID when navigating to run-recap
    #[serde(default)]
    pub task_run_id: Option<i64>,
    /// Optional: select a specific run when navigating
    #[serde(default)]
    pub select_run: Option<i64>,
}

/// Navigate to a specific page in the runner UI.
/// Used by Python tests to trigger page renders for testing.
pub async fn navigate_to_page(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<NavigateRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Navigating to page: {} (task_run_id: {:?}, select_run: {:?})",
        request.page, request.task_run_id, request.select_run
    );

    // Emit navigation event to frontend
    let event_payload = serde_json::json!({
        "type": "navigate",
        "page": request.page,
        "task_run_id": request.task_run_id,
        "select_run": request.select_run,
    });

    if let Err(e) = state.app_handle.emit("test-navigation", &event_payload) {
        error!("MCP API: Failed to emit navigation event: {}", e);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to emit navigation event: {}", e))),
        ));
    }

    // Give the UI a moment to process the navigation
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    Ok(Json(ApiResponse::success(())))
}

// UI Bridge Control handlers removed — see crate::mcp module

// Extraction handler functions removed — see crate::mcp::extraction
// RAG handler functions removed — see crate::mcp::rag
// Model handler functions removed — see crate::mcp::models

pub fn default_true() -> bool {
    true
}

// ============================================================================
// IPC-Based Screenshot Capture
// ============================================================================

/// Capture a screenshot via Python IPC (physical pixel resolution)
pub async fn capture_screenshot_ipc(
    app_state: Arc<crate::AppState>,
    monitor: Option<i32>,
    format: &str,
) -> Result<serde_json::Value, String> {
    let params = serde_json::json!({
        "monitor": monitor,
        "format": format,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("capture_screenshot", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| format!("spawn_blocking error: {}", e))?;

    match result {
        Ok(response) => {
            if response.success {
                Ok(response
                    .data
                    .unwrap_or(serde_json::json!({"success": true})))
            } else {
                Err(response
                    .error
                    .unwrap_or_else(|| "Screenshot capture failed".to_string()))
            }
        }
        Err(e) => Err(e),
    }
}

/// Get monitors via Python IPC (physical pixel coordinates)
pub async fn get_monitors_ipc(
    app_state: Arc<crate::AppState>,
) -> Result<serde_json::Value, String> {
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("get_monitors", None, timeout_duration)
        })?
    })
    .await
    .map_err(|e| format!("spawn_blocking error: {}", e))?;

    match result {
        Ok(response) => {
            if response.success {
                Ok(response
                    .data
                    .unwrap_or(serde_json::json!({"success": true, "monitors": [], "count": 0})))
            } else {
                Err(response
                    .error
                    .unwrap_or_else(|| "Get monitors failed".to_string()))
            }
        }
        Err(e) => Err(e),
    }
}

/// HTTP endpoint to get monitors via IPC (physical pixel coordinates)
pub async fn get_screenshot_monitors_ipc(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Get monitors via IPC");

    match get_monitors_ipc(state.app_state.clone()).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("MCP API: Failed to get monitors via IPC: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// Integration Testing handlers removed — see crate::mcp module

// ============================================================================
// Playwright State Collector API
// ============================================================================

/// Request to start Playwright state collection
#[derive(Debug, Deserialize)]
pub struct StartPlaywrightCollectionRequest {
    /// Target URL to collect from
    pub url: String,
    /// Maximum navigation depth (default: 2)
    #[serde(default)]
    pub max_depth: Option<i32>,
    /// Maximum elements per page (default: 50)
    #[serde(default)]
    pub max_elements_per_page: Option<i32>,
    /// Risk level: "safe", "caution", or "dry_run" (default: "safe")
    #[serde(default)]
    pub max_risk_level: Option<String>,
    /// Skip clicking elements (default: false)
    #[serde(default)]
    pub dry_run: Option<bool>,
    /// Verify extractions with pattern matching (default: true)
    #[serde(default)]
    pub verify_extractions: Option<bool>,
    /// Verification similarity threshold (default: 0.85)
    #[serde(default)]
    pub verification_threshold: Option<f32>,
    /// Additional keywords to block
    #[serde(default)]
    pub additional_blocked_keywords: Option<Vec<String>>,
    /// Additional keywords to allow
    #[serde(default)]
    pub additional_safe_keywords: Option<Vec<String>>,
    /// CSS selectors to skip
    #[serde(default)]
    pub blocked_selectors: Option<Vec<String>>,
}

/// Response for Playwright collection status
#[derive(Debug, Serialize)]
pub struct PlaywrightCollectionStatusResponse {
    pub job_id: Option<String>,
    pub status: String,
    pub url: Option<String>,
    pub progress_message: Option<String>,
    pub progress_percent: Option<i32>,
    pub error: Option<String>,
    pub has_results: Option<bool>,
}

// UI Bridge Exploration handlers removed — see crate::mcp module

// Error Monitor handlers removed — see crate::mcp module

/// Start Playwright state collection
pub async fn start_playwright_collection(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartPlaywrightCollectionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Starting Playwright collection for URL: {}",
        request.url
    );

    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
        "url": request.url,
        "max_depth": request.max_depth.unwrap_or(2),
        "max_elements_per_page": request.max_elements_per_page.unwrap_or(50),
        "max_risk_level": request.max_risk_level.clone().unwrap_or_else(|| "safe".to_string()),
        "dry_run": request.dry_run.unwrap_or(false),
        "verify_extractions": request.verify_extractions.unwrap_or(true),
        "verification_threshold": request.verification_threshold.unwrap_or(0.85),
        "additional_blocked_keywords": request.additional_blocked_keywords.clone(),
        "additional_safe_keywords": request.additional_safe_keywords.clone(),
        "blocked_selectors": request.blocked_selectors.clone(),
    });

    let timeout = std::time::Duration::from_secs(30);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("start_playwright_collection", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: Playwright collection started");
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to start Playwright collection".to_string());
                error!(
                    "MCP API: Playwright collection failed to start: {}",
                    error_msg
                );
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": false,
                    "error": error_msg
                }))))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to start Playwright collection: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get Playwright collection status
pub async fn get_playwright_collection_status(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();
    let job_id = params.get("job_id").cloned();

    let cmd_params = serde_json::json!({
        "job_id": job_id,
    });

    let timeout = std::time::Duration::from_secs(10);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            bridge.send_command_and_wait(
                "get_playwright_collection_status",
                Some(cmd_params),
                timeout,
            )
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "status": "idle",
                        "job_id": null
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Unknown error".to_string());
                error!("MCP API: Playwright collection status error: {}", error_msg);
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "status": "error",
                    "error": error_msg
                }))))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to get Playwright collection status: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get Playwright collection results
pub async fn get_playwright_collection_results(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();
    let job_id = params.get("job_id").cloned();

    let cmd_params = serde_json::json!({
        "job_id": job_id,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            // Use longer timeout for getting results (may include large screenshots)
            let timeout = std::time::Duration::from_secs(60);
            bridge.send_command_and_wait(
                "get_playwright_collection_results",
                Some(cmd_params),
                timeout,
            )
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": false,
                        "error": "No results available"
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to get results".to_string());
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": false,
                    "error": error_msg
                }))))
            }
        }
        Err(e) => {
            error!(
                "MCP API: Failed to get Playwright collection results: {}",
                e
            );
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Stop Playwright collection
pub async fn stop_playwright_collection(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stopping Playwright collection");

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            bridge.send_command("stop_playwright_collection", None)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(_) => {
            info!("MCP API: Playwright collection stopped");
            Ok(Json(ApiResponse::success(
                "Playwright collection stopped".to_string(),
            )))
        }
        Err(e) => {
            error!("MCP API: Failed to stop Playwright collection: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Request to restart the runner (for AI self-healing workflow)
#[derive(Debug, Deserialize)]
pub struct RestartRunnerRequest {
    /// Reason for restart (logged for debugging)
    pub reason: String,
    /// Delay before restart in seconds (default: 3)
    #[serde(default)]
    pub delay_seconds: Option<u64>,
}

// Prompt CRUD types (CreatePromptRequest, UpdatePromptRequest, etc.) moved to crate::mcp::prompts

/// Request to run a prompt
#[derive(Debug, Deserialize)]
pub struct RunPromptRequest {
    // Mode 1: Lookup prompt from database
    /// Prompt ID to lookup from database (mutually exclusive with name+content)
    #[serde(default)]
    pub prompt_id: Option<String>,

    // Mode 2: Ad-hoc prompt (used by qontinui-web)
    /// Task name for display (required for ad-hoc mode)
    #[serde(default)]
    pub name: Option<String>,
    /// Prompt content (required for ad-hoc mode)
    #[serde(default)]
    pub content: Option<String>,

    // Common options
    /// Optional session_id override (auto-generated if not provided)
    #[serde(default)]
    pub session_id: Option<String>,
    /// Optional max_sessions override (uses prompt's setting if not provided)
    #[serde(default)]
    pub max_sessions: Option<u32>,

    // Image analysis options (for multimodal analysis)
    /// Image paths to include (screenshots, etc.) - for multimodal analysis
    #[serde(default)]
    pub image_paths: Option<Vec<String>>,
    /// Video paths to extract frames from
    #[serde(default)]
    pub video_paths: Option<Vec<String>>,
    /// Path to Playwright trace ZIP file (will extract timeline and screenshots)
    #[serde(default)]
    pub trace_path: Option<String>,
    /// Maximum number of frames to extract from each video (default: 3)
    #[serde(default)]
    pub max_video_frames: Option<usize>,
    /// Maximum number of screenshots to extract from trace (default: 5)
    #[serde(default)]
    pub max_trace_screenshots: Option<usize>,

    // Context injection options
    /// Context IDs to explicitly include in the prompt
    #[serde(default)]
    pub context_ids: Option<Vec<String>>,
    /// Whether to auto-detect and include relevant contexts (default: false)
    #[serde(default)]
    pub auto_include_contexts: Option<bool>,
}

/// Response from running a prompt
#[derive(Debug, Serialize)]
pub struct RunPromptResponse {
    pub task_run_id: String,
    pub session_id: String,
    /// Backward compatibility alias for task_run_id
    pub action_id: String,
    pub state_file: String,
    pub log_file: String,
    pub pid: Option<u32>,
}

// Macro CRUD types (CreateMacroRequest, UpdateMacroRequest) moved to crate::mcp::macros

// ============================================================================
// Workflow Request/Response Types
// ============================================================================

// AiOutputEvent, FindingContext, ProgressContext are now defined in crate::mcp::shared
// and re-exported at the top of this file

// Re-export AiSessionContext from the canonical location
pub use crate::execution_context::AiSessionContext;
use crate::runtime_env::{AiSessionContextExt, ExecutionContextExt};

// Playwright CRUD types moved to crate::mcp::playwright
// Prompt snippet CRUD types moved to crate::mcp::prompt_snippets

// ============================================================================
// Inline Python Execution Types
// ============================================================================

/// Request to execute inline Python code
#[derive(Debug, Deserialize)]
pub struct InlinePythonRequest {
    /// Python code to execute
    pub code: String,
    /// Optional pip packages to install (uses uvx for isolation)
    #[serde(default)]
    pub dependencies: Option<Vec<String>>,
    /// Execution timeout in seconds (default: 30)
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    /// Working directory for execution (default: temp dir)
    #[serde(default)]
    pub working_directory: Option<String>,
}

/// Response from inline Python execution
#[derive(Debug, Serialize)]
pub struct InlinePythonResponse {
    /// Whether execution succeeded (exit code 0)
    pub success: bool,
    /// Stdout from the script
    pub stdout: String,
    /// Stderr from the script
    pub stderr: String,
    /// Return value if the script returned JSON via __QONTINUI_RETURN__ marker
    pub return_value: Option<serde_json::Value>,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
}

// emit_ai_output and write_ai_debug_log are now in crate::mcp::shared
// and re-exported at the top of this file

/// Stop the currently running AI analysis
///
/// This endpoint stops all running tasks by:
/// 1. Killing all tracked AI process PIDs (the actual Claude CLI processes)
/// 2. Getting running task runs from the database
/// 3. Stopping monitoring for each task
/// 4. Marking tasks as stopped in the database
pub async fn stop_ai_analysis(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stop AI analysis requested");

    // First, kill all tracked AI processes immediately
    // This is the key fix - previously we only stopped monitoring, not the actual processes
    let pids_to_kill: Vec<u32> = {
        let mut pids = safe_lock_or_recover(&state.current_ai_pids, "current_ai_pids");
        let pids_copy = pids.clone();
        pids.clear(); // Clear the tracker
        pids_copy
    };

    let mut killed_count = 0;
    for pid in &pids_to_kill {
        info!("MCP API: Killing AI process PID {}", pid);
        // Use taskkill with /T to kill the entire process tree (cmd.exe spawns node.exe for claude)
        // /F forces termination, /T terminates child processes
        let result = crate::process_helpers::no_window("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output();

        match result {
            Ok(output) => {
                if output.status.success() {
                    info!("MCP API: Successfully killed process tree for PID {}", pid);
                    killed_count += 1;
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    warn!(
                        "MCP API: taskkill for PID {} returned error: {}",
                        pid, stderr
                    );
                    // Process may have already exited, which is fine
                    killed_count += 1;
                }
            }
            Err(e) => {
                error!("MCP API: Failed to execute taskkill for PID {}: {}", pid, e);
            }
        }
    }

    if !pids_to_kill.is_empty() {
        emit_ai_output(
            &state.app_handle,
            &format!("⛔ Killed {} AI process(es)", killed_count),
            "status",
            None,
            None,
        );
    }

    // Close all interactive Claude sessions via SessionManager
    if let Some(session_manager) = state
        .app_handle
        .try_state::<Arc<crate::claude_session::SessionManager>>()
    {
        session_manager.close_all_sessions();
    }

    // Get running tasks from the database
    let db = match CheckpointDb::new() {
        Ok(db) => db,
        Err(e) => {
            error!("MCP API: Failed to open database: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to open database: {}", e))),
            ));
        }
    };

    let running_tasks = match db.get_running_task_runs(None) {
        Ok(tasks) => tasks,
        Err(e) => {
            error!("MCP API: Failed to get running tasks: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get running tasks: {}", e))),
            ));
        }
    };

    if running_tasks.is_empty() && pids_to_kill.is_empty() {
        info!("MCP API: No running tasks to stop");
        return Ok(Json(ApiResponse::success(())));
    }

    // Stop each running task
    for task in &running_tasks {
        // Mark as stopped in database
        if let Err(e) = db.stop_task_run(&task.id) {
            warn!("MCP API: Failed to stop task run {}: {}", task.id, e);
        }

        info!("MCP API: Stopped task run: {}", task.id);
    }

    // Emit status to frontend
    emit_ai_output(
        &state.app_handle,
        &format!(
            "Stopped {} running task(s), killed {} process(es)",
            running_tasks.len(),
            killed_count
        ),
        "status",
        None,
        None,
    );

    info!(
        "MCP API: Stopped {} AI analysis task(s)",
        running_tasks.len()
    );
    Ok(Json(ApiResponse::success(())))
}

/// Restart the runner (for AI self-healing workflow)
///
/// This endpoint allows the AI to trigger a runner restart after applying fixes.
/// The restart is delayed to allow the response to be sent first.
pub async fn restart_runner(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RestartRunnerRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let delay_secs = request.delay_seconds.unwrap_or(3);

    info!(
        "MCP API: Runner restart requested - reason: {}, delay: {}s",
        request.reason, delay_secs
    );

    // Emit status to frontend so user knows what's happening
    emit_ai_output(
        &state.app_handle,
        &format!(
            "🔄 Restarting runner in {} seconds: {}",
            delay_secs, request.reason
        ),
        "status",
        None, // No action_id for restart status
        None, // No session context for restart status
    );

    // Spawn a task to exit after delay
    // The Tauri dev server will automatically restart the app
    let delay = std::time::Duration::from_secs(delay_secs);
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        info!("MCP API: Exiting for restart...");
        std::process::exit(0);
    });

    Ok(Json(ApiResponse::success(())))
}

// ============================================================================
// AI Developer (Persistent Mode) HTTP Endpoints
// ============================================================================

/// Check if any AI analysis tasks are currently running (sync version).
/// Uses the provided database to check for running task runs.
/// NOTE: This is a synchronous function that blocks. For async contexts,
/// use has_running_ai_tasks_async() or wrap this in spawn_blocking.
#[allow(dead_code)]
pub fn has_running_ai_tasks(db: &Arc<CheckpointDb>) -> bool {
    match db.get_running_task_runs(None) {
        Ok(tasks) => !tasks.is_empty(),
        Err(e) => {
            warn!("Failed to check running tasks: {}", e);
            false
        }
    }
}

/// Check if any AI analysis tasks are currently running (async version).
/// Uses spawn_blocking to avoid blocking the async runtime.
pub async fn has_running_ai_tasks_async(db: Arc<CheckpointDb>) -> bool {
    match tokio::task::spawn_blocking(move || db.get_running_task_runs(None)).await {
        Ok(Ok(tasks)) => !tasks.is_empty(),
        Ok(Err(e)) => {
            warn!("Failed to check running tasks: {}", e);
            false
        }
        Err(e) => {
            warn!("spawn_blocking error checking running tasks: {}", e);
            false
        }
    }
}

/// Migrate JSONL logs to SQLite for a completed task run.
/// This should be called after a task completes (success or failure) to persist logs.
pub async fn migrate_logs_for_task(
    db: Arc<CheckpointDb>,
    task_id: &str,
    workflow_name: Option<String>,
) {
    let task_id_owned = task_id.to_string();

    // Get the dev-logs directory path
    let dev_logs_dir = match std::env::current_exe() {
        Ok(exe_path) => {
            // Navigate up to find the parent directory containing .dev-logs
            let mut current = exe_path.as_path();
            loop {
                if let Some(parent) = current.parent() {
                    let dev_logs = parent.join(".dev-logs");
                    if dev_logs.exists() {
                        break dev_logs;
                    }
                    // Also check parent's parent (for qontinui_parent_directory)
                    if let Some(grandparent) = parent.parent() {
                        let dev_logs = grandparent.join(".dev-logs");
                        if dev_logs.exists() {
                            break dev_logs;
                        }
                    }
                    current = parent;
                } else {
                    // Fallback to a reasonable default
                    warn!("Could not find .dev-logs directory, skipping log migration");
                    return;
                }
            }
        }
        Err(e) => {
            warn!("Failed to get executable path for log migration: {}", e);
            return;
        }
    };

    info!(
        "Migrating JSONL logs to SQLite for task run: {}",
        task_id_owned
    );

    let result = tokio::task::spawn_blocking(move || {
        crate::log_migration::migrate_logs_to_sqlite(
            &db,
            &task_id_owned,
            &dev_logs_dir,
            workflow_name.as_deref(),
        )
    })
    .await;

    match result {
        Ok(Ok(migration_result)) => {
            info!(
                "Log migration complete for task {}: {} general, {} actions, {} image recognition, {} screenshots, {} playwright",
                task_id,
                migration_result.general_events,
                migration_result.action_events,
                migration_result.image_recognition_events,
                migration_result.screenshots,
                migration_result.playwright_results
            );
            if !migration_result.errors.is_empty() {
                warn!(
                    "Log migration had {} errors: {:?}",
                    migration_result.errors.len(),
                    migration_result.errors
                );
            }
        }
        Ok(Err(e)) => {
            warn!("Failed to migrate logs for task {}: {}", task_id, e);
        }
        Err(e) => {
            warn!(
                "spawn_blocking error during log migration for task {}: {}",
                task_id, e
            );
        }
    }
}

/// Helper function to mark a task run as complete with retry logic.
/// Retries up to 3 times with exponential backoff (100ms, 200ms, 400ms).
/// Returns true if successfully marked complete, false otherwise.
/// Also triggers log migration to persist JSONL logs to SQLite.
///
/// Uses gated function - unified workflows have status managed by LoopController only.
pub async fn complete_task_run_with_retry(db: Arc<CheckpointDb>, task_id: &str) -> bool {
    let task_id_owned = task_id.to_string();
    let max_retries = 3;

    // Get workflow name before completion for log migration context
    let workflow_name = db
        .get_task_run(&task_id_owned)
        .ok()
        .flatten()
        .and_then(|t| t.workflow_name);

    for retry in 0..max_retries {
        let db_clone = db.clone();
        let id = task_id_owned.clone();

        // Use gated function - unified workflows have status managed by LoopController
        match tokio::task::spawn_blocking(move || {
            db_clone.complete_task_run_if_allowed(&id, "complete_task_run_with_retry")
        })
        .await
        {
            Ok(Ok(true)) => {
                // Successfully marked complete
                if retry > 0 {
                    info!(
                        "Task run {} marked complete after {} retries",
                        task_id_owned, retry
                    );
                }

                // Migrate logs to SQLite after successful completion
                migrate_logs_for_task(db.clone(), &task_id_owned, workflow_name).await;

                return true;
            }
            Ok(Ok(false)) => {
                // Unified workflow - status managed by LoopController, not an error
                return true;
            }
            Ok(Err(e)) => {
                if retry < max_retries - 1 {
                    let delay_ms = 100 * (1 << retry); // 100, 200, 400ms
                    warn!(
                        "Retry {}/{} marking task_run {} complete (waiting {}ms): {}",
                        retry + 1,
                        max_retries,
                        task_id_owned,
                        delay_ms,
                        e
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                } else {
                    error!(
                        "Failed to mark task_run {} as complete after {} retries: {}",
                        task_id_owned, max_retries, e
                    );
                }
            }
            Err(e) => {
                error!(
                    "spawn_blocking error marking task_run {} complete: {}",
                    task_id_owned, e
                );
                return false;
            }
        }
    }

    false
}

// get_workspace_paths_internal is now in crate::mcp::shared
// and re-exported at the top of this file

/// Generate MCP tool context documentation for AI sessions.
///
/// This function creates a markdown documentation string describing the available
/// MCP tools for GUI automation, including the specific workflows, states, and
/// images available in the loaded configuration.
pub fn generate_mcp_tool_context(config: &crate::config::QontinuiConfig) -> String {
    let mut context = String::from(
        r#"
## Available GUI Automation Tools

The following MCP tools are available for deterministic GUI automation.
All actions execute through the unified action service with the pre-loaded config.

### Tools

"#,
    );

    // Tool: run_workflow
    let workflows: Vec<String> = config
        .workflows
        .iter()
        .filter_map(|w| w.get("name").and_then(|n| n.as_str()))
        .map(|n| format!("- {}", n))
        .collect();

    context.push_str(&format!(
        r#"
#### run_workflow
Run a workflow by name from the loaded configuration.

**Available Workflows:**
{}

**Usage:**
```json
{{"tool": "mcp__qontinui__run_workflow", "workflow_name": "WorkflowName", "monitor": "primary"}}
```
"#,
        if workflows.is_empty() {
            "- (none loaded)".to_string()
        } else {
            workflows.join("\n")
        }
    ));

    // Tool: go_to_state
    let states: Vec<String> = config
        .states
        .iter()
        .filter_map(|s| s.get("name").and_then(|n| n.as_str()))
        .map(|n| format!("- {}", n))
        .collect();

    context.push_str(&format!(
        r#"
#### go_to_state
Navigate to a specific state using pathfinding.

**Available States:**
{}

**Usage:**
```json
{{"tool": "mcp__qontinui__go_to_state", "state_id": "StateName"}}
```
"#,
        if states.is_empty() {
            "- (none loaded)".to_string()
        } else {
            states.join("\n")
        }
    ));

    // Tool: execute_action
    let images: Vec<String> = config
        .images
        .iter()
        .take(20) // Limit to avoid context overflow
        .filter_map(|i| i.get("id").and_then(|id| id.as_str()))
        .map(|id| format!("- {}", id))
        .collect();

    context.push_str(&format!(
        r#"
#### execute_action
Execute a single action (click, type, etc.) on a target image.

**Available Images (first 20):**
{}

**Action Types:** click, double_click, right_click, type

**Usage:**
```json
{{"tool": "mcp__qontinui__execute_action", "action_type": "click", "image_id": "image-123"}}
```
"#,
        if images.is_empty() {
            "- (none loaded)".to_string()
        } else {
            images.join("\n")
        }
    ));

    // Tool: capture_screenshot
    context.push_str(
        r#"
#### capture_screenshot
Capture a screenshot from a specified monitor.

**Usage:**
```json
{"tool": "mcp__qontinui__capture_screenshot", "monitor": 0, "delay_seconds": 1.0}
```
"#,
    );

    // SDK Tools - for interacting with UI Bridge SDK-integrated apps
    context.push_str(
        r#"
## Available SDK Tools (UI Bridge)

The following tools interact with SDK-integrated web apps via the runner's HTTP API.
Use these to inspect, interact with, and test web applications that have the UI Bridge SDK installed.

**Content Discovery:** These tools discover both **interactive elements** (buttons, inputs, links)
and **content elements** (headings, paragraphs, labels, metrics, badges, status indicators).
Content elements have a `contentType` field (e.g., `heading`, `paragraph`, `label`, `metric-value`,
`badge`, `status-message`, `description-text`, `list-item`, `table-cell`, `code-block`, `nav-text`)
and may have a `contentRole` from `data-content-role` attributes (e.g., `heading`, `body-text`,
`label`, `metric`, `badge`, `status`, `description`).

**Content filtering** (supported by element/snapshot tools):
- `includeContent` (bool) — include content elements (default: true)
- `contentOnly` (bool) — return only content elements, excluding interactive ones
- `contentRole` (string) — filter to a specific content role

This lets you read page text, find specific metrics/labels/statuses, and verify content changes without screenshots.

### Connection

#### sdk_connect
Connect to a UI Bridge SDK app for element inspection and interaction.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_connect", "url": "http://localhost:3001"}
```

#### sdk_status
Check SDK app connection status. Returns whether connected and app details.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_status"}
```

### Element Inspection

#### sdk_elements
List all registered UI elements (interactive and content) in the connected SDK app.
Returns element IDs, types, labels, state, and contentType/contentRole for content elements.
Accepts optional `includeContent`, `contentOnly`, and `contentRole` filters.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_elements"}
{"tool": "mcp__qontinui__sdk_elements", "contentOnly": true, "contentRole": "metric"}
```

#### sdk_snapshot
Get a complete UI snapshot with all elements (interactive + content) and their current state.
Includes visibility, bounds, text content, available actions, and contentType/contentRole for content elements.
Accepts optional `includeContent`, `contentOnly`, and `contentRole` filters.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_snapshot"}
{"tool": "mcp__qontinui__sdk_snapshot", "contentOnly": true}
```

### AI-Powered Interaction

#### sdk_ai_search
Search for elements (interactive or content) by natural language description.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_ai_search", "text": "Submit button"}
{"tool": "mcp__qontinui__sdk_ai_search", "text": "total revenue metric"}
```

#### sdk_ai_execute
Execute an action by natural language instruction.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_ai_execute", "instruction": "click the Submit button"}
```

#### sdk_ai_assert
Assert element state using natural language.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_ai_assert", "text": "error message", "state": "hidden"}
```

#### sdk_page_summary
Get an AI-friendly summary of the current page, including layout, navigation, and key elements.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_page_summary"}
```

### Screenshots

#### sdk_screenshot
Capture a screenshot of the monitor where the SDK app is running.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_screenshot"}
```

### Per-App Analysis

These tools analyze the currently connected SDK app's page structure and data.
They work on a single app — use them independently or as building blocks.

#### sdk_analyze_data
Extract labeled data values from the page. Each value is classified by type
(text, number, currency, date, email, url, phone, percentage, boolean) and
normalized for comparison.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_analyze_data"}
```

#### sdk_analyze_regions
Segment the page into semantic regions: header, navigation, sidebar,
main-content, footer, form, table, card, modal, toolbar. Each region
includes its bounding box and contained element IDs.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_analyze_regions"}
```

#### sdk_analyze_structured_data
Detect and extract tables (with column headers and row data) and lists
(with field schemas and items) from the page based on spatial layout patterns.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_analyze_structured_data"}
```

### Cross-App Comparison

#### sdk_cross_app_compare
Compare two SDK-integrated apps by connecting to each, capturing semantic
snapshots (including content elements), and running a full analysis. Returns
scores (0-1) for data completeness, format alignment, presentation alignment,
navigation parity, action parity, and an overall score. Also returns a
prioritized issue list. Content elements enable text-level comparison across apps.

Set `include_components` to true to also fetch and compare registered
components between the two apps.

**Usage:**
```json
{"tool": "mcp__qontinui__sdk_cross_app_compare", "source_url": "http://localhost:1420", "target_url": "http://localhost:3001", "include_components": true}
```
"#,
    );

    context
}

// Prompt CRUD handlers (list, get, create, update, delete, categories, tags,
// import, export, duplicate, search) moved to crate::mcp::prompts

use crate::backup;
use crate::prompts;

/// Run a prompt by spawning a Claude session
///
/// Supports two modes:
/// 1. Lookup prompt from database: provide `prompt_id`
/// 2. Ad-hoc prompt: provide `name` and `content`
///
/// Optional image analysis: provide `image_paths`, `video_paths`, or `trace_path`
/// to enhance the prompt with visual analysis data.
pub async fn run_prompt(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RunPromptRequest>,
) -> Result<Json<ApiResponse<RunPromptResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Determine mode and get prompt name + content + orchestrator config
    // Orchestrator config is extracted from saved prompts (system-level setting, not user-controllable)
    let (
        prompt_name,
        prompt_content,
        prompt_id,
        prompt_max_sessions,
        requires_orchestrator,
        _orchestrator_goal,
        _orchestrator_max_iterations,
        _orchestrator_verification_first,
    ) = if let Some(ref id) = request.prompt_id {
        // Mode 1: Lookup from database
        let prompt = prompts::get_prompt(id).ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Prompt not found: {}", id))),
            )
        })?;
        (
            prompt.name.clone(),
            prompt.content.clone(),
            Some(prompt.id.clone()),
            prompt.max_sessions,
            prompt.requires_orchestrator,
            prompt.orchestrator_goal.clone(),
            prompt.orchestrator_max_iterations,
            prompt.orchestrator_verification_first,
        )
    } else if let (Some(name), Some(content)) = (&request.name, &request.content) {
        // Mode 2: Ad-hoc prompt (no orchestrator by default)
        (
            name.clone(),
            content.clone(),
            None,
            None,
            false,
            None,
            None,
            None,
        )
    } else {
        // Invalid: neither mode satisfied
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "Must provide either prompt_id OR (name AND content)",
            )),
        ));
    };

    // Generate session_id if not provided
    let session_id = request.session_id.unwrap_or_else(|| {
        format!(
            "{}-{}",
            chrono::Utc::now().format("%Y%m%d-%H%M%S"),
            rand::random::<u16>()
        )
    });

    // Use override or prompt's setting (None = unlimited sessions)
    let max_sessions = request.max_sessions.or(prompt_max_sessions);

    // Use session_id as task_run_id (they are the same)
    let task_run_id = session_id.clone();

    // Auto-load last config if not already loaded and auto_load_last_config is enabled
    // This ensures GUI automation tasks have access to workflows
    let config_was_loaded = {
        let config_lock = safe_lock_or_recover(&state.app_state.current_config, "current_config");
        config_lock.is_some()
    };

    let mut config_info: Option<(String, Option<String>, Option<i32>)> = None;
    if !config_was_loaded && settings::get_auto_load_last_config() {
        if let Some(config_path) = settings::get_last_config_path() {
            if std::path::Path::new(&config_path).exists() {
                info!(
                    "MCP API: Auto-loading last config for prompt execution: {}",
                    config_path
                );

                // Load the config
                match crate::config::ConfigLoader::load_from_file(&config_path) {
                    Ok(config) => {
                        // Store the config
                        let mut config_lock =
                            safe_lock_or_recover(&state.app_state.current_config, "current_config");
                        *config_lock = Some(config);

                        let workflow_id = settings::get_last_workflow_id();
                        let monitor_index = settings::get_last_monitor_index();
                        config_info = Some((config_path.clone(), workflow_id, monitor_index));

                        info!(
                            "MCP API: Auto-loaded config: {:?}, workflow: {:?}, monitor: {:?}",
                            config_path,
                            config_info.as_ref().map(|c| &c.1),
                            config_info.as_ref().map(|c| &c.2)
                        );
                    }
                    Err(e) => {
                        warn!("MCP API: Failed to auto-load config: {}", e);
                    }
                }
            }
        }
    }

    // Collect images for analysis if provided
    let image_paths = request.image_paths.unwrap_or_default();
    let video_paths = request.video_paths.unwrap_or_default();
    let max_video_frames = request.max_video_frames.unwrap_or(3) as u32;
    let max_trace_screenshots = request.max_trace_screenshots.unwrap_or(5) as u32;

    let (all_images, trace_timeline) = collect_images_for_analysis(
        &image_paths,
        &video_paths,
        request.trace_path.as_deref(),
        max_video_frames,
        max_trace_screenshots,
    );

    // Build enhanced prompt with trace timeline and image references if available
    let mut enhanced_prompt = prompt_content.clone();

    // Inject contexts into the prompt if requested
    let context_ids = request.context_ids.unwrap_or_default();
    let auto_include_contexts = request.auto_include_contexts.unwrap_or(false);

    // Extract action types from loaded config for auto-detection
    let action_types: Vec<String> = {
        let config_lock = safe_lock_or_recover(&state.app_state.current_config, "current_config");
        if let Some(ref config) = *config_lock {
            // Extract action types from workflows
            config
                .workflows
                .iter()
                .flat_map(|w| {
                    w.get("actions")
                        .and_then(|a| a.as_array())
                        .map(|actions| {
                            actions
                                .iter()
                                .filter_map(|action| {
                                    action
                                        .get("type")
                                        .and_then(|t| t.as_str())
                                        .map(String::from)
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                })
                .collect()
        } else {
            Vec::new()
        }
    };

    // For now, we pass an empty error list for auto-detection
    // In the future, this could be populated from recent log errors
    let recent_errors: Vec<String> = Vec::new();

    // Inject contexts and track which ones were used
    let (prompt_with_contexts, used_context_ids) =
        if !context_ids.is_empty() || auto_include_contexts {
            let (enhanced, used_ids) = context::inject_contexts(
                &enhanced_prompt,
                &context_ids,
                auto_include_contexts,
                &prompt_content, // Use original prompt for auto-detection matching
                &action_types,
                &recent_errors,
            );

            if !used_ids.is_empty() {
                info!(
                    "MCP API: Injected {} contexts into prompt: {:?}",
                    used_ids.len(),
                    used_ids
                );
            }

            (enhanced, used_ids)
        } else {
            (enhanced_prompt.clone(), Vec::new())
        };
    enhanced_prompt = prompt_with_contexts;

    // Prepend runner-triggered context and supervisor instructions
    // This tells the AI session how to safely restart the runner if needed
    let supervisor_available = check_supervisor_available();
    let runner_context = if supervisor_available {
        r#"## IMPORTANT: Runner-Triggered Session Context

You are being run BY the qontinui-runner. You are a child process of the runner.

**CRITICAL RULES:**
1. Do NOT restart the qontinui-runner directly - it will kill your session
2. You CAN restart backend and frontend without issues
3. If the runner needs to be restarted, USE THE SUPERVISOR API

**Restarting Runner via Supervisor (SAFE):**
```powershell
# Simple restart (no rebuild)
Invoke-RestMethod -Uri "http://localhost:9875/runner/restart" -Method Post -ContentType "application/json" -Body '{"trigger_auto_continue": true}'

# Restart with REBUILD (use after modifying runner Rust code)
Invoke-RestMethod -Uri "http://localhost:9875/runner/restart" -Method Post -ContentType "application/json" -Body '{"rebuild": true, "trigger_auto_continue": true}'
```

**Supervisor API (port 9875):**
- GET /health - Check if supervisor is running
- POST /runner/stop - Stop the runner
- POST /runner/restart - Restart runner (options: rebuild, trigger_auto_continue, wait_timeout_seconds)
- POST /workflow-loop/signal-restart - Signal that runner restart is needed (use during workflow loops)

**IMPORTANT:** If you modified qontinui-runner Rust code, use `"rebuild": true` to recompile before restart.

**Workflow Loop Signal:** If you are running inside a supervisor workflow loop and you modify runner code, call:
```powershell
Invoke-RestMethod -Uri "http://localhost:9875/workflow-loop/signal-restart" -Method Post
```
This tells the supervisor to restart the runner between iterations. If you don't signal, the loop skips the restart (saving time when only non-runner repos were changed).

---

"#
    } else {
        r#"## IMPORTANT: Runner-Triggered Session Context

You are being run BY the qontinui-runner. You are a child process of the runner.

**CRITICAL RULES:**
1. Do NOT restart the qontinui-runner directly - it will kill your session
2. You CAN restart backend and frontend without issues
3. The supervisor is NOT currently running - if runner restart is needed, inform the user

**If runner restart is needed:**
Tell the user: "The qontinui-runner needs to be restarted manually to apply changes."

---

"#
    };

    enhanced_prompt = format!("{}{}", runner_context, enhanced_prompt);

    // Inject Multi-Step Task Guide context (user override takes precedence)
    let multi_step_guide = context::get_multi_step_guide();
    let multi_step_section = format!(
        "## Multi-Session Task Context\n\n{}\n\n---\n\n",
        context::format_single_context(&multi_step_guide)
    );
    enhanced_prompt = format!("{}{}", multi_step_section, enhanced_prompt);

    // Inject Service Restart Commands context (user override takes precedence)
    // Replace {{WORKSPACE}} placeholder with actual workspace path
    let service_restart = context::get_service_restart_commands();
    let workspace_path = get_workspace_paths_internal()
        .map(|(root, _, _)| root.to_string_lossy().to_string())
        .unwrap_or_else(|_| "{{WORKSPACE}}".to_string());
    let service_restart_content = service_restart
        .content
        .replace("{{WORKSPACE}}", &workspace_path);
    let mut service_restart_with_path = service_restart.clone();
    service_restart_with_path.content = service_restart_content;
    let service_restart_section = format!(
        "{}\n\n---\n\n",
        context::format_single_context(&service_restart_with_path)
    );
    enhanced_prompt = format!("{}{}", service_restart_section, enhanced_prompt);

    // Inject configured log sources from global settings
    // This tells the AI where to find logs for debugging
    {
        let global_settings = crate::settings::get_global_log_source_settings();
        let enabled_sources: Vec<_> = global_settings
            .sources
            .iter()
            .filter(|s| s.enabled)
            .map(|s| format!("- **{}**: `{}`", s.name, s.path))
            .collect();

        if !enabled_sources.is_empty() {
            let log_sources_section = format!(
                r#"## Configured Log Sources

The following log files have been configured for monitoring. Use these paths to check for errors:

{}

---

"#,
                enabled_sources.join("\n")
            );
            enhanced_prompt = format!("{}{}", log_sources_section, enhanced_prompt);
        }
    }

    // Add GUI automation context if config was auto-loaded
    if let Some((config_path, workflow_id, monitor_index)) = &config_info {
        let workflow_info = workflow_id
            .as_ref()
            .map(|w| format!("- Last workflow: {}", w))
            .unwrap_or_else(|| "- No last workflow saved".to_string());
        let monitor_info = monitor_index
            .map(|m| format!("- Last monitor index: {}", m))
            .unwrap_or_else(|| "- No last monitor index saved".to_string());

        let gui_context = format!(
            r#"
## GUI Automation Available

A workflow configuration has been auto-loaded:
- Config path: {}
{}
{}

**Runner MCP API (port 9876):**
- GET /status - Check runner and config status
- POST /run-workflow - Run a workflow by name
  Example: `Invoke-RestMethod -Uri "http://localhost:9876/run-workflow" -Method Post -ContentType "application/json" -Body '{{"workflow_id": "workflow-name", "monitor_index": 0}}'`
- GET /monitors - List available monitors

If your task requires running visual automation, use the Runner API to execute workflows.

---

"#,
            config_path, workflow_info, monitor_info
        );

        enhanced_prompt = format!("{}{}", gui_context, enhanced_prompt);
    }

    // Add MCP tool context if config is loaded (either pre-loaded or auto-loaded)
    {
        let config_lock = safe_lock_or_recover(&state.app_state.current_config, "current_config");
        if let Some(config) = config_lock.as_ref() {
            let tool_context = generate_mcp_tool_context(config);
            enhanced_prompt = format!("{}\n{}", enhanced_prompt, tool_context);
        }
    }

    if let Some(timeline) = &trace_timeline {
        enhanced_prompt = format!("{}\n\n{}", enhanced_prompt, timeline);
    }

    // Add image paths to prompt if there are any
    if !all_images.is_empty() {
        enhanced_prompt = format!(
            "{}\n\n## Images for Analysis\n\nThe following images are available for analysis. Use the Read tool to view them:\n{}",
            enhanced_prompt,
            all_images.iter().map(|p| format!("- {}", p)).collect::<Vec<_>>().join("\n")
        );
    }

    // Add structured finding output instructions
    enhanced_prompt = format!("{}{}", enhanced_prompt, FINDING_INSTRUCTIONS);

    info!(
        "MCP API: Running prompt '{}' (session: {}, max_sessions: {:?}, requires_orchestrator: {}, images: {})",
        prompt_name,
        session_id,
        max_sessions,
        requires_orchestrator,
        all_images.len()
    );

    // Create TaskRun record in database
    let db = CheckpointDb::new().map_err(|e| {
        error!("MCP API: Failed to open database: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to open database: {}", e))),
        )
    })?;

    {
        let mut input = CreateTaskRunInput::new(&task_run_id, &prompt_name)
            .with_prompt(&enhanced_prompt)
            .with_task_type("task");
        if let Some(ms) = max_sessions {
            input = input.with_max_sessions(ms);
        }
        db.create_task_run(&input)
    }
    .map_err(|e| {
        error!("MCP API: Failed to create task run: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to create task run: {}", e))),
        )
    })?;

    info!("MCP API: Created task run with ID: {}", task_run_id);

    // Create session context for AI output events so frontend can display the task name
    // This is the first turn (iteration 1), so turn_count = 1
    let session_ctx = AiSessionContext::agentic(&task_run_id, &prompt_name, 1)
        .with_runtime_env()
        .with_new_trace()
        .with_ai_settings()
        .with_turn_count(1);

    // Emit prompt to frontend (use original prompt content for display)
    emit_ai_output(
        &state.app_handle,
        &prompt_content,
        "prompt",
        Some(&task_run_id),
        Some(&session_ctx),
    );

    // Emit status indicator
    emit_ai_output(
        &state.app_handle,
        "AI session spawned - check task runs for status",
        "status",
        Some(&task_run_id),
        Some(&session_ctx),
    );

    // Record context usage now that the session is starting
    if !used_context_ids.is_empty() {
        context::record_contexts_used(&used_context_ids);
    }

    // =========================================================================
    // EXECUTION PATH ROUTING
    // =========================================================================
    // When requires_orchestrator is true, route through the unified session API
    // which has full orchestrator support (planning, verification, feedback loops).
    // When false, use the simpler direct spawn path.
    // =========================================================================

    // NOTE: The orchestrator path was removed when run_unified_session_loop was deleted.
    // All paths now use the direct spawn path. Orchestrator functionality will be
    // re-integrated via LoopController in a future update.
    if requires_orchestrator {
        warn!(
            "MCP API: Orchestrator path requested but session loop was removed. Falling through to direct spawn path for prompt '{}' (session: {})",
            prompt_name, session_id
        );
    }

    // Always use the direct spawn path for now
    {
        // =====================================================================
        // DIRECT SPAWN PATH
        // =====================================================================
        // DIRECT SPAWN PATH
        // =====================================================================
        // Use the simpler direct spawn path.
        // Orchestrator functionality will be re-integrated via LoopController.
        // =====================================================================

        info!(
            "MCP API: Using direct spawn path for prompt '{}' (session: {})",
            prompt_name, session_id
        );

        let prompt_name_for_state = prompt_name.clone();
        let result = tokio::task::spawn_blocking(move || {
            let (workspace_root, dev_logs_path, scripts_path) = get_workspace_paths_internal()?;
            let spawn_script = scripts_path.join("spawn-independent-claude.py");
            let state_file = dev_logs_path.join(format!("ai-developer-{}.json", session_id));
            let prompt_file = dev_logs_path.join(format!("ai-developer-{}-prompt.txt", session_id));
            let log_file = dev_logs_path.join(format!("claude-session-{}.log", session_id));

            // Ensure .dev-logs directory exists
            std::fs::create_dir_all(&dev_logs_path)
                .map_err(|e| format!("Failed to create dev-logs directory: {}", e))?;

            // Create initial state file
            let initial_state = serde_json::json!({
                "session_id": session_id,
                "task_run_id": session_id,
                "prompt_id": prompt_id,
                "prompt_name": prompt_name_for_state,
                "session_count": 1,
                "max_sessions": max_sessions,
                "status": "starting",
                "started_at": chrono::Utc::now().to_rfc3339(),
                "stop_requested": false,
                "current_action": "Initializing",
                "errors_fixed": [],
                "errors_remaining": [],
                "activity_log": [],
                // Orchestrator not used in direct spawn path
                "requires_orchestrator": false,
                "orchestrator_goal": null,
                "orchestrator_max_iterations": null
            });

            let state_json = serde_json::to_string_pretty(&initial_state)
                .map_err(|e| format!("Failed to serialize state: {}", e))?;
            std::fs::write(&state_file, state_json)
                .map_err(|e| format!("Failed to write state file: {}", e))?;

            // Write enhanced prompt content to file
            std::fs::write(&prompt_file, &enhanced_prompt)
                .map_err(|e| format!("Failed to write prompt file: {}", e))?;

            info!("MCP API: State file created: {:?}", state_file);
            info!("MCP API: Prompt file created: {:?}", prompt_file);

            // Spawn Claude independently using the spawn script
            // Use spawn_python_with_console to ensure Claude CLI gets a console window
            let spawn_result = spawn_python_with_console(
                "python",
                &[
                    spawn_script.as_os_str(),
                    std::ffi::OsStr::new("--file"),
                    prompt_file.as_os_str(),
                    std::ffi::OsStr::new("--session-id"),
                    std::ffi::OsStr::new(&session_id),
                ],
                &workspace_root,
            );

            match spawn_result {
                Ok(child) => {
                    info!(
                        "MCP API: AI Developer spawned with PID: {} for prompt '{}'",
                        child.id(),
                        prompt_name_for_state
                    );
                    Ok((
                        RunPromptResponse {
                            task_run_id: session_id.clone(),
                            action_id: session_id.clone(), // Backward compatibility
                            session_id,
                            state_file: state_file.to_string_lossy().to_string(),
                            log_file: log_file.to_string_lossy().to_string(),
                            pid: Some(child.id()),
                        },
                        log_file,
                        dev_logs_path,
                    ))
                }
                Err(e) => {
                    error!("MCP API: Failed to spawn AI Developer: {}", e);
                    Err(format!("Failed to spawn AI Developer: {}", e))
                }
            }
        })
        .await
        .map_err(|e| {
            error!("MCP API: spawn_blocking error: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Internal error: {}", e))),
            )
        })?;

        match result {
            Ok((response, _log_file, _dev_logs_path)) => {
                // NOTE: TaskMonitor was removed - task completion is now tracked by LoopController
                Ok(Json(ApiResponse::success(response)))
            }
            Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
        }
    }
}

// Remaining prompt CRUD handlers (categories, tags, import, export, duplicate, search)
// moved to crate::mcp::prompts

// Macro handlers moved to crate::mcp::macros
// Playwright script handlers moved to crate::mcp::playwright
// Prompt snippet handlers moved to crate::mcp::prompt_snippets

// ============================================================================
// Inline Python Execution
// ============================================================================

/// Execute inline Python code
///
/// This handler allows executing arbitrary Python code with optional dependency
/// isolation via uvx. The code is wrapped to capture return values if the
/// script returns a JSON-serializable value.
pub async fn execute_inline_python(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<InlinePythonRequest>,
) -> Result<Json<ApiResponse<InlinePythonResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    use std::time::Instant;
    use tokio::time::timeout;

    let start = Instant::now();
    // Timeouts are disabled by default
    let timeout_secs = request.timeout_seconds;

    // Determine working directory
    let working_dir = request
        .working_directory
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);

    // Create a temporary script file
    let script_id = uuid::Uuid::new_v4();
    let script_path = std::env::temp_dir().join(format!("qontinui_inline_{}.py", script_id));

    // Wrap the code to capture return value
    // The user's code becomes the body of a __main__ function
    // If the function returns a value, it's printed with a special marker
    let indented_code = request
        .code
        .lines()
        .map(|line| format!("    {}", line))
        .collect::<Vec<_>>()
        .join("\n");

    let wrapped_code = format!(
        r#"import json
import sys

def __qontinui_main__():
{indented_code}

if __name__ == "__main__":
    try:
        result = __qontinui_main__()
        if result is not None:
            print("__QONTINUI_RETURN__:" + json.dumps(result))
    except Exception as e:
        print(f"Error: {{e}}", file=sys.stderr)
        sys.exit(1)
"#,
        indented_code = indented_code
    );

    // Write the script to the temp file
    if let Err(e) = std::fs::write(&script_path, &wrapped_code) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to write script: {}", e))),
        ));
    }

    // Build the command - use uvx if dependencies are specified
    // Helper to run with or without timeout
    async fn run_with_optional_timeout(
        mut cmd: tokio::process::Command,
        timeout_secs: Option<u64>,
    ) -> Result<Result<std::process::Output, std::io::Error>, tokio::time::error::Elapsed> {
        if let Some(secs) = timeout_secs {
            timeout(std::time::Duration::from_secs(secs), cmd.output()).await
        } else {
            // No timeout - wrap in Ok to match the return type
            Ok(cmd.output().await)
        }
    }

    let output_result = if let Some(deps) = &request.dependencies {
        if !deps.is_empty() {
            // Use uvx for dependency isolation
            let deps_str = deps.join(",");
            let mut cmd = crate::process_helpers::tokio_no_window("uvx");
            cmd.args(["--with", &deps_str, "python", script_path.to_str().unwrap()])
                .current_dir(&working_dir)
                .kill_on_drop(true);

            run_with_optional_timeout(cmd, timeout_secs).await
        } else {
            // No dependencies, use python directly
            let mut cmd = crate::process_helpers::tokio_no_window("python");
            cmd.arg(script_path.to_str().unwrap())
                .current_dir(&working_dir)
                .kill_on_drop(true);

            run_with_optional_timeout(cmd, timeout_secs).await
        }
    } else {
        // No dependencies, use python directly
        let mut cmd = crate::process_helpers::tokio_no_window("python");
        cmd.arg(script_path.to_str().unwrap())
            .current_dir(&working_dir)
            .kill_on_drop(true);

        run_with_optional_timeout(cmd, timeout_secs).await
    };

    // Cleanup the temp script
    let _ = std::fs::remove_file(&script_path);

    let duration_ms = start.elapsed().as_millis() as u64;

    match output_result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            // Parse return value from stdout if present
            let (stdout_clean, return_value) = if let Some(idx) =
                stdout.find("__QONTINUI_RETURN__:")
            {
                let (before, after) = stdout.split_at(idx);
                let json_str = after.trim_start_matches("__QONTINUI_RETURN__:");
                let parsed: Option<serde_json::Value> = serde_json::from_str(json_str.trim()).ok();
                (before.to_string(), parsed)
            } else {
                (stdout, None)
            };

            Ok(Json(ApiResponse::success(InlinePythonResponse {
                success: output.status.success(),
                stdout: stdout_clean,
                stderr,
                return_value,
                duration_ms,
            })))
        }
        Ok(Err(e)) => {
            // Command failed to execute
            Ok(Json(ApiResponse::success(InlinePythonResponse {
                success: false,
                stdout: String::new(),
                stderr: format!("Failed to execute: {}", e),
                return_value: None,
                duration_ms,
            })))
        }
        Err(_) => {
            // Timeout
            let timeout_msg = timeout_secs
                .map(|t| format!("Execution timed out after {} seconds", t))
                .unwrap_or_else(|| "Execution timed out".to_string());
            Ok(Json(ApiResponse::success(InlinePythonResponse {
                success: false,
                stdout: String::new(),
                stderr: timeout_msg,
                return_value: None,
                duration_ms,
            })))
        }
    }
}

// ============================================================================
// Backup and Restore HTTP Endpoints
// ============================================================================

/// Response for backup creation
#[derive(Debug, Serialize)]
pub struct BackupResponse {
    /// Base64-encoded ZIP file data
    data: String,
    /// Original filename suggestion
    filename: String,
    /// Backup result with details
    result: backup::BackupResult,
}

/// Request for restore operation
#[derive(Debug, Deserialize)]
pub struct RestoreRequest {
    /// Base64-encoded ZIP file data
    data: String,
}

/// Create a backup of all user data
///
/// Returns the backup as base64-encoded ZIP data along with metadata.
pub async fn create_backup_handler(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<BackupResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Creating backup");

    match backup::create_backup() {
        Ok((zip_data, result)) => {
            // Encode ZIP data as base64
            let base64_data =
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &zip_data);

            // Generate filename with timestamp
            let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
            let filename = format!("qontinui_backup_{}.zip", timestamp);

            info!(
                "MCP API: Backup created successfully ({} bytes, {} files)",
                zip_data.len(),
                result.files_backed_up.len()
            );

            Ok(Json(ApiResponse::success(BackupResponse {
                data: base64_data,
                filename,
                result,
            })))
        }
        Err(e) => {
            error!("MCP API: Backup failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get information about a backup without restoring it
pub async fn get_backup_info_handler(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<RestoreRequest>,
) -> Result<Json<ApiResponse<backup::BackupManifest>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Getting backup info");

    // Decode base64 data
    let zip_data =
        match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &request.data) {
            Ok(data) => data,
            Err(e) => {
                error!("MCP API: Failed to decode backup data: {}", e);
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error(format!("Invalid base64 data: {}", e))),
                ));
            }
        };

    match backup::get_backup_info(&zip_data) {
        Ok(manifest) => {
            info!(
                "MCP API: Backup info retrieved - version {}, {} files",
                manifest.version,
                manifest.files.len()
            );
            Ok(Json(ApiResponse::success(manifest)))
        }
        Err(e) => {
            error!("MCP API: Failed to get backup info: {}", e);
            Err((StatusCode::BAD_REQUEST, Json(api_error(e))))
        }
    }
}

/// Restore user data from a backup
///
/// Accepts base64-encoded ZIP data and restores all files to their original locations.
pub async fn restore_backup_handler(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<RestoreRequest>,
) -> Result<Json<ApiResponse<backup::RestoreResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Restoring from backup");

    // Decode base64 data
    let zip_data =
        match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &request.data) {
            Ok(data) => data,
            Err(e) => {
                error!("MCP API: Failed to decode backup data: {}", e);
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error(format!("Invalid base64 data: {}", e))),
                ));
            }
        };

    match backup::restore_backup(&zip_data) {
        Ok(result) => {
            if result.success {
                info!(
                    "MCP API: Restore completed successfully - {} files restored",
                    result.files_restored.len()
                );
            } else {
                warn!(
                    "MCP API: Restore completed with errors: {:?}",
                    result.errors
                );
            }
            Ok(Json(ApiResponse::success(result)))
        }
        Err(e) => {
            error!("MCP API: Restore failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Trace and Video Extraction Utilities
// ============================================================================

/// Extract action timeline and screenshots from a Playwright trace ZIP file
pub fn extract_trace_data(
    trace_path: &str,
    max_screenshots: u32,
) -> Result<(String, Vec<String>), String> {
    use std::io::Read;

    info!("Extracting trace data from: {}", trace_path);

    let file =
        std::fs::File::open(trace_path).map_err(|e| format!("Failed to open trace file: {}", e))?;

    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("Failed to read trace ZIP: {}", e))?;

    let mut timeline = String::new();
    let mut screenshot_paths = Vec::new();

    // Create temp directory for extracted screenshots
    let temp_dir = std::env::temp_dir().join(format!("trace_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).map_err(|e| format!("Failed to create temp dir: {}", e))?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = file.name().to_string();

        // Extract action log for timeline
        if name.contains("actions") && name.ends_with(".json") {
            let mut contents = String::new();
            file.read_to_string(&mut contents).ok();
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
                timeline = format_trace_timeline(&json);
            }
        }

        // Extract screenshots (limited by max_screenshots)
        if name.ends_with(".png") && screenshot_paths.len() < max_screenshots as usize {
            let out_path = temp_dir.join(format!("trace_screenshot_{}.png", i));
            if let Ok(mut out_file) = std::fs::File::create(&out_path) {
                if std::io::copy(&mut file, &mut out_file).is_ok() {
                    screenshot_paths.push(out_path.to_string_lossy().to_string());
                }
            }
        }
    }

    info!(
        "Extracted trace: {} chars timeline, {} screenshots",
        timeline.len(),
        screenshot_paths.len()
    );

    Ok((timeline, screenshot_paths))
}

/// Format trace events into a human-readable timeline
pub fn format_trace_timeline(json: &serde_json::Value) -> String {
    let mut timeline = String::from("## Action Timeline from Trace\n\n");

    if let Some(events) = json.as_array() {
        for (i, event) in events.iter().enumerate() {
            let action_type = event
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("unknown");
            let selector = event.get("selector").and_then(|s| s.as_str()).unwrap_or("");
            let value = event.get("value").and_then(|v| v.as_str()).unwrap_or("");

            if !selector.is_empty() {
                timeline.push_str(&format!(
                    "{}. {} on `{}` {}\n",
                    i + 1,
                    action_type,
                    selector,
                    if !value.is_empty() {
                        format!("with value '{}'", value)
                    } else {
                        String::new()
                    }
                ));
            } else {
                timeline.push_str(&format!("{}. {}\n", i + 1, action_type));
            }
        }
    }

    timeline
}

/// Extract key frames from a video file using ffmpeg
pub fn extract_video_frames(video_path: &str, max_frames: u32) -> Result<Vec<String>, String> {
    info!(
        "Extracting {} frames from video: {}",
        max_frames, video_path
    );

    // Create temp directory for frames
    let temp_dir = std::env::temp_dir().join(format!("video_frames_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).map_err(|e| format!("Failed to create temp dir: {}", e))?;

    let output_pattern = temp_dir
        .join("frame_%03d.png")
        .to_string_lossy()
        .to_string();

    // Use ffmpeg to extract frames evenly distributed throughout the video
    // -vf "select='not(mod(n,X))'" extracts every Xth frame
    // For 3 frames from a 30 fps, 10 sec video (300 frames), we'd extract every 100th frame
    let status = crate::process_helpers::no_window("ffmpeg")
        .args([
            "-y", // Overwrite output
            "-i",
            video_path,
            "-vf",
            &format!("select='lt(n\\,{})',setpts=N/FRAME_RATE/TB", max_frames),
            "-vsync",
            "vfr",
            "-frames:v",
            &max_frames.to_string(),
            &output_pattern,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match status {
        Ok(s) if s.success() => {
            // Collect extracted frames
            let mut frames = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&temp_dir) {
                for entry in entries.flatten() {
                    if entry.path().extension().is_some_and(|e| e == "png") {
                        frames.push(entry.path().to_string_lossy().to_string());
                    }
                }
            }
            frames.sort(); // Ensure frames are in order
            info!("Extracted {} video frames", frames.len());
            Ok(frames)
        }
        Ok(_) => Err(
            "ffmpeg failed to extract frames. Ensure ffmpeg is installed and in PATH.".to_string(),
        ),
        Err(e) => Err(format!(
            "Failed to run ffmpeg: {}. Ensure ffmpeg is installed and in PATH.",
            e
        )),
    }
}

/// Collect all images for AI analysis (screenshots, trace screenshots, video frames)
pub fn collect_images_for_analysis(
    image_paths: &[String],
    video_paths: &[String],
    trace_path: Option<&str>,
    max_video_frames: u32,
    max_trace_screenshots: u32,
) -> (Vec<String>, Option<String>) {
    let mut all_images = image_paths.to_vec();
    let mut trace_timeline = None;

    // Extract trace data if provided
    if let Some(tp) = trace_path {
        match extract_trace_data(tp, max_trace_screenshots) {
            Ok((timeline, screenshots)) => {
                trace_timeline = Some(timeline);
                all_images.extend(screenshots);
            }
            Err(e) => {
                warn!("Failed to extract trace data: {}", e);
            }
        }
    }

    // Extract video frames if provided
    for video_path in video_paths {
        match extract_video_frames(video_path, max_video_frames) {
            Ok(frames) => {
                all_images.extend(frames);
            }
            Err(e) => {
                warn!("Failed to extract video frames: {}", e);
            }
        }
    }

    (all_images, trace_timeline)
}

// NOTE: execute_claude_cli, execute_windows_native, execute_via_wsl, execute_native,
// and execute_claude_api functions were removed. They implemented synchronous execution
// which has been replaced by the TaskRun model using spawn-independent-claude.py.

// NOTE: Unified Session API and start_session, stop_session, delete_session functions removed
// These are now handled by the LoopController

/// Result of parsing worker output for signals
#[derive(Debug, Clone)]
pub enum WorkerOutputSignal {
    /// Worker signals work is complete, ready for verification
    WorkComplete { reason: Option<String> },
    /// Worker requests replan
    NeedReplan { reason: String },
    /// Legacy task complete marker (deprecated but still supported)
    TaskComplete,
    /// No signal found, worker continues
    Continue,
}

/// Parse worker output for orchestrator signals
/// This is the primary signal detection used by the orchestrator architecture
pub fn parse_worker_output_signal(output: &str) -> WorkerOutputSignal {
    // First check for the new orchestrator signals
    if let Some(signal) = WorkerSignal::parse_from_output(output) {
        match signal {
            WorkerSignal::WorkComplete { reason } => {
                info!("Worker signal detected: [WORK_COMPLETE]");
                return WorkerOutputSignal::WorkComplete { reason };
            }
            WorkerSignal::NeedReplan { reason } => {
                info!("Worker signal detected: [NEED_REPLAN] - {}", reason);
                return WorkerOutputSignal::NeedReplan { reason };
            }
            WorkerSignal::Finding(_) => {
                // Findings don't terminate the loop, they're just recorded
            }
            WorkerSignal::Continue => {}
        }
    }

    // Check for legacy TASK_COMPLETE marker (deprecated but supported for backward compatibility)
    let output_upper = output.to_uppercase();
    let legacy_markers = [
        "[TASK_COMPLETE]",
        "[GOAL_COMPLETE]",
        "[GOAL_ACHIEVED]",
        "[STOP_SESSION]",
        "[SESSION_COMPLETE]",
    ];

    for marker in &legacy_markers {
        if output_upper.contains(marker) {
            info!(
                "Legacy completion marker detected: {} - treating as WORK_COMPLETE",
                marker
            );
            // Treat legacy markers as WORK_COMPLETE so verification still runs
            return WorkerOutputSignal::WorkComplete {
                reason: Some(format!("Legacy marker: {}", marker)),
            };
        }
    }

    // Check for structured completion patterns
    let completion_patterns = [
        r#""goal_achieved":\s*true"#,
        r#""goal_achieved": true"#,
        r#"goal_achieved:\s*true"#,
        r#""status":\s*"complete""#,
        r#"status:\s*"complete""#,
    ];

    for pattern in &completion_patterns {
        if let Ok(re) = Regex::new(pattern) {
            if re.is_match(output) {
                info!(
                    "Goal completion pattern detected: {} - treating as WORK_COMPLETE",
                    pattern
                );
                return WorkerOutputSignal::WorkComplete {
                    reason: Some(format!("Pattern match: {}", pattern)),
                };
            }
        }
    }

    WorkerOutputSignal::Continue
}

/// Check if AI output contains goal completion markers
/// Returns true if any marker indicates the goal has been achieved
/// NOTE: This is kept for backward compatibility but parse_worker_output_signal should be preferred
pub fn check_goal_completion_markers(output: &str) -> bool {
    matches!(
        parse_worker_output_signal(output),
        WorkerOutputSignal::WorkComplete { .. } | WorkerOutputSignal::TaskComplete
    )
}

/// Result of running deterministic verification
#[derive(Debug, Clone)]
pub struct DeterministicVerificationResult {
    /// Whether all CRITICAL checks passed (non-critical failures are informational)
    all_passed: bool,
    /// Summary of what was checked
    checks_run: Vec<String>,
    /// Details of CRITICAL failures (these block completion)
    critical_failures: Vec<String>,
    /// Details of non-critical failures (informational only)
    non_critical_failures: Vec<String>,
    /// Raw output from checks
    raw_output: String,
}

/// Run the workflow's actual verification steps (if defined) instead of just build checks
///
/// This function:
/// 1. Gets the task_run from database
/// 2. Extracts verification_steps from execution_steps_json
/// 3. If verification_steps exist, runs them through StepExecutor
/// 4. Otherwise falls back to basic deterministic verification
pub async fn run_workflow_verification_for_task(
    app_state: &std::sync::Arc<crate::AppState>,
    config_storage: &std::sync::Arc<tokio::sync::Mutex<ConfigStorage>>,
    db_task_id: &str,
    workspace_root: &str,
) -> DeterministicVerificationResult {
    use crate::step_executor::{ExecutionStepConfig, StepExecutor};

    // Get the task run to extract verification steps, config_id, and session count
    let task_run = app_state
        .checkpoint_db
        .get_task_run(db_task_id)
        .ok()
        .flatten();

    let config_id = task_run.as_ref().and_then(|t| t.config_id.clone());
    let session_num = task_run.as_ref().map(|t| t.sessions_count as i32);

    // Try to get verification steps from the task's execution_steps_json
    let verification_steps: Vec<ExecutionStepConfig> = task_run
        .as_ref()
        .and_then(|task| {
            task.execution_steps_json
                .as_ref()
                .and_then(|json| serde_json::from_str::<Vec<ExecutionStepConfig>>(json).ok())
                .map(|steps| {
                    steps
                        .into_iter()
                        .filter(|s| s.phase.as_deref() == Some("verification"))
                        .collect()
                })
        })
        .unwrap_or_default();

    // If no verification steps defined, fall back to basic deterministic verification
    if verification_steps.is_empty() {
        info!(
            "WORKFLOW-VERIFICATION: No verification_steps found for task {} - falling back to basic build checks",
            db_task_id
        );
        return run_deterministic_verification(
            workspace_root,
            None,
            Some(&app_state.checkpoint_db),
            config_id.as_deref(),
            Some(db_task_id),
            session_num,
        )
        .await;
    }

    info!(
        "WORKFLOW-VERIFICATION: Running {} verification_steps for task {}",
        verification_steps.len(),
        db_task_id
    );

    // Create a StepExecutor to run the verification steps
    let executor = StepExecutor::new(app_state.clone(), config_storage.clone());

    // Run verification steps
    let verification_result = executor
        .execute_verification_steps(&verification_steps, db_task_id, 1)
        .await;

    // Log the result
    info!(
        "WORKFLOW-VERIFICATION: Result for task {}: all_passed={}, passed={}/{}, failed={}",
        db_task_id,
        verification_result.all_passed,
        verification_result.passed_steps,
        verification_result.total_steps,
        verification_result.failed_steps
    );

    // Convert to DeterministicVerificationResult format
    let mut checks_run = Vec::new();
    let mut critical_failures = Vec::new();
    let mut raw_output = String::new();

    for result in &verification_result.step_results {
        let check_name = format!("{} ({})", result.step_name, result.step_type);
        checks_run.push(check_name.clone());

        if !result.success {
            let failure_msg = if let Some(ref error) = result.error {
                format!("{}: {}", check_name, error)
            } else {
                format!("{}: failed", check_name)
            };
            critical_failures.push(failure_msg);

            // Add detailed output if available
            if let Some(ref details) = result.verification_details {
                if let Some(ref stdout) = details.stdout {
                    raw_output.push_str(&format!("=== {} ===\n{}\n\n", check_name, stdout));
                }
            }
        }
    }

    DeterministicVerificationResult {
        all_passed: verification_result.all_passed,
        checks_run,
        critical_failures,
        non_critical_failures: Vec::new(),
        raw_output,
    }
}

/// Run deterministic verification for a task
/// This runs build, tests, type checks, etc. before allowing task completion
///
/// IMPORTANT: Tests have an `is_critical` flag. Only critical test failures
/// block task completion. Non-critical failures are reported but don't fail verification.
pub async fn run_deterministic_verification(
    workspace_root: &str,
    _verification_config: Option<&serde_json::Value>,
    db: Option<&CheckpointDb>,
    config_id: Option<&str>,
    task_run_id: Option<&str>,
    session_num: Option<i32>,
) -> DeterministicVerificationResult {
    let _verifier = DeterministicVerifier::new(workspace_root.to_string());
    let mut checks_run = Vec::new();
    let mut critical_failures = Vec::new();
    let mut non_critical_failures: Vec<String> = Vec::new();
    let mut raw_output = String::new();

    // For Phase 1: Run basic build checks
    // Build checks are always CRITICAL - if the code doesn't compile, verification fails
    let workspace = std::path::Path::new(workspace_root);

    // Check for npm project
    if workspace.join("package.json").exists() {
        checks_run.push("npm build (critical)".to_string());
        info!("Running npm build verification in {}", workspace_root);

        let output = if cfg!(target_os = "windows") {
            crate::process_helpers::cmd_no_window()
                .args(["/C", "npm run build"])
                .current_dir(workspace_root)
                .output()
        } else {
            crate::process_helpers::no_window("sh")
                .args(["-c", "npm run build"])
                .current_dir(workspace_root)
                .output()
        };

        match output {
            Ok(result) => {
                let stdout = String::from_utf8_lossy(&result.stdout);
                let stderr = String::from_utf8_lossy(&result.stderr);
                raw_output.push_str(&format!(
                    "=== npm build (CRITICAL) ===\nExit: {}\nStdout:\n{}\nStderr:\n{}\n\n",
                    result.status.code().unwrap_or(-1),
                    stdout,
                    stderr
                ));

                if !result.status.success() {
                    critical_failures.push(format!(
                        "npm build failed with exit code {}",
                        result.status.code().unwrap_or(-1)
                    ));
                    // Extract error lines
                    for line in stderr.lines().chain(stdout.lines()) {
                        let lower = line.to_lowercase();
                        if lower.contains("error") || lower.contains("failed") {
                            critical_failures.push(line.to_string());
                        }
                    }
                }
            }
            Err(e) => {
                critical_failures.push(format!("Failed to run npm build: {}", e));
            }
        }
    }

    // Check for Cargo project
    if workspace.join("Cargo.toml").exists() {
        checks_run.push("cargo check (critical)".to_string());
        info!("Running cargo check verification in {}", workspace_root);

        let output = crate::process_helpers::no_window("cargo")
            .args(["check"])
            .current_dir(workspace_root)
            .output();

        match output {
            Ok(result) => {
                let stdout = String::from_utf8_lossy(&result.stdout);
                let stderr = String::from_utf8_lossy(&result.stderr);
                raw_output.push_str(&format!(
                    "=== cargo check (CRITICAL) ===\nExit: {}\nStdout:\n{}\nStderr:\n{}\n\n",
                    result.status.code().unwrap_or(-1),
                    stdout,
                    stderr
                ));

                if !result.status.success() {
                    critical_failures.push(format!(
                        "cargo check failed with exit code {}",
                        result.status.code().unwrap_or(-1)
                    ));
                    // Extract error lines
                    for line in stderr.lines() {
                        if line.contains("error[E") || line.starts_with("error:") {
                            critical_failures.push(line.to_string());
                        }
                    }
                }
            }
            Err(e) => {
                critical_failures.push(format!("Failed to run cargo check: {}", e));
            }
        }
    }

    // If no build system found, verification passes by default
    if checks_run.is_empty() {
        checks_run.push("(no build system detected)".to_string());
        raw_output.push_str("No package.json or Cargo.toml found. Skipping build verification.\n");
    }

    // Phase 2: Run verification tests from database
    if let (Some(db), Some(cfg_id)) = (db, config_id) {
        use crate::database::TriggerPoint;
        use crate::test_executor::task_integration::{
            create_findings_for_failures, execute_tests_for_trigger,
        };

        let test_results =
            execute_tests_for_trigger(db, cfg_id, &TriggerPoint::AfterWorkflow, task_run_id);

        if test_results.total > 0 {
            // Add test names to checks_run
            for result in &test_results.results {
                let criticality = db
                    .get_verification_test(&result.test_id)
                    .ok()
                    .flatten()
                    .map(|t| {
                        if t.is_critical {
                            "critical"
                        } else {
                            "non-critical"
                        }
                    })
                    .unwrap_or("unknown");
                checks_run.push(format!("test: {} ({})", result.test_name, criticality));
            }

            // Categorize failures as critical or non-critical
            for result in &test_results.results {
                if matches!(
                    result.status,
                    crate::test_executor::TestStatus::Passed
                        | crate::test_executor::TestStatus::Skipped
                ) {
                    continue;
                }
                let msg = format!(
                    "Test '{}' failed: {}",
                    result.test_name,
                    result.error.as_deref().unwrap_or("Unknown error")
                );
                let is_critical = db
                    .get_verification_test(&result.test_id)
                    .ok()
                    .flatten()
                    .map(|t| t.is_critical)
                    .unwrap_or(false);
                if is_critical {
                    critical_failures.push(msg);
                } else {
                    non_critical_failures.push(msg);
                }
            }

            // Append test output to raw_output
            if !test_results.ai_context.is_empty() {
                raw_output.push_str("\n\n--- Verification Tests ---\n");
                raw_output.push_str(&test_results.ai_context);
            }

            // Create findings for critical failures
            if let (Some(trid), Some(sn)) = (task_run_id, session_num) {
                create_findings_for_failures(db, trid, sn, &test_results, cfg_id);
            }
        }
    }

    DeterministicVerificationResult {
        // Only CRITICAL failures block completion
        all_passed: critical_failures.is_empty(),
        checks_run,
        critical_failures,
        non_critical_failures,
        raw_output,
    }
}

/// Generate feedback for failed verification to include in next iteration prompt
pub fn generate_verification_feedback(result: &DeterministicVerificationResult) -> String {
    let mut feedback = String::new();

    if !result.all_passed {
        feedback.push_str("## ⚠️ Deterministic Verification Failed\n\n");
        feedback.push_str(
            "The system ran verification after your [WORK_COMPLETE] signal and found issues:\n\n",
        );

        feedback.push_str("### Checks Run\n");
        for check in &result.checks_run {
            feedback.push_str(&format!("- {}\n", check));
        }

        if !result.critical_failures.is_empty() {
            feedback.push_str("\n### ❌ Critical Failures (blocking)\n");
            feedback.push_str("These MUST be fixed before the task can complete:\n");
            for failure in &result.critical_failures {
                feedback.push_str(&format!("- {}\n", failure));
            }
        }

        if !result.non_critical_failures.is_empty() {
            feedback.push_str("\n### ⚠️ Non-Critical Failures (informational)\n");
            feedback.push_str("These don't block completion but should be reviewed:\n");
            for failure in &result.non_critical_failures {
                feedback.push_str(&format!("- {}\n", failure));
            }
        }

        feedback.push_str("\n### Action Required\n");
        feedback.push_str(
            "Please fix the CRITICAL issues above and signal [WORK_COMPLETE] again when ready.\n",
        );
        feedback.push_str("The task will NOT be marked complete until all critical checks pass.\n");
    }

    feedback
}

/// Information about a single active session
#[derive(Debug, Clone, Serialize)]
pub struct ActiveSessionInfo {
    /// Unique session ID
    id: String,
    /// Display name
    name: String,
    /// Current status (running, waiting_for_continuation, etc.)
    status: String,
    /// When the session started
    started_at: String,
    /// Whether this session uses GUI automation (blocks other GUI sessions)
    uses_gui: bool,
}

// NOTE: ResumableWorkflowInfo, get_resumable_workflow, ResumeWorkflowResponse, resume_workflow,
// ForceContinueRequest, ForceContinueResponse, force_continue_session, force_continue_simple
// functions removed - these are now handled by the LoopController

/// Response for auto-continue setting
#[derive(Debug, Serialize)]
pub struct AutoContinueSettingResponse {
    enabled: bool,
}

/// Get the auto-continue AI workflow setting
pub async fn get_auto_continue_setting() -> Json<ApiResponse<AutoContinueSettingResponse>> {
    let enabled = settings::get_auto_continue_ai_workflow();
    Json(ApiResponse::success(AutoContinueSettingResponse {
        enabled,
    }))
}

/// Request body for setting auto-continue
#[derive(Debug, Deserialize)]
pub struct SetAutoContinueRequest {
    enabled: bool,
}

/// Set the auto-continue AI workflow setting
pub async fn set_auto_continue_setting(
    Json(body): Json<SetAutoContinueRequest>,
) -> Json<ApiResponse<AutoContinueSettingResponse>> {
    match settings::save_auto_continue_ai_workflow(body.enabled) {
        Ok(_) => {
            info!(
                "Auto-continue AI workflow setting updated to: {}",
                body.enabled
            );
            Json(ApiResponse::success(AutoContinueSettingResponse {
                enabled: body.enabled,
            }))
        }
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(format!("Failed to save setting: {}", e)),
        }),
    }
}

/// Response for per-workflow auto-continue setting
#[derive(Debug, Serialize)]
pub struct WorkflowAutoContinueResponse {
    enabled: bool,
    workflow_name: Option<String>,
}

/// Get the auto-continue setting for the active workflow
/// Now uses global setting and checks for running tasks in database
pub async fn get_workflow_auto_continue() -> Json<ApiResponse<WorkflowAutoContinueResponse>> {
    let enabled = settings::get_auto_continue_ai_workflow();

    // Check if there are any running tasks
    let workflow_name = if let Ok(db) = CheckpointDb::new() {
        db.get_running_task_runs(None)
            .ok()
            .and_then(|tasks| tasks.first().map(|t| t.task_name.clone()))
    } else {
        None
    };

    Json(ApiResponse::success(WorkflowAutoContinueResponse {
        enabled,
        workflow_name,
    }))
}

/// Set the auto-continue setting for the active workflow
/// Now just updates the global setting
pub async fn set_workflow_auto_continue(
    Json(body): Json<SetAutoContinueRequest>,
) -> Json<ApiResponse<WorkflowAutoContinueResponse>> {
    // Update the global setting
    match settings::save_auto_continue_ai_workflow(body.enabled) {
        Ok(_) => {
            info!("Auto-continue setting updated to: {}", body.enabled);

            // Get the active workflow name if any
            let workflow_name = if let Ok(db) = CheckpointDb::new() {
                db.get_running_task_runs(None)
                    .ok()
                    .and_then(|tasks| tasks.first().map(|t| t.task_name.clone()))
            } else {
                None
            };

            Json(ApiResponse::success(WorkflowAutoContinueResponse {
                enabled: body.enabled,
                workflow_name,
            }))
        }
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(format!("Failed to update auto-continue setting: {}", e)),
        }),
    }
}

/// Check if the supervisor is available on port 9875.
/// Used to determine what restart instructions to give AI sessions.
pub fn check_supervisor_available() -> bool {
    use std::net::TcpStream;
    use std::time::Duration;

    // Try to connect to supervisor health endpoint
    TcpStream::connect_timeout(
        &"127.0.0.1:9875".parse().unwrap(),
        Duration::from_millis(500),
    )
    .is_ok()
}

// NOTE: resume_all_running_tasks_on_startup function removed - now handled by LoopController

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/bridges", get(list_bridges).post(create_bridge))
        .route("/bridges/:bridge_id", get(get_bridge).delete(remove_bridge))
        .route("/bridges/:bridge_id/workflow", post(run_bridge_workflow))
        .route("/gui-lock", get(get_gui_lock))
        .route(
            "/config/headless-only",
            get(get_headless_only).post(set_headless_only),
        )
        .route("/debug/app/errors", get(get_debug_errors))
        .route("/findings/summary", get(get_findings_summary))
        .route("/launch-debug-chrome", post(launch_debug_chrome))
        .route("/status", get(get_status))
        .route("/instances", get(get_instances))
        .route("/tool-version", get(get_tool_version))
        .route("/load-config", post(load_config))
        .route("/load-last-config", post(load_last_config))
        .route("/run-workflow", post(run_workflow))
        .route("/execute-steps", post(execute_steps))
        .route("/stop-execution", post(stop_execution))
        .route("/execute", post(execute_python_command))
        .route("/execute-action", post(execute_action))
        .route("/capture-screenshot", post(capture_screenshot_step))
        .route("/screenshots/list", get(list_screenshots_endpoint))
        .route("/action-log/view", get(get_action_log_view_endpoint))
        .route("/go-to-state", post(go_to_state))
        .route(
            "/playwright-collection/start",
            post(start_playwright_collection),
        )
        .route(
            "/playwright-collection/status",
            get(get_playwright_collection_status),
        )
        .route(
            "/playwright-collection/results",
            get(get_playwright_collection_results),
        )
        .route(
            "/playwright-collection/stop",
            post(stop_playwright_collection),
        )
        .route("/stop-ai-analysis", post(stop_ai_analysis))
        .route("/restart-runner", post(restart_runner))
        .route("/prompts/run", post(run_prompt))
        .route("/execute-python", post(execute_inline_python))
        .route(
            "/workflow/auto-continue",
            get(get_auto_continue_setting).post(set_auto_continue_setting),
        )
        .route(
            "/workflow/active/auto-continue",
            get(get_workflow_auto_continue).post(set_workflow_auto_continue),
        )
        .route("/backup", get(create_backup_handler))
        .route("/backup/info", post(get_backup_info_handler))
        .route("/restore", post(restore_backup_handler))
        .route(
            "/render-log",
            get(get_render_log).delete(clear_render_log_handler),
        )
        .route("/render-log/path", get(get_render_log_path))
        .route("/navigate", post(navigate_to_page))
}
