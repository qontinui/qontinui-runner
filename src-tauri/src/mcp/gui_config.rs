//! GUI Config Pipeline handlers for MCP API
//!
//! Provides HTTP endpoints to capture element images from the current UI
//! and build visual GUI automation configs (QontinuiConfig format).
//!
//! The pipeline combines UI Bridge snapshot data (element positions) with
//! screen captures, then uses the qontinui core library to crop elements
//! and assemble configs.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info};

use crate::executor::with_default_bridge;
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::mcp::ui_bridge::ui_bridge_request_sync;

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

// ============================================================================
// Handlers
// ============================================================================

/// POST /gui-config/capture-elements
///
/// Captures element images from the current UI by:
/// 1. Getting a UI Bridge snapshot (element positions)
/// 2. Capturing a screenshot via Python HAL
/// 3. Running the ElementImagePipeline to crop each element
///
/// Returns element images as a dict of element_id -> {base64_png, width, height, ...}
pub async fn capture_elements(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CaptureElementsRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("GUI Config: Capturing elements from current UI");

    // Step 1: Get UI Bridge snapshot
    let snapshot = match ui_bridge_request_sync(&state, "getSnapshot", serde_json::json!({})).await
    {
        Ok(snap) => snap,
        Err(e) => {
            error!("GUI Config: Failed to get UI Bridge snapshot: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("UI Bridge snapshot failed: {}", e))),
            ));
        }
    };

    // Step 2: Capture screenshot via Python IPC
    let screenshot_response = match crate::mcp::misc::capture_screenshot_ipc(
        state.app_state.clone(),
        None, // all monitors
        "png",
    )
    .await
    {
        Ok(response) => response,
        Err(e) => {
            error!("GUI Config: Failed to capture screenshot: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Screenshot capture failed: {}", e))),
            ));
        }
    };

    let screenshot_base64 = match screenshot_response
        .get("screenshot_base64")
        .and_then(|s| s.as_str())
    {
        Some(s) => s.to_string(),
        None => {
            error!("GUI Config: No screenshot_base64 in response");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error("No screenshot data in response")),
            ));
        }
    };

    // Step 3: Send snapshot + screenshot to Python for pipeline processing
    let params = serde_json::json!({
        "snapshot": snapshot,
        "screenshot_base64": screenshot_base64,
        "window_offset_x": request.window_offset_x.unwrap_or(0),
        "window_offset_y": request.window_offset_y.unwrap_or(0),
        "scale_factor": request.scale_factor.unwrap_or(1.0),
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

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::post;

    axum::Router::new()
        .route("/gui-config/capture-elements", post(capture_elements))
        .route("/gui-config/build", post(build_gui_config))
}
