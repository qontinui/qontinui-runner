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

use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info, warn};

use crate::commands::rag::RAGState;
use crate::commands::AppState;
use crate::config::ConfigLoader;
use crate::rag::{ImportResult, QontinuiConfig, RAGConfigSummary};
use crate::settings;
use axum::routing::delete;
use tauri::{Emitter, Manager};

/// Default port for the MCP API server
pub const MCP_API_PORT: u16 = 9876;

/// Shared state for the API server
pub struct ApiState {
    pub app_state: Arc<AppState>,
    pub rag_state: Arc<RAGState>,
    pub app_handle: tauri::AppHandle,
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
    eprintln!(
        "[MCP_API] run_workflow received: workflow={}, monitor_index={:?}",
        request.workflow_name, request.monitor_index
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
            eprintln!(
                "[MCP_API] Building params: monitor_index={:?}, resolved to {}",
                monitor_index, resolved_monitor
            );
            params.insert(
                "monitor_index".to_string(),
                serde_json::json!(resolved_monitor),
            );
            params.insert(
                "workflow".to_string(),
                serde_json::json!(workflow_name.clone()),
            );
            eprintln!("[MCP_API] Sending to Python: {:?}", params);

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
// ============================================================================
// AI Analysis Trigger Endpoint
// ============================================================================

use crate::commands::ai_settings;
use crate::settings::{AiProvider, CliExecutionMode};

/// Request to trigger AI analysis
#[derive(Debug, Deserialize)]
pub struct TriggerAiAnalysisRequest {
    /// The prompt to send to Claude
    pub prompt: String,
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
}

/// AI output event payload (emitted to frontend)
#[derive(Debug, Clone, Serialize)]
pub struct AiOutputEvent {
    pub id: String,
    pub timestamp: i64,
    pub line: String,
    pub source: String, // "prompt" or "claude"
}

/// Emit AI output event to frontend
fn emit_ai_output(app_handle: &tauri::AppHandle, line: &str, source: &str) {
    let event = AiOutputEvent {
        id: format!(
            "ai-{}-{}",
            chrono::Utc::now().timestamp_millis(),
            rand::random::<u32>()
        ),
        timestamp: chrono::Utc::now().timestamp_millis(),
        line: line.to_string(),
        source: source.to_string(),
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
async fn trigger_ai_analysis(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<TriggerAiAnalysisRequest>,
) -> Result<Json<ApiResponse<TriggerAiAnalysisResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    write_ai_debug_log("=== AI ANALYSIS TRIGGERED ===");

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
        "MCP API: Triggering AI analysis (provider: {:?}, timeout: {}s, prompt length: {})",
        ai_settings.provider,
        timeout_secs,
        request.prompt.len()
    );

    // Emit prompt to frontend
    write_ai_debug_log("Emitting prompt to frontend...");
    emit_ai_output(&state.app_handle, &request.prompt, "prompt");
    write_ai_debug_log("Prompt emitted successfully");

    // Emit hourglass indicator to show AI is processing
    emit_ai_output(&state.app_handle, "⏳ AI is processing...", "status");

    let app_handle = state.app_handle.clone();
    write_ai_debug_log("Starting AI execution...");

    let result = match ai_settings.provider {
        AiProvider::ClaudeCli => {
            write_ai_debug_log("Using Claude CLI provider");
            execute_claude_cli(&ai_settings.claude_cli, &request.prompt, &app_handle).await
        }
        AiProvider::ClaudeApi => {
            write_ai_debug_log("Using Claude API provider");
            execute_claude_api(&ai_settings.claude_api, &request.prompt, &app_handle).await
        }
    };

    match result {
        Ok(response) => {
            if response.success {
                write_ai_debug_log("AI analysis completed successfully");
                info!("MCP API: AI analysis completed successfully");
                // Emit completion indicator
                emit_ai_output(&state.app_handle, "✅ AI analysis complete", "status");
            } else {
                write_ai_debug_log(&format!("AI analysis failed: {:?}", response.error));
                warn!("MCP API: AI analysis failed: {:?}", response.error);
                // Emit failure indicator
                emit_ai_output(&state.app_handle, "❌ AI analysis failed", "status");
            }
            write_ai_debug_log("=== AI ANALYSIS COMPLETE ===\n");
            Ok(Json(ApiResponse::success(response)))
        }
        Err(e) => {
            write_ai_debug_log(&format!("AI analysis error: {}", e));
            error!("MCP API: Failed to trigger AI analysis: {}", e);
            // Emit error to frontend
            emit_ai_output(&state.app_handle, "❌ AI analysis error", "status");
            emit_ai_output(&state.app_handle, &format!("Error: {}", e), "claude");
            write_ai_debug_log("=== AI ANALYSIS FAILED ===\n");
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute AI analysis via Claude CLI
async fn execute_claude_cli(
    cli_settings: &settings::ClaudeCliSettings,
    prompt: &str,
    app_handle: &tauri::AppHandle,
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
    let app_handle = app_handle.clone();

    write_ai_debug_log(&format!(
        "execute_claude_cli: execution_mode = {:?}, custom_path = {:?}, prompt_len = {}",
        execution_mode, custom_path, prompt_len
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
                )
            }
            CliExecutionMode::Wsl => {
                write_ai_debug_log("execute_claude_cli: Calling execute_via_wsl");
                execute_via_wsl(
                    &working_dir_str,
                    &prompt_owned,
                    custom_path.as_deref(),
                    &app_handle,
                )
            }
            CliExecutionMode::Native => {
                write_ai_debug_log("execute_claude_cli: Calling execute_native");
                execute_native(
                    &working_dir_str,
                    &prompt_owned,
                    custom_path.as_deref(),
                    &app_handle,
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

/// Streaming text buffer that emits complete lines or paragraphs
struct StreamingTextBuffer {
    buffer: String,
    app_handle: tauri::AppHandle,
    last_emit_time: std::time::Instant,
}

impl StreamingTextBuffer {
    fn new(app_handle: tauri::AppHandle) -> Self {
        Self {
            buffer: String::new(),
            app_handle,
            last_emit_time: std::time::Instant::now(),
        }
    }

    /// Add text to the buffer and emit complete lines/paragraphs
    fn add_text(&mut self, text: &str) {
        self.buffer.push_str(text);

        // Check for complete lines or paragraphs to emit
        self.try_emit();
    }

    /// Try to emit buffered content if we have complete lines
    fn try_emit(&mut self) {
        // Emit on newlines (complete lines) or after timeout with content
        let should_emit = self.buffer.contains('\n')
            || (self.buffer.len() > 100 && self.last_emit_time.elapsed().as_millis() > 500);

        if !should_emit {
            return;
        }

        // Find the last newline to emit complete lines
        if let Some(last_newline) = self.buffer.rfind('\n') {
            let to_emit = self.buffer[..=last_newline].to_string();
            self.buffer = self.buffer[last_newline + 1..].to_string();

            // Emit each line separately for better formatting
            for line in to_emit.lines() {
                if !line.is_empty() {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        emit_ai_output(&self.app_handle, line, "claude");
                    }));
                }
            }
            self.last_emit_time = std::time::Instant::now();
        } else if self.buffer.len() > 100 && self.last_emit_time.elapsed().as_millis() > 500 {
            // Emit partial content if buffer is getting large and time has passed
            let to_emit = std::mem::take(&mut self.buffer);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                emit_ai_output(&self.app_handle, &to_emit, "claude");
            }));
            self.last_emit_time = std::time::Instant::now();
        }
    }

    /// Flush any remaining content
    fn flush(&mut self) {
        if !self.buffer.is_empty() {
            let to_emit = std::mem::take(&mut self.buffer);
            for line in to_emit.lines() {
                if !line.is_empty() {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        emit_ai_output(&self.app_handle, line, "claude");
                    }));
                }
            }
        }
    }
}

