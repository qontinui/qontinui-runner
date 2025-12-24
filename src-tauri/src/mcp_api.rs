//! MCP API Server
//!
//! Provides an HTTP API for the MCP server to communicate with the runner.
//! This allows Claude Code (running in WSL) to control the Windows runner.
//!
//! # Multi-Monitor Coordinate System
//!
//! Windows uses a "virtual desktop" coordinate system where all monitors are combined
//! into one large coordinate space. The primary monitor is usually at (0, 0), and other
//! monitors can have negative coordinates if positioned to the left or above.
//!
//! ## Example 3-Monitor Setup:
//! ```text
//!     Left Monitor        Primary Monitor       Right Monitor
//!     (-1920, 702)        (0, 0)                (3840, 702)
//!     1920x1080           3840x2160             1920x1080
//!
//!     Virtual Desktop Origin: (-1920, 0) - the minimum X and Y across all monitors
//!     Virtual Desktop Size: 7680x2160
//! ```
//!
//! ## Key Insight: FIND vs CLICK Coordinates
//!
//! When the FIND action captures a screenshot, it captures the **entire virtual desktop**
//! (all monitors combined). The coordinates returned by FIND are relative to the
//! **virtual desktop origin** (the minimum X, minimum Y point across all monitors).
//!
//! When a CLICK action targets the FIND result, pyautogui needs **absolute virtual
//! desktop coordinates** to position the mouse correctly.
//!
//! ## The Offset Calculation
//!
//! The `monitor_offset_x` and `monitor_offset_y` values passed to Python represent
//! the **virtual desktop origin** - NOT a specific monitor's position.
//!
//! ```text
//! Example: User clicks on left monitor at FIND result (65, 1372)
//!
//! Virtual desktop origin: (-1920, 0)  ← minimum X and Y across all monitors
//! FIND result (relative to screenshot): (65, 1372)
//! Final absolute coordinates: (65 + -1920, 1372 + 0) = (-1855, 1372)
//!
//! This correctly places the click on the left monitor!
//! ```
//!
//! ## Common Pitfall (Fixed)
//!
//! Previously, the code incorrectly used the **specific monitor's position** as the offset.
//! For the left monitor at (-1920, 702), this added 702 to the Y coordinate, causing clicks
//! to land on the wrong monitor (702 pixels too low).
//!
//! The fix: Always calculate the virtual desktop origin (min X, min Y across all monitors)
//! regardless of which monitor is specified, because FIND always captures the full virtual desktop.

use crate::debug_lifecycle;
use crate::safe_eprintln;
use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info, warn};

use crate::commands::rag::RAGState;
use crate::commands::AppState;
use crate::config::ConfigLoader;
use crate::rag::{ImportResult, QontinuiConfig, RAGConfigSummary};
use crate::session_manager::SessionManager;
use crate::settings;
// WorkflowManager import removed - using unified SessionManager instead
use axum::routing::{delete, put};
use tauri::{Emitter, Manager};

// Windows-specific imports for process creation flags
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

// Windows constants for process creation
#[cfg(target_os = "windows")]
const CREATE_NEW_CONSOLE: u32 = 0x00000010;
#[cfg(target_os = "windows")]
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x01000000;

/// Spawn Python script with proper console on Windows.
/// Claude CLI requires a console window to function properly.
fn spawn_python_with_console(
    python_path: &str,
    args: &[&std::ffi::OsStr],
    working_dir: &std::path::Path,
) -> std::io::Result<std::process::Child> {
    let mut cmd = std::process::Command::new(python_path);
    cmd.args(args).current_dir(working_dir);

    #[cfg(target_os = "windows")]
    {
        // CREATE_NEW_CONSOLE: Creates a new console window (required for Claude CLI)
        // Note: CREATE_BREAKAWAY_FROM_JOB requires special permissions so we don't use it here.
        // The Python spawn script handles job breakaway internally via subprocess.Popen flags.
        cmd.creation_flags(CREATE_NEW_CONSOLE);
    }

    cmd.spawn()
}

/// Run a Claude CLI session inline (as a child process) and wait for completion.
/// Returns the session output when complete.
///
/// This is the new "in-runner" execution model that replaces independent process spawning.
/// Claude runs as a child process, we wait for completion, then check checkpoint to continue.
fn run_claude_session_inline(
    working_dir: &str,
    prompt: &str,
    session_id: &str,
    app_handle: &tauri::AppHandle,
    timeout_seconds: u64,
) -> Result<(bool, String), String> {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::{Duration, Instant};

    info!(
        "Running Claude session inline: {} (timeout: {}s)",
        session_id, timeout_seconds
    );

    // Write prompt to temp file
    let temp_dir = std::env::temp_dir();
    let prompt_file = temp_dir.join(format!("claude_session_{}.txt", session_id));
    std::fs::write(&prompt_file, prompt)
        .map_err(|e| format!("Failed to write prompt file: {}", e))?;

    let prompt_content =
        std::fs::read(&prompt_file).map_err(|e| format!("Failed to read prompt file: {}", e))?;

    // Spawn Claude CLI with stream-json output
    let mut child = std::process::Command::new("cmd.exe")
        .args([
            "/c",
            "claude",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-mode",
            "bypassPermissions",
        ])
        .current_dir(working_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn Claude CLI: {}", e))?;

    // Write prompt to stdin
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&prompt_content)
            .map_err(|e| format!("Failed to write to Claude stdin: {}", e))?;
    }

    // Track activity for timeout
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let last_activity = Arc::new(AtomicU64::new(now_secs));
    let last_activity_stdout = last_activity.clone();

    let has_output = Arc::new(AtomicBool::new(false));
    let has_output_heartbeat = has_output.clone();

    // Heartbeat thread
    let app_handle_heartbeat = app_handle.clone();
    let session_id_heartbeat = session_id.to_string();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let start_time = Instant::now();

    let heartbeat_handle = thread::spawn(move || {
        let mut last_update = 0u64;
        loop {
            if stop_rx.try_recv().is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(100));

            if has_output_heartbeat.load(Ordering::Relaxed) {
                continue;
            }

            let elapsed_secs = start_time.elapsed().as_secs();
            if elapsed_secs > 0 && elapsed_secs % 30 == 0 && elapsed_secs != last_update {
                last_update = elapsed_secs;
                let mins = elapsed_secs / 60;
                let secs = elapsed_secs % 60;
                let msg = if mins > 0 {
                    format!(
                        "⏳ Session {} processing... ({}m {}s)",
                        session_id_heartbeat, mins, secs
                    )
                } else {
                    format!(
                        "⏳ Session {} processing... ({}s)",
                        session_id_heartbeat, secs
                    )
                };
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    emit_ai_output(&app_handle_heartbeat, &msg, "status", None);
                }));
            }
        }
    });

    // Stdout reader thread
    let stdout = child.stdout.take();
    let app_handle_stdout = app_handle.clone();
    let has_output_stdout = has_output.clone();

    let stdout_handle = thread::spawn(move || {
        let mut all_text = String::new();
        if let Some(stdout) = stdout {
            let reader = BufReader::new(stdout);
            for line_result in reader.lines() {
                if let Ok(line) = line_result {
                    // Update activity time
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    last_activity_stdout.store(now, Ordering::Relaxed);

                    // Extract text from JSON
                    if let Some(text) = extract_text_from_stream_json(&line) {
                        has_output_stdout.store(true, Ordering::Relaxed);
                        if !text.is_empty() {
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                emit_ai_output(&app_handle_stdout, &text, "claude", None);
                            }));
                            all_text.push_str(&text);
                        }
                    }
                }
            }
        }
        all_text
    });

    // Stderr reader thread
    let stderr = child.stderr.take();
    let stderr_handle = thread::spawn(move || {
        let mut output = String::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_string(&mut output);
        }
        output
    });

    // Wait for process with inactivity timeout
    let status = loop {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let last_activity_secs = last_activity.load(Ordering::Relaxed);
        let inactive_secs = now_secs.saturating_sub(last_activity_secs);

        if inactive_secs > timeout_seconds {
            warn!(
                "Session {} timed out after {}s of inactivity",
                session_id, inactive_secs
            );
            let _ = child.kill();
            thread::sleep(Duration::from_millis(500));
            let _ = child.try_wait();
            let _ = stop_tx.send(());
            let _ = heartbeat_handle.join();
            let _ = std::fs::remove_file(&prompt_file);
            return Err(format!(
                "Session timed out after {}s of inactivity",
                inactive_secs
            ));
        }

        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(e) => {
                let _ = stop_tx.send(());
                let _ = heartbeat_handle.join();
                let _ = std::fs::remove_file(&prompt_file);
                return Err(format!("Failed to wait for Claude: {}", e));
            }
        }
    };

    // Cleanup
    let _ = stop_tx.send(());
    let _ = heartbeat_handle.join();
    let all_output = stdout_handle.join().unwrap_or_default();
    let stderr_output = stderr_handle.join().unwrap_or_default();
    let _ = std::fs::remove_file(&prompt_file);

    // Emit stderr if any
    if !stderr_output.is_empty() {
        for line in stderr_output.lines() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                emit_ai_output(app_handle, &format!("[stderr] {}", line), "claude", None);
            }));
        }
    }

    let success = status.success();
    info!(
        "Session {} completed: success={}, output_len={}",
        session_id,
        success,
        all_output.len()
    );

    Ok((success, all_output))
}

/// Read the improve-all state file to check workflow progress
fn read_improve_all_state(dev_logs_path: &std::path::Path) -> Option<serde_json::Value> {
    let state_file = dev_logs_path.join("improve-all-state.json");
    if !state_file.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&state_file).ok()?;
    serde_json::from_str(&content).ok()
}

/// Check if the improve-all workflow is complete based on state file
fn is_improve_all_complete(state: &serde_json::Value) -> bool {
    // Check status field
    if let Some(status) = state.get("status").and_then(|v| v.as_str()) {
        if status == "completed" {
            return true;
        }
    }

    // Check current_part - if > 5, workflow is complete
    if let Some(current_part) = state.get("current_part").and_then(|v| v.as_u64()) {
        if current_part > 5 {
            return true;
        }
    }

    false
}

/// Get the continuation prompt for the next part of improve-all workflow
fn get_improve_all_continuation_prompt(
    state: &serde_json::Value,
    scripts_path: &std::path::Path,
) -> Option<String> {
    let current_part = state.get("current_part").and_then(|v| v.as_u64())? as u32;

    if current_part > 5 {
        return None; // Workflow complete
    }

    // Part definitions matching improve-state.py
    let (name, phases, command) = match current_part {
        1 => ("Setup & Audit", "0-2", "/improve-part-1"),
        2 => ("Security & Architecture", "3-4", "/improve-part-2"),
        3 => ("Code Quality & Types", "5-6", "/improve-part-3"),
        4 => ("TODOs & Features", "7-8", "/improve-part-4"),
        5 => ("Final", "9-12", "/improve-part-5"),
        _ => return None,
    };

    let state_file = scripts_path
        .parent()?
        .parent()?
        .join(".dev-logs")
        .join("improve-all-state.json");
    let improve_state_script = scripts_path.join("improve-state.py");

    Some(format!(
        r#"{}

Continue the improve-all sequential workflow.

This is Part {}/5: {}
Phases: {}

Read the state file first to understand progress:
Get-Content {}

Work autonomously. Do not ask for user input.
When done, run: python {} complete-part {}
"#,
        command,
        current_part,
        name,
        phases,
        state_file.display(),
        improve_state_script.display(),
        current_part
    ))
}

/// Default port for the MCP API server
pub const MCP_API_PORT: u16 = 9876;

/// Shared state for the API server
pub struct ApiState {
    pub app_state: Arc<AppState>,
    pub rag_state: Arc<RAGState>,
    pub app_handle: tauri::AppHandle,
    /// Tracks whether an AI analysis is currently in progress
    pub ai_analysis_running: AtomicBool,
    /// Flag to request stopping the current AI analysis
    pub ai_analysis_stop_requested: AtomicBool,
    /// Unified session manager for all AI sessions (Prompt Library + AI Builder)
    pub session_manager: Arc<SessionManager>,
    /// DEPRECATED: Old AI Developer manager - kept for HTTP handler compatibility
    /// TODO: Remove along with deprecated HTTP handlers
    #[allow(dead_code)]
    pub ai_developer_manager: AiDeveloperManager,
}

// ============================================================================
// DEPRECATED: AI Developer types - kept for compatibility with old HTTP handlers
// TODO: Remove these along with the deprecated HTTP handlers in next cleanup
// ============================================================================

/// Manages AI Developer sessions with iteration support (DEPRECATED)
#[allow(dead_code)]
pub struct AiDeveloperManager {
    sessions: Arc<tokio::sync::RwLock<std::collections::HashMap<String, AiDeveloperSession>>>,
    gui_lock_holder: Arc<tokio::sync::RwLock<Option<String>>>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiDeveloperSession {
    pub session_id: String,
    pub status: AiDeveloperStatus,
    pub iteration: u32,
    pub max_iterations: u32,
    pub uses_gui_automation: bool,
    pub started_at: String,
    pub last_activity: String,
    pub stop_requested: bool,
    pub state_file: String,
    pub log_file: String,
    pub prompt: String,
    pub continuation_prompt: Option<String>,
    pub errors_fixed: Vec<String>,
    pub errors_remaining: Vec<String>,
    pub activity_log: Vec<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AiDeveloperStatus {
    Starting,
    Running,
    WaitingForNextIteration,
    Completed,
    Failed,
    Stopped,
}

#[allow(dead_code)]
impl AiDeveloperManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            gui_lock_holder: Arc::new(tokio::sync::RwLock::new(None)),
        }
    }
    pub async fn add_session(&self, session: AiDeveloperSession) {
        self.sessions.write().await.insert(session.session_id.clone(), session);
    }
    pub async fn get_session(&self, session_id: &str) -> Option<AiDeveloperSession> {
        self.sessions.read().await.get(session_id).cloned()
    }
    pub async fn update_session(&self, session: AiDeveloperSession) {
        self.sessions.write().await.insert(session.session_id.clone(), session);
    }
    pub async fn remove_session(&self, session_id: &str) -> Option<AiDeveloperSession> {
        self.sessions.write().await.remove(session_id)
    }
    pub async fn get_all_sessions(&self) -> Vec<AiDeveloperSession> {
        self.sessions.read().await.values().cloned().collect()
    }
    pub async fn acquire_gui_lock(&self, session_id: &str) -> Result<(), String> {
        let mut lock = self.gui_lock_holder.write().await;
        if let Some(holder) = &*lock {
            if holder != session_id {
                return Err(format!("GUI lock held by {}", holder));
            }
        }
        *lock = Some(session_id.to_string());
        Ok(())
    }
    pub async fn release_gui_lock(&self, session_id: &str) {
        let mut lock = self.gui_lock_holder.write().await;
        if let Some(holder) = &*lock {
            if holder == session_id {
                *lock = None;
            }
        }
    }
    pub async fn gui_lock_holder(&self) -> Option<String> {
        self.gui_lock_holder.read().await.clone()
    }
}

impl Default for AiDeveloperManager {
    fn default() -> Self { Self::new() }
}

// ============================================================================
// End of deprecated AI Developer types
// ============================================================================

/// Response for API endpoints
#[derive(Debug, Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }
}

/// Create an error response
fn api_error(message: impl Into<String>) -> ApiResponse<()> {
    ApiResponse {
        success: false,
        data: None,
        error: Some(message.into()),
    }
}

/// Status response
#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub executor_running: bool,
    pub executor_state: String,
    pub config_loaded: bool,
    pub config_path: Option<String>,
    /// Whether an AI analysis is currently in progress
    pub ai_analysis_running: bool,
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

fn default_timeout() -> u64 {
    300 // 5 minutes default timeout
}

/// Execution result
#[derive(Debug, Serialize)]
pub struct ExecutionResult {
    pub success: bool,
    pub workflow_name: String,
    pub error: Option<String>,
}

/// Monitor info for the API response
#[derive(Debug, Serialize)]
pub struct MonitorInfoResponse {
    pub index: usize,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
    pub position: String,
    pub name: String,
    pub description: String,
}

/// Monitors response
#[derive(Debug, Serialize)]
pub struct MonitorsResponse {
    pub count: usize,
    pub monitors: Vec<MonitorInfoResponse>,
    pub available_descriptors: Vec<String>,
}

/// Health check endpoint
async fn health() -> Json<ApiResponse<String>> {
    Json(ApiResponse::success("ok".to_string()))
}

/// Launch Chrome with remote debugging enabled
async fn launch_debug_chrome() -> Json<ApiResponse<String>> {
    use std::process::Command;

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
            let _ = Command::new("taskkill")
                .args(["/F", "/IM", "chrome.exe"])
                .output();

            // Wait a moment for processes to terminate
            std::thread::sleep(std::time::Duration::from_millis(1000));

            // Now launch Chrome with debug flag and separate profile
            // Using a separate user-data-dir ensures the debug port works
            // even if Chrome would normally restore a previous session
            match Command::new(path)
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

/// Get available monitors with position information
async fn get_monitors(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<MonitorsResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_handle = state.app_handle.clone();

    let window = app_handle.get_webview_window("main").ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error("Failed to get main window")),
        )
    })?;

    let monitors = window.available_monitors().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get monitors: {}", e))),
        )
    })?;

    let primary_monitor = window.current_monitor().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get current monitor: {}", e))),
        )
    })?;

    // Build monitor info with positions
    let mut monitor_infos: Vec<MonitorInfoResponse> = monitors
        .iter()
        .enumerate()
        .map(|(idx, monitor)| {
            let mon_position = monitor.position();
            let mon_size = monitor.size();
            let is_primary = match &primary_monitor {
                Some(current) => {
                    let current_pos = current.position();
                    let current_size = current.size();
                    mon_position.x == current_pos.x
                        && mon_position.y == current_pos.y
                        && mon_size.width == current_size.width
                        && mon_size.height == current_size.height
                }
                None => idx == 0,
            };

            MonitorInfoResponse {
                index: idx,
                x: mon_position.x,
                y: mon_position.y,
                width: mon_size.width,
                height: mon_size.height,
                is_primary,
                position: String::new(), // Will be filled in below
                name: format!("Monitor {}", idx),
                description: String::new(), // Will be filled in below
            }
        })
        .collect();

    // Sort monitors by x position to determine left/middle/right
    let mut sorted_by_x: Vec<(usize, i32)> = monitor_infos.iter().map(|m| (m.index, m.x)).collect();
    sorted_by_x.sort_by_key(|&(_, x)| x);

    // Assign positions based on x-coordinate order
    for (order, (idx, _)) in sorted_by_x.iter().enumerate() {
        if let Some(monitor) = monitor_infos.iter_mut().find(|m| m.index == *idx) {
            monitor.position = match (order, sorted_by_x.len()) {
                (0, 1) => "primary".to_string(),
                (0, _) => "left".to_string(),
                (o, len) if o == len - 1 => "right".to_string(),
                _ => "middle".to_string(),
            };

            let mut desc_parts = vec![format!("Monitor {}", monitor.index)];
            if monitor.is_primary {
                desc_parts.push("primary".to_string());
            }
            desc_parts.push(monitor.position.clone());
            desc_parts.push(format!("{}x{}", monitor.width, monitor.height));
            monitor.description = format!("{} ({})", desc_parts[0], desc_parts[1..].join(", "));
        }
    }

    // Build available descriptors
    let mut descriptors = vec!["primary".to_string()];
    for m in &monitor_infos {
        if !descriptors.contains(&m.position) {
            descriptors.push(m.position.clone());
        }
    }
    for m in &monitor_infos {
        descriptors.push(m.index.to_string());
    }

    Ok(Json(ApiResponse::success(MonitorsResponse {
        count: monitor_infos.len(),
        monitors: monitor_infos,
        available_descriptors: descriptors,
    })))
}

