//! Miscellaneous handlers for MCP API
//!
//! Contains bridge management, debug/status, configuration loading,
//! step execution, screenshot/render log, IPC bridge, and inline Python handlers.
//!
//! Extracted modules:
//! - `ai_session` - AI session management (stop/restart/run_prompt)
//! - `playwright_collection` - Playwright state collector
//! - `trace_verification` - Trace extraction & deterministic verification
//! - `backup_restore` - Backup/restore endpoints
//! - `auto_continue` - Auto-continue settings & supervisor checks

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::Emitter;
use tracing::{error, info, warn};

use crate::config::ConfigLoader;
use crate::executor::{
    with_default_bridge, BridgeInfo, BridgeMode, CreateBridgeResult, GuiLockInfo,
};
use crate::findings::storage as finding_storage;
use crate::mcp::shared::emit_ai_output;
use crate::mcp::types::{api_error, ApiResponse, ApiState, GoToStateRequest, GoToStateResult};
use crate::safe_eprintln;
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
        if config.port == 0 || config.port == self_port || config.port < 1024 {
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

// Playwright State Collector handlers moved to crate::mcp::playwright_collection

// AI session management handlers moved to crate::mcp::ai_session
use crate::mcp::ai_session::{
    has_running_ai_tasks_async, InlinePythonRequest, InlinePythonResponse,
};

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

// Backup/restore handlers moved to crate::mcp::backup_restore

// Trace, verification, and media utilities moved to crate::mcp::trace_verification
// Trace, verification, and media utilities moved to crate::mcp::trace_verification

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

// Auto-continue settings moved to crate::mcp::auto_continue
// Auto-continue settings moved to crate::mcp::auto_continue

// ─── Instance Management HTTP Endpoints ─────────────────────────────────────

/// POST /instances/spawn — create config + launch a new runner instance.
async fn spawn_instance(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<SpawnInstanceRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (axum::http::StatusCode, Json<ApiResponse<()>>)> {
    use crate::settings::{self, RunnerInstanceConfig};

    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            Json(ApiResponse::error("Instance name must not be empty")),
        ));
    }
    let port = body.port;
    if port < 1024 {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            Json(ApiResponse::error("Port must be >= 1024")),
        ));
    }

    // Generate ID
    let id = format!(
        "inst-{}-{}",
        port,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            % 100000
    );

    let config = RunnerInstanceConfig {
        id: id.clone(),
        name: name.clone(),
        port,
    };

    // Save config
    settings::save_runner_instance(config.clone()).map_err(|e| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::error(format!("Failed to save config: {}", e))),
        )
    })?;

    // Launch
    let pid = state
        .instance_manager
        .launch_instance(&config)
        .await
        .map_err(|e| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::error(format!("Failed to launch: {}", e))),
            )
        })?;

    // Register with supervisor if reachable
    let supervisor_port: u16 = std::env::var("QONTINUI_SUPERVISOR_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9875);
    let sup_url = format!("http://127.0.0.1:{}/runners", supervisor_port);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok();
    if let Some(client) = client {
        let _ = client
            .post(&sup_url)
            .json(&serde_json::json!({ "name": name, "port": port }))
            .send()
            .await;
    }

    tracing::info!(
        "Spawned instance '{}' (id={}, port={}, pid={})",
        name,
        id,
        port,
        pid
    );

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "name": name,
        "port": port,
        "pid": pid,
    }))))
}

/// POST /instances/{id}/stop — stop a running instance.
async fn stop_instance(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<String>>, (axum::http::StatusCode, Json<ApiResponse<()>>)> {
    state
        .instance_manager
        .stop_instance(&id)
        .await
        .map_err(|e| {
            (
                axum::http::StatusCode::NOT_FOUND,
                Json(ApiResponse::error(e)),
            )
        })?;

    Ok(Json(ApiResponse::success("stopped".to_string())))
}

/// POST /instances/{id}/launch — launch an existing configured instance.
async fn launch_instance(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (axum::http::StatusCode, Json<ApiResponse<()>>)> {
    let configs = crate::settings::get_runner_instances();
    let config = configs.iter().find(|c| c.id == id).ok_or_else(|| {
        (
            axum::http::StatusCode::NOT_FOUND,
            Json(ApiResponse::error(format!(
                "Instance '{}' not found in config",
                id
            ))),
        )
    })?;

    let pid = state
        .instance_manager
        .launch_instance(config)
        .await
        .map_err(|e| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::error(e)),
            )
        })?;

    Ok(Json(ApiResponse::success(
        serde_json::json!({ "pid": pid }),
    )))
}

#[derive(serde::Deserialize)]
struct SpawnInstanceRequest {
    name: String,
    port: u16,
}

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
        .route("/instances/spawn", post(spawn_instance))
        .route("/instances/:id/stop", post(stop_instance))
        .route("/instances/:id/launch", post(launch_instance))
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
        .route("/execute-python", post(execute_inline_python))
        .route(
            "/render-log",
            get(get_render_log).delete(clear_render_log_handler),
        )
        .route("/render-log/path", get(get_render_log_path))
        .route("/navigate", post(navigate_to_page))
}