/// Parse a stream-json line and extract text content
fn parse_stream_json_line(line: &str) -> Option<String> {
    // Parse the JSON line
    let json: serde_json::Value = serde_json::from_str(line).ok()?;

    // Handle different event types
    match json.get("type")?.as_str()? {
        "content_block_delta" => {
            // Extract text from delta: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"..."}}
            let delta = json.get("delta")?;
            if delta.get("type")?.as_str()? == "text_delta" {
                return delta.get("text")?.as_str().map(String::from);
            }
        }
        "result" => {
            // Handle result event which contains the full text
            // {"type":"result","subtype":"success","result":"full text here",...}
            if let Some(result) = json.get("result").and_then(|r| r.as_str()) {
                return Some(result.to_string());
            }
        }
        _ => {}
    }
    None
}

/// Execute Claude CLI on Windows natively with streaming output
fn execute_windows_native(
    working_dir: &str,
    prompt: &str,
    custom_path: Option<&str>,
    app_handle: &tauri::AppHandle,
) -> Result<TriggerAiAnalysisResponse, String> {
    use std::io::{BufRead, BufReader};

    write_ai_debug_log("execute_windows_native: Starting (streaming mode)");
    write_ai_debug_log(&format!(
        "execute_windows_native: working_dir = {}, custom_path = {:?}",
        working_dir, custom_path
    ));

    // Write prompt to a temp file to avoid shell escaping issues
    let temp_dir = std::env::temp_dir();
    let prompt_file = temp_dir.join("qontinui_ai_prompt.txt");
    write_ai_debug_log(&format!(
        "execute_windows_native: Writing prompt to {:?}",
        prompt_file
    ));

    std::fs::write(&prompt_file, prompt).map_err(|e| {
        let err = format!("Failed to write prompt file: {}", e);
        write_ai_debug_log(&format!("execute_windows_native ERROR: {}", err));
        err
    })?;
    write_ai_debug_log(&format!(
        "execute_windows_native: Prompt written ({} bytes)",
        prompt.len()
    ));

    // On Windows, Claude Code installed via npm uses 'claude.cmd' not 'claude.exe'
    // We use cmd.exe /c to handle both .cmd and .exe files
    let program = custom_path.unwrap_or("claude");
    write_ai_debug_log(&format!(
        "execute_windows_native: Using program = {}",
        program
    ));
    info!(
        "Running Claude Code on Windows via cmd.exe: {} with prompt from {:?} (streaming mode)",
        program, prompt_file
    );

    // Read the prompt file and pipe it to claude via stdin
    write_ai_debug_log("execute_windows_native: Reading prompt file...");
    let prompt_content = std::fs::read(&prompt_file).map_err(|e| {
        let err = format!("Failed to read prompt file: {}", e);
        write_ai_debug_log(&format!("execute_windows_native ERROR: {}", err));
        err
    })?;
    write_ai_debug_log(&format!(
        "execute_windows_native: Read {} bytes from prompt file",
        prompt_content.len()
    ));

    // Use cmd.exe /c to run claude with stream-json for real-time output
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
    write_ai_debug_log("execute_windows_native: Writing to stdin...");
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        if let Err(e) = stdin.write_all(&prompt_content) {
            let err = format!("Failed to write to claude stdin: {}", e);
            write_ai_debug_log(&format!("execute_windows_native ERROR: {}", err));
            return Err(err);
        }
        write_ai_debug_log("execute_windows_native: Stdin written and closed");
        // Stdin is dropped here, signaling EOF to Claude
    } else {
        write_ai_debug_log("execute_windows_native WARNING: No stdin available");
    }
    info!("Prompt written to Claude stdin, waiting for streaming output...");

    // Stream stdout and parse JSON events
    write_ai_debug_log("execute_windows_native: Reading streaming stdout...");
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let mut all_output = String::new();
    let mut line_count = 0;
    let mut text_buffer = StreamingTextBuffer::new(app_handle.clone());

    if let Some(stdout) = stdout {
        let reader = BufReader::new(stdout);
        for line_result in reader.lines() {
            match line_result {
                Ok(line) => {
                    line_count += 1;
                    if line_count <= 10 || line_count % 100 == 0 {
                        write_ai_debug_log(&format!(
                            "execute_windows_native: stream line {} ({} chars)",
                            line_count,
                            line.len()
                        ));
                    }

                    // Parse the JSON line and extract text
                    if let Some(text) = parse_stream_json_line(&line) {
                        all_output.push_str(&text);
                        text_buffer.add_text(&text);
                    }
                }
                Err(e) => {
                    write_ai_debug_log(&format!(
                        "execute_windows_native: stdout read error: {}",
                        e
                    ));
                }
            }
        }
    }

    // Flush any remaining buffered text
    text_buffer.flush();

    write_ai_debug_log(&format!(
        "execute_windows_native: stdout complete - {} JSON lines, {} chars extracted",
        line_count,
        all_output.len()
    ));
    info!(
        "Claude stdout complete: {} JSON lines, {} chars extracted",
        line_count,
        all_output.len()
    );

    // Capture any stderr
    write_ai_debug_log("execute_windows_native: Reading stderr...");
    let mut stderr_output = String::new();
    if let Some(stderr) = stderr {
        let reader = BufReader::new(stderr);
        for line_result in reader.lines() {
            match line_result {
                Ok(line) => {
                    write_ai_debug_log(&format!("execute_windows_native: stderr: {}", line));
                    stderr_output.push_str(&line);
                    stderr_output.push('\n');
                    // Also emit errors to frontend
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        emit_ai_output(app_handle, &format!("[stderr] {}", line), "claude");
                    }));
                }
                Err(e) => {
                    write_ai_debug_log(&format!(
                        "execute_windows_native: stderr read error: {}",
                        e
                    ));
                }
            }
        }
    }
    write_ai_debug_log(&format!(
        "execute_windows_native: stderr complete - {} chars",
        stderr_output.len()
    ));

    write_ai_debug_log("execute_windows_native: Waiting for process to exit...");
    let status = match child.wait() {
        Ok(s) => {
            write_ai_debug_log(&format!(
                "execute_windows_native: Process exited with status: {:?}",
                s
            ));
            s
        }
        Err(e) => {
            let err = format!("Failed to wait for claude: {}", e);
            write_ai_debug_log(&format!("execute_windows_native ERROR: {}", err));
            return Err(err);
        }
    };

    write_ai_debug_log("execute_windows_native: Cleaning up temp file...");
    let _ = std::fs::remove_file(&prompt_file);

    if status.success() {
        write_ai_debug_log("execute_windows_native: SUCCESS");
        Ok(TriggerAiAnalysisResponse {
            success: true,
            message: "AI analysis completed successfully".to_string(),
            error: None,
        })
    } else {
        write_ai_debug_log(&format!(
            "execute_windows_native: FAILED with code {:?}",
            status.code()
        ));
        Ok(TriggerAiAnalysisResponse {
            success: false,
            message: all_output,
            error: Some(if stderr_output.is_empty() {
                format!("Claude Code exited with code {:?}", status.code())
            } else {
                stderr_output
            }),
        })
    }
}