/// Get executor status
async fn get_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<StatusResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Clone Arc for use in spawn_blocking
    let app_state = state.app_state.clone();

    // Run blocking operations in a separate thread to avoid blocking the async runtime
    let result = tokio::task::spawn_blocking(move || {
        // Use unwrap_or_else to recover from poisoned mutex (after a panic)
        let bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        let (executor_running, executor_state) = if let Some(ref bridge) = *bridge_lock {
            (bridge.is_running(), bridge.get_state().name().to_string())
        } else {
            (false, "not_started".to_string())
        };
        drop(bridge_lock);

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

    Ok(Json(ApiResponse::success(StatusResponse {
        executor_running: result.0,
        executor_state: result.1,
        config_loaded: result.2,
        config_path: result.3,
        ai_analysis_running: state.ai_analysis_running.load(Ordering::SeqCst),
    })))
}

/// Load a configuration file
///
/// This mirrors the behavior from commands/config.rs:
/// 1. Loads and validates the configuration file
/// 2. Stores it in the app state (current_config)
/// 3. Saves the path for auto-load functionality
/// 4. Sends debug settings to the Python executor
/// 5. Sends the configuration to the Python executor
async fn load_config(
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

        // Create config data for event emission
        let config_data = serde_json::json!({
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
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
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
            Ok((summary, config_data))
        } else {
            Err("Python executor not initialized".to_string())
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
        Ok((summary, config_data)) => {
            info!("MCP API: Config loaded successfully");

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
async fn run_workflow(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RunWorkflowRequest>,
) -> Result<Json<ApiResponse<ExecutionResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Running workflow: {} (timeout: {}s)",
        request.workflow_name, request.timeout_seconds
    );
    safe_eprintln!(
        "[MCP_API] run_workflow received: workflow={}, monitor_index={:?}",
        request.workflow_name,
        request.monitor_index
    );

    let app_state = state.app_state.clone();
    let workflow_name = request.workflow_name.clone();
    let monitor_index = request.monitor_index;
    let timeout_duration = Duration::from_secs(request.timeout_seconds);

    info!("MCP API: Step 1 - Getting lifecycle from bridge");

    // First, get the lifecycle Arc from the bridge
    // We need to use spawn_blocking here because bridge.is_running() uses block_on internally
    let app_state_clone = app_state.clone();
    let lifecycle = tokio::task::spawn_blocking(move || {
        let bridge_lock = app_state_clone
            .python_bridge
            .lock()
            .unwrap_or_else(|poisoned| {
                warn!("MCP API: python_bridge mutex was poisoned, recovering");
                poisoned.into_inner()
            });

        if let Some(ref bridge) = *bridge_lock {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            Ok(bridge.get_lifecycle())
        } else {
            Err("Python executor not initialized".to_string())
        }
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error getting lifecycle: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?
    .map_err(|e| {
        error!("MCP API: Bridge error: {}", e);
        (StatusCode::BAD_REQUEST, Json(api_error(e)))
    })?;

    info!("MCP API: Step 2 - Got lifecycle, registering for completion");

    // Register for completion notification - we're already in an async context,
    // so we can just await directly instead of using block_on
    let lifecycle_guard = lifecycle.read().await;
    info!("MCP API: Step 3 - Got read lock on lifecycle");
    let completion_rx = lifecycle_guard.register_execution_completion().await;
    drop(lifecycle_guard);
    info!("MCP API: Step 4 - Registered for completion, dropped read lock");

    // ==========================================================================
    // MONITOR SELECTION - Passed to Python, offset calculated by qontinui library
    // ==========================================================================
    //
    // The qontinui Python library handles monitor offset calculation internally
    // using MSS (the same library used for screen capture). This ensures the
    // coordinate system is consistent between screenshot capture and click actions.
    //
    // The runner only passes the monitor_index to Python. The library's
    // StateExecutor.set_monitor() method looks up the monitor position via MSS.
    // ==========================================================================

    // Start the workflow execution - use spawn_blocking because send_command uses block_on internally
    // which cannot be called from within an async context
    let start_result = tokio::task::spawn_blocking(move || {
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
            // Build params for execution - only pass monitor_index, Python looks up offset via MSS
            let mut params = serde_json::Map::new();
            let resolved_monitor = monitor_index.unwrap_or(0);
            safe_eprintln!(
                "[MCP_API] Building params: monitor_index={:?}, resolved to {}",
                monitor_index,
                resolved_monitor
            );
            params.insert(
                "monitor_index".to_string(),
                serde_json::json!(resolved_monitor),
            );
            params.insert(
                "workflow".to_string(),
                serde_json::json!(workflow_name.clone()),
            );
            safe_eprintln!("[MCP_API] Sending to Python: {:?}", params);

            match bridge.start_execution_with_params(Some(serde_json::Value::Object(params))) {
                Ok(_) => Ok(()),
                Err(e) => Err(format!("Failed to start workflow: {}", e)),
            }
        } else {
            Err("Python executor not initialized".to_string())
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

    if let Err(e) = start_result {
        error!("MCP API: Failed to start workflow: {}", e);
        return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))));
    }

    info!("MCP API: Workflow started, waiting for completion...");

    // Wait for execution completion with timeout
    match timeout(timeout_duration, completion_rx).await {
        Ok(Ok(completion_result)) => {
            info!(
                "MCP API: Workflow completed: success={}, error={:?}",
                completion_result.success, completion_result.error
            );

            Ok(Json(ApiResponse::success(ExecutionResult {
                success: completion_result.success,
                workflow_name: request.workflow_name,
                error: completion_result.error,
            })))
        }
        Ok(Err(_)) => {
            // Channel was closed without sending - this shouldn't normally happen
            warn!("MCP API: Completion channel closed unexpectedly");
            Ok(Json(ApiResponse::success(ExecutionResult {
                success: true,
                workflow_name: request.workflow_name,
                error: Some("Completion channel closed unexpectedly".to_string()),
            })))
        }
        Err(_) => {
            error!(
                "MCP API: Workflow execution timed out after {}s",
                request.timeout_seconds
            );
            Err((
                StatusCode::REQUEST_TIMEOUT,
                Json(api_error(format!(
                    "Workflow execution timed out after {} seconds",
                    request.timeout_seconds
                ))),
            ))
        }
    }
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
async fn load_last_config(
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

        // Create config data for event emission
        let config_data = serde_json::json!({
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
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
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
            Ok((summary, config_data))
        } else {
            Err("Python executor not initialized".to_string())
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
async fn stop_execution(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stopping execution");

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
            match bridge.stop_execution() {
                Ok(_) => Ok(()),
                Err(e) => Err(format!("Failed to stop execution: {}", e)),
            }
        } else {
            Err("Python executor not initialized".to_string())
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

// ============================================================================
// RAG API Endpoints
// ============================================================================

/// Request to import a RAG configuration
///
/// Accepts the full QontinuiConfig format directly from the frontend.
/// The runner extracts what it needs (images, states, patterns) internally.
/// This eliminates the need for frontend transformation code.
#[derive(Debug, Deserialize)]
pub struct ImportRAGRequest {
    /// Full QontinuiConfig - the canonical format from TypeScript/Python
    pub config: QontinuiConfig,
    /// Optional project_id override (defaults to derived from metadata.name)
    #[serde(default)]
    pub project_id: Option<String>,
}

/// Import a RAG configuration
///
/// Accepts the full QontinuiConfig format directly from the frontend.
/// Saves the complete config and extracts images for embedding generation.
async fn import_rag(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ImportRAGRequest>,
) -> Result<Json<ApiResponse<ImportResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Use provided project_id or derive from metadata.name
    let project_id = request
        .project_id
        .clone()
        .unwrap_or_else(|| request.config.project_id());

    let image_count = request.config.images.len();
    let state_image_count = request.config.state_image_count();
    let pattern_count = request.config.pattern_count();

    info!(
        "MCP API: Importing QontinuiConfig: project_id={}, name={}, images={}, states={}, stateImages={}, patterns={}",
        project_id,
        request.config.metadata.name,
        image_count,
        request.config.states.len(),
        state_image_count,
        pattern_count
    );

    // Save the full QontinuiConfig
    let storage = state.rag_state.storage.lock().await;
    let storage_path = storage
        .save_qontinui_config(&project_id, &request.config)
        .map_err(|e| {
            error!("Failed to save QontinuiConfig: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to save config: {}", e))),
            )
        })?;

    // Extract and save images from config.images[]
    // Only save images that are referenced by patterns
    let referenced_ids = request.config.referenced_image_ids();
    let saved_image_count = storage
        .save_images_from_config(&project_id, &request.config.images, &referenced_ids)
        .map_err(|e| {
            error!("Failed to save images: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to save images: {}", e))),
            )
        })?;

    let storage_path_str = storage_path.to_string_lossy().to_string();
    drop(storage);

    // Trigger embedding generation in background
    info!(
        "MCP API: Starting background embedding generation for project_id={}",
        project_id
    );
    let embedding_generator = state.rag_state.embedding_generator.lock().await;
    let _progress_rx = embedding_generator.generate_embeddings_async(project_id.clone());
    drop(embedding_generator);

    let result = ImportResult {
        success: true,
        project_id: project_id.clone(),
        message: format!(
            "Successfully imported QontinuiConfig '{}' with {} images ({} saved for RAG) and {} patterns. Embedding generation started.",
            request.config.metadata.name, image_count, saved_image_count, pattern_count
        ),
        screenshot_count: saved_image_count,
        element_count: pattern_count,
        storage_path: storage_path_str,
    };

    Ok(Json(ApiResponse::success(result)))
}

/// List all RAG configurations
async fn list_rag_configs(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<RAGConfigSummary>>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Listing RAG configurations");

    let storage = state.rag_state.storage.lock().await;
    let summaries = storage.list_configs().map_err(|e| {
        error!("Failed to list RAG configs: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to list configs: {}", e))),
        )
    })?;

    info!("MCP API: Found {} RAG configurations", summaries.len());

    Ok(Json(ApiResponse::success(summaries)))
}

/// Get RAG embedding status for a project
async fn get_rag_status(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(project_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Getting RAG status for project_id={}", project_id);

    let embedding_generator = state.rag_state.embedding_generator.lock().await;

    // Get progress from state if available (includes in-progress tracking)
    if let Some(progress) = embedding_generator.get_progress(&project_id) {
        let status_str = match &progress.status {
            crate::rag::EmbeddingStatus::NotStarted => "not_started",
            crate::rag::EmbeddingStatus::InProgress(_) => "in_progress",
            crate::rag::EmbeddingStatus::Completed => "completed",
            crate::rag::EmbeddingStatus::Failed(_) => "failed",
        };

        let mut data = serde_json::json!({
            "status": status_str,
            "message": progress.message,
        });

        // Add optional fields if present
        if let Some(percent) = progress.percent {
            data["percent"] = serde_json::json!(percent);
        }
        if let Some(elements_processed) = progress.elements_processed {
            data["elements_processed"] = serde_json::json!(elements_processed);
        }
        if let Some(total_elements) = progress.total_elements {
            data["total_elements"] = serde_json::json!(total_elements);
        }

        return Ok(Json(ApiResponse::success(data)));
    }

    // Fallback to file-based check (for completed/not started)
    let status = embedding_generator.check_status(&project_id);

    let status_str = match &status {
        crate::rag::EmbeddingStatus::NotStarted => "not_started",
        crate::rag::EmbeddingStatus::InProgress(pct) => {
            return Ok(Json(ApiResponse::success(serde_json::json!({
                "status": "in_progress",
                "percent": pct
            }))));
        }
        crate::rag::EmbeddingStatus::Completed => "completed",
        crate::rag::EmbeddingStatus::Failed(_) => "failed",
    };

    let message = match &status {
        crate::rag::EmbeddingStatus::Failed(err) => {
            Some(format!("Embedding generation failed: {}", err))
        }
        crate::rag::EmbeddingStatus::Completed => {
            Some("Embeddings generated successfully".to_string())
        }
        _ => None,
    };

    let mut data = serde_json::json!({
        "status": status_str
    });
    if let Some(msg) = message {
        data["message"] = serde_json::json!(msg);
    }

    Ok(Json(ApiResponse::success(data)))
}

/// Delete a RAG configuration
async fn delete_rag_config(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(project_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Deleting RAG config for project_id={}", project_id);

    let storage = state.rag_state.storage.lock().await;
    storage.delete_config(&project_id).map_err(|e| {
        error!("Failed to delete RAG config: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to delete config: {}", e))),
        )
    })?;

    info!(
        "MCP API: Successfully deleted RAG config for project_id={}",
        project_id
    );

    Ok(Json(ApiResponse::success(format!(
        "Successfully deleted RAG config: {}",
        project_id
    ))))
}

