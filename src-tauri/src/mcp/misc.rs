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

use axum::{extract::State, http::StatusCode, response::Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::Emitter;
use tracing::{error, info, warn};

use crate::executor::with_default_bridge;
use crate::findings::{FindingCategoryExt, FindingSeverityExt, FindingStatusExt};
use crate::mcp::shared::emit_ai_output;
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use regex::Regex;

// Re-export handlers from extracted submodules for backward compatibility
// (external code may reference crate::mcp::misc::load_config_internal etc.)
pub use super::bridges::*;
pub use super::gui_execution::*;

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
    let task_run_id_filter = params.get("task_run_id").cloned();

    let pg = match crate::database::pg::PgDb::try_global() {
        Some(pg) => pg,
        None => {
            return Json(ApiResponse::success(serde_json::json!({
                "total_findings": 0,
                "code_related_findings": 0,
                "by_severity": {},
                "findings": [],
                "error": "PostgreSQL not available"
            })));
        }
    };

    // Get findings based on filter
    let mut all_findings = Vec::new();
    if let Some(task_run_id) = task_run_id_filter {
        if let Ok(findings) = pg.get_findings_for_task(&task_run_id).await {
            all_findings.extend(findings);
        }
    } else {
        // Get recent task run IDs
        if let Ok(task_runs) = pg.get_recent_task_runs(5, None).await {
            for task_run in &task_runs {
                if let Ok(findings) = pg.get_findings_for_task(&task_run.id).await {
                    all_findings.extend(findings);
                }
            }
        }
    }

    let total = all_findings.len();

    // Count by severity
    let mut by_severity: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut code_related = 0usize;

    let findings_json: Vec<serde_json::Value> = all_findings
        .iter()
        .map(|f| {
            let severity = f.severity.as_str().to_string();
            *by_severity.entry(severity.clone()).or_insert(0) += 1;
            if matches!(
                f.category,
                crate::findings::FindingCategory::CodeBug
                    | crate::findings::FindingCategory::TestIssue
            ) {
                code_related += 1;
            }
            serde_json::json!({
                "id": f.id,
                "task_run_id": f.task_run_id,
                "category": f.category.as_str(),
                "severity": f.severity.as_str(),
                "status": f.status.as_str(),
                "title": f.title,
                "description": f.description,
                "file_path": f.code_context.as_ref().and_then(|c| c.file.as_deref()),
                "detected_at": f.detected_at,
            })
        })
        .collect();

    Json(ApiResponse::success(serde_json::json!({
        "total_findings": total,
        "code_related_findings": code_related,
        "by_severity": by_severity,
        "findings": findings_json,
    })))
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
                        error_detail: None,
                        hint: None,
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
                error_detail: None,
                hint: None,
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

    // Check AI analysis status via PG
    let ai_running = !state
        .app_state
        .pg_db
        .get_running_task_runs(None)
        .await
        .unwrap_or_default()
        .is_empty();

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
    /// Per-instance spawn-window placement, if configured. None for
    /// the primary, registered/discovered instances, and configured
    /// instances that haven't had a placement set.
    #[serde(skip_serializing_if = "Option::is_none")]
    spawn_placement: Option<crate::settings::SpawnPlacement>,
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
        spawn_placement: None,
    }];

    // Collect all known ports to avoid duplicates
    let mut seen_ports: std::collections::HashSet<u16> = std::collections::HashSet::new();
    seen_ports.insert(self_port);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1))
        .build()
        .ok();

    // Add configured secondary instances (skip any that match our own port)
    let configs = crate::settings::get_runner_instances();
    for config in &configs {
        if config.port == 0 || config.port < 1024 || !seen_ports.insert(config.port) {
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
            spawn_placement: config.spawn_placement.clone(),
        });
    }

    // Add externally-registered instances (from in-memory register endpoint)
    let registered = state.instance_manager.get_registered_instances().await;
    for reg in &registered {
        if !seen_ports.insert(reg.port) {
            continue;
        }
        let reachable = if let Some(ref client) = client {
            let url = format!("http://localhost:{}/health", reg.port);
            client.get(&url).send().await.is_ok()
        } else {
            false
        };
        instances.push(DiscoveredInstance {
            name: Some(reg.name.clone()),
            port: reg.port,
            is_self: false,
            reachable,
            spawn_placement: None,
        });
    }

    // Add instances from PostgreSQL (survives restarts, catches externally-registered runners)
    if let Ok(db_instances) = state.app_state.pg_db.get_all_runner_instances().await {
        for db_inst in &db_instances {
            let port = db_inst.port as u16;
            if !seen_ports.insert(port) {
                continue; // already listed from another source
            }
            let reachable = if let Some(ref client) = client {
                let url = format!("http://localhost:{}/health", port);
                client.get(&url).send().await.is_ok()
            } else {
                false
            };
            instances.push(DiscoveredInstance {
                name: Some(db_inst.name.clone()),
                port,
                is_self: false,
                reachable,
                spawn_placement: None,
            });
        }
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

    // checkpoint_db removed — verification tests not yet migrated to PG
    let test_count = 0usize;

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

fn default_true() -> bool {
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
use crate::mcp::ai_session::{InlinePythonRequest, InlinePythonResponse};

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

    // Auto-allocate port if not specified
    let port = match body.port {
        Some(p) if p < 1024 => {
            return Err((
                axum::http::StatusCode::BAD_REQUEST,
                Json(ApiResponse::error("Port must be >= 1024")),
            ));
        }
        Some(p) => p,
        None => state.instance_manager.allocate_port().await.map_err(|e| {
            (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(ApiResponse::error(e)),
            )
        })?,
    };

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
        spawn_placement: body.spawn_placement.clone(),
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
        .launch_instance_with_app(&config, Some(&state.app_handle))
        .await
        .map_err(|e| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::error(format!("Failed to launch: {}", e))),
            )
        })?;

    // Register with supervisor if reachable (best-effort).
    // If no supervisor, the primary runner is already the coordinator via
    // the instance registry (Phase 1-4 of multi-instance awareness).
    // QONTINUI_SUPERVISOR_PORT (legacy) takes precedence over the registry
    // for backward compatibility.
    let sup_url = match std::env::var("QONTINUI_SUPERVISOR_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
    {
        Some(port) => format!("http://127.0.0.1:{}/runners", port),
        None => format!("{}/runners", crate::api_config::get_supervisor_url()),
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok();
    if let Some(client) = client {
        match client
            .post(&sup_url)
            .json(&serde_json::json!({ "name": name, "port": port }))
            .send()
            .await
        {
            Ok(_) => tracing::debug!("Registered spawned instance with supervisor"),
            Err(_) => {
                tracing::debug!("Supervisor not reachable — primary runner is the coordinator")
            }
        }
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
        .launch_instance_with_app(config, Some(&state.app_handle))
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
    /// Port for the new instance. If omitted, auto-allocated from 9877-9899.
    port: Option<u16>,
    /// Optional per-instance spawn-window placement. Persisted with
    /// the instance config and applied at every launch (this one and
    /// future ones).
    #[serde(default)]
    spawn_placement: Option<crate::settings::SpawnPlacement>,
}

#[derive(serde::Deserialize)]
struct RegisterInstanceRequest {
    name: String,
    port: u16,
    pid: Option<u32>,
}

/// POST /instances/register — register an externally-started runner with the primary.
async fn register_instance(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<RegisterInstanceRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (axum::http::StatusCode, Json<ApiResponse<()>>)> {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            Json(ApiResponse::error("Instance name must not be empty")),
        ));
    }
    if body.port < 1024 {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            Json(ApiResponse::error("Port must be >= 1024")),
        ));
    }

    let self_port = state
        .app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);
    if body.port == self_port {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            Json(ApiResponse::error(
                "Cannot register with the same port as the primary",
            )),
        ));
    }

    let id = state
        .instance_manager
        .register_instance(name.clone(), body.port, body.pid)
        .await;

    // Note: we intentionally do NOT persist to settings.json here.
    // Externally-registered instances are transient — they re-register
    // on startup via heartbeat. Persistent storage is added in Phase 2
    // via the runner_instances PostgreSQL table.

    tracing::info!(
        "Registered instance '{}' (id={}, port={}, pid={:?})",
        name,
        id,
        body.port,
        body.pid
    );

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "primary_port": self_port,
    }))))
}