/// Execute Claude CLI via WSL with streaming output
fn execute_via_wsl(
    working_dir: &str,
    prompt: &str,
    custom_path: Option<&str>,
    app_handle: &tauri::AppHandle,
) -> Result<TriggerAiAnalysisResponse, String> {
    use std::io::{BufRead, BufReader};

    write_ai_debug_log("execute_via_wsl: Starting (streaming mode)");

    // Convert Windows path to WSL path
    let wsl_working_dir = working_dir.replace('\\', "/").replace("C:", "/mnt/c");
    let program = custom_path.unwrap_or("claude");

    write_ai_debug_log(&format!(
        "execute_via_wsl: wsl_working_dir = {}, program = {}",
        wsl_working_dir, program
    ));

    info!(
        "Running Claude Code via WSL: {} in {} (streaming mode)",
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

    // Use bash to read the file and pipe to claude with stream-json
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

    // Stream stdout and parse JSON events
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let mut all_output = String::new();
    let mut line_count = 0;
    let mut text_buffer = StreamingTextBuffer::new(app_handle.clone());

    if let Some(stdout) = stdout {
        let reader = BufReader::new(stdout);
        for line_result in reader.lines() {
            if let Ok(line) = line_result {
                line_count += 1;
                // Parse the JSON line and extract text
                if let Some(text) = parse_stream_json_line(&line) {
                    all_output.push_str(&text);
                    text_buffer.add_text(&text);
                }
            }
        }
    }

    // Flush any remaining buffered text
    text_buffer.flush();

    write_ai_debug_log(&format!(
        "execute_via_wsl: stdout complete - {} JSON lines, {} chars extracted",
        line_count,
        all_output.len()
    ));

    // Capture any stderr
    let mut stderr_output = String::new();
    if let Some(stderr) = stderr {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            if let Ok(line) = line {
                stderr_output.push_str(&line);
                stderr_output.push('\n');
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    emit_ai_output(app_handle, &format!("[stderr] {}", line), "claude");
                }));
            }
        }
    }

    let status = child
        .wait()
        .map_err(|e| format!("Failed to wait for WSL: {}", e))?;

    let _ = std::fs::remove_file(&prompt_file);

    if status.success() {
        write_ai_debug_log("execute_via_wsl: SUCCESS");
        Ok(TriggerAiAnalysisResponse {
            success: true,
            message: "AI analysis completed successfully".to_string(),
            error: None,
        })
    } else {
        write_ai_debug_log(&format!(
            "execute_via_wsl: FAILED with code {:?}",
            status.code()
        ));
        Ok(TriggerAiAnalysisResponse {
            success: false,
            message: all_output,
            error: Some(if stderr_output.is_empty() {
                format!("Claude Code exited with code {:?}", status.code())
            } else {
                stderr_output
            }),
        })
    }
}

