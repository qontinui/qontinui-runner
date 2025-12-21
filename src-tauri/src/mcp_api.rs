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
use crate::settings;
use crate::workflow_monitor::WorkflowManager;
use axum::routing::{delete, put};
use tauri::{Emitter, Manager};

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
    /// Manages multi-session workflow runs
    pub workflow_manager: WorkflowManager,
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
}

/// Response from spawning an AI Developer session
#[derive(Debug, Serialize)]
pub struct SpawnAiDeveloperResponse {
    pub session_id: String,
    pub state_file: String,
    pub log_file: String,
    pub pid: Option<u32>,
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
// Workflow Request/Response Types
// ============================================================================

use crate::workflow_monitor::{self, WorkflowRun, WorkflowStatus};

/// Request to start a workflow run
#[derive(Debug, Deserialize)]
pub struct StartWorkflowRequest {
    /// ID of the prompt to run as a workflow
    pub prompt_id: String,
}

/// Response containing workflow run info
#[derive(Debug, Serialize)]
pub struct WorkflowRunResponse {
    pub run: WorkflowRun,
}

/// Response containing list of workflow runs
#[derive(Debug, Serialize)]
pub struct WorkflowListResponse {
    pub runs: Vec<WorkflowRun>,
}

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
/// This creates a state file and spawns Claude as a completely independent process
/// using spawn-independent-claude.py. The Claude process can restart any service
/// including the runner itself.
async fn spawn_ai_developer_http(
    State(_state): State<Arc<ApiState>>,
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

    info!(
        "MCP API: Spawning AI Developer session: {} (max {} iterations)",
        session_id, max_iterations
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

        // Write prompt to file
        std::fs::write(&prompt_file, &request.prompt)
            .map_err(|e| format!("Failed to write prompt file: {}", e))?;

        info!("MCP API: State file created: {:?}", state_file);
        info!("MCP API: Prompt file created: {:?}", prompt_file);

        // Spawn Claude independently using the spawn script
        let spawn_result = std::process::Command::new("python")
            .arg(&spawn_script)
            .arg("--file")
            .arg(&prompt_file)
            .arg("--session-id")
            .arg(&session_id)
            .current_dir(&workspace_root)
            .spawn();

        match spawn_result {
            Ok(child) => {
                info!("MCP API: AI Developer spawned with PID: {}", child.id());
                Ok(SpawnAiDeveloperResponse {
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
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<StopAiDeveloperRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let session_id = request.session_id;
    info!(
        "MCP API: Requesting stop for AI Developer session: {}",
        session_id
    );

    let result = tokio::task::spawn_blocking(move || {
        let (_, dev_logs_path, _) = get_workspace_paths_internal()?;
        let state_file = dev_logs_path.join(format!("ai-developer-{}.json", session_id));

        if !state_file.exists() {
            return Err(format!("No state file found for session {}", session_id));
        }

        // Read current state
        let content = std::fs::read_to_string(&state_file)
            .map_err(|e| format!("Failed to read state file: {}", e))?;

        let mut state: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse state file: {}", e))?;

        // Set stop_requested flag
        state["stop_requested"] = serde_json::Value::Bool(true);

        // Write back
        std::fs::write(&state_file, serde_json::to_string_pretty(&state).unwrap())
            .map_err(|e| format!("Failed to write state file: {}", e))?;

        info!("MCP API: Stop requested for session {}", session_id);
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
        let spawn_result = std::process::Command::new("python")
            .arg(&spawn_script)
            .arg("--file")
            .arg(&prompt_file)
            .arg("--session-id")
            .arg(&session_id)
            .current_dir(&workspace_root)
            .spawn();

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
        "execute_windows_native: working_dir = {}, custom_path = {:?}, timeout = {}s",
        working_dir, custom_path, timeout_seconds
    ));

    // Calculate the deadline for the process
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);

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

    // Wait for process to complete with timeout
    debug_lifecycle::log_claude_cli(
        "wait",
        &format!(
            "waiting for Claude CLI process to complete (timeout in {}s)",
            timeout_seconds
        ),
    );

    let status = loop {
        // Check if we've exceeded the deadline
        if Instant::now() > deadline {
            debug_lifecycle::log_claude_cli(
                "timeout",
                &format!("Process exceeded {}s timeout, killing...", timeout_seconds),
            );
            write_ai_debug_log(&format!(
                "execute_windows_native: TIMEOUT after {}s, killing process",
                timeout_seconds
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
                    &format!("⏰ AI analysis timed out after {} seconds", timeout_seconds),
                    "status",
                    Some(&action_id_owned),
                );
            }));

            // Cleanup temp file
            let _ = std::fs::remove_file(&prompt_file);

            return Ok(TriggerAiAnalysisResponse {
                success: false,
                message: "AI analysis timed out".to_string(),
                error: Some(format!(
                    "Claude CLI process exceeded {} second timeout and was killed",
                    timeout_seconds
                )),
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
        })
    }
}

// ============================================================================
// Workflow API Handlers
// ============================================================================

/// Start a workflow run for a prompt
async fn start_workflow_run(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartWorkflowRequest>,
) -> Result<Json<ApiResponse<WorkflowRunResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Get the prompt
    let prompt = match prompts::get_prompt(&request.prompt_id) {
        Some(p) => p,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!(
                    "Prompt not found: {}",
                    request.prompt_id
                ))),
            ))
        }
    };

    // Check if workflow is enabled
    if !prompt.workflow.enabled {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "Workflow mode not enabled for this prompt".to_string(),
            )),
        ));
    }

    // Start the workflow run
    let run = match state.workflow_manager.start_workflow(&prompt).await {
        Ok(r) => r,
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    };

    // Spawn the workflow execution loop in background (returns immediately)
    let workflow_id = run.id.clone();
    let prompt_clone = prompt.clone();
    let state_clone = state.clone();
    tokio::spawn(async move {
        run_workflow_loop(state_clone, workflow_id, prompt_clone).await;
    });

    Ok(Json(ApiResponse::success(WorkflowRunResponse { run })))
}