/// Load a RAG project into the executor
async fn load_rag_project(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(project_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Loading RAG project for project_id={}", project_id);

    // Try to load as QontinuiConfig first
    let storage = state.rag_state.storage.lock().await;

    if let Ok(config) = storage.load_qontinui_config(&project_id) {
        drop(storage);

        // TODO: Load into executor if needed
        return Ok(Json(ApiResponse::success(serde_json::json!({
            "project_id": project_id,
            "name": config.metadata.name,
            "states": config.states.len(),
            "patterns": config.pattern_count(),
            "loaded": true
        }))));
    }

    // Try legacy format
    if let Ok(config) = storage.load_config(&project_id) {
        drop(storage);

        return Ok(Json(ApiResponse::success(serde_json::json!({
            "project_id": project_id,
            "name": config.project_name,
            "screenshots": config.screenshots.len(),
            "elements": config.total_element_count(),
            "loaded": true
        }))));
    }

    Err((
        StatusCode::NOT_FOUND,
        Json(api_error(format!("Project not found: {}", project_id))),
    ))
}

/// Get RAG availability (ML models status)
async fn get_rag_availability(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Checking RAG availability");

    // TODO: Actually check ML model availability
    // For now, return a placeholder response
    Ok(Json(ApiResponse::success(serde_json::json!({
        "available": true,
        "models": {
            "clip": true,
            "text": true,
            "ocr": true,
            "sam": false
        }
    }))))
}

/// Request to segment a screenshot using SAM3
#[derive(Debug, Deserialize)]
pub struct SegmentScreenshotRequest {
    /// Base64-encoded screenshot image (PNG or JPEG)
    pub screenshot_base64: String,
    /// Optional minimum segment area in pixels
    #[serde(default)]
    pub min_area: Option<i32>,
    /// Optional SAM model to use (e.g., "sam2_hiera_tiny")
    #[serde(default)]
    pub model: Option<String>,
}

/// Segment in the response
#[derive(Debug, Serialize)]
pub struct SegmentInfo {
    /// Unique segment ID
    pub id: String,
    /// Bounding box [x, y, width, height]
    pub bbox: Vec<i32>,
    /// Segment area in pixels
    pub area: i32,
    /// Base64-encoded cropped image of the segment
    pub image_base64: Option<String>,
}

/// Response from screenshot segmentation
#[derive(Debug, Serialize)]
pub struct SegmentScreenshotResponse {
    /// Whether segmentation was successful
    pub success: bool,
    /// List of detected segments
    pub segments: Vec<SegmentInfo>,
    /// Error message if failed
    pub error: Option<String>,
    /// Processing time in milliseconds
    pub processing_time_ms: Option<i64>,
}

/// Segment a screenshot using SAM3 (Segment Anything Model 3)
///
/// This endpoint receives a base64-encoded screenshot and returns
/// the detected segments with their bounding boxes and cropped images.
/// SAM3 runs locally on the user's machine via the Python executor.
async fn segment_screenshot(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<SegmentScreenshotRequest>,
) -> Result<Json<ApiResponse<SegmentScreenshotResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Segmenting screenshot ({} bytes base64)",
        request.screenshot_base64.len()
    );

    let start_time = std::time::Instant::now();
    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
        "screenshot_base64": request.screenshot_base64,
        "min_area": request.min_area,
        "model": request.model,
    });

    // Use spawn_blocking for the synchronous bridge operation
    let result = tokio::task::spawn_blocking(move || {
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Send command and wait for response (2 minute timeout for SAM3 processing)
            let timeout_duration = std::time::Duration::from_secs(120);
            bridge.send_command_and_wait("segment_screenshot", Some(params), timeout_duration)
        } else {
            Err("Python executor not initialized".to_string())
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

    let elapsed = start_time.elapsed();

    match result {
        Ok(response) => {
            if response.success {
                // Parse segments from response data
                let segments: Vec<SegmentInfo> = if let Some(data) = response.data {
                    if let Some(segments_arr) = data.get("segments").and_then(|s| s.as_array()) {
                        segments_arr
                            .iter()
                            .filter_map(|seg| {
                                let id = seg.get("id")?.as_str()?.to_string();
                                let bbox = seg.get("bbox")?.as_array()?;
                                let bbox_vec: Vec<i32> = bbox
                                    .iter()
                                    .filter_map(|v| v.as_i64().map(|n| n as i32))
                                    .collect();
                                if bbox_vec.len() != 4 {
                                    return None;
                                }
                                let area =
                                    seg.get("area").and_then(|a| a.as_i64()).unwrap_or(0) as i32;
                                let image_base64 = seg
                                    .get("image_base64")
                                    .and_then(|i| i.as_str())
                                    .map(|s| s.to_string());

                                Some(SegmentInfo {
                                    id,
                                    bbox: bbox_vec,
                                    area,
                                    image_base64,
                                })
                            })
                            .collect()
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                };

                info!(
                    "MCP API: Segmentation completed with {} segments in {}ms",
                    segments.len(),
                    elapsed.as_millis()
                );

                Ok(Json(ApiResponse::success(SegmentScreenshotResponse {
                    success: true,
                    segments,
                    error: None,
                    processing_time_ms: Some(elapsed.as_millis() as i64),
                })))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Segmentation failed".to_string());
                error!("MCP API: Segmentation failed: {}", error_msg);

                Ok(Json(ApiResponse::success(SegmentScreenshotResponse {
                    success: false,
                    segments: Vec::new(),
                    error: Some(error_msg),
                    processing_time_ms: Some(elapsed.as_millis() as i64),
                })))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to segment screenshot: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}
// ============================================================================
// AI Analysis Trigger Endpoint
// ============================================================================

use crate::commands::ai_settings;
use crate::settings::{AiProvider, CliExecutionMode};

/// Request to trigger AI analysis
#[derive(Debug, Deserialize)]
pub struct TriggerAiAnalysisRequest {
    /// The prompt to send to Claude (may include conversation history)
    pub prompt: String,
    /// The prompt to display in the UI (just the new message, no history)
    /// If not provided, falls back to the full prompt
    #[serde(default)]
    pub display_prompt: Option<String>,
    /// Timeout in seconds (optional - uses settings if not provided)
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

/// Response from AI analysis trigger
#[derive(Debug, Serialize)]
pub struct TriggerAiAnalysisResponse {
    pub success: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The full output from Claude (if captured synchronously)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
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

// ============================================================================
// AI Developer (Persistent Mode) Request/Response Types
// ============================================================================

/// Execution mode for AI Developer sessions
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AiDeveloperExecutionMode {
    /// Independent process mode (legacy) - spawns Claude as a separate process
    Independent,
    /// In-runner mode - runs Claude as a child process, waits for completion
    /// Uses checkpoint-based continuation for multi-session workflows
    InRunner,
}

impl Default for AiDeveloperExecutionMode {
    fn default() -> Self {
        // Default to in-runner mode (the new approach)
        Self::InRunner
    }
}

/// Request to spawn an AI Developer session (persistent mode)
#[derive(Debug, Deserialize)]
pub struct SpawnAiDeveloperRequest {
    /// The prompt to send to Claude
    pub prompt: String,
    /// Unique identifier for this session (generated by caller or auto-generated)
    #[serde(default)]
    pub session_id: Option<String>,
    /// Maximum number of iterations (default: 10)
    #[serde(default)]
    pub max_iterations: Option<u32>,
    /// Whether this session uses GUI automation (mouse/keyboard control)
    /// If true, only one such session can run at a time
    #[serde(default)]
    pub uses_gui_automation: bool,
    /// Optional continuation prompt for subsequent iterations
    /// If not provided, will use a default continuation prompt
    #[serde(default)]
    pub continuation_prompt: Option<String>,
    /// Execution mode - in_runner (default) or independent
    /// in_runner: Claude runs as child process, checkpoint-based continuation
    /// independent: Claude runs as separate process (legacy)
    #[serde(default)]
    pub execution_mode: AiDeveloperExecutionMode,
    /// Timeout in seconds for each session (default: 600)
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

/// Response from spawning an AI Developer session
#[derive(Debug, Serialize)]
pub struct SpawnAiDeveloperResponse {
    pub session_id: String,
    pub state_file: String,
    pub log_file: String,
    pub pid: Option<u32>,
    /// Whether this session uses GUI automation
    pub uses_gui_automation: bool,
    /// Whether the GUI lock was successfully acquired (only relevant if uses_gui_automation is true)
    pub gui_lock_acquired: bool,
}

/// Request to read AI Developer session state
#[derive(Debug, Deserialize)]
pub struct ReadAiDeveloperStateRequest {
    pub session_id: String,
}

/// Request to stop an AI Developer session
#[derive(Debug, Deserialize)]
pub struct StopAiDeveloperRequest {
    pub session_id: String,
}

/// Request to read Claude session log
#[derive(Debug, Deserialize)]
pub struct ReadClaudeSessionLogRequest {
    pub session_id: String,
    /// Number of lines to return from end of log (default: 50)
    #[serde(default)]
    pub tail_lines: Option<usize>,
}

/// Session summary for listing
#[derive(Debug, Serialize)]
pub struct AiDeveloperSessionSummary {
    pub session_id: String,
    pub status: String,
    pub iteration: u64,
    pub max_iterations: u64,
    pub errors_fixed: usize,
    pub started_at: String,
}

/// Response from listing AI Developer sessions
#[derive(Debug, Serialize)]
pub struct ListAiDeveloperSessionsResponse {
    pub sessions: Vec<AiDeveloperSessionSummary>,
}

/// Response from reading Claude session log
#[derive(Debug, Serialize)]
pub struct ReadClaudeSessionLogResponse {
    pub content: String,
    pub total_lines: usize,
    pub file_size: u64,
    pub last_modified: u64,
    pub log_file: String,
}

// ============================================================================
// Prompt Library Request/Response Types
// ============================================================================

/// Request to create a new prompt
#[derive(Debug, Deserialize)]
pub struct CreatePromptRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub content: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_prompt_max_iterations")]
    pub max_iterations: u32,
    #[serde(default)]
    pub workflow: Option<prompts::WorkflowConfig>,
}

fn default_prompt_max_iterations() -> u32 {
    10
}

/// Request to update an existing prompt
#[derive(Debug, Deserialize)]
pub struct UpdatePromptRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub max_iterations: Option<u32>,
    #[serde(default)]
    pub workflow: Option<prompts::WorkflowConfig>,
}

/// Request to run a prompt
#[derive(Debug, Deserialize)]
pub struct RunPromptRequest {
    /// Optional session_id override (auto-generated if not provided)
    #[serde(default)]
    pub session_id: Option<String>,
    /// Optional max_iterations override (uses prompt's setting if not provided)
    #[serde(default)]
    pub max_iterations: Option<u32>,
}

/// Request to import prompts
#[derive(Debug, Deserialize)]
pub struct ImportPromptsRequest {
    /// JSON array of prompts to import
    pub prompts_json: String,
}

/// Request to duplicate a prompt
#[derive(Debug, Deserialize)]
pub struct DuplicatePromptRequest {
    /// Optional new name (defaults to "Original Name (Copy)")
    #[serde(default)]
    pub new_name: Option<String>,
}

// ============================================================================
// AI Workflow Request/Response Types
// ============================================================================

use crate::ai_workflows;

/// Request to create a new AI workflow
#[derive(Debug, Deserialize)]
pub struct CreateAiWorkflowRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub steps: Vec<ai_workflows::ExecutionStep>,
    #[serde(default)]
    pub goal: String,
    #[serde(default = "default_ai_workflow_max_iterations")]
    pub max_iterations: u32,
    #[serde(default)]
    pub persistent_session: bool,
    #[serde(default)]
    pub capture_input_validation: bool,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

fn default_ai_workflow_max_iterations() -> u32 {
    10
}

/// Request to update an existing AI workflow
#[derive(Debug, Deserialize)]
pub struct UpdateAiWorkflowRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub steps: Option<Vec<ai_workflows::ExecutionStep>>,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub max_iterations: Option<u32>,
    #[serde(default)]
    pub persistent_session: Option<bool>,
    #[serde(default)]
    pub capture_input_validation: Option<bool>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

// ============================================================================
// Workflow Request/Response Types
// ============================================================================

use crate::workflow_monitor;

/// AI output event payload (emitted to frontend)
#[derive(Debug, Clone, Serialize)]
pub struct AiOutputEvent {
    pub id: String,
    pub timestamp: i64,
    pub line: String,
    pub source: String, // "prompt" or "claude"
    #[serde(rename = "actionId")]
    pub action_id: Option<String>, // Unique ID per AI analysis session
}

// ============================================================================
// Playwright Script Request Types
// ============================================================================

use crate::playwright::{self, DisplayMode};

/// Request to create a new Playwright script
#[derive(Debug, Deserialize)]
pub struct CreatePlaywrightScriptRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub ai_instructions: Option<String>,
    #[serde(default)]
    pub target_url: String,
    pub script_content: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_playwright_timeout")]
    pub timeout_seconds: u32,
    #[serde(default)]
    pub display_mode: DisplayMode,
    #[serde(default = "default_playwright_browser")]
    pub browser: String,
}

fn default_playwright_timeout() -> u32 {
    60
}

fn default_playwright_browser() -> String {
    "chromium".to_string()
}

/// Request to update an existing Playwright script
#[derive(Debug, Deserialize)]
pub struct UpdatePlaywrightScriptRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub ai_instructions: Option<String>,
    #[serde(default)]
    pub target_url: Option<String>,
    #[serde(default)]
    pub script_content: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub timeout_seconds: Option<u32>,
    #[serde(default)]
    pub display_mode: Option<DisplayMode>,
    #[serde(default)]
    pub browser: Option<String>,
}

/// Request to run a Playwright script
#[derive(Debug, Deserialize)]
pub struct RunPlaywrightScriptRequest {
    /// Optional URL override for this run
    #[serde(default)]
    pub target_url_override: Option<String>,
}

/// Request to import Playwright scripts
#[derive(Debug, Deserialize)]
pub struct ImportPlaywrightScriptsRequest {
    /// JSON array of scripts to import
    pub scripts_json: String,
}

/// Request to duplicate a Playwright script
#[derive(Debug, Deserialize)]
pub struct DuplicatePlaywrightScriptRequest {
    /// Optional new name (defaults to "Original Name (Copy)")
    #[serde(default)]
    pub new_name: Option<String>,
}

/// Write workflow event to log file for debugging/persistence
fn log_workflow_event(workflow_id: &str, event_type: &str, message: &str) {
    use std::io::Write;

    let event = serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "workflow_id": workflow_id,
        "event_type": event_type,
        "message": message,
    });

    // Write to log file
    if let Ok((_, dev_logs_path, _)) = get_workspace_paths_internal() {
        let log_file = dev_logs_path.join("workflow-monitor.jsonl");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
        {
            let _ = writeln!(
                file,
                "{}",
                serde_json::to_string(&event).unwrap_or_default()
            );
        }
    }
}

/// Emit AI output event to frontend
fn emit_ai_output(
    app_handle: &tauri::AppHandle,
    line: &str,
    source: &str,
    action_id: Option<&str>,
) {
    let event = AiOutputEvent {
        id: format!(
            "ai-{}-{}",
            chrono::Utc::now().timestamp_millis(),
            rand::random::<u32>()
        ),
        timestamp: chrono::Utc::now().timestamp_millis(),
        line: line.to_string(),
        source: source.to_string(),
        action_id: action_id.map(|s| s.to_string()),
    };

    if let Err(e) = app_handle.emit("ai-output", &event) {
        warn!("Failed to emit AI output event: {}", e);
    }
}

/// Write AI debug log to file
fn write_ai_debug_log(message: &str) {
    use std::io::Write;

    // Get the .dev-logs directory
    let log_dir = if let Ok(exe_path) = std::env::current_exe() {
        exe_path
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .map(|p| p.join(".dev-logs"))
            .unwrap_or_else(|| std::path::PathBuf::from("."))
    } else {
        std::path::PathBuf::from(".")
    };

    let log_file = log_dir.join("ai_execution_debug.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
    {
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let _ = writeln!(file, "[{}] {}", timestamp, message);
    }
}

/// Trigger AI analysis via configured provider
///
/// This endpoint triggers AI analysis using the configured provider:
/// - Claude CLI: Invokes Claude Code CLI (subscription-based)
/// - Claude API: Direct HTTP calls to Anthropic API (per-token billing)
///
/// Returns an error if an AI analysis is already in progress.
async fn trigger_ai_analysis(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<TriggerAiAnalysisRequest>,
) -> Result<Json<ApiResponse<TriggerAiAnalysisResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    write_ai_debug_log("=== AI ANALYSIS TRIGGERED ===");

    // Check if AI analysis is already running (atomic compare-and-swap)
    // This prevents multiple concurrent AI analyses
    if state
        .ai_analysis_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        write_ai_debug_log("AI analysis already in progress - rejecting request");
        warn!("MCP API: AI analysis already in progress, rejecting new request");
        return Err((
            StatusCode::CONFLICT,
            Json(api_error("AI analysis already in progress. Wait for it to complete before triggering another.")),
        ));
    }

    // Ensure we clear the flag when we're done (even on error)
    // Clone the Arc so the guard owns it and can access it when dropped
    let state_for_guard = state.clone();
    let _guard = scopeguard::guard((), move |_| {
        state_for_guard
            .ai_analysis_running
            .store(false, Ordering::SeqCst);
        write_ai_debug_log("AI analysis flag cleared");
    });

    // Clear any previous stop request
    state
        .ai_analysis_stop_requested
        .store(false, Ordering::SeqCst);

    // Generate a unique action_id for this AI analysis session
    // This groups all output from this analysis into one "AI Loop"
    let action_id = format!(
        "ai-loop-{}-{}",
        chrono::Utc::now().timestamp_millis(),
        rand::random::<u32>()
    );
    write_ai_debug_log(&format!("Generated action_id: {}", action_id));

    // Load AI settings
    let ai_settings = settings::get_ai_settings();
    let timeout_secs = request
        .timeout_seconds
        .unwrap_or(ai_settings.claude_cli.timeout_seconds);

    write_ai_debug_log(&format!(
        "Provider: {:?}, Timeout: {}s, Prompt length: {} chars",
        ai_settings.provider,
        timeout_secs,
        request.prompt.len()
    ));

    info!(
        "MCP API: Triggering AI analysis (provider: {:?}, timeout: {}s, prompt length: {}, action_id: {})",
        ai_settings.provider,
        timeout_secs,
        request.prompt.len(),
        action_id
    );

    // Emit prompt to frontend (use display_prompt if provided, else full prompt)
    // display_prompt shows only the new message, not the conversation history
    let ui_prompt = request.display_prompt.as_deref().unwrap_or(&request.prompt);
    write_ai_debug_log("Emitting prompt to frontend...");
    emit_ai_output(&state.app_handle, ui_prompt, "prompt", Some(&action_id));
    write_ai_debug_log("Prompt emitted successfully");

    // Emit hourglass indicator to show AI is processing
    emit_ai_output(
        &state.app_handle,
        "⏳ AI is processing...",
        "status",
        Some(&action_id),
    );

    let app_handle = state.app_handle.clone();
    write_ai_debug_log("Starting AI execution...");

    let result = match ai_settings.provider {
        AiProvider::ClaudeCli => {
            write_ai_debug_log("Using Claude CLI provider");
            execute_claude_cli(
                &ai_settings.claude_cli,
                &request.prompt,
                &app_handle,
                &action_id,
            )
            .await
        }
        AiProvider::ClaudeApi => {
            write_ai_debug_log("Using Claude API provider");
            execute_claude_api(
                &ai_settings.claude_api,
                &request.prompt,
                &app_handle,
                &action_id,
            )
            .await
        }
    };

    match result {
        Ok(response) => {
            if response.success {
                write_ai_debug_log("AI analysis completed successfully");
                info!("MCP API: AI analysis completed successfully");
                // Emit completion indicator
                emit_ai_output(
                    &state.app_handle,
                    "✅ AI analysis complete",
                    "status",
                    Some(&action_id),
                );
            } else {
                write_ai_debug_log(&format!("AI analysis failed: {:?}", response.error));
                warn!("MCP API: AI analysis failed: {:?}", response.error);
                // Emit failure indicator
                emit_ai_output(
                    &state.app_handle,
                    "❌ AI analysis failed",
                    "status",
                    Some(&action_id),
                );
            }
            write_ai_debug_log("=== AI ANALYSIS COMPLETE ===\n");
            Ok(Json(ApiResponse::success(response)))
        }
        Err(e) => {
            write_ai_debug_log(&format!("AI analysis error: {}", e));
            error!("MCP API: Failed to trigger AI analysis: {}", e);
            // Emit error to frontend
            emit_ai_output(
                &state.app_handle,
                "❌ AI analysis error",
                "status",
                Some(&action_id),
            );
            emit_ai_output(
                &state.app_handle,
                &format!("Error: {}", e),
                "claude",
                Some(&action_id),
            );
            write_ai_debug_log("=== AI ANALYSIS FAILED ===\n");
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Stop the currently running AI analysis
///
/// This endpoint sets a flag that the AI analysis process checks
/// to gracefully terminate the Claude CLI subprocess.
async fn stop_ai_analysis(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stop AI analysis requested");

    // Check if AI analysis is running
    if !state.ai_analysis_running.load(Ordering::SeqCst) {
        return Ok(Json(ApiResponse::success(())));
    }

    // Set the stop flag
    state
        .ai_analysis_stop_requested
        .store(true, Ordering::SeqCst);

    // Emit status to frontend
    emit_ai_output(
        &state.app_handle,
        "🛑 Stop requested - AI analysis will terminate",
        "status",
        None,
    );

    info!("MCP API: AI analysis stop flag set");
    Ok(Json(ApiResponse::success(())))
}

/// Restart the runner (for AI self-healing workflow)
///
/// This endpoint allows the AI to trigger a runner restart after applying fixes.
/// The restart is delayed to allow the response to be sent first.
async fn restart_runner(
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

/// Helper function to get workspace paths (reused from config.rs pattern)
fn get_workspace_paths_internal(
) -> Result<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf), String> {
    let exe_path =
        std::env::current_exe().map_err(|e| format!("Failed to get executable path: {}", e))?;

    let mut current = exe_path.as_path();
    let runner_dir = loop {
        if let Some(parent) = current.parent() {
            if parent.join("src-tauri").exists()
                || parent.file_name().is_some_and(|n| n == "qontinui-runner")
            {
                break parent.to_path_buf();
            }
            current = parent;
        } else {
            let cwd = std::env::current_dir()
                .map_err(|e| format!("Failed to get current directory: {}", e))?;
            break cwd;
        }
    };

    let workspace_root = runner_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| runner_dir.clone());
    let dev_logs_path = workspace_root.join(".dev-logs");
    let scripts_path = workspace_root
        .join("qontinui-claude-config")
        .join("scripts");

    Ok((workspace_root, dev_logs_path, scripts_path))
}

/// Spawn an AI Developer session (persistent mode)
///
/// Supports two execution modes:
/// - in_runner (default): Claude runs as a child process, waits for completion,
///   uses checkpoint-based continuation for multi-session workflows
/// - independent (legacy): Spawns Claude as a completely independent process
///
/// If `uses_gui_automation` is true, the session will acquire an exclusive GUI lock
/// to prevent multiple sessions from controlling mouse/keyboard simultaneously.
async fn spawn_ai_developer_http(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<SpawnAiDeveloperRequest>,
) -> Result<Json<ApiResponse<SpawnAiDeveloperResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Generate session_id if not provided
    let session_id = request.session_id.unwrap_or_else(|| {
        format!(
            "{}-{}",
            chrono::Utc::now().format("%Y%m%d-%H%M%S"),
            rand::random::<u16>()
        )
    });
    let max_iterations = request.max_iterations.unwrap_or(10);
    let uses_gui_automation = request.uses_gui_automation;
    let continuation_prompt = request.continuation_prompt.clone();
    let prompt = request.prompt.clone();
    let execution_mode = request.execution_mode;
    let timeout_seconds = request.timeout_seconds.unwrap_or(600);

    info!(
        "MCP API: Spawning AI Developer session: {} (mode: {:?}, max {} iterations, gui_automation: {}, timeout: {}s)",
        session_id, execution_mode, max_iterations, uses_gui_automation, timeout_seconds
    );

    // Try to acquire GUI lock if needed
    let mut gui_lock_acquired = false;
    if uses_gui_automation {
        match state
            .ai_developer_manager
            .acquire_gui_lock(&session_id)
            .await
        {
            Ok(()) => {
                gui_lock_acquired = true;
                info!("MCP API: GUI lock acquired for session {}", session_id);
            }
            Err(e) => {
                // Another GUI session is running - return error
                error!("MCP API: Failed to acquire GUI lock: {}", e);
                return Err((
                    StatusCode::CONFLICT,
                    Json(api_error(format!(
                        "Cannot start GUI automation session: {}",
                        e
                    ))),
                ));
            }
        }
    }

    // Branch based on execution mode
    match execution_mode {
        AiDeveloperExecutionMode::InRunner => {
            // In-runner mode: Run Claude as a child process with checkpoint-based continuation
            spawn_ai_developer_in_runner(
                state,
                session_id,
                prompt,
                continuation_prompt,
                max_iterations,
                uses_gui_automation,
                gui_lock_acquired,
                timeout_seconds,
            )
            .await
        }
        AiDeveloperExecutionMode::Independent => {
            // Independent mode: Spawn as separate process (legacy)
            spawn_ai_developer_independent(
                state,
                session_id,
                prompt,
                continuation_prompt,
                max_iterations,
                uses_gui_automation,
                gui_lock_acquired,
            )
            .await
        }
    }
}

/// Spawn AI Developer session in-runner mode (checkpoint-based continuation)
///
/// This runs Claude as a child process of the runner. When Claude exits,
/// the runner checks the checkpoint file to determine if more work is needed.
/// If so, it spawns another session automatically.
async fn spawn_ai_developer_in_runner(
    state: Arc<ApiState>,
    session_id: String,
    prompt: String,
    continuation_prompt: Option<String>,
    max_iterations: u32,
    uses_gui_automation: bool,
    gui_lock_acquired: bool,
    timeout_seconds: u64,
) -> Result<Json<ApiResponse<SpawnAiDeveloperResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Starting in-runner AI Developer session: {}",
        session_id
    );

    let (workspace_root, dev_logs_path, scripts_path) =
        get_workspace_paths_internal().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get workspace paths: {}", e))),
            )
        })?;

    let state_file = dev_logs_path.join(format!("ai-developer-{}.json", session_id));
    let log_file = dev_logs_path.join(format!("claude-session-{}.log", session_id));

    // Ensure .dev-logs directory exists
    std::fs::create_dir_all(&dev_logs_path).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "Failed to create dev-logs directory: {}",
                e
            ))),
        )
    })?;

    // Create initial state file
    let initial_state = serde_json::json!({
        "session_id": session_id,
        "iteration": 1,
        "max_iterations": max_iterations,
        "status": "running",
        "execution_mode": "in_runner",
        "started_at": chrono::Utc::now().to_rfc3339(),
        "last_activity": chrono::Utc::now().to_rfc3339(),
        "stop_requested": false,
        "uses_gui_automation": uses_gui_automation,
        "continuation_prompt": continuation_prompt.clone(),
        "restart_permitted": false,
        "current_action": "Running session",
        "errors_fixed": [],
        "errors_remaining": [],
        "activity_log": ["Session started (in-runner mode)"]
    });

    std::fs::write(
        &state_file,
        serde_json::to_string_pretty(&initial_state).unwrap(),
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to write state file: {}", e))),
        )
    })?;

    // Register session with manager
    let ai_session = AiDeveloperSession {
        session_id: session_id.clone(),
        status: AiDeveloperStatus::Running,
        iteration: 1,
        max_iterations,
        uses_gui_automation,
        started_at: chrono::Utc::now().to_rfc3339(),
        last_activity: chrono::Utc::now().to_rfc3339(),
        stop_requested: false,
        state_file: state_file.to_string_lossy().to_string(),
        log_file: log_file.to_string_lossy().to_string(),
        prompt: prompt.clone(),
        continuation_prompt: continuation_prompt.clone(),
        errors_fixed: vec![],
        errors_remaining: vec![],
        activity_log: vec![format!(
            "Session started (in-runner) at {}",
            chrono::Utc::now().to_rfc3339()
        )],
    };
    state.ai_developer_manager.add_session(ai_session).await;

    // Spawn background task to run the workflow with checkpoint-based continuation
    let state_clone = state.clone();
    let session_id_for_task = session_id.clone();
    let workspace_root_str = workspace_root.to_string_lossy().to_string();
    let dev_logs_path_clone = dev_logs_path.clone();
    let scripts_path_clone = scripts_path.clone();
    let app_handle = state.app_handle.clone();

    tokio::spawn(async move {
        run_ai_developer_workflow_in_runner(
            state_clone,
            session_id_for_task,
            prompt,
            continuation_prompt,
            max_iterations,
            timeout_seconds,
            workspace_root_str,
            dev_logs_path_clone,
            scripts_path_clone,
            app_handle,
        )
        .await;
    });

    Ok(Json(ApiResponse::success(SpawnAiDeveloperResponse {
        session_id,
        state_file: state_file.to_string_lossy().to_string(),
        log_file: log_file.to_string_lossy().to_string(),
        pid: None, // No separate PID in in-runner mode
        uses_gui_automation,
        gui_lock_acquired,
    })))
}

