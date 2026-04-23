//! UI Bridge exploration + window listing HTTP handlers.
//!
//! Exploration is the qontinui-library driven automation surface: it spawns
//! a Python job that crawls a connected runner/web/mobile UI Bridge and
//! reports back discovered elements/states. `discover_states_from_renders`
//! is the post-hoc state-discovery variant over existing render logs.
//!
//! Window listing (`ui_bridge_list_windows_handler` + its `list_windows_native`
//! helper) lives here too because the only family it would otherwise belong
//! to is screenshots, which hasn't been extracted yet.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
};
use tracing::{error, info};

use crate::executor::with_default_bridge;
use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::types::{
    DiscoverStatesRequest, StartUIBridgeExplorationRequest, UIBridgeExplorationStatusRequest,
};

/// Start UI Bridge exploration (spawns a Python background job).
pub async fn start_ui_bridge_exploration(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let request: StartUIBridgeExplorationRequest = match serde_json::from_value(body) {
        Ok(r) => r,
        Err(e) => {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ApiResponse::error(format!(
                    "Invalid request: {}. Required fields: connection_url (string). \
                     Optional: target_type (\"web\"|\"desktop\"|\"mobile\", default \"web\"), \
                     max_depth (int, default 2), max_elements_per_page (int, default 20), \
                     max_total_elements (int, default 100), action_delay_ms (int, default 500), \
                     blocked_keywords (string[]), safe_keywords (string[]), \
                     blocked_selectors (string[]), capture_screenshots (bool, default false), \
                     run_state_discovery (bool, default true). \
                     Example: {{\"connection_url\": \"http://localhost:3001\", \"target_type\": \"web\"}}",
                    e
                ))),
            ));
        }
    };
    info!(
        "MCP API: Starting UI Bridge exploration for URL: {} (type: {})",
        request.connection_url, request.target_type
    );

    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
        "target_type": request.target_type,
        "connection_url": request.connection_url,
        "max_depth": request.max_depth.unwrap_or(2),
        "max_elements_per_page": request.max_elements_per_page.unwrap_or(20),
        "max_total_elements": request.max_total_elements.unwrap_or(100),
        "action_delay_ms": request.action_delay_ms.unwrap_or(500),
        "blocked_keywords": request.blocked_keywords.clone().unwrap_or_default(),
        "safe_keywords": request.safe_keywords.clone().unwrap_or_default(),
        "blocked_selectors": request.blocked_selectors.clone().unwrap_or_default(),
        "capture_screenshots": request.capture_screenshots.unwrap_or(false),
        "run_state_discovery": request.run_state_discovery.unwrap_or(true),
    });

    // Short timeout since this just starts the background job
    let timeout = std::time::Duration::from_secs(30);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("start_ui_bridge_exploration", Some(params), timeout)
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
                info!("MCP API: UI Bridge exploration job started");
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
                    .unwrap_or_else(|| "Failed to start UI Bridge exploration".to_string());
                error!(
                    "MCP API: Failed to start UI Bridge exploration: {}",
                    error_msg
                );
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to start UI Bridge exploration: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get UI Bridge exploration status
pub async fn get_ui_bridge_exploration_status(
    State(state): State<Arc<ApiState>>,
    Query(request): Query<UIBridgeExplorationStatusRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "job_id": request.job_id,
    });

    let timeout = std::time::Duration::from_secs(10);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("get_ui_bridge_exploration_status", Some(params), timeout)
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
                        "status": "unknown"
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to get exploration status".to_string());
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to get UI Bridge exploration status: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get UI Bridge exploration results
pub async fn get_ui_bridge_exploration_results(
    State(state): State<Arc<ApiState>>,
    Query(request): Query<UIBridgeExplorationStatusRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "job_id": request.job_id,
    });

    let timeout = std::time::Duration::from_secs(30);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("get_ui_bridge_exploration_results", Some(params), timeout)
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
                        "data": null
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to get exploration results".to_string());
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!(
                "MCP API: Failed to get UI Bridge exploration results: {}",
                e
            );
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Stop UI Bridge exploration
pub async fn stop_ui_bridge_exploration(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stopping UI Bridge exploration");

    let app_state = state.app_state.clone();

    let timeout = std::time::Duration::from_secs(10);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("stop_ui_bridge_exploration", None, timeout)
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
                info!("MCP API: UI Bridge exploration stop requested");
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "message": "Stop requested"
                }))))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to stop exploration".to_string());
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to stop UI Bridge exploration: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Discover states from render logs using co-occurrence analysis
/// This endpoint runs state discovery on existing render logs without exploration
pub async fn discover_states_from_renders(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<DiscoverStatesRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Discovering states from {} render logs",
        request.render_logs.len()
    );

    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
        "render_logs": request.render_logs,
    });

    // Allow more time for analysis of large render logs
    let timeout = std::time::Duration::from_secs(60);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("discover_states_from_renders", Some(params), timeout)
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
                info!("MCP API: State discovery completed successfully");
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "states": [],
                        "elements": [],
                        "elementToRenders": {},
                        "renderCount": 0,
                        "uniqueElementCount": 0
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to discover states from renders".to_string());
                error!("MCP API: Failed to discover states: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to discover states from renders: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// =============================================================================
// Window Listing (xcap)
// =============================================================================

/// Info about a capturable window
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    id: u32,
    title: String,
    app_name: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    is_minimized: bool,
    is_maximized: bool,
    is_focused: bool,
}