#[derive(serde::Deserialize)]
struct HeartbeatRequest {
    running_task_count: Option<u32>,
    #[allow(dead_code)]
    running_task_ids: Option<Vec<String>>,
}

/// DELETE /instances/{id} — deregister a runner instance from both the
/// in-memory map and the DB registry.
///
/// The matching `POST /instances/register` adds an in-memory entry (this
/// process) and a DB row (shared across processes). Stopping a managed
/// child via `POST /instances/{id}/stop` cleans up its row, but external
/// runners that crash, get force-killed, or otherwise exit without
/// signalling the runner leave a phantom row behind that
/// `purge_unreachable_registered` only catches once the heartbeat has
/// gone stale (`STALE_THRESHOLD_SECS`). This endpoint lets a caller
/// reclaim the slot immediately.
///
/// Returns 200 if either side was cleaned up, 404 if the id wasn't known
/// in either. Body carries the granular flags so callers can tell.
async fn delete_instance(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (axum::http::StatusCode, Json<ApiResponse<()>>)> {
    let result = state.instance_manager.deregister_instance(&id).await;

    if !result.any() {
        return Err((
            axum::http::StatusCode::NOT_FOUND,
            Json(ApiResponse::error(format!(
                "Instance '{}' not found in in-memory map or DB registry",
                id
            ))),
        ));
    }

    tracing::info!(
        "Deregistered instance '{}' (in_memory={}, db={})",
        id,
        result.removed_in_memory,
        result.removed_db
    );

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "removed_in_memory": result.removed_in_memory,
        "removed_db": result.removed_db,
    }))))
}