/// Run the AI Developer workflow in-runner with checkpoint-based continuation
async fn run_ai_developer_workflow_in_runner(
    state: Arc<ApiState>,
    session_id: String,
    initial_prompt: String,
    continuation_prompt: Option<String>,
    max_iterations: u32,
    timeout_seconds: u64,
    workspace_root: String,
    dev_logs_path: std::path::PathBuf,
    scripts_path: std::path::PathBuf,
    app_handle: tauri::AppHandle,
) {
    info!(
        "MCP API: Running in-runner workflow for session {}",
        session_id
    );

    let mut iteration = 1u32;
    let mut current_prompt = initial_prompt;

    loop {
        // Check if stop requested
        if let Some(session) = state.ai_developer_manager.get_session(&session_id).await {
            if session.stop_requested {
                info!(
                    "MCP API: Stop requested for session {}, ending workflow",
                    session_id
                );
                finalize_ai_developer_session(&state, &session_id, AiDeveloperStatus::Stopped)
                    .await;
                return;
            }
        } else {
            warn!(
                "MCP API: Session {} not found in manager, stopping",
                session_id
            );
            return;
        }

        // Check iteration limit
        if iteration > max_iterations {
            info!(
                "MCP API: Session {} reached max iterations ({})",
                session_id, max_iterations
            );
            finalize_ai_developer_session(&state, &session_id, AiDeveloperStatus::Completed).await;
            return;
        }

        info!(
            "MCP API: Running iteration {} of session {}",
            iteration, session_id
        );

        // Emit status update
        emit_ai_output(
            &app_handle,
            &format!(
                "🚀 Starting iteration {} of session {}",
                iteration, session_id
            ),
            "status",
            None,
        );

        // Run Claude session inline (as child process)
        let session_id_iter = format!("{}-iter{}", session_id, iteration);
        let result = tokio::task::spawn_blocking({
            let workspace_root = workspace_root.clone();
            let current_prompt = current_prompt.clone();
            let app_handle = app_handle.clone();
            move || {
                run_claude_session_inline(
                    &workspace_root,
                    &current_prompt,
                    &session_id_iter,
                    &app_handle,
                    timeout_seconds,
                )
            }
        })
        .await;

        match result {
            Ok(Ok((success, _output))) => {
                if !success {
                    warn!(
                        "MCP API: Session {} iteration {} completed with error",
                        session_id, iteration
                    );
                    // Continue anyway - Claude might have fixed something
                }

                // Update session state
                if let Some(mut session) = state.ai_developer_manager.get_session(&session_id).await
                {
                    session.iteration = iteration;
                    session.last_activity = chrono::Utc::now().to_rfc3339();
                    session.activity_log.push(format!(
                        "Iteration {} completed at {}",
                        iteration,
                        chrono::Utc::now().to_rfc3339()
                    ));
                    state.ai_developer_manager.update_session(session).await;
                }

                // Check checkpoint to determine if more work is needed
                if let Some(improve_state) = read_improve_all_state(&dev_logs_path) {
                    if is_improve_all_complete(&improve_state) {
                        info!(
                            "MCP API: Session {} workflow complete (checkpoint indicates done)",
                            session_id
                        );
                        emit_ai_output(
                            &app_handle,
                            "✅ Workflow complete! All phases finished.",
                            "status",
                            None,
                        );
                        finalize_ai_developer_session(
                            &state,
                            &session_id,
                            AiDeveloperStatus::Completed,
                        )
                        .await;
                        return;
                    }

                    // Get next continuation prompt based on checkpoint
                    if let Some(next_prompt) =
                        get_improve_all_continuation_prompt(&improve_state, &scripts_path)
                    {
                        current_prompt = next_prompt;
                        iteration += 1;

                        // Small delay between iterations
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        continue;
                    }
                }

                // No checkpoint-based continuation - use provided continuation prompt or end
                if let Some(ref cont_prompt) = continuation_prompt {
                    current_prompt = cont_prompt.clone();
                    iteration += 1;
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }

                // No more work to do
                info!(
                    "MCP API: Session {} completed (no continuation needed)",
                    session_id
                );
                finalize_ai_developer_session(&state, &session_id, AiDeveloperStatus::Completed)
                    .await;
                return;
            }
            Ok(Err(e)) => {
                error!(
                    "MCP API: Session {} iteration {} failed: {}",
                    session_id, iteration, e
                );
                emit_ai_output(
                    &app_handle,
                    &format!("❌ Session failed: {}", e),
                    "status",
                    None,
                );
                finalize_ai_developer_session(&state, &session_id, AiDeveloperStatus::Failed).await;
                return;
            }
            Err(e) => {
                error!(
                    "MCP API: Session {} spawn_blocking error: {}",
                    session_id, e
                );
                finalize_ai_developer_session(&state, &session_id, AiDeveloperStatus::Failed).await;
                return;
            }
        }
    }
}