/// Execute Claude CLI natively (Unix/macOS/Linux) with streaming output
fn execute_native(
    working_dir: &str,
    prompt: &str,
    custom_path: Option<&str>,
    app_handle: &tauri::AppHandle,
) -> Result<TriggerAiAnalysisResponse, String> {
    use std::io::{BufRead, BufReader, Write};

    write_ai_debug_log("execute_native: Starting (streaming mode)");

    let program = custom_path.unwrap_or("claude");
    info!("Running Claude Code natively: {} (streaming mode)", program);

    // Write prompt to a temp file
    let temp_dir = std::env::temp_dir();
    let prompt_file = temp_dir.join("qontinui_ai_prompt.txt");
    std::fs::write(&prompt_file, prompt)
        .map_err(|e| format!("Failed to write prompt file: {}", e))?;

    let prompt_content =
        std::fs::read(&prompt_file).map_err(|e| format!("Failed to read prompt file: {}", e))?;

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

    // Stream stdout and parse JSON events
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let mut all_output = String::new();
    let mut line_count = 0;
    let mut text_buffer = StreamingTextBuffer::new(app_handle.clone());

    if let Some(stdout) = stdout {
        let reader = BufReader::new(stdout);
        for line_result in reader.lines() {
            if let Ok(line) = line_result {
                line_count += 1;
                // Parse the JSON line and extract text
                if let Some(text) = parse_stream_json_line(&line) {
                    all_output.push_str(&text);
                    text_buffer.add_text(&text);
                }
            }
        }
    }

    // Flush any remaining buffered text
    text_buffer.flush();

    write_ai_debug_log(&format!(
        "execute_native: stdout complete - {} JSON lines, {} chars extracted",
        line_count,
        all_output.len()
    ));

    // Capture any stderr
    let mut stderr_output = String::new();
    if let Some(stderr) = stderr {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            if let Ok(line) = line {
                stderr_output.push_str(&line);
                stderr_output.push('\n');
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    emit_ai_output(app_handle, &format!("[stderr] {}", line), "claude");
                }));
            }
        }
    }

    let status = child
        .wait()
        .map_err(|e| format!("Failed to wait for {}: {}", program, e))?;

    let _ = std::fs::remove_file(&prompt_file);

    if status.success() {
        write_ai_debug_log("execute_native: SUCCESS");
        Ok(TriggerAiAnalysisResponse {
            success: true,
            message: "AI analysis completed successfully".to_string(),
            error: None,
        })
    } else {
        write_ai_debug_log(&format!(
            "execute_native: FAILED with code {:?}",
            status.code()
        ));
        Ok(TriggerAiAnalysisResponse {
            success: false,
            message: all_output,
            error: Some(if stderr_output.is_empty() {
                format!("Claude Code exited with code {:?}", status.code())
            } else {
                stderr_output
            }),
        })
    }
}