/// POST /instances/{id}/heartbeat — update heartbeat for a registered instance.
///
/// Returns 200 if the instance is known, 404 if not (secondary should re-register).
async fn instance_heartbeat(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<HeartbeatRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (axum::http::StatusCode, Json<ApiResponse<()>>)> {
    let updated = state
        .instance_manager
        .update_heartbeat(&id, body.running_task_count)
        .await;

    if updated {
        Ok(Json(ApiResponse::success(serde_json::json!({
            "acknowledged": true,
        }))))
    } else {
        Err((
            axum::http::StatusCode::NOT_FOUND,
            Json(ApiResponse::error(
                "Instance not registered — re-register with POST /instances/register",
            )),
        ))
    }
}

/// GET /runners — supervisor-compatible listing of all known runner instances.
///
/// Returns the same shape as the supervisor's `GET /runners` endpoint so that
/// scripts (runner_status.py, runner_lock.py, manual-test) can target either
/// the supervisor (port 9875) or the primary runner (port 9876).
async fn list_runners(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    let self_port = state
        .app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);
    let self_name = std::env::var("QONTINUI_INSTANCE_NAME")
        .ok()
        .unwrap_or_else(|| "primary".to_string());
    let is_primary = !crate::instance::is_secondary();

    let mut runners = Vec::new();

    // Self
    runners.push(serde_json::json!({
        "id": format!("primary-{}", self_port),
        "name": self_name,
        "port": self_port,
        "is_primary": is_primary,
        "running": true,
        "pid": std::process::id(),
        "api_responding": true,
    }));

    let mut seen_ports: std::collections::HashSet<u16> = std::collections::HashSet::new();
    seen_ports.insert(self_port);

    // From DB
    if let Ok(db_instances) = state.app_state.pg_db.get_all_runner_instances().await {
        for inst in &db_instances {
            let port = inst.port as u16;
            if !seen_ports.insert(port) {
                continue;
            }
            runners.push(serde_json::json!({
                "id": inst.id,
                "name": inst.name,
                "port": port,
                "is_primary": inst.is_primary,
                "running": inst.status == "healthy" || inst.status == "starting",
                "pid": inst.pid,
                "api_responding": inst.status == "healthy",
            }));
        }
    }

    // From in-memory registered
    let registered = state.instance_manager.get_registered_instances().await;
    for reg in &registered {
        if !seen_ports.insert(reg.port) {
            continue;
        }
        runners.push(serde_json::json!({
            "id": reg.id,
            "name": reg.name,
            "port": reg.port,
            "is_primary": false,
            "running": true,
            "pid": reg.pid,
            "api_responding": true,
        }));
    }

    Json(serde_json::json!(runners))
}