/// Synchronous workflow execution loop
///
/// Runs sessions one after another, checking the checkpoint immediately after each
/// session completes. This provides zero-latency continuation without polling.
async fn run_workflow_loop(
    state: Arc<ApiState>,
    workflow_id: String,
    prompt: prompts::SavedPrompt,
) {
    let config = &prompt.workflow;
    let mut session_count = 0u32;
    let mut current_prompt_content = prompt.content.clone();

    info!("Starting workflow execution loop for {}", workflow_id);
    log_workflow_event(
        &workflow_id,
        "loop_started",
        &format!("Workflow execution loop started for '{}'", prompt.name),
    );

    // Delete any existing checkpoint file to start fresh
    if !config.checkpoint_path.is_empty() {
        if let Err(e) = std::fs::remove_file(&config.checkpoint_path) {
            // It's OK if file doesn't exist
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!("Failed to delete existing checkpoint: {}", e);
            }
        } else {
            info!("Deleted existing checkpoint file for fresh start");
            log_workflow_event(
                &workflow_id,
                "checkpoint_reset",
                "Deleted existing checkpoint file to start fresh",
            );
        }
    }

    loop {
        session_count += 1;
        let session_id = if session_count == 1 {
            format!("workflow-{}", &workflow_id[..8])
        } else {
            format!("workflow-{}-cont-{}", &workflow_id[..8], session_count)
        };
        // Use consistent action_id for all sessions in the workflow
        // This ensures all sessions are grouped together in the AI Output tab
        let action_id = format!("workflow-{}", &workflow_id[..8]);

        // Mark session as active
        state
            .workflow_manager
            .set_active_session(&workflow_id, Some(session_id.clone()))
            .await;
        log_workflow_event(
            &workflow_id,
            "session_started",
            &format!(
                "Session {} started (#{} in workflow)",
                session_id, session_count
            ),
        );

        // Get CLI settings
        let ai_settings = settings::get_ai_settings();
        let cli_settings = ai_settings.claude_cli;

        // Execute session (blocks until complete)
        let result = execute_claude_cli(
            &cli_settings,
            &current_prompt_content,
            &state.app_handle,
            &action_id,
        )
        .await;

        // Session ended - clear active session ID
        state
            .workflow_manager
            .set_active_session(&workflow_id, None)
            .await;

        // Handle session result
        if let Err(e) = result {
            error!(
                "Workflow {} session {} failed: {}",
                workflow_id, session_id, e
            );
            log_workflow_event(
                &workflow_id,
                "session_failed",
                &format!("Session {} failed: {}", session_id, e),
            );
            state.workflow_manager.fail_workflow(&workflow_id, &e).await;
            break;
        }

        log_workflow_event(
            &workflow_id,
            "session_completed",
            &format!("Session {} completed", session_id),
        );

        // Immediately check checkpoint (no polling delay!)
        match workflow_monitor::read_checkpoint_phase(&config.checkpoint_path, &config.phase_field)
        {
            Ok(phase) => {
                info!(
                    "Workflow {} checkpoint: phase {} (target: {})",
                    workflow_id, phase, config.completion_value
                );
                log_workflow_event(
                    &workflow_id,
                    "checkpoint_read",
                    &format!(
                        "Checkpoint phase: {} (target: {})",
                        phase, config.completion_value
                    ),
                );

                // Update workflow manager with current phase
                if let Some(run) = state.workflow_manager.get_run(&workflow_id).await {
                    let mut updated_run = run;
                    updated_run.current_phase = phase;
                    updated_run.previous_phase = phase;
                    state.workflow_manager.update_run(updated_run).await;
                }

                // Check if complete
                if phase >= config.completion_value {
                    info!(
                        "Workflow {} completed! Reached phase {}",
                        workflow_id, phase
                    );
                    log_workflow_event(
                        &workflow_id,
                        "completed",
                        &format!("Workflow completed! Reached phase {}", phase),
                    );
                    // Mark as completed in workflow manager
                    if let Some(run) = state.workflow_manager.get_run(&workflow_id).await {
                        state
                            .workflow_manager
                            .update_run({
                                let mut r = run;
                                r.status = WorkflowStatus::Completed;
                                r
                            })
                            .await;
                    }
                    break;
                }

                // Not complete - continue with next session immediately
                info!(
                    "Workflow {} phase {} < {}, spawning continuation",
                    workflow_id, phase, config.completion_value
                );

                // Use continuation prompt for subsequent sessions
                current_prompt_content = if config.continuation_prompt.is_empty() {
                    format!(
                        "Continue the workflow. Read checkpoint from {} and resume from phase {}. Complete the next phase(s), update checkpoint, then exit.",
                        config.checkpoint_path, phase
                    )
                } else {
                    config.continuation_prompt.clone()
                };

                // Loop continues immediately - no delay!
            }
            Err(e) => {
                // Checkpoint doesn't exist or is invalid
                // For first session, this might be expected (checkpoint not created yet)
                if session_count == 1 {
                    warn!(
                        "Workflow {} checkpoint not found after first session: {}",
                        workflow_id, e
                    );
                    log_workflow_event(
                        &workflow_id,
                        "checkpoint_missing",
                        &format!("Checkpoint not found after first session: {}. The session may not have created it.", e),
                    );
                    // Give the first session a pass - it might not have created the checkpoint
                    // Fall back to continuation prompt
                    current_prompt_content = if config.continuation_prompt.is_empty() {
                        format!(
                            "The checkpoint file was not found at {}. Please create it with the current phase and continue the workflow.",
                            config.checkpoint_path
                        )
                    } else {
                        config.continuation_prompt.clone()
                    };
                } else {
                    // For subsequent sessions, checkpoint should exist
                    error!("Workflow {} checkpoint read failed: {}", workflow_id, e);
                    log_workflow_event(
                        &workflow_id,
                        "checkpoint_error",
                        &format!("Failed to read checkpoint: {}", e),
                    );
                    state
                        .workflow_manager
                        .fail_workflow(&workflow_id, &format!("Checkpoint error: {}", e))
                        .await;
                    break;
                }
            }
        }
    }

    info!("Workflow execution loop for {} stopped", workflow_id);
}