/// Spawn AI Developer session in independent mode (legacy)
///
/// This spawns Claude as a completely independent process using spawn-independent-claude.py.
/// The Claude process can restart any service including the runner itself.
async fn spawn_ai_developer_independent(
    state: Arc<ApiState>,
    session_id: String,
    prompt: String,
    continuation_prompt: Option<String>,
    max_iterations: u32,
    uses_gui_automation: bool,
    gui_lock_acquired: bool,
) -> Result<Json<ApiResponse<SpawnAiDeveloperResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Clone values for the blocking task
    let session_id_clone = session_id.clone();
    let prompt_clone = prompt.clone();
    let continuation_prompt_clone = continuation_prompt.clone();

    let result = tokio::task::spawn_blocking(move || {
        let (workspace_root, dev_logs_path, scripts_path) = get_workspace_paths_internal()?;
        let spawn_script = scripts_path.join("spawn-independent-claude.py");
        let state_file = dev_logs_path.join(format!("ai-developer-{}.json", session_id_clone));
        let prompt_file =
            dev_logs_path.join(format!("ai-developer-{}-prompt.txt", session_id_clone));
        let log_file = dev_logs_path.join(format!("claude-session-{}.log", session_id_clone));

        // Ensure .dev-logs directory exists
        std::fs::create_dir_all(&dev_logs_path)
            .map_err(|e| format!("Failed to create dev-logs directory: {}", e))?;

        // Create initial state file with new fields
        let initial_state = serde_json::json!({
            "session_id": session_id_clone,
            "iteration": 1,
            "max_iterations": max_iterations,
            "status": "starting",
            "execution_mode": "independent",
            "started_at": chrono::Utc::now().to_rfc3339(),
            "last_activity": chrono::Utc::now().to_rfc3339(),
            "stop_requested": false,
            "uses_gui_automation": uses_gui_automation,
            "continuation_prompt": continuation_prompt_clone,
            "restart_permitted": false,
            "current_action": "Initializing",
            "errors_fixed": [],
            "errors_remaining": [],
            "activity_log": []
        });

        std::fs::write(
            &state_file,
            serde_json::to_string_pretty(&initial_state).unwrap(),
        )
        .map_err(|e| format!("Failed to write state file: {}", e))?;

        // Write prompt to file
        std::fs::write(&prompt_file, &prompt_clone)
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
                std::ffi::OsStr::new(&session_id_clone),
            ],
            &workspace_root,
        );

        match spawn_result {
            Ok(child) => {
                info!("MCP API: AI Developer spawned with PID: {}", child.id());
                Ok((
                    SpawnAiDeveloperResponse {
                        session_id: session_id_clone,
                        state_file: state_file.to_string_lossy().to_string(),
                        log_file: log_file.to_string_lossy().to_string(),
                        pid: Some(child.id()),
                        uses_gui_automation,
                        gui_lock_acquired,
                    },
                    state_file,
                    log_file,
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
        Ok((response, state_file, log_file)) => {
            // Register session with manager
            let ai_session = AiDeveloperSession {
                session_id: session_id.clone(),
                status: AiDeveloperStatus::Running,
                iteration: 1,
                max_iterations,
                uses_gui_automation,
                started_at: chrono::Utc::now().to_rfc3339(),
                last_activity: chrono::Utc::now().to_rfc3339(),
                stop_requested: false,
                state_file: state_file.to_string_lossy().to_string(),
                log_file: log_file.to_string_lossy().to_string(),
                prompt: prompt.clone(),
                continuation_prompt,
                errors_fixed: vec![],
                errors_remaining: vec![],
                activity_log: vec![format!(
                    "Session started at {}",
                    chrono::Utc::now().to_rfc3339()
                )],
            };
            state.ai_developer_manager.add_session(ai_session).await;

            // Spawn background task to monitor this session for iteration completion
            let state_clone = state.clone();
            let session_id_for_monitor = session_id.clone();
            tokio::spawn(async move {
                monitor_ai_developer_session(state_clone, session_id_for_monitor).await;
            });

            Ok(Json(ApiResponse::success(response)))
        }
        Err(e) => {
            // Release GUI lock if we acquired it but failed to spawn
            if gui_lock_acquired {
                state
                    .ai_developer_manager
                    .release_gui_lock(&session_id)
                    .await;
            }
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Monitor an AI Developer session for iteration completion and spawn continuations
///
/// This function polls for the completion of each iteration and spawns the next
/// iteration if needed. It handles:
/// - Detecting when an iteration completes (via .completed marker file)
/// - Reading the state file to check status
/// - Spawning continuation sessions for subsequent iterations
/// - Releasing GUI lock when the session completes
async fn monitor_ai_developer_session(state: Arc<ApiState>, session_id: String) {
    info!(
        "MCP API: Starting iteration monitor for AI Developer session: {}",
        session_id
    );

    let poll_interval = std::time::Duration::from_secs(5);
    let max_poll_attempts = 720; // 1 hour max per iteration (720 * 5s = 3600s)

    loop {
        let session_opt = state.ai_developer_manager.get_session(&session_id).await;
        let session = match session_opt {
            Some(s) => s,
            None => {
                warn!(
                    "MCP API: Session {} no longer exists in manager, stopping monitor",
                    session_id
                );
                return;
            }
        };

        // Check if stop requested
        if session.stop_requested {
            info!(
                "MCP API: Stop requested for session {}, ending monitoring",
                session_id
            );
            finalize_ai_developer_session(&state, &session_id, AiDeveloperStatus::Stopped).await;
            return;
        }

        // Check if already completed/failed
        match session.status {
            AiDeveloperStatus::Completed
            | AiDeveloperStatus::Failed
            | AiDeveloperStatus::Stopped => {
                info!(
                    "MCP API: Session {} is in terminal state {:?}, ending monitoring",
                    session_id, session.status
                );
                return;
            }
            _ => {}
        }

        let current_iteration = session.iteration;

        // Wait for the current iteration to complete
        info!(
            "MCP API: Waiting for iteration {} of session {} to complete",
            current_iteration, session_id
        );

        let completed = wait_for_ai_developer_iteration_completion(
            &session.state_file,
            &session_id,
            poll_interval,
            max_poll_attempts,
        )
        .await;

        if !completed {
            warn!(
                "MCP API: Iteration {} of session {} timed out or failed",
                current_iteration, session_id
            );
            finalize_ai_developer_session(&state, &session_id, AiDeveloperStatus::Failed).await;
            return;
        }

        // Read updated state file to check status and get next iteration info
        let state_result = read_ai_developer_state_file(&session.state_file).await;
        let file_state = match state_result {
            Ok(s) => s,
            Err(e) => {
                error!(
                    "MCP API: Failed to read state file for session {}: {}",
                    session_id, e
                );
                finalize_ai_developer_session(&state, &session_id, AiDeveloperStatus::Failed).await;
                return;
            }
        };

        // Check file state for status
        let status_str = file_state
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let stop_requested = file_state
            .get("stop_requested")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let file_iteration = file_state
            .get("iteration")
            .and_then(|v| v.as_u64())
            .unwrap_or(current_iteration as u64) as u32;
        let max_iterations = file_state
            .get("max_iterations")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as u32;

        info!(
            "MCP API: Session {} iteration {} completed with status: {}, file_iteration: {}/{}",
            session_id, current_iteration, status_str, file_iteration, max_iterations
        );

        // Check for terminal states
        if status_str == "completed"
            || status_str == "failed"
            || status_str == "stopped"
            || stop_requested
        {
            let final_status = match status_str {
                "completed" => AiDeveloperStatus::Completed,
                "stopped" => AiDeveloperStatus::Stopped,
                _ => {
                    if stop_requested {
                        AiDeveloperStatus::Stopped
                    } else {
                        AiDeveloperStatus::Failed
                    }
                }
            };
            info!(
                "MCP API: Session {} reached terminal state: {:?}",
                session_id, final_status
            );
            finalize_ai_developer_session(&state, &session_id, final_status).await;
            return;
        }

        // Check if we've reached max iterations
        if file_iteration >= max_iterations {
            info!(
                "MCP API: Session {} reached max iterations ({})",
                session_id, max_iterations
            );
            finalize_ai_developer_session(&state, &session_id, AiDeveloperStatus::Completed).await;
            return;
        }

        // Spawn next iteration
        let next_iteration = file_iteration + 1;
        info!(
            "MCP API: Spawning iteration {} for session {}",
            next_iteration, session_id
        );

        // Update manager state
        let mut updated_session = session.clone();
        updated_session.iteration = next_iteration;
        updated_session.status = AiDeveloperStatus::Running;
        updated_session.last_activity = chrono::Utc::now().to_rfc3339();
        updated_session.activity_log.push(format!(
            "Iteration {} started at {}",
            next_iteration,
            chrono::Utc::now().to_rfc3339()
        ));
        state
            .ai_developer_manager
            .update_session(updated_session.clone())
            .await;

        // Spawn the next iteration
        let spawn_result = spawn_ai_developer_continuation(
            &session.state_file,
            &session_id,
            next_iteration,
            &session.continuation_prompt,
        )
        .await;

        if let Err(e) = spawn_result {
            error!(
                "MCP API: Failed to spawn continuation for session {}: {}",
                session_id, e
            );
            finalize_ai_developer_session(&state, &session_id, AiDeveloperStatus::Failed).await;
            return;
        }

        // Loop back to wait for this iteration to complete
    }
}

/// Wait for an AI Developer iteration to complete by checking for completion marker
async fn wait_for_ai_developer_iteration_completion(
    state_file: &str,
    session_id: &str,
    poll_interval: std::time::Duration,
    max_attempts: u32,
) -> bool {
    // The completion marker is based on the session_id (same as workflow sessions)
    let (_, dev_logs_path, _) = match get_workspace_paths_internal() {
        Ok(paths) => paths,
        Err(e) => {
            error!("MCP API: Failed to get workspace paths: {}", e);
            return false;
        }
    };

    let completion_marker = dev_logs_path.join(format!("claude-session-{}.completed", session_id));

    for attempt in 0..max_attempts {
        // Check for completion marker
        if completion_marker.exists() {
            info!(
                "MCP API: Found completion marker for session {} (attempt {})",
                session_id, attempt
            );

            // Remove the completion marker for the next iteration
            if let Err(e) = std::fs::remove_file(&completion_marker) {
                warn!("MCP API: Failed to remove completion marker: {}", e);
            }

            return true;
        }

        // Also check state file for terminal states
        if let Ok(file_state) = read_ai_developer_state_file(state_file).await {
            let status = file_state
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            if status == "completed" || status == "failed" || status == "stopped" {
                info!(
                    "MCP API: Session {} state file shows terminal status: {}",
                    session_id, status
                );
                return true;
            }

            let stop_requested = file_state
                .get("stop_requested")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            if stop_requested {
                info!("MCP API: Session {} has stop_requested=true", session_id);
                return true;
            }
        }

        tokio::time::sleep(poll_interval).await;
    }

    warn!(
        "MCP API: Timeout waiting for session {} iteration completion after {} attempts",
        session_id, max_attempts
    );
    false
}

/// Read AI Developer state file
async fn read_ai_developer_state_file(state_file: &str) -> Result<serde_json::Value, String> {
    let state_file = state_file.to_string();
    tokio::task::spawn_blocking(move || {
        let content = std::fs::read_to_string(&state_file)
            .map_err(|e| format!("Failed to read state file: {}", e))?;
        let state: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse state file: {}", e))?;
        Ok(state)
    })
    .await
    .map_err(|e| format!("Spawn blocking error: {}", e))?
}

/// Update AI Developer state file
async fn update_ai_developer_state_file(
    state_file: &str,
    updates: serde_json::Value,
) -> Result<(), String> {
    let state_file = state_file.to_string();
    tokio::task::spawn_blocking(move || {
        // Read current state
        let content = std::fs::read_to_string(&state_file)
            .map_err(|e| format!("Failed to read state file: {}", e))?;
        let mut state: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse state file: {}", e))?;

        // Merge updates
        if let (Some(state_obj), Some(updates_obj)) = (state.as_object_mut(), updates.as_object()) {
            for (key, value) in updates_obj {
                state_obj.insert(key.clone(), value.clone());
            }
        }

        // Write back
        std::fs::write(&state_file, serde_json::to_string_pretty(&state).unwrap())
            .map_err(|e| format!("Failed to write state file: {}", e))?;

        Ok(())
    })
    .await
    .map_err(|e| format!("Spawn blocking error: {}", e))?
}

/// Spawn a continuation session for the next iteration
async fn spawn_ai_developer_continuation(
    state_file: &str,
    session_id: &str,
    iteration: u32,
    continuation_prompt: &Option<String>,
) -> Result<(), String> {
    let (workspace_root, dev_logs_path, scripts_path) = get_workspace_paths_internal()?;
    let spawn_script = scripts_path.join("spawn-independent-claude.py");

    // Create continuation prompt file
    let prompt_file = dev_logs_path.join(format!(
        "ai-developer-{}-iter{}-prompt.txt",
        session_id, iteration
    ));

    let prompt = if let Some(custom_prompt) = continuation_prompt {
        custom_prompt.clone()
    } else {
        format!(
            r#"You are continuing an AI Developer session.

Session ID: {}
Current Iteration: {}
State File: {}

Read the state file to understand the current context and what needs to be done next.
Check errors_remaining and errors_fixed to understand progress.
Update the state file as you work:
- Add fixed errors to errors_fixed
- Update errors_remaining
- Add entries to activity_log
- Set status to "running" while working, "completed" when done, or "failed" if you encounter unrecoverable issues

If stop_requested is true, finish your current task and set status to "stopped".
If you've fixed all errors or completed all tasks, set status to "completed".

Continue where the previous iteration left off."#,
            session_id, iteration, state_file
        )
    };

    // Write prompt file
    std::fs::write(&prompt_file, &prompt)
        .map_err(|e| format!("Failed to write continuation prompt: {}", e))?;

    // Update state file for new iteration
    update_ai_developer_state_file(
        state_file,
        serde_json::json!({
            "iteration": iteration,
            "status": "running",
            "last_activity": chrono::Utc::now().to_rfc3339()
        }),
    )
    .await?;

    // Spawn the continuation
    // Use spawn_python_with_console to ensure Claude CLI gets a console window
    let spawn_result = spawn_python_with_console(
        "python",
        &[
            spawn_script.as_os_str(),
            std::ffi::OsStr::new("--file"),
            prompt_file.as_os_str(),
            std::ffi::OsStr::new("--session-id"),
            std::ffi::OsStr::new(session_id),
        ],
        &workspace_root,
    );

    match spawn_result {
        Ok(child) => {
            info!(
                "MCP API: AI Developer continuation spawned with PID: {} (session: {}, iteration: {})",
                child.id(), session_id, iteration
            );
            Ok(())
        }
        Err(e) => {
            error!("MCP API: Failed to spawn AI Developer continuation: {}", e);
            Err(format!("Failed to spawn continuation: {}", e))
        }
    }
}

/// Finalize an AI Developer session (cleanup, release locks)
async fn finalize_ai_developer_session(
    state: &Arc<ApiState>,
    session_id: &str,
    final_status: AiDeveloperStatus,
) {
    info!(
        "MCP API: Finalizing AI Developer session {} with status {:?}",
        session_id, final_status
    );

    // Update session status in manager
    if let Some(mut session) = state.ai_developer_manager.get_session(session_id).await {
        session.status = final_status.clone();
        session.last_activity = chrono::Utc::now().to_rfc3339();
        session.activity_log.push(format!(
            "Session finalized with status {:?} at {}",
            final_status,
            chrono::Utc::now().to_rfc3339()
        ));

        // Update state file
        let status_str = match final_status {
            AiDeveloperStatus::Completed => "completed",
            AiDeveloperStatus::Failed => "failed",
            AiDeveloperStatus::Stopped => "stopped",
            _ => "unknown",
        };

        if let Err(e) = update_ai_developer_state_file(
            &session.state_file,
            serde_json::json!({
                "status": status_str,
                "last_activity": chrono::Utc::now().to_rfc3339()
            }),
        )
        .await
        {
            error!(
                "MCP API: Failed to update state file for session {}: {}",
                session_id, e
            );
        }

        // Release GUI lock if this session held it
        if session.uses_gui_automation {
            state
                .ai_developer_manager
                .release_gui_lock(session_id)
                .await;
            info!("MCP API: Released GUI lock for session {}", session_id);
        }

        state.ai_developer_manager.update_session(session).await;
    }
}

/// Resume AI Developer sessions on startup by scanning for active state files
///
/// This function scans the .dev-logs directory for ai-developer-*.json files
/// and resumes monitoring for any sessions that have:
/// - status = "running" or "starting"
/// - restart_permitted = true
async fn resume_ai_developer_sessions_on_startup(state: Arc<ApiState>) {
    info!("MCP API: Scanning for AI Developer sessions to resume...");

    let (_, dev_logs_path, _) = match get_workspace_paths_internal() {
        Ok(paths) => paths,
        Err(e) => {
            error!(
                "MCP API: Failed to get workspace paths for AI Developer resume: {}",
                e
            );
            return;
        }
    };

    // Scan for ai-developer-*.json files
    let entries = match std::fs::read_dir(&dev_logs_path) {
        Ok(e) => e,
        Err(e) => {
            warn!("MCP API: Could not read dev-logs directory: {}", e);
            return;
        }
    };

    let mut sessions_resumed = 0;

    for entry in entries.flatten() {
        let path = entry.path();
        let filename = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };

        // Check if this is an AI Developer state file
        if !filename.starts_with("ai-developer-") || !filename.ends_with(".json") {
            continue;
        }

        // Parse session_id from filename: ai-developer-{session_id}.json
        let session_id = filename
            .strip_prefix("ai-developer-")
            .and_then(|s| s.strip_suffix(".json"))
            .unwrap_or("")
            .to_string();

        if session_id.is_empty() {
            continue;
        }

        // Read state file
        let state_file_content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                warn!("MCP API: Failed to read state file {:?}: {}", path, e);
                continue;
            }
        };

        let file_state: serde_json::Value = match serde_json::from_str(&state_file_content) {
            Ok(s) => s,
            Err(e) => {
                warn!("MCP API: Failed to parse state file {:?}: {}", path, e);
                continue;
            }
        };

        // Check status
        let status = file_state
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        // Only resume sessions that are in running/starting state
        if status != "running" && status != "starting" {
            continue;
        }

        // Check restart_permitted
        let restart_permitted = file_state
            .get("restart_permitted")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !restart_permitted {
            info!(
                "MCP API: AI Developer session {} was interrupted but restart_permitted=false, skipping",
                session_id
            );

            // Update state file to mark as failed due to restart
            if let Err(e) = update_ai_developer_state_file(
                &path.to_string_lossy(),
                serde_json::json!({
                    "status": "failed",
                    "last_activity": chrono::Utc::now().to_rfc3339(),
                    "failure_reason": "Runner restarted without restart_permitted. Set restart_permitted=true before triggering restarts."
                }),
            )
            .await
            {
                warn!("MCP API: Failed to update state file: {}", e);
            }
            continue;
        }

        info!(
            "MCP API: Resuming AI Developer session {} (status: {}, restart_permitted: true)",
            session_id, status
        );

        // Read other fields from state file
        let iteration = file_state
            .get("iteration")
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as u32;
        let max_iterations = file_state
            .get("max_iterations")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as u32;
        let uses_gui_automation = file_state
            .get("uses_gui_automation")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let continuation_prompt = file_state
            .get("continuation_prompt")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let started_at = file_state
            .get("started_at")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        // Try to acquire GUI lock if needed
        if uses_gui_automation {
            if let Err(e) = state
                .ai_developer_manager
                .acquire_gui_lock(&session_id)
                .await
            {
                warn!(
                    "MCP API: Could not acquire GUI lock for session {}, skipping: {}",
                    session_id, e
                );
                continue;
            }
        }

        // Clear restart_permitted since we're resuming
        if let Err(e) = update_ai_developer_state_file(
            &path.to_string_lossy(),
            serde_json::json!({
                "restart_permitted": false,
                "last_activity": chrono::Utc::now().to_rfc3339()
            }),
        )
        .await
        {
            warn!("MCP API: Failed to clear restart_permitted: {}", e);
        }

        // Create session in manager
        let log_file = dev_logs_path.join(format!("claude-session-{}.log", session_id));
        let ai_session = AiDeveloperSession {
            session_id: session_id.clone(),
            status: AiDeveloperStatus::Running,
            iteration,
            max_iterations,
            uses_gui_automation,
            started_at,
            last_activity: chrono::Utc::now().to_rfc3339(),
            stop_requested: false,
            state_file: path.to_string_lossy().to_string(),
            log_file: log_file.to_string_lossy().to_string(),
            prompt: "Resumed after restart".to_string(),
            continuation_prompt,
            errors_fixed: vec![],
            errors_remaining: vec![],
            activity_log: vec![format!(
                "Session resumed after runner restart at {}",
                chrono::Utc::now().to_rfc3339()
            )],
        };
        state
            .ai_developer_manager
            .add_session(ai_session.clone())
            .await;

        // Spawn a continuation session for this iteration
        let state_clone = state.clone();
        let session_id_clone = session_id.clone();
        let state_file_path = path.to_string_lossy().to_string();
        let cont_prompt = ai_session.continuation_prompt.clone();

        tokio::spawn(async move {
            // Spawn continuation
            if let Err(e) = spawn_ai_developer_continuation(
                &state_file_path,
                &session_id_clone,
                iteration,
                &cont_prompt,
            )
            .await
            {
                error!(
                    "MCP API: Failed to spawn continuation for resumed session {}: {}",
                    session_id_clone, e
                );
                finalize_ai_developer_session(
                    &state_clone,
                    &session_id_clone,
                    AiDeveloperStatus::Failed,
                )
                .await;
                return;
            }

            // Start monitoring
            monitor_ai_developer_session(state_clone, session_id_clone).await;
        });

        sessions_resumed += 1;
    }

    info!(
        "MCP API: Resumed {} AI Developer session(s) on startup",
        sessions_resumed
    );
}

/// Resume incomplete improve-all workflow on startup
///
/// This checks for an incomplete improve-all-state.json file and
/// automatically starts a new session to continue the workflow.
async fn resume_improve_all_workflow_on_startup(state: Arc<ApiState>) {
    info!("MCP API: Checking for incomplete improve-all workflows...");

    let (workspace_root, dev_logs_path, scripts_path) = match get_workspace_paths_internal() {
        Ok(paths) => paths,
        Err(e) => {
            warn!(
                "MCP API: Failed to get workspace paths for improve-all resume: {}",
                e
            );
            return;
        }
    };

    // Check if improve-all-state.json exists and is incomplete
    let improve_state = match read_improve_all_state(&dev_logs_path) {
        Some(state) => state,
        None => {
            info!("MCP API: No improve-all state file found, nothing to resume");
            return;
        }
    };

    // Check if workflow is complete
    if is_improve_all_complete(&improve_state) {
        info!("MCP API: Improve-all workflow is already complete");
        return;
    }

    // Check for restart_permitted flag
    let restart_permitted = improve_state
        .get("restart_permitted")
        .and_then(|v| {
            // Handle both object and bool formats
            if let Some(b) = v.as_bool() {
                return Some(b);
            }
            v.get("permitted").and_then(|p| p.as_bool())
        })
        .unwrap_or(false);

    if !restart_permitted {
        info!(
            "MCP API: Improve-all workflow is incomplete but restart_permitted=false, not resuming"
        );
        // Emit notification to UI
        emit_ai_output(
            &state.app_handle,
            "⚠️ Incomplete improve-all workflow found but restart not permitted. Set restart_permitted=true to resume.",
            "status",
            None,
        );
        return;
    }

    // Get continuation prompt for next part
    let continuation_prompt =
        match get_improve_all_continuation_prompt(&improve_state, &scripts_path) {
            Some(prompt) => prompt,
            None => {
                info!("MCP API: Could not generate continuation prompt for improve-all");
                return;
            }
        };

    let current_part = improve_state
        .get("current_part")
        .and_then(|v| v.as_u64())
        .unwrap_or(1);

    info!(
        "MCP API: Resuming improve-all workflow at part {}",
        current_part
    );

    // Emit status
    emit_ai_output(
        &state.app_handle,
        &format!(
            "🔄 Resuming improve-all workflow at part {}/5",
            current_part
        ),
        "status",
        None,
    );

    // Clear restart_permitted flag
    let state_file = dev_logs_path.join("improve-all-state.json");
    if let Ok(content) = std::fs::read_to_string(&state_file) {
        if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(obj) = json.as_object_mut() {
                obj.remove("restart_permitted");
                if let Ok(updated) = serde_json::to_string_pretty(&json) {
                    let _ = std::fs::write(&state_file, updated);
                }
            }
        }
    }

    // Generate session ID
    let run_id = improve_state
        .get("run_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let session_id = format!("improve-all-resume-part{}-{}", current_part, run_id);

    // Spawn the workflow
    let workspace_root_str = workspace_root.to_string_lossy().to_string();
    let dev_logs_path_clone = dev_logs_path.clone();
    let scripts_path_clone = scripts_path.clone();
    let app_handle = state.app_handle.clone();

    // Register session with manager
    let ai_session = AiDeveloperSession {
        session_id: session_id.clone(),
        status: AiDeveloperStatus::Running,
        iteration: 1,
        max_iterations: 10,
        uses_gui_automation: false,
        started_at: chrono::Utc::now().to_rfc3339(),
        last_activity: chrono::Utc::now().to_rfc3339(),
        stop_requested: false,
        state_file: state_file.to_string_lossy().to_string(),
        log_file: dev_logs_path
            .join(format!("claude-session-{}.log", session_id))
            .to_string_lossy()
            .to_string(),
        prompt: continuation_prompt.clone(),
        continuation_prompt: None,
        errors_fixed: vec![],
        errors_remaining: vec![],
        activity_log: vec![format!(
            "Session resumed on startup at {}",
            chrono::Utc::now().to_rfc3339()
        )],
    };
    state.ai_developer_manager.add_session(ai_session).await;

    // Start the workflow
    let state_clone = state.clone();
    tokio::spawn(async move {
        run_ai_developer_workflow_in_runner(
            state_clone,
            session_id,
            continuation_prompt,
            None, // No custom continuation prompt - use checkpoint-based
            10,   // max iterations
            600,  // timeout
            workspace_root_str,
            dev_logs_path_clone,
            scripts_path_clone,
            app_handle,
        )
        .await;
    });
}

/// Read the current state of an AI Developer session
async fn read_ai_developer_state_http(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<ReadAiDeveloperStateRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let session_id = request.session_id;

    let result = tokio::task::spawn_blocking(move || {
        let (_, dev_logs_path, _) = get_workspace_paths_internal()?;
        let state_file = dev_logs_path.join(format!("ai-developer-{}.json", session_id));

        if !state_file.exists() {
            return Err(format!("No state file found for session {}", session_id));
        }

        let content = std::fs::read_to_string(&state_file)
            .map_err(|e| format!("Failed to read state file: {}", e))?;

        let state: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse state file: {}", e))?;

        Ok(state)
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
        Ok(state) => Ok(Json(ApiResponse::success(state))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Request an AI Developer session to stop
async fn stop_ai_developer_http(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StopAiDeveloperRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let session_id = request.session_id.clone();
    info!(
        "MCP API: Requesting stop for AI Developer session: {}",
        session_id
    );

    // Also update manager state
    if let Some(mut session) = state.ai_developer_manager.get_session(&session_id).await {
        session.stop_requested = true;
        session.last_activity = chrono::Utc::now().to_rfc3339();
        session.activity_log.push(format!(
            "Stop requested at {}",
            chrono::Utc::now().to_rfc3339()
        ));
        state.ai_developer_manager.update_session(session).await;
    }

    let result = tokio::task::spawn_blocking(move || {
        let (_, dev_logs_path, _) = get_workspace_paths_internal()?;
        let state_file = dev_logs_path.join(format!("ai-developer-{}.json", session_id));

        if !state_file.exists() {
            return Err(format!("No state file found for session {}", session_id));
        }

        // Read current state
        let content = std::fs::read_to_string(&state_file)
            .map_err(|e| format!("Failed to read state file: {}", e))?;

        let mut file_state: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse state file: {}", e))?;

        // Set stop_requested flag
        file_state["stop_requested"] = serde_json::Value::Bool(true);

        // Write back
        std::fs::write(
            &state_file,
            serde_json::to_string_pretty(&file_state).unwrap(),
        )
        .map_err(|e| format!("Failed to write state file: {}", e))?;

        info!("MCP API: Stop requested for session {}", session_id);
        Ok(file_state)
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
        Ok(file_state) => Ok(Json(ApiResponse::success(file_state))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Get all managed AI Developer sessions from the manager
///
/// Returns sessions that are actively being monitored by the runner.
/// This includes more runtime info than just reading state files.
async fn get_managed_ai_developer_sessions_http(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<AiDeveloperSession>>> {
    let sessions = state.ai_developer_manager.get_all_sessions().await;
    Json(ApiResponse::success(sessions))
}

/// Response for GUI lock status
#[derive(Debug, Serialize)]
struct GuiLockStatus {
    locked: bool,
    holder: Option<String>,
}

/// Get the current GUI automation lock status
async fn get_gui_lock_status_http(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<GuiLockStatus>> {
    let holder = state.ai_developer_manager.gui_lock_holder().await;
    let status = GuiLockStatus {
        locked: holder.is_some(),
        holder,
    };
    Json(ApiResponse::success(status))
}

/// List all AI Developer sessions
async fn list_ai_developer_sessions_http(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<ListAiDeveloperSessionsResponse>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let result: Result<ListAiDeveloperSessionsResponse, String> =
        tokio::task::spawn_blocking(move || {
            let (_, dev_logs_path, _) = get_workspace_paths_internal()?;

            let mut sessions = Vec::new();

            if let Ok(entries) = std::fs::read_dir(&dev_logs_path) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name.starts_with("ai-developer-")
                            && name.ends_with(".json")
                            && !name.contains("-prompt")
                        {
                            // Extract session ID
                            let session_id = name
                                .strip_prefix("ai-developer-")
                                .and_then(|s| s.strip_suffix(".json"))
                                .unwrap_or("unknown")
                                .to_string();

                            // Try to read state
                            if let Ok(content) = std::fs::read_to_string(&path) {
                                if let Ok(state) =
                                    serde_json::from_str::<serde_json::Value>(&content)
                                {
                                    sessions.push(AiDeveloperSessionSummary {
                                        session_id,
                                        status: state
                                            .get("status")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("unknown")
                                            .to_string(),
                                        iteration: state
                                            .get("iteration")
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(0),
                                        max_iterations: state
                                            .get("max_iterations")
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(10),
                                        errors_fixed: state
                                            .get("errors_fixed")
                                            .and_then(|v| v.as_array())
                                            .map(|a| a.len())
                                            .unwrap_or(0),
                                        started_at: state
                                            .get("started_at")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("unknown")
                                            .to_string(),
                                    });
                                }
                            }
                        }
                    }
                }
            }

            // Sort by started_at descending (most recent first)
            sessions.sort_by(|a, b| b.started_at.cmp(&a.started_at));

            Ok(ListAiDeveloperSessionsResponse { sessions })
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
        Ok(response) => Ok(Json(ApiResponse::success(response))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Read the Claude session log file (tail last N lines)
async fn read_claude_session_log_http(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<ReadClaudeSessionLogRequest>,
) -> Result<Json<ApiResponse<ReadClaudeSessionLogResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let session_id = request.session_id;
    let lines_to_read = request.tail_lines.unwrap_or(50);

    let result = tokio::task::spawn_blocking(move || {
        let (_, dev_logs_path, _) = get_workspace_paths_internal()?;
        let log_file = dev_logs_path.join(format!("claude-session-{}.log", session_id));

        if !log_file.exists() {
            return Err(format!("No log file found for session {}", session_id));
        }

        let content = std::fs::read_to_string(&log_file)
            .map_err(|e| format!("Failed to read log file: {}", e))?;

        // Get last N lines
        let lines: Vec<&str> = content.lines().collect();
        let start = if lines.len() > lines_to_read {
            lines.len() - lines_to_read
        } else {
            0
        };
        let tail_content = lines[start..].join("\n");

        // Get file size and modification time
        let metadata = std::fs::metadata(&log_file)
            .map_err(|e| format!("Failed to get log file metadata: {}", e))?;

        let modified = metadata
            .modified()
            .map(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            })
            .unwrap_or(0);

        Ok(ReadClaudeSessionLogResponse {
            content: tail_content,
            total_lines: lines.len(),
            file_size: metadata.len(),
            last_modified: modified,
            log_file: log_file.to_string_lossy().to_string(),
        })
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
        Ok(response) => Ok(Json(ApiResponse::success(response))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

// ============================================================================
// Prompt Library HTTP Endpoints
// ============================================================================

use crate::prompts;

/// List all prompts
async fn list_prompts(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<prompts::SavedPrompt>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let prompts = prompts::get_all_prompts();
    Ok(Json(ApiResponse::success(prompts)))
}

/// Get a single prompt by ID
async fn get_prompt(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<prompts::SavedPrompt>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::get_prompt(&id) {
        Some(prompt) => Ok(Json(ApiResponse::success(prompt))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Prompt not found: {}", id))),
        )),
    }
}

/// Create a new prompt
async fn create_prompt(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<CreatePromptRequest>,
) -> Result<Json<ApiResponse<prompts::SavedPrompt>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::create_prompt(
        request.name,
        request.description,
        request.content,
        request.category,
        request.tags,
        request.max_iterations,
        request.workflow,
    ) {
        Ok(prompt) => Ok(Json(ApiResponse::success(prompt))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Update an existing prompt
async fn update_prompt(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<UpdatePromptRequest>,
) -> Result<Json<ApiResponse<prompts::SavedPrompt>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::update_prompt(
        &id,
        request.name,
        request.description,
        request.content,
        request.category,
        request.tags,
        request.max_iterations,
        request.workflow,
    ) {
        Ok(prompt) => Ok(Json(ApiResponse::success(prompt))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Delete a prompt
async fn delete_prompt(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::delete_prompt(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Run a prompt by spawning an AI Developer session
async fn run_prompt(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<RunPromptRequest>,
) -> Result<Json<ApiResponse<SpawnAiDeveloperResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Get the prompt
    let prompt = prompts::get_prompt(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Prompt not found: {}", id))),
        )
    })?;

    // Generate session_id if not provided
    let session_id = request.session_id.unwrap_or_else(|| {
        format!(
            "{}-{}",
            chrono::Utc::now().format("%Y%m%d-%H%M%S"),
            rand::random::<u16>()
        )
    });

    // Use override or prompt's setting
    let max_iterations = request.max_iterations.unwrap_or(prompt.max_iterations);

    info!(
        "MCP API: Running prompt '{}' (session: {}, max_iterations: {})",
        prompt.name, session_id, max_iterations
    );

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
            "prompt_id": prompt.id,
            "prompt_name": prompt.name,
            "iteration": 1,
            "max_iterations": max_iterations,
            "status": "starting",
            "started_at": chrono::Utc::now().to_rfc3339(),
            "stop_requested": false,
            "current_action": "Initializing",
            "errors_fixed": [],
            "errors_remaining": [],
            "activity_log": []
        });

        std::fs::write(
            &state_file,
            serde_json::to_string_pretty(&initial_state).unwrap(),
        )
        .map_err(|e| format!("Failed to write state file: {}", e))?;

        // Write prompt content to file
        std::fs::write(&prompt_file, &prompt.content)
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
                    prompt.name
                );
                Ok(SpawnAiDeveloperResponse {
                    session_id,
                    state_file: state_file.to_string_lossy().to_string(),
                    log_file: log_file.to_string_lossy().to_string(),
                    pid: Some(child.id()),
                    uses_gui_automation: false,
                    gui_lock_acquired: false,
                })
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
        Ok(response) => Ok(Json(ApiResponse::success(response))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Get all categories
async fn get_prompt_categories(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let categories = prompts::get_categories();
    Ok(Json(ApiResponse::success(categories)))
}

/// Get all tags
async fn get_prompt_tags(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let tags = prompts::get_all_tags();
    Ok(Json(ApiResponse::success(tags)))
}

/// Import prompts from JSON
async fn import_prompts(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<ImportPromptsRequest>,
) -> Result<Json<ApiResponse<Vec<prompts::SavedPrompt>>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::import_prompts(&request.prompts_json) {
        Ok(imported) => Ok(Json(ApiResponse::success(imported))),
        Err(e) => Err((StatusCode::BAD_REQUEST, Json(api_error(e)))),
    }
}

/// Export all prompts as JSON
async fn export_prompts(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::export_prompts() {
        Ok(json) => Ok(Json(ApiResponse::success(json))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Duplicate a prompt
async fn duplicate_prompt(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<DuplicatePromptRequest>,
) -> Result<Json<ApiResponse<prompts::SavedPrompt>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::duplicate_prompt(&id, request.new_name) {
        Ok(prompt) => Ok(Json(ApiResponse::success(prompt))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Search prompts by query
async fn search_prompts(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<prompts::SavedPrompt>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let query = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let results = prompts::search_prompts(query);
    Ok(Json(ApiResponse::success(results)))
}

// ============================================================================
// AI Workflow Handlers
// ============================================================================

/// List all AI workflows
async fn list_ai_workflows(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<ai_workflows::AiWorkflow>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let category = params.get("category").map(|s| s.as_str());
    let workflows = ai_workflows::list_workflows(category);
    Ok(Json(ApiResponse::success(workflows)))
}

/// Get a single AI workflow by ID
async fn get_ai_workflow(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<ai_workflows::AiWorkflow>>, (StatusCode, Json<ApiResponse<()>>)> {
    match ai_workflows::get_workflow(&id) {
        Some(workflow) => Ok(Json(ApiResponse::success(workflow))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("AI workflow not found: {}", id))),
        )),
    }
}

/// Create a new AI workflow
async fn create_ai_workflow(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<CreateAiWorkflowRequest>,
) -> Result<Json<ApiResponse<ai_workflows::AiWorkflow>>, (StatusCode, Json<ApiResponse<()>>)> {
    match ai_workflows::create_workflow(
        request.name,
        request.description,
        request.steps,
        request.goal,
        request.max_iterations,
        request.persistent_session,
        request.capture_input_validation,
        request.category,
        request.tags,
    ) {
        Ok(workflow) => Ok(Json(ApiResponse::success(workflow))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Update an existing AI workflow
async fn update_ai_workflow(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<UpdateAiWorkflowRequest>,
) -> Result<Json<ApiResponse<ai_workflows::AiWorkflow>>, (StatusCode, Json<ApiResponse<()>>)> {
    match ai_workflows::update_workflow(
        &id,
        request.name,
        request.description,
        request.steps,
        request.goal,
        request.max_iterations,
        request.persistent_session,
        request.capture_input_validation,
        request.category,
        request.tags,
    ) {
        Ok(workflow) => Ok(Json(ApiResponse::success(workflow))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Delete an AI workflow
async fn delete_ai_workflow(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match ai_workflows::delete_workflow(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Search AI workflows by query
async fn search_ai_workflows(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<ai_workflows::AiWorkflow>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let query = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let results = ai_workflows::search_workflows(query);
    Ok(Json(ApiResponse::success(results)))
}

/// Get all AI workflow categories
async fn get_ai_workflow_categories(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let categories = ai_workflows::get_categories();
    Ok(Json(ApiResponse::success(categories)))
}

/// Get all AI workflow tags
async fn get_ai_workflow_tags(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let tags = ai_workflows::get_tags();
    Ok(Json(ApiResponse::success(tags)))
}

// ============================================================================
// Playwright Script Handlers
// ============================================================================

/// List all Playwright scripts
async fn list_playwright_scripts(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<playwright::PlaywrightScript>>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let scripts = playwright::get_all_scripts();
    Ok(Json(ApiResponse::success(scripts)))
}

/// Get a single Playwright script by ID
async fn get_playwright_script(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<playwright::PlaywrightScript>>, (StatusCode, Json<ApiResponse<()>>)> {
    match playwright::get_script(&id) {
        Some(script) => Ok(Json(ApiResponse::success(script))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Playwright script not found: {}", id))),
        )),
    }
}

/// Create a new Playwright script
async fn create_playwright_script(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<CreatePlaywrightScriptRequest>,
) -> Result<Json<ApiResponse<playwright::PlaywrightScript>>, (StatusCode, Json<ApiResponse<()>>)> {
    match playwright::create_script(
        request.name,
        request.description,
        request.ai_instructions,
        request.target_url,
        request.script_content,
        request.category,
        request.tags,
        request.timeout_seconds,
        request.display_mode,
        request.browser,
    ) {
        Ok(script) => Ok(Json(ApiResponse::success(script))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Update an existing Playwright script
async fn update_playwright_script(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<UpdatePlaywrightScriptRequest>,
) -> Result<Json<ApiResponse<playwright::PlaywrightScript>>, (StatusCode, Json<ApiResponse<()>>)> {
    match playwright::update_script(
        &id,
        request.name,
        request.description,
        request.ai_instructions,
        request.target_url,
        request.script_content,
        request.category,
        request.tags,
        request.timeout_seconds,
        request.display_mode,
        request.browser,
    ) {
        Ok(script) => Ok(Json(ApiResponse::success(script))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Delete a Playwright script
async fn delete_playwright_script(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match playwright::delete_script(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Run a Playwright script
async fn run_playwright_script(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<RunPlaywrightScriptRequest>,
) -> Result<Json<ApiResponse<playwright::PlaywrightResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    let target_url_override = request.target_url_override;

    // Run in spawn_blocking since it's a blocking operation
    let result =
        tokio::task::spawn_blocking(move || playwright::run_script(&id, target_url_override))
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Task error: {}", e))),
                )
            })?;

    match result {
        Ok(play_result) => Ok(Json(ApiResponse::success(play_result))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Get Playwright script categories
async fn get_playwright_categories(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let categories = playwright::get_categories();
    Ok(Json(ApiResponse::success(categories)))
}

/// Get Playwright script tags
async fn get_playwright_tags(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let tags = playwright::get_all_tags();
    Ok(Json(ApiResponse::success(tags)))
}

/// Search Playwright scripts
async fn search_playwright_scripts(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<playwright::PlaywrightScript>>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let query = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let results = playwright::search_scripts(query);
    Ok(Json(ApiResponse::success(results)))
}

/// Import Playwright scripts
async fn import_playwright_scripts(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<ImportPlaywrightScriptsRequest>,
) -> Result<Json<ApiResponse<Vec<playwright::PlaywrightScript>>>, (StatusCode, Json<ApiResponse<()>>)>
{
    match playwright::import_scripts(&request.scripts_json) {
        Ok(scripts) => Ok(Json(ApiResponse::success(scripts))),
        Err(e) => Err((StatusCode::BAD_REQUEST, Json(api_error(e)))),
    }
}

/// Export all Playwright scripts
async fn export_playwright_scripts(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    match playwright::export_scripts() {
        Ok(json) => Ok(Json(ApiResponse::success(json))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Duplicate a Playwright script
async fn duplicate_playwright_script(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<DuplicatePlaywrightScriptRequest>,
) -> Result<Json<ApiResponse<playwright::PlaywrightScript>>, (StatusCode, Json<ApiResponse<()>>)> {
    match playwright::duplicate_script(&id, request.new_name) {
        Ok(script) => Ok(Json(ApiResponse::success(script))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Execute AI analysis via Claude CLI
async fn execute_claude_cli(
    cli_settings: &settings::ClaudeCliSettings,
    prompt: &str,
    app_handle: &tauri::AppHandle,
    action_id: &str,
) -> Result<TriggerAiAnalysisResponse, String> {
    write_ai_debug_log("execute_claude_cli: Starting");

    let system = std::env::consts::OS;
    write_ai_debug_log(&format!("execute_claude_cli: OS = {}", system));

    // Get the working directory (qontinui_parent_directory)
    let exe_path = std::env::current_exe().map_err(|e| {
        let err = format!("Failed to get exe path: {}", e);
        write_ai_debug_log(&format!("execute_claude_cli ERROR: {}", err));
        err
    })?;
    write_ai_debug_log(&format!("execute_claude_cli: exe_path = {:?}", exe_path));

    // Navigate from exe to qontinui_parent_directory
    let working_dir = exe_path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .unwrap_or_else(|| std::path::Path::new("."));

    let working_dir_str = working_dir.to_string_lossy().to_string();
    write_ai_debug_log(&format!(
        "execute_claude_cli: working_dir = {}",
        working_dir_str
    ));

    let prompt_owned = prompt.to_string();
    let prompt_len = prompt_owned.len();
    let custom_path = cli_settings.custom_path.clone();
    let execution_mode = cli_settings.execution_mode;
    let timeout_seconds = cli_settings.timeout_seconds;
    let app_handle = app_handle.clone();
    let action_id_owned = action_id.to_string();

    write_ai_debug_log(&format!(
        "execute_claude_cli: execution_mode = {:?}, custom_path = {:?}, prompt_len = {}, timeout = {}s",
        execution_mode, custom_path, prompt_len, timeout_seconds
    ));

    write_ai_debug_log("execute_claude_cli: Spawning blocking task...");

    // Spawn a blocking task to run the command
    let result = tokio::task::spawn_blocking(move || {
        write_ai_debug_log("execute_claude_cli: Inside spawn_blocking");

        // Determine execution mode
        let effective_mode = match execution_mode {
            CliExecutionMode::Auto => {
                if system == "windows" {
                    write_ai_debug_log("execute_claude_cli: Auto -> WindowsNative (Windows OS)");
                    CliExecutionMode::WindowsNative
                } else {
                    write_ai_debug_log("execute_claude_cli: Auto -> Native (non-Windows OS)");
                    CliExecutionMode::Native
                }
            }
            mode => {
                write_ai_debug_log(&format!(
                    "execute_claude_cli: Using specified mode: {:?}",
                    mode
                ));
                mode
            }
        };

        let result = match effective_mode {
            CliExecutionMode::WindowsNative | CliExecutionMode::Auto => {
                write_ai_debug_log("execute_claude_cli: Calling execute_windows_native");
                execute_windows_native(
                    &working_dir_str,
                    &prompt_owned,
                    custom_path.as_deref(),
                    &app_handle,
                    &action_id_owned,
                    timeout_seconds,
                )
            }
            CliExecutionMode::Wsl => {
                write_ai_debug_log("execute_claude_cli: Calling execute_via_wsl");
                execute_via_wsl(
                    &working_dir_str,
                    &prompt_owned,
                    custom_path.as_deref(),
                    &app_handle,
                    &action_id_owned,
                )
            }
            CliExecutionMode::Native => {
                write_ai_debug_log("execute_claude_cli: Calling execute_native");
                execute_native(
                    &working_dir_str,
                    &prompt_owned,
                    custom_path.as_deref(),
                    &app_handle,
                    &action_id_owned,
                )
            }
        };

        write_ai_debug_log(&format!(
            "execute_claude_cli: Execution result: {:?}",
            result.as_ref().map(|r| &r.success)
        ));
        result
    })
    .await;

    match result {
        Ok(inner_result) => {
            write_ai_debug_log("execute_claude_cli: spawn_blocking completed successfully");
            inner_result
        }
        Err(e) => {
            let err = format!("spawn_blocking error: {}", e);
            write_ai_debug_log(&format!("execute_claude_cli ERROR: {}", err));
            Err(err)
        }
    }
}

/// Extract text content from Claude CLI stream-json format
fn extract_text_from_stream_json(json_line: &str) -> Option<String> {
    // Parse the JSON line
    let parsed: serde_json::Value = serde_json::from_str(json_line).ok()?;

    // Handle different message types
    match parsed.get("type")?.as_str()? {
        "assistant" => {
            // Extract text from assistant message content
            let content = parsed.get("message")?.get("content")?.as_array()?;
            let mut text_parts = Vec::new();
            for item in content {
                if item.get("type")?.as_str()? == "text" {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        text_parts.push(text.to_string());
                    }
                }
            }
            if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.join(""))
            }
        }
        "content_block_delta" => {
            // Handle streaming deltas (partial text)
            parsed
                .get("delta")?
                .get("text")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
        }
        "result" => {
            // Final result - extract text from content blocks
            let content = parsed.get("result")?.get("content")?.as_array()?;
            let mut text_parts = Vec::new();
            for item in content {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        text_parts.push(text.to_string());
                    }
                }
            }
            if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.join(""))
            }
        }
        _ => None,
    }
}

/// Execute Claude CLI on Windows natively with real-time streaming output
fn execute_windows_native(
    working_dir: &str,
    prompt: &str,
    custom_path: Option<&str>,
    app_handle: &tauri::AppHandle,
    action_id: &str,
    timeout_seconds: u64,
) -> Result<TriggerAiAnalysisResponse, String> {
    use std::io::{BufRead, BufReader, Read};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    write_ai_debug_log("execute_windows_native: Starting (streaming mode)");
    write_ai_debug_log(&format!(
        "execute_windows_native: working_dir = {}, custom_path = {:?}, inactivity_timeout = {}s",
        working_dir, custom_path, timeout_seconds
    ));

    // Clone action_id for threads
    let action_id_owned = action_id.to_string();

    // Write prompt to a temp file to avoid shell escaping issues
    let temp_dir = std::env::temp_dir();
    let prompt_file = temp_dir.join("qontinui_ai_prompt.txt");

    std::fs::write(&prompt_file, prompt).map_err(|e| {
        let err = format!("Failed to write prompt file: {}", e);
        write_ai_debug_log(&format!("execute_windows_native ERROR: {}", err));
        err
    })?;

    let program = custom_path.unwrap_or("claude");
    write_ai_debug_log(&format!(
        "execute_windows_native: Using program = {}, prompt {} bytes",
        program,
        prompt.len()
    ));
    info!(
        "Running Claude Code on Windows via cmd.exe: {} with prompt from {:?}",
        program, prompt_file
    );

    // Read the prompt file
    let prompt_content =
        std::fs::read(&prompt_file).map_err(|e| format!("Failed to read prompt file: {}", e))?;

    // Spawn the process - use stream-json for real-time streaming output
    // Note: stream-json requires --verbose flag
    write_ai_debug_log("execute_windows_native: Spawning cmd.exe process with stream-json...");
    let spawn_result = std::panic::catch_unwind(|| {
        std::process::Command::new("cmd.exe")
            .args([
                "/c",
                program,
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "bypassPermissions",
            ])
            .current_dir(working_dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
    });

    let mut child = match spawn_result {
        Ok(Ok(child)) => {
            write_ai_debug_log("execute_windows_native: Process spawned successfully");
            child
        }
        Ok(Err(e)) => {
            let err = format!(
                "Failed to spawn {}: {}. Is Claude Code installed and in PATH?",
                program, e
            );
            write_ai_debug_log(&format!("execute_windows_native ERROR: {}", err));
            return Err(err);
        }
        Err(panic) => {
            let err = format!("PANIC during spawn: {:?}", panic);
            write_ai_debug_log(&format!("execute_windows_native PANIC: {}", err));
            return Err(err);
        }
    };

    // Write prompt to stdin
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        if let Err(e) = stdin.write_all(&prompt_content) {
            let err = format!("Failed to write to claude stdin: {}", e);
            write_ai_debug_log(&format!("execute_windows_native ERROR: {}", err));
            return Err(err);
        }
        write_ai_debug_log("execute_windows_native: Stdin written and closed");
    }

    // Track whether we've received any output (to control heartbeat)
    let has_output = Arc::new(AtomicBool::new(false));
    let has_output_heartbeat = has_output.clone();

    // Track the last time we received output (for inactivity timeout)
    // Store as epoch seconds (u64)
    use std::sync::atomic::AtomicU64;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let last_activity = Arc::new(AtomicU64::new(now_secs));
    let last_activity_stdout = last_activity.clone();

    // Start a heartbeat thread to show progress while waiting
    let app_handle_heartbeat = app_handle.clone();
    let action_id_heartbeat = action_id_owned.clone();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let start_time = Instant::now();

    debug_lifecycle::log_claude_cli("spawn", "starting heartbeat thread");
    let heartbeat_handle = thread::spawn(move || {
        debug_lifecycle::log_thread_start("claude_cli_heartbeat");
        let mut last_update = 0u64;
        loop {
            // Check if we should stop every 100ms
            if stop_rx.try_recv().is_ok() {
                debug_lifecycle::log_thread_end("claude_cli_heartbeat", "stop signal received");
                break;
            }
            thread::sleep(Duration::from_millis(100));

            // Only show heartbeat if we haven't received output yet
            if has_output_heartbeat.load(Ordering::Relaxed) {
                continue;
            }

            let elapsed_secs = start_time.elapsed().as_secs();
            // Update every 30 seconds
            if elapsed_secs > 0 && elapsed_secs % 30 == 0 && elapsed_secs != last_update {
                last_update = elapsed_secs;
                let mins = elapsed_secs / 60;
                let secs = elapsed_secs % 60;
                let msg = if mins > 0 {
                    format!("⏳ AI processing... ({}m {}s elapsed)", mins, secs)
                } else {
                    format!("⏳ AI processing... ({}s elapsed)", secs)
                };
                debug_lifecycle::log_ai_session("heartbeat", &msg);
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    emit_ai_output(
                        &app_handle_heartbeat,
                        &msg,
                        "status",
                        Some(&action_id_heartbeat),
                    );
                }));
            }
        }
    });

    // Read stdout in a separate thread - streaming JSON line by line
    let stdout = child.stdout.take();
    let app_handle_stdout = app_handle.clone();
    let action_id_stdout = action_id_owned.clone();
    let has_output_stdout = has_output.clone();

    debug_lifecycle::log_claude_cli("spawn", "starting stdout reader thread");
    let stdout_handle = thread::spawn(move || {
        debug_lifecycle::log_thread_start("claude_cli_stdout_reader");
        let mut all_text = String::new();
        let mut line_count = 0;
        let mut last_log_count = 0;
        if let Some(stdout) = stdout {
            let reader = BufReader::new(stdout);
            for line_result in reader.lines() {
                match line_result {
                    Ok(line) => {
                        line_count += 1;

                        // Update last activity time - we're receiving output
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        last_activity_stdout.store(now, Ordering::Relaxed);

                        // Log progress every 50 lines
                        if line_count - last_log_count >= 50 {
                            debug_lifecycle::log_claude_cli(
                                "stdout_progress",
                                &format!(
                                    "{} lines processed, {} chars",
                                    line_count,
                                    all_text.len()
                                ),
                            );
                            last_log_count = line_count;
                        }

                        // Try to extract text from the JSON line
                        if let Some(text) = extract_text_from_stream_json(&line) {
                            // Mark that we've received output
                            has_output_stdout.store(true, Ordering::Relaxed);

                            if !text.is_empty() {
                                // Emit the extracted text immediately
                                let _ =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        emit_ai_output(
                                            &app_handle_stdout,
                                            &text,
                                            "claude",
                                            Some(&action_id_stdout),
                                        );
                                    }));
                                all_text.push_str(&text);
                                write_ai_debug_log(&format!(
                                    "execute_windows_native: stream line {} ({} chars)",
                                    line_count,
                                    all_text.len()
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        debug_lifecycle::log_claude_cli(
                            "stdout_error",
                            &format!("Error reading line: {}", e),
                        );
                        write_ai_debug_log(&format!(
                            "execute_windows_native: Error reading stdout line: {}",
                            e
                        ));
                        break;
                    }
                }
            }
        }
        debug_lifecycle::log_thread_end(
            "claude_cli_stdout_reader",
            &format!("{} lines, {} chars", line_count, all_text.len()),
        );
        write_ai_debug_log(&format!(
            "execute_windows_native: stdout complete - {} JSON lines, {} chars extracted",
            line_count,
            all_text.len()
        ));
        all_text
    });

    // Read stderr in a separate thread (collect all at once, usually small)
    let stderr = child.stderr.take();

    debug_lifecycle::log_claude_cli("spawn", "starting stderr reader thread");
    let stderr_handle = thread::spawn(move || {
        debug_lifecycle::log_thread_start("claude_cli_stderr_reader");
        let mut output = String::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_string(&mut output);
        }
        debug_lifecycle::log_thread_end(
            "claude_cli_stderr_reader",
            &format!("{} chars", output.len()),
        );
        output
    });

    // Wait for process to complete with inactivity timeout
    // The process will only be killed if it stops producing output for timeout_seconds
    debug_lifecycle::log_claude_cli(
        "wait",
        &format!(
            "waiting for Claude CLI process to complete (inactivity timeout: {}s)",
            timeout_seconds
        ),
    );

    let status = loop {
        // Check for inactivity - only timeout if no output received for timeout_seconds
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let last_activity_secs = last_activity.load(Ordering::Relaxed);
        let inactive_secs = now_secs.saturating_sub(last_activity_secs);

        if inactive_secs > timeout_seconds {
            debug_lifecycle::log_claude_cli(
                "timeout",
                &format!(
                    "Process inactive for {}s (> {}s threshold), killing...",
                    inactive_secs, timeout_seconds
                ),
            );
            write_ai_debug_log(&format!(
                "execute_windows_native: INACTIVITY TIMEOUT - no output for {}s (threshold: {}s), killing process",
                inactive_secs, timeout_seconds
            ));

            // Kill the process
            if let Err(e) = child.kill() {
                debug_lifecycle::log_claude_cli("error", &format!("Failed to kill process: {}", e));
            }

            // Wait a bit for the process to terminate
            thread::sleep(Duration::from_millis(500));

            // Try to reap the process
            let _ = child.try_wait();

            // Stop heartbeat
            let _ = stop_tx.send(());
            let _ = heartbeat_handle.join();

            // Emit timeout message to UI
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                emit_ai_output(
                    app_handle,
                    &format!(
                        "⏰ AI analysis stopped - no response for {} seconds",
                        inactive_secs
                    ),
                    "status",
                    Some(&action_id_owned),
                );
            }));

            // Cleanup temp file
            let _ = std::fs::remove_file(&prompt_file);

            return Ok(TriggerAiAnalysisResponse {
                success: false,
                message: "AI analysis stopped due to inactivity".to_string(),
                error: Some(format!(
                    "Claude CLI process was unresponsive for {} seconds and was killed",
                    inactive_secs
                )),
                output: None,
            });
        }

        // Try to check if process has exited (non-blocking)
        match child.try_wait() {
            Ok(Some(status)) => {
                debug_lifecycle::log_claude_cli(
                    "wait",
                    &format!("process exited with status: {:?}", status),
                );
                break status;
            }
            Ok(None) => {
                // Process still running, wait a bit and check again
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                debug_lifecycle::log_claude_cli("error", &format!("try_wait failed: {}", e));
                let _ = stop_tx.send(());
                let _ = heartbeat_handle.join();
                let _ = std::fs::remove_file(&prompt_file);
                return Err(format!("Failed to wait for claude: {}", e));
            }
        }
    };

    // Stop heartbeat
    debug_lifecycle::log_claude_cli("cleanup", "stopping heartbeat thread");
    let _ = stop_tx.send(());
    let _ = heartbeat_handle.join();
    debug_lifecycle::log_claude_cli("cleanup", "heartbeat thread joined");

    // Get output from threads
    debug_lifecycle::log_claude_cli("cleanup", "joining stdout thread");
    let all_output = stdout_handle.join().unwrap_or_default();
    debug_lifecycle::log_claude_cli("cleanup", "stdout thread joined");

    debug_lifecycle::log_claude_cli("cleanup", "joining stderr thread");
    let stderr_output = stderr_handle.join().unwrap_or_default();
    debug_lifecycle::log_claude_cli("cleanup", "stderr thread joined");

    let elapsed = start_time.elapsed();
    debug_lifecycle::log_claude_cli(
        "complete",
        &format!(
            "finished in {:.1}s, {} chars output",
            elapsed.as_secs_f64(),
            all_output.len()
        ),
    );
    write_ai_debug_log(&format!(
        "execute_windows_native: Process completed in {:.1}s, output {} chars, stderr {} chars",
        elapsed.as_secs_f64(),
        all_output.len(),
        stderr_output.len()
    ));

    // Emit stderr if any (this is usually error messages, so emit at end)
    if !stderr_output.is_empty() {
        for line in stderr_output.lines() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                emit_ai_output(
                    app_handle,
                    &format!("[stderr] {}", line),
                    "claude",
                    Some(&action_id_owned),
                );
            }));
        }
    }

    // Cleanup
    let _ = std::fs::remove_file(&prompt_file);

    if status.success() {
        write_ai_debug_log("execute_windows_native: SUCCESS");
        Ok(TriggerAiAnalysisResponse {
            success: true,
            message: "AI analysis completed successfully".to_string(),
            error: None,
            output: Some(all_output),
        })
    } else {
        write_ai_debug_log(&format!(
            "execute_windows_native: FAILED with code {:?}",
            status.code()
        ));
        Ok(TriggerAiAnalysisResponse {
            success: false,
            message: "AI analysis failed".to_string(),
            error: Some(if stderr_output.is_empty() {
                format!("Claude Code exited with code {:?}", status.code())
            } else {
                stderr_output
            }),
            output: Some(all_output),
        })
    }
}

/// Execute Claude CLI via WSL with real-time streaming output
fn execute_via_wsl(
    working_dir: &str,
    prompt: &str,
    custom_path: Option<&str>,
    app_handle: &tauri::AppHandle,
    action_id: &str,
) -> Result<TriggerAiAnalysisResponse, String> {
    use std::io::{BufRead, BufReader, Read};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    let action_id_owned = action_id.to_string();
    write_ai_debug_log("execute_via_wsl: Starting");

    // Convert Windows path to WSL path
    let wsl_working_dir = working_dir.replace('\\', "/").replace("C:", "/mnt/c");
    let program = custom_path.unwrap_or("claude");

    write_ai_debug_log(&format!(
        "execute_via_wsl: wsl_working_dir = {}, program = {}",
        wsl_working_dir, program
    ));

    info!(
        "Running Claude Code via WSL: {} in {}",
        program, wsl_working_dir
    );

    // Write prompt to a temp file
    let temp_dir = std::env::temp_dir();
    let prompt_file = temp_dir.join("qontinui_ai_prompt.txt");
    std::fs::write(&prompt_file, prompt)
        .map_err(|e| format!("Failed to write prompt file: {}", e))?;

    // Convert temp file path to WSL path
    let wsl_prompt_file = prompt_file
        .to_string_lossy()
        .replace('\\', "/")
        .replace("C:", "/mnt/c");

    // Use bash to read the file and pipe to claude with stream-json for real-time output
    // Note: stream-json requires --verbose flag
    let bash_command = format!(
        "cd '{}' && cat '{}' | {} --output-format stream-json --verbose --permission-mode bypassPermissions",
        wsl_working_dir, wsl_prompt_file, program
    );

    write_ai_debug_log("execute_via_wsl: Spawning WSL process...");
    let mut child = std::process::Command::new("wsl")
        .args(["bash", "-c", &bash_command])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run WSL: {}. Is WSL installed?", e))?;

    // Track whether we've received any output (to control heartbeat)
    let has_output = Arc::new(AtomicBool::new(false));
    let has_output_heartbeat = has_output.clone();

    // Start a heartbeat thread to show progress while waiting
    let app_handle_heartbeat = app_handle.clone();
    let action_id_heartbeat = action_id_owned.clone();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let start_time = Instant::now();

    let heartbeat_handle = thread::spawn(move || {
        let mut last_update = 0u64;
        loop {
            // Check if we should stop every 100ms
            if stop_rx.try_recv().is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(100));

            // Only show heartbeat if we haven't received output yet
            if has_output_heartbeat.load(Ordering::Relaxed) {
                continue;
            }

            let elapsed_secs = start_time.elapsed().as_secs();
            // Update every 30 seconds
            if elapsed_secs > 0 && elapsed_secs % 30 == 0 && elapsed_secs != last_update {
                last_update = elapsed_secs;
                let mins = elapsed_secs / 60;
                let secs = elapsed_secs % 60;
                let msg = if mins > 0 {
                    format!("⏳ AI processing... ({}m {}s elapsed)", mins, secs)
                } else {
                    format!("⏳ AI processing... ({}s elapsed)", secs)
                };
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    emit_ai_output(
                        &app_handle_heartbeat,
                        &msg,
                        "status",
                        Some(&action_id_heartbeat),
                    );
                }));
            }
        }
    });

    // Read stdout in a separate thread - streaming line by line
    let stdout = child.stdout.take();
    let app_handle_stdout = app_handle.clone();
    let action_id_stdout = action_id_owned.clone();
    let has_output_stdout = has_output.clone();

    let stdout_handle = thread::spawn(move || {
        let mut all_text = String::new();
        let mut line_count = 0;
        if let Some(stdout) = stdout {
            let reader = BufReader::new(stdout);
            for line_result in reader.lines() {
                match line_result {
                    Ok(line) => {
                        line_count += 1;

                        // Try to extract text from the JSON line
                        if let Some(text) = extract_text_from_stream_json(&line) {
                            // Mark that we've received output
                            has_output_stdout.store(true, Ordering::Relaxed);

                            if !text.is_empty() {
                                // Emit the extracted text immediately
                                let _ =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        emit_ai_output(
                                            &app_handle_stdout,
                                            &text,
                                            "claude",
                                            Some(&action_id_stdout),
                                        );
                                    }));
                                all_text.push_str(&text);
                            }
                        }
                    }
                    Err(e) => {
                        write_ai_debug_log(&format!(
                            "execute_via_wsl: Error reading stdout line: {}",
                            e
                        ));
                        break;
                    }
                }
            }
        }
        write_ai_debug_log(&format!(
            "execute_via_wsl: stdout complete - {} JSON lines, {} chars extracted",
            line_count,
            all_text.len()
        ));
        all_text
    });

    // Read stderr in a separate thread (collect all at once, usually small)
    let stderr = child.stderr.take();

    let stderr_handle = thread::spawn(move || {
        let mut output = String::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_string(&mut output);
        }
        output
    });

    // Wait for process to complete
    let status = match child.wait() {
        Ok(s) => s,
        Err(e) => {
            let _ = stop_tx.send(());
            let _ = heartbeat_handle.join();
            return Err(format!("Failed to wait for WSL: {}", e));
        }
    };

    // Stop heartbeat
    let _ = stop_tx.send(());
    let _ = heartbeat_handle.join();

    // Get output from threads
    let all_output = stdout_handle.join().unwrap_or_default();
    let stderr_output = stderr_handle.join().unwrap_or_default();

    let elapsed = start_time.elapsed();
    write_ai_debug_log(&format!(
        "execute_via_wsl: Process completed in {:.1}s, output {} chars, stderr {} chars",
        elapsed.as_secs_f64(),
        all_output.len(),
        stderr_output.len()
    ));

    // Emit stderr if any (this is usually error messages, so emit at end)
    if !stderr_output.is_empty() {
        for line in stderr_output.lines() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                emit_ai_output(
                    app_handle,
                    &format!("[stderr] {}", line),
                    "claude",
                    Some(&action_id_owned),
                );
            }));
        }
    }

    // Cleanup
    let _ = std::fs::remove_file(&prompt_file);

    if status.success() {
        write_ai_debug_log("execute_via_wsl: SUCCESS");
        Ok(TriggerAiAnalysisResponse {
            success: true,
            message: "AI analysis completed successfully".to_string(),
            error: None,
            output: Some(all_output),
        })
    } else {
        write_ai_debug_log(&format!(
            "execute_via_wsl: FAILED with code {:?}",
            status.code()
        ));
        Ok(TriggerAiAnalysisResponse {
            success: false,
            message: "AI analysis failed".to_string(),
            error: Some(if stderr_output.is_empty() {
                format!("Claude Code exited with code {:?}", status.code())
            } else {
                stderr_output
            }),
            output: Some(all_output),
        })
    }
}

/// Execute Claude CLI natively (Unix/macOS/Linux) with real-time streaming output
fn execute_native(
    working_dir: &str,
    prompt: &str,
    custom_path: Option<&str>,
    app_handle: &tauri::AppHandle,
    action_id: &str,
) -> Result<TriggerAiAnalysisResponse, String> {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    let action_id_owned = action_id.to_string();
    write_ai_debug_log("execute_native: Starting");

    let program = custom_path.unwrap_or("claude");
    info!("Running Claude Code natively: {}", program);

    // Write prompt to a temp file
    let temp_dir = std::env::temp_dir();
    let prompt_file = temp_dir.join("qontinui_ai_prompt.txt");
    std::fs::write(&prompt_file, prompt)
        .map_err(|e| format!("Failed to write prompt file: {}", e))?;

    let prompt_content =
        std::fs::read(&prompt_file).map_err(|e| format!("Failed to read prompt file: {}", e))?;

    // Note: stream-json requires --verbose flag
    write_ai_debug_log("execute_native: Spawning process with stream-json...");
    let mut child = std::process::Command::new(program)
        .args([
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-mode",
            "bypassPermissions",
        ])
        .current_dir(working_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "Failed to spawn {}: {}. Is Claude Code installed and in PATH?",
                program, e
            )
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&prompt_content)
            .map_err(|e| format!("Failed to write to claude stdin: {}", e))?;
    }

    // Track whether we've received any output (to control heartbeat)
    let has_output = Arc::new(AtomicBool::new(false));
    let has_output_heartbeat = has_output.clone();

    // Start a heartbeat thread to show progress while waiting
    let app_handle_heartbeat = app_handle.clone();
    let action_id_heartbeat = action_id_owned.clone();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let start_time = Instant::now();

    let heartbeat_handle = thread::spawn(move || {
        let mut last_update = 0u64;
        loop {
            // Check if we should stop every 100ms
            if stop_rx.try_recv().is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(100));

            // Only show heartbeat if we haven't received output yet
            if has_output_heartbeat.load(Ordering::Relaxed) {
                continue;
            }

            let elapsed_secs = start_time.elapsed().as_secs();
            // Update every 30 seconds
            if elapsed_secs > 0 && elapsed_secs % 30 == 0 && elapsed_secs != last_update {
                last_update = elapsed_secs;
                let mins = elapsed_secs / 60;
                let secs = elapsed_secs % 60;
                let msg = if mins > 0 {
                    format!("⏳ AI processing... ({}m {}s elapsed)", mins, secs)
                } else {
                    format!("⏳ AI processing... ({}s elapsed)", secs)
                };
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    emit_ai_output(
                        &app_handle_heartbeat,
                        &msg,
                        "status",
                        Some(&action_id_heartbeat),
                    );
                }));
            }
        }
    });

    // Read stdout in a separate thread - streaming line by line
    let stdout = child.stdout.take();
    let app_handle_stdout = app_handle.clone();
    let action_id_stdout = action_id_owned.clone();
    let has_output_stdout = has_output.clone();

    let stdout_handle = thread::spawn(move || {
        let mut all_text = String::new();
        let mut line_count = 0;
        if let Some(stdout) = stdout {
            let reader = BufReader::new(stdout);
            for line_result in reader.lines() {
                match line_result {
                    Ok(line) => {
                        line_count += 1;

                        // Try to extract text from the JSON line
                        if let Some(text) = extract_text_from_stream_json(&line) {
                            // Mark that we've received output
                            has_output_stdout.store(true, Ordering::Relaxed);

                            if !text.is_empty() {
                                // Emit the extracted text immediately
                                let _ =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        emit_ai_output(
                                            &app_handle_stdout,
                                            &text,
                                            "claude",
                                            Some(&action_id_stdout),
                                        );
                                    }));
                                all_text.push_str(&text);
                            }
                        }
                    }
                    Err(e) => {
                        write_ai_debug_log(&format!(
                            "execute_native: Error reading stdout line: {}",
                            e
                        ));
                        break;
                    }
                }
            }
        }
        write_ai_debug_log(&format!(
            "execute_native: stdout complete - {} JSON lines, {} chars extracted",
            line_count,
            all_text.len()
        ));
        all_text
    });

    // Read stderr in a separate thread (collect all at once, usually small)
    let stderr = child.stderr.take();

    let stderr_handle = thread::spawn(move || {
        let mut output = String::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_string(&mut output);
        }
        output
    });

    // Wait for process to complete
    let status = match child.wait() {
        Ok(s) => s,
        Err(e) => {
            let _ = stop_tx.send(());
            let _ = heartbeat_handle.join();
            return Err(format!("Failed to wait for {}: {}", program, e));
        }
    };

    // Stop heartbeat
    let _ = stop_tx.send(());
    let _ = heartbeat_handle.join();

    // Get output from threads
    let all_output = stdout_handle.join().unwrap_or_default();
    let stderr_output = stderr_handle.join().unwrap_or_default();

    let elapsed = start_time.elapsed();
    write_ai_debug_log(&format!(
        "execute_native: Process completed in {:.1}s, output {} chars, stderr {} chars",
        elapsed.as_secs_f64(),
        all_output.len(),
        stderr_output.len()
    ));

    // Emit stderr if any (this is usually error messages, so emit at end)
    if !stderr_output.is_empty() {
        for line in stderr_output.lines() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                emit_ai_output(
                    app_handle,
                    &format!("[stderr] {}", line),
                    "claude",
                    Some(&action_id_owned),
                );
            }));
        }
    }

    // Cleanup
    let _ = std::fs::remove_file(&prompt_file);

    if status.success() {
        write_ai_debug_log("execute_native: SUCCESS");
        Ok(TriggerAiAnalysisResponse {
            success: true,
            message: "AI analysis completed successfully".to_string(),
            error: None,
            output: Some(all_output),
        })
    } else {
        write_ai_debug_log(&format!(
            "execute_native: FAILED with code {:?}",
            status.code()
        ));
        Ok(TriggerAiAnalysisResponse {
            success: false,
            message: "AI analysis failed".to_string(),
            error: Some(if stderr_output.is_empty() {
                format!("Claude Code exited with code {:?}", status.code())
            } else {
                stderr_output
            }),
            output: Some(all_output),
        })
    }
}