/// POST /instances/purge-stale — immediately clean up stale/dead instances.
async fn purge_stale_instances(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<serde_json::Value>> {
    let (marked, cleaned) = crate::instance_health::purge_stale_instances(
        &state.app_state.pg_db,
        &state.instance_manager,
    )
    .await;

    Json(ApiResponse::success(serde_json::json!({
        "marked_unhealthy": marked,
        "cleaned_up": cleaned,
    })))
}

// ─── Spawn-placement preview endpoint ───────────────────────────────────────

#[derive(serde::Deserialize)]
struct SpawnPlacementPreviewQuery {
    /// Slot index. 0 = primary, 1.. = configured `runner_instances`
    /// in saved order.
    slot: usize,
    /// Behavior when `slot` is past the end of the list. `wrap` rotates
    /// `slot % count` over slots that have placements; default = 404.
    #[serde(default)]
    overflow: Option<String>,
}

#[derive(serde::Serialize)]
struct SpawnPlacementPreviewResponse {
    global_x: i32,
    global_y: i32,
    width: u32,
    height: u32,
    monitor_label: String,
    slot_index: usize,
    /// Either the resolved instance name or `"primary"`.
    slot_label: String,
    /// Always `"configured"` for now — kept as a discriminator for
    /// future supervisor-side fallback sources.
    source: &'static str,
}

/// `GET /spawn-placement/preview?slot=N[&overflow=wrap|default]`
///
/// Resolve the placement for the given slot against the live monitor
/// list. Slot 0 is the primary (which never has its own placement, so
/// always 404 unless overflow=wrap). Slots 1.. map to the configured
/// `runner_instances` in saved order.
async fn preview_spawn_placement_route(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(q): axum::extract::Query<SpawnPlacementPreviewQuery>,
) -> Result<
    Json<ApiResponse<SpawnPlacementPreviewResponse>>,
    (axum::http::StatusCode, Json<serde_json::Value>),
> {
    let configs = crate::settings::get_runner_instances();
    // Slot 0 is the primary; slots 1.. are configured instances.
    let total_slots = configs.len() + 1;

    let (effective_slot, slot_label, placement) = if q.slot == 0 {
        // Primary has no placement of its own. Allow overflow=wrap to
        // rotate, but the simple "0" call always 404s.
        if q.overflow.as_deref() == Some("wrap") {
            // Find the first configured slot with a placement.
            let placed: Vec<(usize, &crate::settings::RunnerInstanceConfig)> = configs
                .iter()
                .enumerate()
                .filter(|(_, c)| c.spawn_placement.is_some())
                .collect();
            if placed.is_empty() {
                return Err((
                    axum::http::StatusCode::NOT_FOUND,
                    Json(serde_json::json!({
                        "error": "no slots have placements",
                        "slot": q.slot,
                    })),
                ));
            }
            let pick = q.slot % placed.len();
            let (idx, cfg) = placed[pick];
            (idx + 1, cfg.name.clone(), cfg.spawn_placement.clone())
        } else {
            return Err((
                axum::http::StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": "slot has no placement",
                    "slot": q.slot,
                })),
            ));
        }
    } else if q.slot < total_slots {
        // 1-based into configs.
        let cfg_idx = q.slot - 1;
        let cfg = &configs[cfg_idx];
        if cfg.spawn_placement.is_none() {
            // overflow=wrap rotates over placement-having slots even when
            // the requested slot is in-range but un-placed. Without this,
            // a sparse runner_instances list (e.g. slots 1-3 unplaced,
            // slot 5 placed) would 404 every request from slot 1.
            if q.overflow.as_deref() == Some("wrap") {
                let placed: Vec<(usize, &crate::settings::RunnerInstanceConfig)> = configs
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.spawn_placement.is_some())
                    .collect();
                if placed.is_empty() {
                    return Err((
                        axum::http::StatusCode::NOT_FOUND,
                        Json(serde_json::json!({
                            "error": "no slots have placements",
                            "slot": q.slot,
                        })),
                    ));
                }
                let pick = q.slot % placed.len();
                let (idx, cfg) = placed[pick];
                (idx + 1, cfg.name.clone(), cfg.spawn_placement.clone())
            } else {
                return Err((
                    axum::http::StatusCode::NOT_FOUND,
                    Json(serde_json::json!({
                        "error": "slot has no placement",
                        "slot": q.slot,
                    })),
                ));
            }
        } else {
            (q.slot, cfg.name.clone(), cfg.spawn_placement.clone())
        }
    } else {
        // Slot past the end of the list.
        match q.overflow.as_deref() {
            Some("wrap") => {
                // Rotate `q.slot % len` over slots that have placements.
                let placed: Vec<(usize, &crate::settings::RunnerInstanceConfig)> = configs
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.spawn_placement.is_some())
                    .collect();
                if placed.is_empty() {
                    return Err((
                        axum::http::StatusCode::NOT_FOUND,
                        Json(serde_json::json!({
                            "error": "no slots have placements",
                            "slot": q.slot,
                        })),
                    ));
                }
                let pick = q.slot % placed.len();
                let (idx, cfg) = placed[pick];
                (idx + 1, cfg.name.clone(), cfg.spawn_placement.clone())
            }
            _ => {
                return Err((
                    axum::http::StatusCode::NOT_FOUND,
                    Json(serde_json::json!({
                        "error": "slot out of range",
                        "slot": q.slot,
                        "max_slot": total_slots.saturating_sub(1),
                    })),
                ));
            }
        }
    };

    let placement = placement.ok_or_else(|| {
        (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "slot has no placement",
                "slot": q.slot,
            })),
        )
    })?;

    let resolved =
        crate::spawn_placement::resolve_to_global_physical(&state.app_handle, &placement).map_err(
            |e| {
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": format!("failed to resolve placement: {}", e),
                        "slot": q.slot,
                    })),
                )
            },
        )?;

    Ok(Json(ApiResponse::success(SpawnPlacementPreviewResponse {
        global_x: resolved.global_x,
        global_y: resolved.global_y,
        width: resolved.width,
        height: resolved.height,
        monitor_label: resolved.monitor_label,
        slot_index: effective_slot,
        slot_label,
        source: "configured",
    })))
}

