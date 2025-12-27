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
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;
use tower_http::cors::{Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tracing::{debug, error, info, warn};

use regex::Regex;

use crate::commands::rag::{send_embeddings_to_web, RAGState};
use crate::commands::AppState;
use crate::config::ConfigLoader;
use crate::rag::{ImportResult, QontinuiConfig, RAGConfigSummary};
use crate::scriptlets;
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
            for line in reader.lines().map_while(Result::ok) {
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
}

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

/// WebSocket handler for streaming execution events
///
/// Clients connect to /ws/events to receive real-time execution events including:
/// - Image recognition results with found coordinates
/// - Tree events (state activation/deactivation)
/// - Workflow execution progress
///
/// This enables the web frontend to display live perception of automation state.
async fn ws_events_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ApiState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_events(socket, state))
}

/// Handle WebSocket connection for event streaming
async fn handle_ws_events(socket: WebSocket, state: Arc<ApiState>) {
    let (mut sender, mut receiver) = socket.split();

    // Subscribe to the broadcast channel
    let mut event_rx = state.app_state.event_broadcast.subscribe();

    info!("WebSocket client connected for event streaming");

    // Spawn task to forward broadcast events to WebSocket
    let send_task = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    // Serialize event to JSON string
                    match serde_json::to_string(&event) {
                        Ok(json_str) => {
                            if sender.send(Message::Text(json_str)).await.is_err() {
                                debug!("WebSocket client disconnected");
                                break;
                            }
                        }
                        Err(e) => {
                            warn!("Failed to serialize event for WebSocket: {}", e);
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("WebSocket client lagged, skipped {} events", n);
                    // Continue receiving - client can handle gaps
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    debug!("Event broadcast channel closed");
                    break;
                }
            }
        }
    });

    // Handle incoming messages (for ping/pong or future commands)
    while let Some(result) = receiver.next().await {
        match result {
            Ok(Message::Ping(data)) => {
                // Ping/pong is handled automatically by axum
                debug!("Received ping from WebSocket client");
                let _ = data; // Acknowledge we received it
            }
            Ok(Message::Close(_)) => {
                debug!("WebSocket client sent close");
                break;
            }
            Ok(_) => {
                // Ignore other message types for now
            }
            Err(e) => {
                warn!("WebSocket receive error: {}", e);
                break;
            }
        }
    }

    // Clean up send task
    send_task.abort();
    info!("WebSocket client disconnected from event streaming");
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

            // Debug: Log metadata being sent
            if let Some(metadata) = config_data.get("metadata") {
                info!("MCP API: Config metadata being emitted: {:?}", metadata);
            } else {
                warn!("MCP API: No metadata in config_data!");
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
// Web Extraction API Endpoints
// ============================================================================

/// Request to start web extraction
#[derive(Debug, Deserialize)]
pub struct StartExtractionRequest {
    /// URLs to extract from
    pub urls: Vec<String>,
    /// Viewport sizes as [width, height] pairs
    #[serde(default)]
    pub viewports: Vec<(u32, u32)>,
    /// Whether to capture hover states
    #[serde(default = "default_true")]
    pub capture_hover_states: bool,
    /// Whether to capture focus states
    #[serde(default = "default_true")]
    pub capture_focus_states: bool,
    /// Maximum crawl depth
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    /// Maximum pages to crawl
    #[serde(default = "default_max_pages")]
    pub max_pages: u32,
    /// Backend session ID to update with progress
    #[serde(default)]
    pub session_id: Option<String>,
    /// Backend API URL for progress updates
    #[serde(default)]
    pub backend_url: Option<String>,
    /// Auth token for backend API calls
    #[serde(default)]
    pub auth_token: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_max_depth() -> u32 {
    5
}

fn default_max_pages() -> u32 {
    100
}

/// Response from extraction status endpoint
#[derive(Debug, Serialize)]
pub struct ExtractionStatusResponse {
    pub is_running: bool,
    pub extraction_id: Option<String>,
    pub stats: Option<ExtractionStats>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExtractionStats {
    pub states_found: u32,
    pub transitions_found: u32,
    pub warnings: u32,
    pub errors: u32,
}

/// Start web extraction
async fn start_web_extraction(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartExtractionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Starting web extraction for {} URLs",
        request.urls.len()
    );

    // Build extraction params
    let params = serde_json::json!({
        "urls": request.urls,
        "viewports": request.viewports,
        "capture_hover_states": request.capture_hover_states,
        "capture_focus_states": request.capture_focus_states,
        "max_depth": request.max_depth,
        "max_pages": request.max_pages,
        "session_id": request.session_id,
        "backend_url": request.backend_url,
        "auth_token": request.auth_token,
    });

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
            bridge.send_command("start_web_extraction", Some(params))
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
            info!("MCP API: Web extraction started");
            Ok(Json(ApiResponse::success(serde_json::json!({
                "started": true,
                "message": "Web extraction started"
            }))))
        }
        Err(e) => {
            error!("MCP API: Failed to start web extraction: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Stop web extraction
async fn stop_web_extraction(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stopping web extraction");

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
            bridge.send_command("stop_web_extraction", None)
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
            info!("MCP API: Web extraction stopped");
            Ok(Json(ApiResponse::success(
                "Web extraction stopped".to_string(),
            )))
        }
        Err(e) => {
            error!("MCP API: Failed to stop web extraction: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get extraction status
async fn get_extraction_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<ExtractionStatusResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
            bridge.send_command("get_extraction_status", None)
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
            // Note: send_command doesn't return data, so we return a default status
            // TODO: Implement proper extraction status tracking in app state
            Ok(Json(ApiResponse::success(ExtractionStatusResponse {
                is_running: false,
                extraction_id: None,
                stats: None,
            })))
        }
        Err(e) => {
            error!("MCP API: Failed to get extraction status: {}", e);
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
    let mut progress_rx = embedding_generator.generate_embeddings_async(project_id.clone());
    drop(embedding_generator);

    // Spawn background task to sync embeddings to web backend when complete
    let project_id_for_sync = project_id.clone();
    tokio::spawn(async move {
        while let Some(progress) = progress_rx.recv().await {
            match progress.status {
                crate::rag::EmbeddingStatus::Completed => {
                    info!(
                        "MCP API: Embedding generation completed for project_id={}, syncing to web backend",
                        project_id_for_sync
                    );
                    match send_embeddings_to_web(&project_id_for_sync).await {
                        Ok(()) => {
                            info!(
                                "MCP API: Successfully synced embeddings to web for project_id={}",
                                project_id_for_sync
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                "MCP API: Failed to sync embeddings to web for project_id={}: {}",
                                project_id_for_sync,
                                e
                            );
                        }
                    }
                    break;
                }
                crate::rag::EmbeddingStatus::Failed(ref err) => {
                    tracing::warn!(
                        "MCP API: Embedding generation failed for project_id={}: {}",
                        project_id_for_sync,
                        err
                    );
                    break;
                }
                _ => {
                    // Continue polling for in-progress updates
                }
            }
        }
    });

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

    // Load QontinuiConfig
    let storage = state.rag_state.storage.lock().await;

    match storage.load_qontinui_config(&project_id) {
        Ok(config) => {
            drop(storage);

            // TODO: Load into executor if needed
            Ok(Json(ApiResponse::success(serde_json::json!({
                "project_id": project_id,
                "name": config.metadata.name,
                "states": config.states.len(),
                "patterns": config.pattern_count(),
                "loaded": true
            }))))
        }
        Err(_) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Project not found: {}", project_id))),
        )),
    }
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
    /// Image paths to include (screenshots, etc.) - for multimodal analysis
    #[serde(default)]
    pub image_paths: Vec<String>,
    /// Video paths to extract frames from
    #[serde(default)]
    pub video_paths: Vec<String>,
    /// Path to Playwright trace ZIP file (will extract timeline and screenshots)
    #[serde(default)]
    pub trace_path: Option<String>,
    /// Maximum number of frames to extract from each video (default: 3)
    #[serde(default)]
    pub max_video_frames: Option<u32>,
    /// Maximum number of screenshots to extract from trace (default: 5)
    #[serde(default)]
    pub max_trace_screenshots: Option<u32>,
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

/// Response from running a prompt
#[derive(Debug, Serialize)]
pub struct RunPromptResponse {
    pub session_id: String,
    pub state_file: String,
    pub log_file: String,
    pub pid: Option<u32>,
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

// ============================================================================
// Scriptlet Request Types
// ============================================================================

/// Request to create a new scriptlet
#[derive(Debug, Deserialize)]
pub struct CreateScriptletRequest {
    pub name: String,
    pub content: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub source_log_ids: Option<Vec<String>>,
}

/// Request to update an existing scriptlet
#[derive(Debug, Deserialize)]
pub struct UpdateScriptletRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
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

    // Collect all images for analysis (screenshots, trace screenshots, video frames)
    let max_video_frames = request.max_video_frames.unwrap_or(3);
    let max_trace_screenshots = request.max_trace_screenshots.unwrap_or(5);

    let (all_images, trace_timeline) = collect_images_for_analysis(
        &request.image_paths,
        &request.video_paths,
        request.trace_path.as_deref(),
        max_video_frames,
        max_trace_screenshots,
    );

    write_ai_debug_log(&format!(
        "Collected {} images for analysis (trace timeline: {})",
        all_images.len(),
        trace_timeline.is_some()
    ));

    // Build enhanced prompt with trace timeline if available
    let enhanced_prompt = if let Some(timeline) = &trace_timeline {
        format!("{}\n\n{}", request.prompt, timeline)
    } else {
        request.prompt.clone()
    };

    let result = match ai_settings.provider {
        AiProvider::ClaudeCli => {
            write_ai_debug_log("Using Claude CLI provider");
            execute_claude_cli(
                &ai_settings.claude_cli,
                &enhanced_prompt,
                &all_images,
                &app_handle,
                &action_id,
            )
            .await
        }
        AiProvider::ClaudeApi => {
            write_ai_debug_log("Using Claude API provider");
            execute_claude_api(
                &ai_settings.claude_api,
                &enhanced_prompt,
                &all_images,
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

// ============================================================================
// Prompt Library HTTP Endpoints
// ============================================================================

use crate::backup;
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

/// Run a prompt by spawning a Claude session
async fn run_prompt(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<RunPromptRequest>,
) -> Result<Json<ApiResponse<RunPromptResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
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
                Ok(RunPromptResponse {
                    session_id,
                    state_file: state_file.to_string_lossy().to_string(),
                    log_file: log_file.to_string_lossy().to_string(),
                    pid: Some(child.id()),
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

// ============================================================================
// Scriptlet HTTP Endpoints
// ============================================================================

/// List all scriptlets
async fn list_scriptlets(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<scriptlets::Scriptlet>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let scriptlets = scriptlets::get_all_scriptlets();
    Ok(Json(ApiResponse::success(scriptlets)))
}

/// Get a single scriptlet by ID
async fn get_scriptlet(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<scriptlets::Scriptlet>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scriptlets::get_scriptlet(&id) {
        Some(scriptlet) => Ok(Json(ApiResponse::success(scriptlet))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Scriptlet not found: {}", id))),
        )),
    }
}

/// Create a new scriptlet
async fn create_scriptlet(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<CreateScriptletRequest>,
) -> Result<Json<ApiResponse<scriptlets::Scriptlet>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scriptlets::create_scriptlet(
        request.name,
        request.content,
        request.category,
        request.tags,
        request.source_log_ids,
    ) {
        Ok(scriptlet) => Ok(Json(ApiResponse::success(scriptlet))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Update an existing scriptlet
async fn update_scriptlet(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<UpdateScriptletRequest>,
) -> Result<Json<ApiResponse<scriptlets::Scriptlet>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scriptlets::update_scriptlet(
        &id,
        request.name,
        request.content,
        request.category,
        request.tags,
    ) {
        Ok(scriptlet) => Ok(Json(ApiResponse::success(scriptlet))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Delete a scriptlet
async fn delete_scriptlet(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scriptlets::delete_scriptlet(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Get all scriptlet categories
async fn get_scriptlet_categories(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let categories = scriptlets::get_categories();
    Ok(Json(ApiResponse::success(categories)))
}

/// Search scriptlets
async fn search_scriptlets(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<scriptlets::Scriptlet>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let query = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let results = scriptlets::search_scriptlets(query);
    Ok(Json(ApiResponse::success(results)))
}

// ============================================================================
// Backup and Restore HTTP Endpoints
// ============================================================================

/// Response for backup creation
#[derive(Debug, Serialize)]
struct BackupResponse {
    /// Base64-encoded ZIP file data
    data: String,
    /// Original filename suggestion
    filename: String,
    /// Backup result with details
    result: backup::BackupResult,
}

/// Request for restore operation
#[derive(Debug, Deserialize)]
struct RestoreRequest {
    /// Base64-encoded ZIP file data
    data: String,
}

/// Create a backup of all user data
///
/// Returns the backup as base64-encoded ZIP data along with metadata.
async fn create_backup_handler(
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
async fn get_backup_info_handler(
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
async fn restore_backup_handler(
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
fn extract_trace_data(
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
fn format_trace_timeline(json: &serde_json::Value) -> String {
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
fn extract_video_frames(video_path: &str, max_frames: u32) -> Result<Vec<String>, String> {
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
    let status = std::process::Command::new("ffmpeg")
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
fn collect_images_for_analysis(
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

// ============================================================================
// Claude CLI Execution
// ============================================================================

/// Execute AI analysis via Claude CLI
async fn execute_claude_cli(
    cli_settings: &settings::ClaudeCliSettings,
    prompt: &str,
    image_paths: &[String],
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

    // Build prompt with image paths if provided
    // Claude CLI (Claude Code) can read images from file paths
    let prompt_with_images = if image_paths.is_empty() {
        prompt.to_string()
    } else {
        let mut enhanced_prompt = prompt.to_string();
        enhanced_prompt.push_str("\n\n## Visual Context\n\n");
        enhanced_prompt.push_str("The following images are available for your analysis. Please examine them to understand the visual state:\n\n");
        for (i, path) in image_paths.iter().enumerate() {
            enhanced_prompt.push_str(&format!("{}. Image: {}\n", i + 1, path));
        }
        enhanced_prompt.push_str(
            "\nPlease read and analyze these images to understand the current state of the page.\n",
        );
        enhanced_prompt
    };

    let prompt_owned = prompt_with_images;
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
    image_paths: &[String],
    app_handle: &tauri::AppHandle,
    action_id: &str,
) -> Result<TriggerAiAnalysisResponse, String> {
    use base64::Engine;

    // Get API key from keychain
    let api_key = ai_settings::get_provider_api_key("claude_api")?.ok_or_else(|| {
        "No API key configured. Please configure your Claude API key in Settings > AI.".to_string()
    })?;

    info!(
        "Calling Claude API with model: {}, images: {}",
        api_settings.model,
        image_paths.len()
    );

    // Build content array (multimodal if images provided)
    let content = if image_paths.is_empty() {
        // Text-only: simple string content
        serde_json::json!(prompt)
    } else {
        // Multimodal: array of content blocks
        let mut content_parts: Vec<serde_json::Value> = Vec::new();

        // Add text first
        content_parts.push(serde_json::json!({
            "type": "text",
            "text": prompt
        }));

        // Add images (base64 encoded)
        for image_path in image_paths {
            if let Ok(image_data) = std::fs::read(image_path) {
                let base64_data = base64::engine::general_purpose::STANDARD.encode(&image_data);
                let media_type = if image_path.ends_with(".png") {
                    "image/png"
                } else if image_path.ends_with(".jpg") || image_path.ends_with(".jpeg") {
                    "image/jpeg"
                } else if image_path.ends_with(".webp") {
                    "image/webp"
                } else if image_path.ends_with(".gif") {
                    "image/gif"
                } else {
                    info!("Skipping unsupported image format: {}", image_path);
                    continue;
                };

                content_parts.push(serde_json::json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": media_type,
                        "data": base64_data
                    }
                }));
            } else {
                warn!("Failed to read image file: {}", image_path);
            }
        }

        serde_json::json!(content_parts)
    };

    let client = reqwest::Client::new();
    let response = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "model": api_settings.model,
            "max_tokens": api_settings.max_tokens,
            "messages": [{"role": "user", "content": content}]
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

    // Multi-session workflow configuration
    /// Path to external checkpoint file for cross-session workflows
    #[serde(default)]
    checkpoint_path: Option<String>,
    /// JSON field name in checkpoint that tracks current phase (default: "current_phase")
    #[serde(default = "default_phase_field")]
    phase_field: String,
    /// Workflow is complete when phase_field reaches this value
    #[serde(default)]
    completion_value: Option<u32>,
}

fn default_phase_field() -> String {
    "current_phase".to_string()
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

    // Clone values BEFORE moving them into config (for potential continuation sessions)
    let original_prompt = request.prompt.clone();
    let continuation_prompt_template = request.continuation_prompt.clone();
    let session_type_str = request.session_type.clone();
    let total_phases = request.total_phases;
    let uses_gui = request.uses_gui;
    let timeout_seconds = request.timeout_seconds;
    let session_name = request.name.clone();

    // Multi-session workflow config
    let workflow_checkpoint_path = request.checkpoint_path.clone();
    let workflow_phase_field = request.phase_field.clone();
    let workflow_completion_value = request.completion_value;

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
                // =====================================================================
                // CROSS-SESSION CONTINUATION LOOP (Deterministic, Runner-Managed)
                // =====================================================================
                //
                // This outer loop handles continuation ACROSS sessions (spawning new
                // sessions when one ends). This is different from the inner phase loop
                // in run_unified_session_loop which handles phases WITHIN a single session.
                //
                // Why this is here (not in the AI):
                // - The AI might crash, timeout, or hit context limits mid-work
                // - The runner ALWAYS runs after the session ends
                // - The runner can reliably check the checkpoint and continue
                // - The AI just needs to save progress; the runner handles continuation
                //
                // Multi-session workflows are configured with:
                // - checkpoint_path: Path to the checkpoint JSON file
                // - phase_field: JSON field name containing current phase (e.g., "current_phase")
                // - completion_value: Workflow is complete when phase_field >= this value
                //
                // Max sessions is a safety limit to prevent infinite loops.
                // =====================================================================

                // Check if multi-session workflow is enabled
                let has_workflow_config =
                    workflow_checkpoint_path.is_some() && workflow_completion_value.is_some();

                if !has_workflow_config {
                    // No workflow config - run single session and exit
                    info!("No multi-session workflow config. Running single session.");
                    run_unified_session_loop(
                        state_clone.clone(),
                        session_id.clone(),
                        workspace_root.clone(),
                    )
                    .await;
                    return;
                }

                // Multi-session workflow enabled
                let checkpoint_path_str = workflow_checkpoint_path.unwrap();
                let checkpoint_path = std::path::PathBuf::from(&checkpoint_path_str);
                let phase_field = workflow_phase_field;
                let completion_value = workflow_completion_value.unwrap();

                const MAX_CROSS_SESSION_ITERATIONS: u32 = 20; // Safety limit
                let mut cross_session_count = 0u32;
                let mut current_session_id = session_id.clone();

                // Save active workflow config for resume-on-startup
                let active_config = ActiveWorkflowConfig {
                    session_type: session_type_str.clone(),
                    name: session_name.clone(),
                    prompt: original_prompt.clone(),
                    continuation_prompt: continuation_prompt_template.clone(),
                    total_phases,
                    uses_gui,
                    timeout_seconds,
                    checkpoint_path: checkpoint_path_str.clone(),
                    phase_field: phase_field.clone(),
                    completion_value,
                    started_at: chrono::Utc::now().to_rfc3339(),
                    cross_session_count: 0,
                };
                if let Err(e) = save_active_workflow_config(&active_config) {
                    warn!("Failed to save active workflow config: {}", e);
                }

                loop {
                    cross_session_count += 1;
                    info!(
                        "Starting cross-session iteration {} (session: {})",
                        cross_session_count, current_session_id
                    );

                    // Run the session loop (handles phases within this session)
                    run_unified_session_loop(
                        state_clone.clone(),
                        current_session_id.clone(),
                        workspace_root.clone(),
                    )
                    .await;

                    // After session ends, check if we should spawn a continuation
                    if let Some((is_complete, current_phase)) = check_external_checkpoint_status(
                        &checkpoint_path,
                        &phase_field,
                        completion_value,
                    ) {
                        if is_complete {
                            info!(
                                "Workflow complete: {}={} >= completion_value={}. Finished after {} sessions.",
                                phase_field, current_phase, completion_value, cross_session_count
                            );
                            emit_ai_output(
                                &state_clone.app_handle,
                                &format!(
                                    "✅ Workflow completed after {} sessions (phase {} >= {})",
                                    cross_session_count, current_phase, completion_value
                                ),
                                "status",
                                Some(&current_session_id),
                            );
                            // Delete the active workflow config since workflow is complete
                            delete_active_workflow_config();
                            break;
                        }

                        // Not complete - check if we should continue
                        if cross_session_count >= MAX_CROSS_SESSION_ITERATIONS {
                            warn!(
                                "Reached max cross-session iterations ({}). Stopping to prevent infinite loop.",
                                MAX_CROSS_SESSION_ITERATIONS
                            );
                            emit_ai_output(
                                &state_clone.app_handle,
                                &format!(
                                    "⚠️ Reached max sessions ({}). Phase {} of {}.",
                                    MAX_CROSS_SESSION_ITERATIONS, current_phase, completion_value
                                ),
                                "warning",
                                Some(&current_session_id),
                            );
                            break;
                        }

                        // Spawn a continuation session
                        info!(
                            "Workflow not complete (phase {} < {}). Spawning continuation session ({}/{})",
                            current_phase, completion_value, cross_session_count + 1, MAX_CROSS_SESSION_ITERATIONS
                        );

                        // Create continuation config with modified prompt
                        let continuation_config = SessionConfig {
                            session_type: match session_type_str.as_str() {
                                "prompt_workflow" => SessionType::PromptWorkflow,
                                "ai_builder" => SessionType::AiBuilder,
                                _ => SessionType::OneShot,
                            },
                            prompt: format!(
                                "{}\n\n## Continuation Session {}\n\nThis is an automatic continuation. Read the checkpoint file and resume from {}={}.\nCheckpoint: {}\nTarget: {} >= {}\n",
                                original_prompt,
                                cross_session_count + 1,
                                phase_field,
                                current_phase,
                                checkpoint_path.display(),
                                phase_field,
                                completion_value
                            ),
                            continuation_prompt: continuation_prompt_template.clone(),
                            total_phases,
                            uses_gui,
                            timeout_seconds,
                            stall_threshold_seconds: 300,
                            name: format!("{}-cont-{}", session_name, cross_session_count + 1),
                            description: format!("Continuation session {} of workflow (phase {}/{})", cross_session_count + 1, current_phase, completion_value),
                            custom_config: serde_json::json!({}),
                        };

                        match state_clone
                            .session_manager
                            .start_session(continuation_config)
                            .await
                        {
                            Ok(new_session) => {
                                current_session_id = new_session.id.clone();
                                emit_ai_output(
                                    &state_clone.app_handle,
                                    &format!(
                                        "🔄 Continuation session {} (phase {}/{}, id: {})",
                                        cross_session_count + 1,
                                        current_phase,
                                        completion_value,
                                        current_session_id
                                    ),
                                    "status",
                                    Some(&current_session_id),
                                );
                                // Update the cross_session_count in the active workflow config
                                update_active_workflow_cross_session_count(cross_session_count + 1);
                                // Continue the loop - this will run run_unified_session_loop again
                            }
                            Err(e) => {
                                error!("Failed to create continuation session: {}", e);
                                emit_ai_output(
                                    &state_clone.app_handle,
                                    &format!("❌ Failed to spawn continuation: {}", e),
                                    "error",
                                    Some(&current_session_id),
                                );
                                break;
                            }
                        }
                    } else {
                        // Checkpoint file not found or unreadable
                        warn!(
                            "Checkpoint not found or unreadable at {:?}. Stopping workflow.",
                            checkpoint_path
                        );
                        emit_ai_output(
                            &state_clone.app_handle,
                            &format!(
                                "⚠️ Checkpoint not found: {}. Session complete.",
                                checkpoint_path.display()
                            ),
                            "warning",
                            Some(&current_session_id),
                        );
                        break;
                    }
                }
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

/// Check if AI output contains goal completion markers
/// Returns true if any marker indicates the goal has been achieved
fn check_goal_completion_markers(output: &str) -> bool {
    // List of markers that indicate goal completion
    // Claude should output one of these when the goal is achieved
    let completion_markers = [
        "[GOAL_COMPLETE]",
        "[GOAL_ACHIEVED]",
        "[STOP_SESSION]",
        "[SESSION_COMPLETE]",
    ];

    // Check for explicit markers (case-insensitive)
    let output_upper = output.to_uppercase();
    for marker in &completion_markers {
        if output_upper.contains(marker) {
            info!("Goal completion marker detected: {}", marker);
            return true;
        }
    }

    // Also check for common completion patterns in structured output
    // These patterns appear in Claude's session summaries
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
                info!("Goal completion pattern detected: {}", pattern);
                return true;
            }
        }
    }

    false
}

// ============================================================================
// Active Workflow Persistence (for Resume-on-Startup)
// ============================================================================

/// Configuration for an active multi-session workflow that should be resumed on startup.
/// This is persisted to `.dev-logs/active-workflow.json` when a multi-session workflow starts.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ActiveWorkflowConfig {
    /// Session type ("prompt_workflow", "ai_builder", "one_shot")
    session_type: String,
    /// Display name for the workflow
    name: String,
    /// Original prompt content
    prompt: String,
    /// Continuation prompt template (if any)
    continuation_prompt: Option<String>,
    /// Total phases/iterations per session (0 = unlimited)
    total_phases: u32,
    /// Whether this workflow uses GUI automation
    uses_gui: bool,
    /// Timeout per phase in seconds
    timeout_seconds: u64,
    /// Path to the checkpoint JSON file
    checkpoint_path: String,
    /// JSON field name in checkpoint that tracks current phase
    phase_field: String,
    /// Workflow is complete when phase_field >= this value
    completion_value: u32,
    /// When the workflow was started
    started_at: String,
    /// Number of cross-session continuations so far
    cross_session_count: u32,
}

/// Get the path to the active workflow config file
fn get_active_workflow_config_path() -> std::path::PathBuf {
    std::path::PathBuf::from(
        r"C:\Users\Joshua\Documents\qontinui_parent_directory\.dev-logs\active-workflow.json",
    )
}

/// Save an active workflow config (called when starting a multi-session workflow)
fn save_active_workflow_config(config: &ActiveWorkflowConfig) -> Result<(), String> {
    let path = get_active_workflow_config_path();
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize workflow config: {}", e))?;
    std::fs::write(&path, json)
        .map_err(|e| format!("Failed to write workflow config to {:?}: {}", path, e))?;
    info!("Saved active workflow config to {:?}", path);
    Ok(())
}

/// Load an active workflow config (called on startup to check for resumption)
fn load_active_workflow_config() -> Option<ActiveWorkflowConfig> {
    let path = get_active_workflow_config_path();
    if !path.exists() {
        return None;
    }
    match std::fs::read_to_string(&path) {
        Ok(contents) => match serde_json::from_str::<ActiveWorkflowConfig>(&contents) {
            Ok(config) => {
                info!(
                    "Loaded active workflow config: {} (phase_field={}, completion_value={})",
                    config.name, config.phase_field, config.completion_value
                );
                Some(config)
            }
            Err(e) => {
                warn!("Failed to parse active workflow config: {}", e);
                None
            }
        },
        Err(e) => {
            warn!("Failed to read active workflow config: {}", e);
            None
        }
    }
}

/// Delete the active workflow config (called when workflow completes or is stopped)
fn delete_active_workflow_config() {
    let path = get_active_workflow_config_path();
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            warn!("Failed to delete active workflow config: {}", e);
        } else {
            info!("Deleted active workflow config (workflow complete)");
        }
    }
}

/// Update the cross_session_count in the active workflow config
fn update_active_workflow_cross_session_count(count: u32) {
    if let Some(mut config) = load_active_workflow_config() {
        config.cross_session_count = count;
        if let Err(e) = save_active_workflow_config(&config) {
            warn!("Failed to update cross_session_count: {}", e);
        }
    }
}

/// Response for resumable workflow check
#[derive(Debug, Serialize)]
struct ResumableWorkflowInfo {
    /// Whether a resumable workflow exists
    has_resumable: bool,
    /// Workflow name (if resumable)
    name: Option<String>,
    /// Session type
    session_type: Option<String>,
    /// Current phase/iteration
    current_phase: Option<u32>,
    /// Total phases (0 = unlimited)
    total_phases: Option<u32>,
    /// When the workflow was started
    started_at: Option<String>,
    /// Number of cross-session continuations
    cross_session_count: Option<u32>,
    /// Status from checkpoint
    status: Option<String>,
}

/// Get information about any resumable workflow
async fn get_resumable_workflow(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<ResumableWorkflowInfo>> {
    // Load the active workflow config
    let config = match load_active_workflow_config() {
        Some(c) => c,
        None => {
            return Json(ApiResponse::success(ResumableWorkflowInfo {
                has_resumable: false,
                name: None,
                session_type: None,
                current_phase: None,
                total_phases: None,
                started_at: None,
                cross_session_count: None,
                status: None,
            }));
        }
    };

    // Check if restart is permitted
    let checkpoint_path = std::path::PathBuf::from(&config.checkpoint_path);
    if !check_workflow_restart_permitted(&checkpoint_path) {
        return Json(ApiResponse::success(ResumableWorkflowInfo {
            has_resumable: false,
            name: None,
            session_type: None,
            current_phase: None,
            total_phases: None,
            started_at: None,
            cross_session_count: None,
            status: None,
        }));
    }

    // Check if the workflow is already complete
    if let Some((is_complete, current_phase)) = check_external_checkpoint_status(
        &checkpoint_path,
        &config.phase_field,
        config.completion_value,
    ) {
        if is_complete {
            // Workflow is complete, clean up
            delete_active_workflow_config();
            return Json(ApiResponse::success(ResumableWorkflowInfo {
                has_resumable: false,
                name: None,
                session_type: None,
                current_phase: None,
                total_phases: None,
                started_at: None,
                cross_session_count: None,
                status: None,
            }));
        }

        // Get status from checkpoint
        let status = std::fs::read_to_string(&checkpoint_path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| {
                v.get("status")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string())
            });

        return Json(ApiResponse::success(ResumableWorkflowInfo {
            has_resumable: true,
            name: Some(config.name),
            session_type: Some(config.session_type),
            current_phase: Some(current_phase),
            total_phases: Some(config.total_phases),
            started_at: Some(config.started_at),
            cross_session_count: Some(config.cross_session_count),
            status,
        }));
    }

    // No valid checkpoint
    Json(ApiResponse::success(ResumableWorkflowInfo {
        has_resumable: false,
        name: None,
        session_type: None,
        current_phase: None,
        total_phases: None,
        started_at: None,
        cross_session_count: None,
        status: None,
    }))
}

/// Response type for resume workflow
#[derive(Debug, Serialize)]
struct ResumeWorkflowResponse {
    message: String,
    name: String,
}

/// Manually resume an active workflow
async fn resume_workflow(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<ResumeWorkflowResponse>> {
    // Check if AI analysis is already running
    if state
        .ai_analysis_running
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Json(ApiResponse {
            success: false,
            data: None,
            error: Some(
                "AI analysis is already running. Stop it first before resuming a workflow."
                    .to_string(),
            ),
        });
    }

    // Load the active workflow config
    let config = match load_active_workflow_config() {
        Some(c) => c,
        None => {
            return Json(ApiResponse {
                success: false,
                data: None,
                error: Some("No active workflow to resume".to_string()),
            });
        }
    };

    // Check if restart is permitted
    let checkpoint_path = std::path::PathBuf::from(&config.checkpoint_path);
    if !check_workflow_restart_permitted(&checkpoint_path) {
        return Json(ApiResponse {
            success: false,
            data: None,
            error: Some(
                "Workflow restart not permitted (restart_permitted=false in checkpoint)"
                    .to_string(),
            ),
        });
    }

    // Check if the workflow is already complete
    if let Some((is_complete, _)) = check_external_checkpoint_status(
        &checkpoint_path,
        &config.phase_field,
        config.completion_value,
    ) {
        if is_complete {
            delete_active_workflow_config();
            return Json(ApiResponse {
                success: false,
                data: None,
                error: Some("Workflow is already complete".to_string()),
            });
        }
    }

    info!("Manually resuming workflow: {}", config.name);
    let workflow_name = config.name.clone();

    // Call the resume function (spawn it so we don't block)
    let state_clone = state.clone();
    tokio::spawn(async move {
        resume_active_workflow_on_startup(state_clone).await;
    });

    Json(ApiResponse::success(ResumeWorkflowResponse {
        message: format!("Resuming workflow: {}", workflow_name),
        name: workflow_name,
    }))
}

/// Response for auto-continue setting
#[derive(Debug, Serialize)]
struct AutoContinueSettingResponse {
    enabled: bool,
}

/// Get the auto-continue AI workflow setting
async fn get_auto_continue_setting() -> Json<ApiResponse<AutoContinueSettingResponse>> {
    let enabled = settings::get_auto_continue_ai_workflow();
    Json(ApiResponse::success(AutoContinueSettingResponse {
        enabled,
    }))
}

/// Request body for setting auto-continue
#[derive(Debug, Deserialize)]
struct SetAutoContinueRequest {
    enabled: bool,
}

/// Set the auto-continue AI workflow setting
async fn set_auto_continue_setting(
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

/// Check the active workflow checkpoint for restart_permitted flag
/// Returns true if restart is permitted (or if the flag is absent)
fn check_workflow_restart_permitted(checkpoint_path: &std::path::Path) -> bool {
    if !checkpoint_path.exists() {
        return false;
    }
    match std::fs::read_to_string(checkpoint_path) {
        Ok(contents) => match serde_json::from_str::<serde_json::Value>(&contents) {
            Ok(json) => {
                // If restart_permitted is explicitly false, don't resume
                // If absent or true, resume
                json.get("restart_permitted")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true)
            }
            Err(_) => false,
        },
        Err(_) => false,
    }
}

/// Resume an active workflow on startup if one exists and should be resumed.
/// This is called during server startup to check for workflows that were interrupted
/// (e.g., by runner restart during code changes).
async fn resume_active_workflow_on_startup(state: Arc<ApiState>) {
    // Load the active workflow config
    let config = match load_active_workflow_config() {
        Some(c) => c,
        None => {
            info!("No active workflow to resume on startup");
            return;
        }
    };

    info!(
        "Found active workflow '{}' to potentially resume (checkpoint: {}, phase_field: {}, completion_value: {})",
        config.name, config.checkpoint_path, config.phase_field, config.completion_value
    );

    // Check if restart is permitted
    let checkpoint_path = std::path::PathBuf::from(&config.checkpoint_path);
    if !check_workflow_restart_permitted(&checkpoint_path) {
        info!("Workflow restart not permitted (restart_permitted=false). Not resuming.");
        return;
    }

    // Check if the workflow is already complete
    if let Some((is_complete, current_phase)) = check_external_checkpoint_status(
        &checkpoint_path,
        &config.phase_field,
        config.completion_value,
    ) {
        if is_complete {
            info!(
                "Workflow already complete ({}={} >= {}). Cleaning up.",
                config.phase_field, current_phase, config.completion_value
            );
            delete_active_workflow_config();
            return;
        }

        // Workflow is not complete - resume it
        info!(
            "Resuming workflow '{}' from phase {} (target: {})",
            config.name, current_phase, config.completion_value
        );

        emit_ai_output(
            &state.app_handle,
            &format!(
                "🔄 Resuming workflow '{}' from phase {} (interrupted by restart)",
                config.name, current_phase
            ),
            "status",
            None,
        );

        // Create a continuation session config
        let continuation_config = SessionConfig {
            session_type: match config.session_type.as_str() {
                "prompt_workflow" => SessionType::PromptWorkflow,
                "ai_builder" => SessionType::AiBuilder,
                _ => SessionType::OneShot,
            },
            prompt: format!(
                "{}\n\n## Resume After Runner Restart\n\nThis session is resuming after a runner restart. Read the checkpoint file and continue from {}={}.\nCheckpoint: {}\nTarget: {} >= {}\n",
                config.prompt,
                config.phase_field,
                current_phase,
                config.checkpoint_path,
                config.phase_field,
                config.completion_value
            ),
            continuation_prompt: config.continuation_prompt.clone(),
            total_phases: config.total_phases,
            uses_gui: config.uses_gui,
            timeout_seconds: config.timeout_seconds,
            stall_threshold_seconds: 300,
            name: format!("{}-resume", config.name),
            description: format!("Resumed after runner restart (phase {}/{})", current_phase, config.completion_value),
            custom_config: serde_json::json!({}),
        };

        // Start the continuation session
        match state
            .session_manager
            .start_session(continuation_config)
            .await
        {
            Ok(session) => {
                info!("Started resume session: {}", session.id);
                let session_id = session.id.clone();
                let workspace_root = get_workspace_paths_internal()
                    .map(|(root, _, _)| root.to_string_lossy().to_string())
                    .unwrap_or_else(|_| ".".to_string());

                // Spawn the cross-session continuation loop
                let state_clone = state.clone();
                let original_prompt = config.prompt.clone();
                let continuation_prompt_template = config.continuation_prompt.clone();
                let session_type_str = config.session_type.clone();
                let session_name = config.name.clone();
                let total_phases = config.total_phases;
                let uses_gui = config.uses_gui;
                let timeout_seconds = config.timeout_seconds;
                let checkpoint_path_str = config.checkpoint_path.clone();
                let phase_field = config.phase_field.clone();
                let completion_value = config.completion_value;
                let initial_cross_session_count = config.cross_session_count;

                tokio::spawn(async move {
                    const MAX_CROSS_SESSION_ITERATIONS: u32 = 20;
                    let mut cross_session_count = initial_cross_session_count;
                    let mut current_session_id = session_id.clone();
                    let checkpoint_path = std::path::PathBuf::from(&checkpoint_path_str);

                    loop {
                        cross_session_count += 1;
                        info!(
                            "Resume: Starting cross-session iteration {} (session: {})",
                            cross_session_count, current_session_id
                        );

                        // Run the session loop
                        run_unified_session_loop(
                            state_clone.clone(),
                            current_session_id.clone(),
                            workspace_root.clone(),
                        )
                        .await;

                        // Check if we should spawn a continuation
                        if let Some((is_complete, current_phase)) = check_external_checkpoint_status(
                            &checkpoint_path,
                            &phase_field,
                            completion_value,
                        ) {
                            if is_complete {
                                info!(
                                    "Resume workflow complete: {}={} >= {}. Finished after {} sessions.",
                                    phase_field, current_phase, completion_value, cross_session_count
                                );
                                emit_ai_output(
                                    &state_clone.app_handle,
                                    &format!(
                                        "✅ Resumed workflow completed after {} sessions (phase {} >= {})",
                                        cross_session_count, current_phase, completion_value
                                    ),
                                    "status",
                                    Some(&current_session_id),
                                );
                                delete_active_workflow_config();
                                break;
                            }

                            if cross_session_count >= MAX_CROSS_SESSION_ITERATIONS {
                                warn!(
                                    "Resume: Reached max cross-session iterations ({}). Stopping.",
                                    MAX_CROSS_SESSION_ITERATIONS
                                );
                                emit_ai_output(
                                    &state_clone.app_handle,
                                    &format!(
                                        "⚠️ Reached max sessions ({}). Phase {} of {}.",
                                        MAX_CROSS_SESSION_ITERATIONS,
                                        current_phase,
                                        completion_value
                                    ),
                                    "warning",
                                    Some(&current_session_id),
                                );
                                break;
                            }

                            // Spawn a continuation session
                            let continuation_config = SessionConfig {
                                session_type: match session_type_str.as_str() {
                                    "prompt_workflow" => SessionType::PromptWorkflow,
                                    "ai_builder" => SessionType::AiBuilder,
                                    _ => SessionType::OneShot,
                                },
                                prompt: format!(
                                    "{}\n\n## Continuation Session {}\n\nThis is an automatic continuation. Read the checkpoint file and resume from {}={}.\nCheckpoint: {}\nTarget: {} >= {}\n",
                                    original_prompt,
                                    cross_session_count + 1,
                                    phase_field,
                                    current_phase,
                                    checkpoint_path.display(),
                                    phase_field,
                                    completion_value
                                ),
                                continuation_prompt: continuation_prompt_template.clone(),
                                total_phases,
                                uses_gui,
                                timeout_seconds,
                                stall_threshold_seconds: 300,
                                name: format!("{}-cont-{}", session_name, cross_session_count + 1),
                                description: format!("Continuation session {} (phase {}/{})", cross_session_count + 1, current_phase, completion_value),
                                custom_config: serde_json::json!({}),
                            };

                            match state_clone
                                .session_manager
                                .start_session(continuation_config)
                                .await
                            {
                                Ok(new_session) => {
                                    current_session_id = new_session.id.clone();
                                    emit_ai_output(
                                        &state_clone.app_handle,
                                        &format!(
                                            "🔄 Continuation session {} (phase {}/{}, id: {})",
                                            cross_session_count + 1,
                                            current_phase,
                                            completion_value,
                                            current_session_id
                                        ),
                                        "status",
                                        Some(&current_session_id),
                                    );
                                    update_active_workflow_cross_session_count(
                                        cross_session_count + 1,
                                    );
                                }
                                Err(e) => {
                                    error!("Resume: Failed to create continuation session: {}", e);
                                    break;
                                }
                            }
                        } else {
                            warn!("Resume: Checkpoint not found or unreadable. Stopping.");
                            break;
                        }
                    }
                });
            }
            Err(e) => {
                error!("Failed to start resume session: {}", e);
                emit_ai_output(
                    &state.app_handle,
                    &format!("❌ Failed to resume workflow: {}", e),
                    "error",
                    None,
                );
            }
        }
    } else {
        warn!("Could not read checkpoint for resumption. Not resuming.");
    }
}

/// Check external checkpoint file for workflow completion status.
///
/// # Why This Is Deterministic (Not AI-Managed)
///
/// We check the external checkpoint file from the RUNNER, not from the AI, because:
///
/// 1. **Reliability**: The AI might forget to spawn a continuation, timeout before
///    doing so, or exit without saving state. The runner ALWAYS runs after each
///    Claude session ends, so it can reliably check and continue.
///
/// 2. **Edge Case Handling**: If the AI crashes, times out, or hits context limits,
///    the runner can still read the checkpoint and spawn a continuation. The AI
///    cannot handle its own failure.
///
/// 3. **Iteration Limits**: The runner enforces max_iterations to prevent infinite
///    loops. The AI might not track this correctly across sessions.
///
/// 4. **Single Source of Truth**: The checkpoint file is the authoritative state.
///    The runner reads it deterministically; the AI just writes to it.
///
/// 5. **User Experience**: The user starts improve-all once and walks away.
///    Multiple sessions may run, but the user is never asked to intervene.
///
/// # Arguments
/// * `checkpoint_path` - Path to the checkpoint JSON file
/// * `phase_field` - JSON field name that contains the current phase (e.g., "current_phase")
/// * `completion_value` - Workflow is complete when phase_field >= this value
///
/// # Returns
/// * `Some((true, phase))` - Checkpoint exists and workflow is complete (phase >= completion_value)
/// * `Some((false, phase))` - Checkpoint exists and workflow is NOT complete (continue)
/// * `None` - Checkpoint doesn't exist or couldn't be read
fn check_external_checkpoint_status(
    checkpoint_path: &std::path::Path,
    phase_field: &str,
    completion_value: u32,
) -> Option<(bool, u32)> {
    if !checkpoint_path.exists() {
        return None;
    }

    match std::fs::read_to_string(checkpoint_path) {
        Ok(contents) => {
            match serde_json::from_str::<serde_json::Value>(&contents) {
                Ok(json) => {
                    // Get current phase from the configured field
                    let current_phase =
                        json.get(phase_field).and_then(|v| v.as_u64()).unwrap_or(0) as u32;

                    // Workflow is complete when current_phase >= completion_value
                    let is_complete = current_phase >= completion_value;

                    info!(
                        "External checkpoint status: {}={}, completion_value={}, is_complete={}",
                        phase_field, current_phase, completion_value, is_complete
                    );

                    Some((is_complete, current_phase))
                }
                Err(e) => {
                    warn!("Failed to parse external checkpoint: {}", e);
                    None
                }
            }
        }
        Err(e) => {
            warn!("Failed to read external checkpoint: {}", e);
            None
        }
    }
}

/// Run the unified session execution loop
async fn run_unified_session_loop(
    state: Arc<ApiState>,
    session_id: String,
    workspace_root: String,
) {
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
            &format!(
                "🚀 Running phase {} (session {})...",
                phase, phase_session_id
            ),
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

                // Note: External checkpoint is now checked by the CROSS-SESSION loop
                // (in start_session's tokio::spawn), not here. This allows:
                // - Better separation of concerns: within-session vs cross-session
                // - The cross-session loop uses configurable completion_value
                // - Sessions end naturally via total_phases or goal markers

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

                    // Check if AI output indicates goal completion
                    if check_goal_completion_markers(&output) {
                        s.status = SessionStatus::Completed;
                        s.checkpoint.completed = true;
                        s.checkpoint.status = "goal_achieved".to_string();
                        let _ = state.session_manager.update_session(s).await;

                        emit_ai_output(
                            &app_handle,
                            &format!(
                                "🎯 Session {} completed - goal achieved after {} phases",
                                session_id, phase
                            ),
                            "status",
                            Some(&session_id),
                        );
                        info!(
                            "Session {} completed early - goal achieved after {} phases",
                            session_id, phase
                        );
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

        // Check for active workflows to resume after runner restart
        // This runs after session restore, with a small additional delay
        // Only auto-resume if the setting is enabled
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        if settings::get_auto_continue_ai_workflow() {
            info!("Auto-continue is enabled, checking for workflows to resume...");
            resume_active_workflow_on_startup(state_for_restore).await;
        } else {
            info!("Auto-continue is disabled, skipping automatic workflow resume");
        }
    });

    // Configure CORS to allow requests from WSL
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        // WebSocket endpoint for live execution event streaming
        .route("/ws/events", get(ws_events_handler))
        .route("/health", get(health))
        .route("/launch-debug-chrome", post(launch_debug_chrome))
        .route("/status", get(get_status))
        .route("/monitors", get(get_monitors))
        .route("/load-config", post(load_config))
        .route("/load-last-config", post(load_last_config))
        .route("/run-workflow", post(run_workflow))
        .route("/stop-execution", post(stop_execution))
        // Web extraction routes
        .route("/extraction/start", post(start_web_extraction))
        .route("/extraction/stop", post(stop_web_extraction))
        .route("/extraction/status", get(get_extraction_status))
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
            get(get_ai_workflow)
                .put(update_ai_workflow)
                .delete(delete_ai_workflow),
        )
        // Unified Session routes (replaces workflows and ai-developer)
        .route("/sessions", get(list_sessions))
        .route("/sessions/start", post(start_session))
        .route("/sessions/:id", get(get_session).delete(delete_session))
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
        // Scriptlet routes
        .route("/scriptlets", get(list_scriptlets))
        .route("/scriptlets", post(create_scriptlet))
        .route("/scriptlets/search", get(search_scriptlets))
        .route("/scriptlets/categories", get(get_scriptlet_categories))
        .route("/scriptlets/:id", get(get_scriptlet))
        .route("/scriptlets/:id", put(update_scriptlet))
        .route("/scriptlets/:id", delete(delete_scriptlet))
        // Workflow resume routes
        .route("/workflow/resumable", get(get_resumable_workflow))
        .route("/workflow/resume", post(resume_workflow))
        // Auto-continue setting routes
        .route("/workflow/auto-continue", get(get_auto_continue_setting))
        .route("/workflow/auto-continue", post(set_auto_continue_setting))
        // Backup and Restore routes
        .route("/backup", get(create_backup_handler))
        .route("/backup/info", post(get_backup_info_handler))
        .route("/restore", post(restore_backup_handler))
        .layer(cors)
        // Allow up to 100MB request bodies for configs with embedded images
        .layer(RequestBodyLimitLayer::new(100 * 1024 * 1024))
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