/// Get status of a specific workflow run
async fn get_workflow_run(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(run_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<WorkflowRunResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state.workflow_manager.get_run(&run_id).await {
        Some(run) => Ok(Json(ApiResponse::success(WorkflowRunResponse { run }))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Workflow run not found: {}", run_id))),
        )),
    }
}

/// List all workflow runs
async fn list_workflow_runs(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<WorkflowListResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let runs = state.workflow_manager.get_all_runs().await;
    Ok(Json(ApiResponse::success(WorkflowListResponse { runs })))
}

/// Stop a workflow run
async fn stop_workflow_run(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(run_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<WorkflowRunResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Mark the workflow as failed (which will stop the monitor)
    state
        .workflow_manager
        .fail_workflow(&run_id, "Stopped by user")
        .await;

    match state.workflow_manager.get_run(&run_id).await {
        Some(run) => Ok(Json(ApiResponse::success(WorkflowRunResponse { run }))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Workflow run not found: {}", run_id))),
        )),
    }
}

/// Delete a workflow run record
async fn delete_workflow_run(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(run_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    if state.workflow_manager.remove_run(&run_id).await {
        Ok(Json(ApiResponse::success(())))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Workflow run not found: {}", run_id))),
        ))
    }
}

/// Delete all workflow run records
async fn delete_all_workflow_runs(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<DeleteAllResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let count = state.workflow_manager.clear_all_runs().await;
    Ok(Json(ApiResponse::success(DeleteAllResponse {
        deleted_count: count,
    })))
}

#[derive(Debug, Serialize)]
struct DeleteAllResponse {
    deleted_count: usize,
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
        ai_analysis_running: AtomicBool::new(false),
        ai_analysis_stop_requested: AtomicBool::new(false),
        workflow_manager: WorkflowManager::new(),
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
        .route("/rag/segment", post(segment_screenshot))
        .route("/rag/:project_id/status", get(get_rag_status))
        .route("/rag/:project_id/load", post(load_rag_project))
        .route("/rag/:project_id", delete(delete_rag_config))
        // AI Analysis routes (standard/inline mode)
        .route("/trigger-ai-analysis", post(trigger_ai_analysis))
        .route("/stop-ai-analysis", post(stop_ai_analysis))
        // Runner restart route (for AI self-healing)
        .route("/restart-runner", post(restart_runner))
        // AI Developer routes (persistent mode)
        .route("/ai-developer/spawn", post(spawn_ai_developer_http))
        .route("/ai-developer/state", post(read_ai_developer_state_http))
        .route("/ai-developer/stop", post(stop_ai_developer_http))
        .route("/ai-developer/list", get(list_ai_developer_sessions_http))
        .route("/ai-developer/log", post(read_claude_session_log_http))
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
        // Workflow routes (multi-session prompts)
        .route(
            "/workflows",
            get(list_workflow_runs).delete(delete_all_workflow_runs),
        )
        .route("/workflows/start", post(start_workflow_run))
        .route(
            "/workflows/:id",
            get(get_workflow_run).delete(delete_workflow_run),
        )
        .route("/workflows/:id/stop", post(stop_workflow_run))
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