/// Execute AI analysis via Claude API (direct HTTP calls)
async fn execute_claude_api(
    api_settings: &settings::ClaudeApiSettings,
    prompt: &str,
    app_handle: &tauri::AppHandle,
    action_id: &str,
) -> Result<TriggerAiAnalysisResponse, String> {
    // Get API key from keychain
    let api_key = ai_settings::get_provider_api_key("claude_api")?.ok_or_else(|| {
        "No API key configured. Please configure your Claude API key in Settings > AI.".to_string()
    })?;

    info!("Calling Claude API with model: {}", api_settings.model);

    let client = reqwest::Client::new();
    let response = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "model": api_settings.model,
            "max_tokens": api_settings.max_tokens,
            "messages": [{"role": "user", "content": prompt}]
        }))
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if response.status().is_success() {
        let response_body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse API response: {}", e))?;

        // Extract the text content from the response
        let content = response_body["content"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|c| c["text"].as_str())
            .unwrap_or("No content in response");

        // Emit response to frontend (emit line by line for consistency)
        for line in content.lines() {
            emit_ai_output(app_handle, line, "claude", Some(action_id));
        }

        Ok(TriggerAiAnalysisResponse {
            success: true,
            message: "AI analysis completed successfully".to_string(),
            error: None,
            output: Some(content.to_string()),
        })
    } else {
        let status = response.status();
        let error_body = response.text().await.unwrap_or_default();

        let error_message = if status.as_u16() == 401 {
            "Invalid API key. Please check your API key in Settings > AI.".to_string()
        } else if status.as_u16() == 429 {
            "Rate limited. Please wait and try again.".to_string()
        } else {
            format!("API error ({}): {}", status, error_body)
        };

        // Emit error to frontend
        emit_ai_output(
            app_handle,
            &format!("Error: {}", error_message),
            "claude",
            Some(action_id),
        );

        Ok(TriggerAiAnalysisResponse {
            success: false,
            message: "API call failed".to_string(),
            error: Some(error_message),
            output: None,
        })
    }
}