// ─── Temp spawn-placement endpoints ─────────────────────────────────────────

#[derive(serde::Serialize)]
struct TempPlacementsListResponse {
    placements: Vec<crate::settings::SpawnPlacement>,
    count: usize,
}

#[derive(serde::Deserialize)]
struct TempPlacementsReplaceRequest {
    placements: Vec<crate::settings::SpawnPlacement>,
}

/// `GET /spawn-placement/temps`
///
/// List the currently-configured temp-runner spawn placements. Used by the
/// supervisor's spawn-test path and the runner's own settings UI.
async fn list_temp_spawn_placements() -> Json<ApiResponse<TempPlacementsListResponse>> {
    let placements = crate::settings::get_temp_spawn_placements();
    let count = placements.len();
    Json(ApiResponse::success(TempPlacementsListResponse {
        placements,
        count,
    }))
}

/// `PUT /spawn-placement/temps`
///
/// Replace the temp-runner spawn placement list. Returns the persisted list
/// in the same shape as `GET /spawn-placement/temps`.
async fn replace_temp_spawn_placements(
    Json(body): Json<TempPlacementsReplaceRequest>,
) -> Result<
    Json<ApiResponse<TempPlacementsListResponse>>,
    (axum::http::StatusCode, Json<serde_json::Value>),
