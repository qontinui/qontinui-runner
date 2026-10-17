//! GUI Config Pipeline handlers for MCP API
//!
//! Provides HTTP endpoints to build visual GUI automation configs
//! (QontinuiConfig format) from caller-supplied element images, plus the
//! window-focus and executor-start helpers.
//!
//! ## The element-capture routes are gone, and answer 410
//!
//! `POST /gui-config/capture-elements` and `POST /gui-config/capture-multi-state`
//! used to dispatch the Python commands `gui_config_capture_elements` /
//! `gui_config_capture_multi_state`, which fetched element images from
//! `POST /ui-bridge/control/capture-element-images` — an html2canvas render in
//! the frontend. That runner route was deleted by the vision-pipeline Phase 2
//! (#119), so the pipeline already failed on every call. qontinui-runner#2242
//! then deleted the frontend `capture_element_images` handler and html2canvas
//! itself, and the Python commands were deleted after it.
//!
//! The two routes stay mounted only so a remaining external caller (the
//! qontinui-mcp `capture_gui_elements` / `capture_multi_state_gui_config` tools)
//! gets an explicit `410 Gone` naming the removal instead of a bare 404 or a
//! Python "unknown command". They never reach the executor.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::executor::{get_or_create_default_bridge, with_default_bridge};
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct BuildGuiConfigRequest {
    pub name: String,
    pub states: serde_json::Value,
    pub transitions: serde_json::Value,
    pub element_images: serde_json::Value,
    pub description: Option<String>,
    pub similarity: Option<f64>,
}

// ============================================================================
// Helpers
// ============================================================================

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

/// Why both element-capture routes answer 410 — see the module docs.
const CAPTURE_REMOVED_MESSAGE: &str = "unsupported: GUI element capture was removed in \
     qontinui-runner#2242 (the html2canvas DOM capture behind \
     /ui-bridge/control/capture-element-images is gone). Supply element images \
     to POST /gui-config/build yourself, or capture through /ui-bridge/vision/*.";

/// POST /gui-config/capture-elements and POST /gui-config/capture-multi-state
///
/// Always `410 Gone` with [`CAPTURE_REMOVED_MESSAGE`]. No body is read and the
/// Python executor is never contacted.
pub async fn capture_removed() -> (StatusCode, Json<ApiResponse<()>>) {
    warn!("GUI Config: element-capture route called after its removal (#2242)");
    (StatusCode::GONE, Json(api_error(CAPTURE_REMOVED_MESSAGE)))
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
    let result = spawn_blocking_tracked(move || {
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
    let result = spawn_blocking_tracked(move || {
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
        .route("/gui-config/capture-elements", post(capture_removed))
        .route("/gui-config/build", post(build_gui_config))
        .route("/gui-config/capture-multi-state", post(capture_removed))
        .route("/gui-config/focus-window", post(focus_window))
        .route("/gui-config/start-executor", post(start_executor))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both removed capture routes answer 410 with a message naming the
    /// removal, so a remaining caller learns why rather than seeing a 404.
    #[tokio::test]
    async fn capture_removed_answers_gone_and_names_the_removal() {
        let (status, Json(body)) = capture_removed().await;
        assert_eq!(status, StatusCode::GONE);
        assert!(!body.success);
        let error = body.error.expect("410 body carries an error message");
        assert!(error.starts_with("unsupported:"), "{error}");
        assert!(error.contains("#2242"), "{error}");
    }
}