// ============================================================================
// Independent Session Helpers (for workflow prompts)
// ============================================================================

/// Result of spawning an independent workflow session
#[derive(Debug)]
struct IndependentSessionResult {
    session_id: String,
    log_file: std::path::PathBuf,
    completion_marker: std::path::PathBuf,
}

/// Spawn a workflow session as an independent process using spawn-independent-claude.py.
/// The session will continue running even if the runner restarts.
fn spawn_workflow_session_independent(
    prompt: &str,
    session_id: &str,
    app_handle: &tauri::AppHandle,
    action_id: &str,
) -> Result<IndependentSessionResult, String> {
    info!(
        "Spawning independent workflow session: {} (action_id: {})",
        session_id, action_id
    );
    log_workflow_event(
        action_id,
        "session_spawn_independent",
        &format!("Spawning independent session {}", session_id),
    );

    // Get workspace paths
    let (workspace_root, dev_logs_path, scripts_path) = get_workspace_paths_internal()?;
    let spawn_script = scripts_path.join("spawn-independent-claude.py");

    if !spawn_script.exists() {
        return Err(format!(
            "spawn-independent-claude.py not found at {:?}",
            spawn_script
        ));
    }

    // Write prompt to file
    let prompt_file = dev_logs_path.join(format!("workflow-session-{}-prompt.txt", session_id));
    std::fs::write(&prompt_file, prompt)
        .map_err(|e| format!("Failed to write prompt file: {}", e))?;

    // Define output paths
    let log_file = dev_logs_path.join(format!("claude-session-{}.log", session_id));
    let completion_marker = dev_logs_path.join(format!("claude-session-{}.completed", session_id));

    // Remove old completion marker if it exists
    let _ = std::fs::remove_file(&completion_marker);

    // Emit status to frontend
    emit_ai_output(
        app_handle,
        &format!("🚀 Spawning independent session {}...", session_id),
        "status",
        Some(action_id),
    );

    // Spawn using Python script
    // Use spawn_python_with_console to ensure Claude CLI gets a console window
    let spawn_result = spawn_python_with_console(
        "python",
        &[
            spawn_script.as_os_str(),
            std::ffi::OsStr::new("--file"),
            prompt_file.as_os_str(),
            std::ffi::OsStr::new("--session-id"),
            std::ffi::OsStr::new(session_id),
        ],
        &workspace_root,
    );

    match spawn_result {
        Ok(child) => {
            info!(
                "Independent session {} spawned with PID: {}",
                session_id,
                child.id()
            );
            log_workflow_event(
                action_id,
                "session_spawned",
                &format!("Session {} spawned with PID {}", session_id, child.id()),
            );

            emit_ai_output(
                app_handle,
                &format!(
                    "✅ Session {} started (PID: {}). Output: {:?}",
                    session_id,
                    child.id(),
                    log_file
                ),
                "status",
                Some(action_id),
            );

            Ok(IndependentSessionResult {
                session_id: session_id.to_string(),
                log_file,
                completion_marker,
            })
        }
        Err(e) => {
            error!("Failed to spawn independent session: {}", e);
            Err(format!("Failed to spawn independent session: {}", e))
        }
    }
}

