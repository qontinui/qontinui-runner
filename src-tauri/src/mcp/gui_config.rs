//! GUI Config Pipeline handlers for MCP API
//!
//! Provides HTTP endpoints to capture element images from the current UI
//! and build visual GUI automation configs (QontinuiConfig format).
//!
//! The pipeline combines UI Bridge snapshot data (element positions) with
//! screen captures, then uses the qontinui core library to crop elements
//! and assemble configs.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::executor::{get_or_create_default_bridge, with_default_bridge};
use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct CaptureElementsRequest {
    pub window_offset_x: Option<i32>,
    pub window_offset_y: Option<i32>,
    pub scale_factor: Option<f64>,
    pub category_filter: Option<Vec<String>>,
    pub min_element_size: Option<i32>,
    pub padding: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct BuildGuiConfigRequest {
    pub name: String,
    pub states: serde_json::Value,
    pub transitions: serde_json::Value,
    pub element_images: serde_json::Value,
    pub description: Option<String>,
    pub similarity: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct CaptureMultiStateRequest {
    pub name: String,
    pub interactions: Vec<CaptureInteraction>,
    pub min_element_size: Option<i32>,
    pub description: Option<String>,
    pub similarity: Option<f64>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CaptureInteraction {
    pub action_type: String,
    pub target: Option<String>,
    pub state_name: String,
    pub wait_seconds: Option<f64>,
}

// ============================================================================
// Helpers
// ============================================================================

/// Auto-detect the webview content area position and DPI scale factor.
///
/// UI Bridge element rects are relative to the viewport, but screenshots capture
/// the entire monitor. This helper maps viewport coords to screenshot coords by
/// detecting the content area's position on the monitor.
fn detect_window_geometry(
    app_handle: &tauri::AppHandle,
    override_x: Option<i32>,
    override_y: Option<i32>,
    override_scale: Option<f64>,
) -> (i32, i32, f64) {
    use tauri::Manager;
    let window = app_handle.get_webview_window(qontinui_runner_lib::get_main_window_label());
    if let Some(win) = window {
        let detected_scale = win.scale_factor().unwrap_or(1.0);
        let pos = win.inner_position().unwrap_or_default();
        info!(
            "GUI Config: Detected window content area at ({}, {}), scale={}",
            pos.x, pos.y, detected_scale
        );
        (
            override_x.unwrap_or(pos.x),
            override_y.unwrap_or(pos.y),
            override_scale.unwrap_or(detected_scale),
        )
    } else {
        (
            override_x.unwrap_or(0),
            override_y.unwrap_or(0),
            override_scale.unwrap_or(1.0),
        )
    }
}

/// Bring the runner window to the foreground so screenshots capture its content.
///
/// Screenshots capture the full screen, so the runner window must be on top.
/// Without this, other windows (terminals, editors) covering the runner would
/// be captured instead of the runner's UI.
fn focus_runner_window(app_handle: &tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let win = app_handle
        .get_webview_window(qontinui_runner_lib::get_main_window_label())
        .ok_or_else(|| "runner main window not found".to_string())?;
    win.set_focus().map_err(|e| {
        error!("GUI Config: Failed to focus runner window: {}", e);
        format!("failed to focus runner window: {e}")
    })?;
    info!("GUI Config: Runner window focused for screenshot capture");
    Ok(())
}

/// Whether the runner's main window currently holds the OS foreground.
///
/// `set_focus()` can return `Ok` while Windows foreground-lock silently refuses
/// to raise the window (another app owns the foreground). Comparing the runner
/// window's HWND to `GetForegroundWindow()` — after the focus transition has
/// settled — detects that no-op so the caller can report it honestly instead of
/// claiming a capture-ready window. On non-Windows there is no foreground-lock
/// equivalent, so we report confirmed.
#[cfg(windows)]
fn runner_window_has_foreground(app_handle: &tauri::AppHandle) -> Result<bool, String> {
    use tauri::Manager;
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    let win = app_handle
        .get_webview_window(qontinui_runner_lib::get_main_window_label())
        .ok_or_else(|| "runner main window not found".to_string())?;
    let hwnd = win
        .hwnd()
        .map_err(|e| format!("window hwnd() failed: {e}"))?;
    // SAFETY: GetForegroundWindow takes no arguments and only reads OS state.
    let foreground = unsafe { GetForegroundWindow() };
    Ok(hwnd.0 as isize == foreground.0 as isize)
}

#[cfg(not(windows))]
fn runner_window_has_foreground(_app_handle: &tauri::AppHandle) -> Result<bool, String> {
    Ok(true)
}

// ============================================================================
// Handlers
// ============================================================================

/// POST /gui-config/capture-elements
///
/// Captures element images from the current UI by delegating to Python which:
/// 1. Fetches the UI Bridge snapshot via HTTP
/// 2. Captures a screenshot via the HAL
/// 3. Runs the ElementImagePipeline to crop each element
///
/// Returns element images as a dict of element_id -> {base64_png, width, height, ...}
pub async fn capture_elements(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CaptureElementsRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("GUI Config: Capturing elements from current UI");

    // NOTE: Window focus is handled by Python via Windows API (ctypes) before
    // each screenshot capture. We don't call focus_runner_window() here because
    // Python runs inside spawn_blocking — an HTTP callback would deadlock.

    let api_port = state
        .app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);

    let (offset_x, offset_y, scale) = detect_window_geometry(
        &state.app_handle,
        request.window_offset_x,
        request.window_offset_y,
        request.scale_factor,
    );

    let params = serde_json::json!({
        "api_port": api_port,
        "window_offset_x": offset_x,
        "window_offset_y": offset_y,
        "scale_factor": scale,
        "category_filter": request.category_filter,
        "min_element_size": request.min_element_size.unwrap_or(4),
        "padding": request.padding.unwrap_or(0),
    });

    let app_state_clone = state.app_state.clone();
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state_clone, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            let timeout = std::time::Duration::from_secs(60);
            bridge.send_command_and_wait("gui_config_capture_elements", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("spawn_blocking error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                Ok(Json(ApiResponse::success(
                    response
                        .data
                        .unwrap_or(serde_json::json!({"success": true})),
                )))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Pipeline failed".to_string());
                error!("GUI Config: capture_elements failed: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("GUI Config: capture_elements IPC error: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// POST /gui-config/build
///
/// Builds a QontinuiConfig from element images and state/transition definitions.
/// The output is a complete JSON config ready for import into the web's
/// State Machine page at /automation-builder/states.
pub async fn build_gui_config(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<BuildGuiConfigRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("GUI Config: Building config '{}'", request.name);

    let params = serde_json::json!({
        "name": request.name,
        "states": request.states,
        "transitions": request.transitions,
        "element_images": request.element_images,
        "description": request.description.unwrap_or_default(),
        "similarity": request.similarity.unwrap_or(0.85),
    });

    let app_state_clone = state.app_state.clone();
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state_clone, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            let timeout = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("gui_config_build", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("spawn_blocking error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                Ok(Json(ApiResponse::success(
                    response
                        .data
                        .unwrap_or(serde_json::json!({"success": true})),
                )))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Config build failed".to_string());
                error!("GUI Config: build failed: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("GUI Config: build IPC error: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// POST /gui-config/capture-multi-state
///
/// Orchestrates multi-state GUI config capture by:
/// 1. Walking through a sequence of interactions (click, scroll, etc.)
/// 2. At each step: capturing a screenshot + UI Bridge snapshot
/// 3. Diffing element sets to find only NEW elements per state
/// 4. Cropping element images and building a complete QontinuiConfig
///
/// Returns a complete multi-state config with images, states, and transitions.
pub async fn capture_multi_state(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CaptureMultiStateRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "GUI Config: Multi-state capture '{}' with {} interactions",
        request.name,
        request.interactions.len()
    );

    // NOTE: Window focus is handled by Python via Windows API (ctypes) before
    // each screenshot capture inside the loop.

    let api_port = state
        .app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);

    let (offset_x, offset_y, scale) = detect_window_geometry(&state.app_handle, None, None, None);

    let params = serde_json::json!({
        "api_port": api_port,
        "window_offset_x": offset_x,
        "window_offset_y": offset_y,
        "scale_factor": scale,
        "name": request.name,
        "interactions": request.interactions,
        "min_element_size": request.min_element_size.unwrap_or(4),
        "description": request.description.unwrap_or_default(),
        "similarity": request.similarity.unwrap_or(0.85),
    });

    let app_state_clone = state.app_state.clone();
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state_clone, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            // Multi-state capture with html2canvas rendering can take several minutes
            // (30+ elements × 9 states × ~2s per element render)
            let timeout = std::time::Duration::from_secs(600);
            bridge.send_command_and_wait("gui_config_capture_multi_state", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("spawn_blocking error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                Ok(Json(ApiResponse::success(
                    response
                        .data
                        .unwrap_or(serde_json::json!({"success": true})),
                )))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Multi-state capture failed".to_string());
                error!("GUI Config: capture_multi_state failed: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("GUI Config: capture_multi_state IPC error: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// POST /gui-config/focus-window
///
/// Brings the runner window to the foreground. Called by the Python bridge
/// before each screenshot capture to ensure the runner's content is visible.
///
/// Reports focus failure honestly: returns 500 (not a false success) if the
/// runner window can't be found, `set_focus()` errors, OR `set_focus()`
/// silently no-ops — i.e. it returned `Ok` but the window never actually
/// reached the foreground (Windows foreground-lock when another app owns
/// focus). The post-settle `GetForegroundWindow()` check catches that no-op so
/// a caller doesn't proceed to capture an occluded window believing it's on
/// top. (Removing the focus dependency entirely is the occlusion-immune
/// per-window capture follow-up; this endpoint stays load-bearing for
/// full-screen / Python-HAL captures, which remain focus-dependent.)
pub async fn focus_window(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    focus_runner_window(&state.app_handle)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    // Small delay to let the window manager finish the focus transition.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    // Verify the window actually reached the foreground — `set_focus()` can
    // return Ok while foreground-lock keeps another window on top.
    match runner_window_has_foreground(&state.app_handle) {
        Ok(true) => Ok(Json(ApiResponse::success(()))),
        Ok(false) => {
            warn!(
                "GUI Config: focus-window no-op — runner did not reach the \
                 foreground (foreground-lock / another window holds focus)"
            );
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(
                    "runner window did not reach the foreground (another window \
                     holds focus / Windows foreground-lock); a capture now may \
                     show the occluding window",
                )),
            ))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// POST /gui-config/start-executor
///
/// Ensures the Python executor is running. Creates and starts the bridge if
/// it doesn't exist. This is an HTTP-accessible version of the Tauri IPC
/// `start_python_executor` command for use by external tools.
pub async fn start_executor(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("GUI Config: Starting Python executor via HTTP");

    let app_state = state.app_state.clone();
    let result = tokio::task::spawn_blocking(move || {
        get_or_create_default_bridge(&app_state).map_err(|e| {
            error!("Failed to start Python executor: {}", e);
            format!("Failed to start Python executor: {}", e)
        })
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("spawn_blocking error: {}", e))),
        )
    })?;

    match result {
        Ok(_) => {
            info!("GUI Config: Python executor started successfully");
            Ok(Json(ApiResponse::success(
                "Python executor started".to_string(),
            )))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::post;

    axum::Router::new()
        .route("/gui-config/capture-elements", post(capture_elements))
        .route("/gui-config/build", post(build_gui_config))
        .route("/gui-config/capture-multi-state", post(capture_multi_state))
        .route("/gui-config/focus-window", post(focus_window))
        .route("/gui-config/start-executor", post(start_executor))
}