/// List all capturable windows using xcap.
fn list_windows_native() -> Result<Vec<WindowInfo>, String> {
    use xcap::Window;

    let windows = Window::all().map_err(|e| format!("Failed to enumerate windows: {}", e))?;
    let mut result = Vec::new();

    for w in &windows {
        let id = w.id().unwrap_or(0);
        let title = w.title().unwrap_or_default();
        let app_name = w.app_name().unwrap_or_default();

        // Skip windows with no title (background/system windows)
        if title.is_empty() {
            continue;
        }

        result.push(WindowInfo {
            id,
            title,
            app_name,
            x: w.x().unwrap_or(0),
            y: w.y().unwrap_or(0),
            width: w.width().unwrap_or(0),
            height: w.height().unwrap_or(0),
            is_minimized: w.is_minimized().unwrap_or(false),
            is_maximized: w.is_maximized().unwrap_or(false),
            is_focused: w.is_focused().unwrap_or(false),
        });
    }

    Ok(result)
}

/// GET /ui-bridge/control/windows — List all capturable windows
pub async fn ui_bridge_list_windows_handler(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<WindowInfo>>> {
    match tokio::task::spawn_blocking(list_windows_native).await {
        Ok(Ok(windows)) => {
            info!("UI Bridge: Listed {} capturable windows", windows.len());
            Json(ApiResponse::success(windows))
        }
        Ok(Err(e)) => {
            error!("UI Bridge: Failed to list windows: {}", e);
            Json(ApiResponse::error(format!("Failed to list windows: {}", e)))
        }
        Err(e) => {
            error!("UI Bridge: Window list task failed: {}", e);
            Json(ApiResponse::error(format!(
                "Window list task failed: {}",
                e
            )))
        }
    }
}

// ============================================================================
// Route registration
// ============================================================================

/// UI Bridge exploration + window listing routes.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/ui-bridge/explore", post(start_ui_bridge_exploration))
        .route(
            "/ui-bridge/explore/status",
            get(get_ui_bridge_exploration_status),
        )
        .route(
            "/ui-bridge/explore/results",
            get(get_ui_bridge_exploration_results),
        )
        .route("/ui-bridge/explore/stop", post(stop_ui_bridge_exploration))
        .route(
            "/ui-bridge/discover-states",
            post(discover_states_from_renders),
        )
        .route(
            "/ui-bridge/control/windows",
            get(ui_bridge_list_windows_handler),
        )
}

/// Static (method, path) tuples matching every route registered by `routes()`.
pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("POST", "/ui-bridge/explore"),
        ("GET", "/ui-bridge/explore/status"),
        ("GET", "/ui-bridge/explore/results"),
        ("POST", "/ui-bridge/explore/stop"),
        ("POST", "/ui-bridge/discover-states"),
        ("GET", "/ui-bridge/control/windows"),
    ]
}