> {
    crate::settings::save_temp_spawn_placements(body.placements.clone()).map_err(|e| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("failed to save temp placements: {}", e),
            })),
        )
    })?;
    let placements = crate::settings::get_temp_spawn_placements();
    let count = placements.len();
    Ok(Json(ApiResponse::success(TempPlacementsListResponse {
        placements,
        count,
    })))
}

#[derive(serde::Deserialize)]
struct TempPlacementLookupQuery {
    /// 0-based index into the temp placement list. Round-robin'd via
    /// `index % len` when `overflow=wrap`.
    index: usize,
    /// Behavior when `index >= len`. `wrap` (default if missing) rotates;
    /// `default` (or anything else) returns 404.
    #[serde(default)]
    overflow: Option<String>,
}

/// `GET /spawn-placement/temp?index=N&overflow=wrap`
///
/// Supervisor-facing lookup. Returns the resolved placement at index N (or
/// `N % len` when overflow=wrap is in effect). 404s when the list is empty
/// or N is out of range and overflow is not "wrap".
async fn lookup_temp_spawn_placement(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(q): axum::extract::Query<TempPlacementLookupQuery>,
) -> Result<
    Json<ApiResponse<SpawnPlacementPreviewResponse>>,
    (axum::http::StatusCode, Json<serde_json::Value>),
> {
    let placements = crate::settings::get_temp_spawn_placements();
    if placements.is_empty() {
        return Err((
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "no temp placements configured",
            })),
        ));
    }

    let len = placements.len();
    let resolved_index = if q.index < len {
        q.index
    } else {
        // index >= len: branch on overflow. Default behavior (overflow
        // missing) is "wrap" per the supervisor contract.
        match q.overflow.as_deref() {
            None | Some("wrap") => q.index % len,
            _ => {
                return Err((
                    axum::http::StatusCode::NOT_FOUND,
                    Json(serde_json::json!({
                        "error": "index out of range",
                        "index": q.index,
                        "max_index": len - 1,
                    })),
                ));
            }
        }
    };

    let placement = placements[resolved_index].clone();
    let resolved =
        crate::spawn_placement::resolve_to_global_physical(&state.app_handle, &placement).map_err(
            |e| {
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": format!("failed to resolve placement: {}", e),
                        "index": q.index,
                    })),
                )
            },
        )?;

    Ok(Json(ApiResponse::success(SpawnPlacementPreviewResponse {
        global_x: resolved.global_x,
        global_y: resolved.global_y,
        width: resolved.width,
        height: resolved.height,
        monitor_label: resolved.monitor_label,
        slot_index: resolved_index,
        slot_label: format!("temp[{}]", resolved_index),
        source: "temp",
    })))
}

pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{delete, get, post};
    axum::Router::new()
        .route("/bridges", get(list_bridges).post(create_bridge))
        .route(
            "/bridges/{bridge_id}",
            get(get_bridge).delete(remove_bridge),
        )
        .route("/bridges/{bridge_id}/workflow", post(run_bridge_workflow))
        .route("/gui-lock", get(get_gui_lock))
        .route(
            "/config/headless-only",
            get(get_headless_only).post(set_headless_only),
        )
        .route("/debug/app/errors", get(get_debug_errors))
        .route("/findings/summary", get(get_findings_summary))
        .route("/launch-debug-chrome", post(launch_debug_chrome))
        .route("/status", get(get_status))
        .route("/runners", get(list_runners))
        .route("/instances", get(get_instances))
        .route("/instances/spawn", post(spawn_instance))
        .route("/instances/register", post(register_instance))
        .route("/instances/purge-stale", post(purge_stale_instances))
        .route("/instances/{id}", delete(delete_instance))
        .route("/instances/{id}/stop", post(stop_instance))
        .route("/instances/{id}/launch", post(launch_instance))
        .route("/instances/{id}/heartbeat", post(instance_heartbeat))
        .route(
            "/spawn-placement/preview",
            get(preview_spawn_placement_route),
        )
        .route(
            "/spawn-placement/temps",
            get(list_temp_spawn_placements).put(replace_temp_spawn_placements),
        )
        .route("/spawn-placement/temp", get(lookup_temp_spawn_placement))
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