/// Wait for an independent session to complete by polling the completion marker file.
/// Returns Ok(exit_code) when session completes, or Err if timeout.
async fn wait_for_session_completion(
    completion_marker: &std::path::Path,
    app_handle: &tauri::AppHandle,
    action_id: &str,
    timeout_secs: u64,
    poll_interval_secs: u64,
) -> Result<i32, String> {
    use tokio::time::{sleep, Duration};

    let start_time = std::time::Instant::now();
    let mut last_status_update = std::time::Instant::now();

    info!(
        "Waiting for session completion (marker: {:?}, timeout: {}s)",
        completion_marker, timeout_secs
    );

    loop {
        // Check if completion marker exists
        if completion_marker.exists() {
            // Read and parse the completion marker
            match std::fs::read_to_string(completion_marker) {
                Ok(content) => {
                    info!("Session completed. Marker content: {}", content);

                    // Parse JSON to get exit code
                    let exit_code =
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                            json.get("exit_code")
                                .and_then(|v| v.as_i64())
                                .map(|v| v as i32)
                                .unwrap_or(0)
                        } else {
                            0
                        };

                    emit_ai_output(
                        app_handle,
                        &format!("✅ Session completed (exit code: {})", exit_code),
                        "status",
                        Some(action_id),
                    );

                    return Ok(exit_code);
                }
                Err(e) => {
                    warn!("Failed to read completion marker: {}", e);
                    // File might be being written, wait a bit
                    sleep(Duration::from_millis(500)).await;
                    continue;
                }
            }
        }

        // Check timeout
        let elapsed = start_time.elapsed();
        if elapsed.as_secs() >= timeout_secs {
            error!("Session timed out after {}s", timeout_secs);
            emit_ai_output(
                app_handle,
                &format!("⏰ Session timed out after {}s", timeout_secs),
                "status",
                Some(action_id),
            );
            return Err(format!("Session timed out after {}s", timeout_secs));
        }

        // Emit periodic status updates (every 60 seconds)
        if last_status_update.elapsed().as_secs() >= 60 {
            let mins = elapsed.as_secs() / 60;
            let secs = elapsed.as_secs() % 60;
            emit_ai_output(
                app_handle,
                &format!("⏳ Session running... ({}m {}s elapsed)", mins, secs),
                "status",
                Some(action_id),
            );
            last_status_update = std::time::Instant::now();
        }

        // Wait before next poll
        sleep(Duration::from_secs(poll_interval_secs)).await;
    }
}

/// Clean up session files after completion
fn cleanup_session_files(session_id: &str) {
    let (_, dev_logs_path, _) = match get_workspace_paths_internal() {
        Ok(paths) => paths,
        Err(_) => return,
    };

    // Remove completion marker (keep log file for debugging)
    let completion_marker = dev_logs_path.join(format!("claude-session-{}.completed", session_id));
    let _ = std::fs::remove_file(&completion_marker);

    // Optionally remove prompt file
    let prompt_file = dev_logs_path.join(format!("workflow-session-{}-prompt.txt", session_id));
    let _ = std::fs::remove_file(&prompt_file);
}

// ============================================================================
// Unified Session API Handlers
// ============================================================================

use crate::session_manager::{Session, SessionConfig, SessionStatus, SessionType};

/// Request to start a new unified session
#[derive(Debug, Deserialize)]
struct StartSessionRequest {
    /// Session type
    session_type: String, // "prompt_workflow", "ai_builder", "one_shot"
    /// Session name (for display)
    name: String,
    /// Initial prompt content
    prompt: String,
    /// Prompt for continuation (if different)
    continuation_prompt: Option<String>,
    /// Total phases/iterations (0 = unlimited)
    #[serde(default)]
    total_phases: u32,
    /// Whether this session uses GUI automation
    #[serde(default)]
    uses_gui: bool,
    /// Timeout per phase in seconds (default 1800 = 30 min)
    #[serde(default = "default_session_timeout")]
    timeout_seconds: u64,
}

fn default_session_timeout() -> u64 {
    1800
}

/// Response from starting a session
#[derive(Debug, Serialize)]
struct StartSessionResponse {
    session: Session,
}

/// List all unified sessions
async fn list_sessions(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<Session>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let sessions = state.session_manager.list_sessions().await;
    Ok(Json(ApiResponse::success(sessions)))
}

/// Get a specific session
async fn get_session(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<Session>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state.session_manager.get_session(&session_id).await {
        Some(session) => Ok(Json(ApiResponse::success(session))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Session {} not found", session_id))),
        )),
    }
}

/// Start a new unified session
async fn start_session(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartSessionRequest>,
) -> Result<Json<ApiResponse<StartSessionResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Parse session type
    let session_type = match request.session_type.as_str() {
        "prompt_workflow" => SessionType::PromptWorkflow,
        "ai_builder" => SessionType::AiBuilder,
        "one_shot" => SessionType::OneShot,
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!("Invalid session type: {}", other))),
            ))
        }
    };

    let config = SessionConfig {
        session_type,
        prompt: request.prompt,
        continuation_prompt: request.continuation_prompt,
        total_phases: request.total_phases,
        uses_gui: request.uses_gui,
        timeout_seconds: request.timeout_seconds,
        stall_threshold_seconds: 300,
        name: request.name,
        description: String::new(),
        custom_config: serde_json::json!({}),
    };

    match state.session_manager.start_session(config).await {
        Ok(session) => {
            info!("Started unified session: {}", session.id);

            // Spawn the execution loop
            let session_id = session.id.clone();
            let state_clone = state.clone();
            let workspace_root = get_workspace_paths_internal()
                .map(|(root, _, _)| root.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string());

            tokio::spawn(async move {
                run_unified_session_loop(state_clone, session_id, workspace_root).await;
            });

            Ok(Json(ApiResponse::success(StartSessionResponse { session })))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to start session: {}", e))),
        )),
    }
}

/// Stop a unified session
async fn stop_session(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<Option<Session>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let session = state
        .session_manager
        .stop_session(&session_id, "Stopped by user")
        .await;
    Ok(Json(ApiResponse::success(session)))
}

/// Delete a unified session
async fn delete_session(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    state.session_manager.remove_session(&session_id).await;
    Ok(Json(ApiResponse::success(())))
}

/// Run the unified session execution loop
async fn run_unified_session_loop(state: Arc<ApiState>, session_id: String, workspace_root: String) {
    let session = match state.session_manager.get_session(&session_id).await {
        Some(s) => s,
        None => {
            error!("Session {} not found for execution", session_id);
            return;
        }
    };

    let config = session.config.clone();
    let timeout = config.timeout_seconds;
    let mut current_prompt = config.prompt.clone();
    let continuation_prompt = config.continuation_prompt.clone();
    let app_handle = state.app_handle.clone();

    info!(
        "Starting unified session loop for {} ({:?})",
        session_id, config.session_type
    );

    let mut phase = 0u32;

    loop {
        phase += 1;
        let phase_session_id = if phase == 1 {
            format!("session-{}", &session_id[..8.min(session_id.len())])
        } else {
            format!(
                "session-{}-phase-{}",
                &session_id[..8.min(session_id.len())],
                phase
            )
        };

        // Update session state to running
        if let Some(mut s) = state.session_manager.get_session(&session_id).await {
            s.status = SessionStatus::Running;
            s.checkpoint.current_phase = phase;
            s.checkpoint.sessions_spawned += 1;
            s.checkpoint.status = "running".to_string();
            s.log_event("phase_started", &format!("Phase {} started", phase));
            let _ = state.session_manager.update_session(s).await;
        }

        emit_ai_output(
            &app_handle,
            &format!("🚀 Running phase {} (session {})...", phase, phase_session_id),
            "status",
            Some(&session_id),
        );

        // Run Claude session
        let workspace = workspace_root.clone();
        let prompt = current_prompt.clone();
        let sid = phase_session_id.clone();
        let handle = app_handle.clone();
        let timeout_secs = timeout;

        let result = tokio::task::spawn_blocking(move || {
            run_claude_session_inline(&workspace, &prompt, &sid, &handle, timeout_secs)
        })
        .await;

        let session_result = match result {
            Ok(Ok((success, output))) => Ok((success, output)),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(format!("Task join error: {}", e)),
        };

        match session_result {
            Ok((success, output)) => {
                if !success {
                    warn!("Phase {} completed with errors, continuing...", phase);
                }

                // Check session checkpoint for completion
                if let Some(mut s) = state.session_manager.get_session(&session_id).await {
                    if s.checkpoint.is_complete() {
                        s.status = SessionStatus::Completed;
                        s.checkpoint.mark_completed();
                        let _ = state.session_manager.update_session(s).await;

                        emit_ai_output(
                            &app_handle,
                            &format!("✅ Session {} completed successfully", session_id),
                            "status",
                            Some(&session_id),
                        );
                        info!("Session {} completed after {} phases", session_id, phase);
                        return;
                    }

                    // Check if max phases reached
                    if config.total_phases > 0 && phase >= config.total_phases {
                        s.status = SessionStatus::Completed;
                        s.checkpoint.completed = true;
                        s.checkpoint.status = "completed".to_string();
                        let _ = state.session_manager.update_session(s).await;

                        emit_ai_output(
                            &app_handle,
                            &format!(
                                "✅ Session {} completed (reached {} phases)",
                                session_id, phase
                            ),
                            "status",
                            Some(&session_id),
                        );
                        return;
                    }

                    // Prepare continuation prompt
                    if let Some(ref cont_prompt) = continuation_prompt {
                        current_prompt = cont_prompt
                            .replace("{phase}", &phase.to_string())
                            .replace("{output}", &output);
                    }

                    s.status = SessionStatus::WaitingForContinuation;
                    s.log_event(
                        "phase_completed",
                        &format!("Phase {} completed, continuing...", phase),
                    );
                    let _ = state.session_manager.update_session(s).await;
                }
            }
            Err(e) => {
                error!("Phase {} failed: {}", phase, e);

                if let Some(mut s) = state.session_manager.get_session(&session_id).await {
                    s.status = SessionStatus::Failed;
                    s.checkpoint.mark_failed(&e);
                    let _ = state.session_manager.update_session(s).await;
                }

                emit_ai_output(
                    &app_handle,
                    &format!("❌ Session {} failed: {}", session_id, e),
                    "error",
                    Some(&session_id),
                );
                return;
            }
        }

        // Persist state for restart recovery
        let _ = state.session_manager.persist_state().await;
    }
}

#[derive(Debug, Serialize)]
struct DeleteAllResponse {
    deleted_count: usize,
}

// DEPRECATED: Old resume_workflow_monitoring function removed
// Session resume is now handled by SessionManager::restore_state()

/// Create the API router
pub fn create_router(
    app_state: Arc<AppState>,
    rag_state: Arc<RAGState>,
    app_handle: tauri::AppHandle,
) -> Router {
    // Get dev_logs path for session manager
    let dev_logs_path = get_workspace_paths_internal()
        .map(|(_, dev_logs, _)| dev_logs)
        .unwrap_or_else(|_| std::path::PathBuf::from(".dev-logs"));

    // Ensure dev_logs directory exists
    let _ = std::fs::create_dir_all(&dev_logs_path);

    let api_state = Arc::new(ApiState {
        app_state,
        rag_state,
        app_handle: app_handle.clone(),
        ai_analysis_running: AtomicBool::new(false),
        ai_analysis_stop_requested: AtomicBool::new(false),
        session_manager: Arc::new(SessionManager::new(dev_logs_path)),
        ai_developer_manager: AiDeveloperManager::new(),
    });

    // Restore persisted session state on startup
    let state_for_restore = api_state.clone();
    tokio::spawn(async move {
        // Small delay to let the server fully start
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        // Restore unified session manager state
        if let Err(e) = state_for_restore.session_manager.restore_state().await {
            warn!("Failed to restore session state: {}", e);
        } else {
            let sessions = state_for_restore.session_manager.list_sessions().await;
            let active_count = sessions
                .iter()
                .filter(|s| {
                    matches!(
                        s.status,
                        crate::session_manager::SessionStatus::Running
                            | crate::session_manager::SessionStatus::WaitingForContinuation
                    )
                })
                .count();
            if active_count > 0 {
                info!(
                    "Restored {} session(s), {} active",
                    sessions.len(),
                    active_count
                );
            }
        }

        // Check for incomplete improve-all workflows and offer to resume
        resume_improve_all_workflow_on_startup(state_for_restore.clone()).await;
    });

    // Configure CORS to allow requests from WSL
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/health", get(health))
        .route("/launch-debug-chrome", post(launch_debug_chrome))
        .route("/status", get(get_status))
        .route("/monitors", get(get_monitors))
        .route("/load-config", post(load_config))
        .route("/load-last-config", post(load_last_config))
        .route("/run-workflow", post(run_workflow))
        .route("/stop-execution", post(stop_execution))
        // RAG routes
        .route("/rag/import", post(import_rag))
        .route("/rag/list", get(list_rag_configs))
        .route("/rag/availability", get(get_rag_availability))
        .route("/rag/segment", post(segment_screenshot))
        .route("/rag/:project_id/status", get(get_rag_status))
        .route("/rag/:project_id/load", post(load_rag_project))
        .route("/rag/:project_id", delete(delete_rag_config))
        // AI Analysis routes (standard/inline mode)
        .route("/trigger-ai-analysis", post(trigger_ai_analysis))
        .route("/stop-ai-analysis", post(stop_ai_analysis))
        // Runner restart route (for AI self-healing)
        .route("/restart-runner", post(restart_runner))
        // REMOVED: Old AI Developer routes - use /sessions API instead
        // Prompt Library routes
        .route("/prompts", get(list_prompts))
        .route("/prompts", post(create_prompt))
        .route("/prompts/search", get(search_prompts))
        .route("/prompts/categories", get(get_prompt_categories))
        .route("/prompts/tags", get(get_prompt_tags))
        .route("/prompts/import", post(import_prompts))
        .route("/prompts/export", get(export_prompts))
        .route("/prompts/:id", get(get_prompt))
        .route("/prompts/:id", put(update_prompt))
        .route("/prompts/:id", delete(delete_prompt))
        .route("/prompts/:id/run", post(run_prompt))
        .route("/prompts/:id/duplicate", post(duplicate_prompt))
        // AI Workflow Library routes
        .route("/ai-workflows", get(list_ai_workflows))
        .route("/ai-workflows", post(create_ai_workflow))
        .route("/ai-workflows/search", get(search_ai_workflows))
        .route("/ai-workflows/categories", get(get_ai_workflow_categories))
        .route("/ai-workflows/tags", get(get_ai_workflow_tags))
        .route(
            "/ai-workflows/:id",
            get(get_ai_workflow).put(update_ai_workflow).delete(delete_ai_workflow),
        )
        // Unified Session routes (replaces workflows and ai-developer)
        .route("/sessions", get(list_sessions))
        .route("/sessions/start", post(start_session))
        .route(
            "/sessions/:id",
            get(get_session).delete(delete_session),
        )
        .route("/sessions/:id/stop", post(stop_session))
        // Playwright Script Library routes
        .route("/playwright/scripts", get(list_playwright_scripts))
        .route("/playwright/scripts", post(create_playwright_script))
        .route("/playwright/scripts/search", get(search_playwright_scripts))
        .route(
            "/playwright/scripts/categories",
            get(get_playwright_categories),
        )
        .route("/playwright/scripts/tags", get(get_playwright_tags))
        .route(
            "/playwright/scripts/import",
            post(import_playwright_scripts),
        )
        .route("/playwright/scripts/export", get(export_playwright_scripts))
        .route("/playwright/scripts/:id", get(get_playwright_script))
        .route("/playwright/scripts/:id", put(update_playwright_script))
        .route("/playwright/scripts/:id", delete(delete_playwright_script))
        .route("/playwright/scripts/:id/run", post(run_playwright_script))
        .route(
            "/playwright/scripts/:id/duplicate",
            post(duplicate_playwright_script),
        )
        .layer(cors)
        .with_state(api_state)
}

/// Start the MCP API server
pub async fn start_server(
    app_state: Arc<AppState>,
    rag_state: Arc<RAGState>,
    app_handle: tauri::AppHandle,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let router = create_router(app_state, rag_state, app_handle);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    info!("MCP API server listening on port {}", port);

    axum::serve(listener, router).await?;

    Ok(())
}