/// Execute AI analysis via Claude API (direct HTTP calls)
async fn execute_claude_api(
    api_settings: &settings::ClaudeApiSettings,
    prompt: &str,
    app_handle: &tauri::AppHandle,
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
            emit_ai_output(app_handle, line, "claude");
        }

        Ok(TriggerAiAnalysisResponse {
            success: true,
            message: content.to_string(),
            error: None,
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
        emit_ai_output(app_handle, &format!("Error: {}", error_message), "claude");

        Ok(TriggerAiAnalysisResponse {
            success: false,
            message: "API call failed".to_string(),
            error: Some(error_message),
        })
    }
}

/// Create the API router
pub fn create_router(
    app_state: Arc<AppState>,
    rag_state: Arc<RAGState>,
    app_handle: tauri::AppHandle,
) -> Router {
    let api_state = Arc::new(ApiState {
        app_state,
        rag_state,
        app_handle,
    });

    // Configure CORS to allow requests from WSL
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/health", get(health))
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
        .route("/rag/:project_id/status", get(get_rag_status))
        .route("/rag/:project_id/load", post(load_rag_project))
        .route("/rag/:project_id", delete(delete_rag_config))
        // AI Analysis route
        .route("/trigger-ai-analysis", post(trigger_ai_analysis))
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
