//! UI Bridge handlers for MCP API
//!
//! Provides HTTP handlers for UI Bridge control (React UI automation)
//! and UI Bridge exploration (qontinui library via Python bridge).

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use tauri::Emitter;

use crate::executor::with_default_bridge;
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::timeout_config::Timeouts;

// ============================================================================
// Types
// ============================================================================

/// Request to execute an action on an element
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIBridgeActionRequest {
    action: String,
    #[serde(default)]
    params: Option<serde_json::Value>,
    #[serde(default)]
    wait_options: Option<serde_json::Value>,
}

/// Request to execute an action on a component
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIBridgeComponentActionRequest {
    #[serde(default)]
    params: Option<serde_json::Value>,
}

/// Discovery options request
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIBridgeDiscoveryRequest {
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    interactive_only: Option<bool>,
    #[serde(default)]
    include_hidden: Option<bool>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    types: Option<Vec<String>>,
    #[serde(default)]
    selector: Option<String>,
}

/// Request to start UI Bridge exploration
#[derive(Debug, Deserialize)]
pub struct StartUIBridgeExplorationRequest {
    /// Target type: "web", "desktop", or "mobile"
    #[serde(default = "default_target_type")]
    pub target_type: String,
    /// Connection URL for the target application
    pub connection_url: String,
    /// Maximum navigation depth (default: 2)
    #[serde(default)]
    pub max_depth: Option<i32>,
    /// Maximum elements per page (default: 20)
    #[serde(default)]
    pub max_elements_per_page: Option<i32>,
    /// Maximum total elements to explore (default: 100)
    #[serde(default)]
    pub max_total_elements: Option<i32>,
    /// Delay between actions in milliseconds (default: 500)
    #[serde(default)]
    pub action_delay_ms: Option<i32>,
    /// Keywords in element text/id to skip
    #[serde(default)]
    pub blocked_keywords: Option<Vec<String>>,
    /// Keywords that are always safe to interact with
    #[serde(default)]
    pub safe_keywords: Option<Vec<String>>,
    /// CSS selectors to skip
    #[serde(default)]
    pub blocked_selectors: Option<Vec<String>>,
    /// Whether to capture screenshots (default: false)
    #[serde(default)]
    pub capture_screenshots: Option<bool>,
    /// Whether to run state discovery on results (default: true)
    #[serde(default)]
    pub run_state_discovery: Option<bool>,
}

fn default_target_type() -> String {
    "web".to_string()
}

/// Request to write to the clipboard
#[derive(Debug, Deserialize)]
pub struct ClipboardWriteRequest {
    pub text: String,
    #[serde(default)]
    pub html: Option<String>,
}

/// Request for getting UI Bridge exploration status
#[derive(Debug, Deserialize)]
pub struct UIBridgeExplorationStatusRequest {
    pub job_id: Option<String>,
}

/// Request for discovering states from render logs
#[derive(Debug, Deserialize)]
pub struct DiscoverStatesRequest {
    /// Array of DOM snapshot render log entries
    pub render_logs: Vec<serde_json::Value>,
}

// ============================================================================
// Helper Functions
// ============================================================================

/// UI Bridge timeout is fetched from centralized config
/// This needs a reasonable timeout since it's synchronous communication with the frontend.
fn get_ui_bridge_timeout_ms() -> u64 {
    Timeouts::ui_bridge_ipc().as_millis() as u64
}

/// Send a UI Bridge request and wait for the response synchronously.
///
/// This creates a oneshot channel, stores the sender in the pending map,
/// emits the request to the frontend, and waits for the response with a timeout.
pub async fn ui_bridge_request_sync(
    state: &Arc<ApiState>,
    request_type: &str,
    additional_payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let request_id = uuid::Uuid::new_v4().to_string();

    // Create the full event payload
    let mut event_payload = serde_json::json!({
        "requestId": request_id,
        "type": request_type
    });

    // Merge additional payload fields
    if let (Some(base), Some(additional)) = (
        event_payload.as_object_mut(),
        additional_payload.as_object(),
    ) {
        for (key, value) in additional {
            base.insert(key.clone(), value.clone());
        }
    }

    // Create oneshot channel for the response
    let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();

    // Store the sender in the pending map
    {
        let mut pending = state.ui_bridge_pending.lock().await;
        pending.insert(request_id.clone(), tx);
    }

    // Emit request to React frontend
    if let Err(e) = state.app_handle.emit("ui-bridge-request", &event_payload) {
        // Clean up the pending entry
        let mut pending = state.ui_bridge_pending.lock().await;
        pending.remove(&request_id);
        return Err(format!("Failed to emit UI Bridge request: {}", e));
    }

    // Wait for response with timeout
    let timeout_duration = std::time::Duration::from_millis(get_ui_bridge_timeout_ms());
    match tokio::time::timeout(timeout_duration, rx).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(_)) => {
            // Channel was closed without sending
            Err("UI Bridge request channel closed unexpectedly".to_string())
        }
        Err(_) => {
            // Timeout - clean up the pending entry
            let mut pending = state.ui_bridge_pending.lock().await;
            pending.remove(&request_id);
            Err(format!(
                "UI Bridge request timed out after {}ms. Is the frontend running?",
                get_ui_bridge_timeout_ms()
            ))
        }
    }
}

// ============================================================================
// Public: Response Handler (used by Tauri event listener in create_router)
// ============================================================================

/// Handle incoming UI Bridge response from the frontend.
///
/// This is called by the Tauri event listener set up in create_router.
pub async fn handle_ui_bridge_response(
    pending: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>,
        >,
    >,
    response: serde_json::Value,
) {
    let request_id = response
        .get("requestId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if let Some(request_id) = request_id {
        let mut pending_map = pending.lock().await;
        if let Some(sender) = pending_map.remove(&request_id) {
            // Extract the data portion of the response
            let data = response.get("data").cloned().unwrap_or(response.clone());
            if sender.send(data).is_err() {
                warn!(
                    "UI Bridge: Failed to send response, receiver dropped for request {}",
                    request_id
                );
            } else {
                debug!("UI Bridge: Delivered response for request {}", request_id);
            }
        } else {
            warn!(
                "UI Bridge: No pending request found for response {}",
                request_id
            );
        }
    } else {
        warn!("UI Bridge: Response missing requestId: {:?}", response);
    }
}

// ============================================================================
// Control Handlers
// ============================================================================

/// Get all registered UI elements from the React UI Bridge.
pub async fn ui_bridge_get_elements_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting all elements");

    match ui_bridge_request_sync(&state, "get_elements", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a specific element by ID.
pub async fn ui_bridge_get_element_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting element {}", id);

    match ui_bridge_request_sync(
        &state,
        "get_element",
        serde_json::json!({ "elementId": id }),
    )
    .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute an action on an element.
pub async fn ui_bridge_execute_action_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<UIBridgeActionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: Executing action {} on element {}",
        request.action, id
    );

    let payload = serde_json::json!({
        "elementId": id,
        "action": {
            "action": request.action,
            "params": request.params,
            "waitOptions": request.wait_options
        }
    });

    match ui_bridge_request_sync(&state, "execute_action", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get all registered components.
pub async fn ui_bridge_get_components_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting all components");

    match ui_bridge_request_sync(&state, "get_components", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a specific component by ID.
pub async fn ui_bridge_get_component_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting component {}", id);

    match ui_bridge_request_sync(
        &state,
        "get_component",
        serde_json::json!({ "componentId": id }),
    )
    .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute an action on a component.
pub async fn ui_bridge_execute_component_action_handler(
    State(state): State<Arc<ApiState>>,
    Path((id, action_id)): Path<(String, String)>,
    Json(request): Json<UIBridgeComponentActionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: Executing action {} on component {}",
        action_id, id
    );

    let payload = serde_json::json!({
        "componentId": id,
        "actionId": action_id,
        "params": request.params
    });

    match ui_bridge_request_sync(&state, "execute_component_action", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Discover controllable elements in the UI.
pub async fn ui_bridge_discover_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<UIBridgeDiscoveryRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Discovering elements");

    let payload = serde_json::json!({
        "options": {
            "root": request.root,
            "interactiveOnly": request.interactive_only,
            "includeHidden": request.include_hidden,
            "limit": request.limit,
            "types": request.types,
            "selector": request.selector
        }
    });

    match ui_bridge_request_sync(&state, "discover", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a full snapshot of the UI Bridge state.
pub async fn ui_bridge_get_snapshot_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting snapshot");

    match ui_bridge_request_sync(&state, "get_snapshot", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Read the current system clipboard content.
pub async fn ui_bridge_clipboard_read_handler(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<serde_json::Value>> {
    info!("UI Bridge API: Reading clipboard");

    match arboard::Clipboard::new() {
        Ok(mut clipboard) => {
            let text = clipboard.get_text().ok();
            let has_text = text.is_some();
            Json(ApiResponse::success(serde_json::json!({
                "text": text,
                "formats": if has_text { vec!["text/plain"] } else { vec![] as Vec<&str> },
            })))
        }
        Err(e) => {
            error!("UI Bridge API: Clipboard read failed: {}", e);
            Json(ApiResponse::error(format!("Clipboard read failed: {}", e)))
        }
    }
}

/// Write text to the system clipboard.
pub async fn ui_bridge_clipboard_write_handler(
    State(_state): State<Arc<ApiState>>,
    Json(body): Json<ClipboardWriteRequest>,
) -> Json<ApiResponse<serde_json::Value>> {
    info!("UI Bridge API: Writing to clipboard");

    match arboard::Clipboard::new() {
        Ok(mut clipboard) => {
            if let Some(html) = &body.html {
                // Write both HTML and plain text alternatives
                let alt_text = body.text.clone();
                match clipboard.set_html(html.as_str(), Some(&alt_text)) {
                    Ok(()) => Json(ApiResponse::success(serde_json::json!({
                        "written": true,
                        "formats": ["text/html", "text/plain"],
                    }))),
                    Err(e) => {
                        error!("UI Bridge API: Clipboard HTML write failed: {}", e);
                        Json(ApiResponse::error(format!("Clipboard write failed: {}", e)))
                    }
                }
            } else {
                match clipboard.set_text(&body.text) {
                    Ok(()) => Json(ApiResponse::success(serde_json::json!({
                        "written": true,
                        "formats": ["text/plain"],
                    }))),
                    Err(e) => {
                        error!("UI Bridge API: Clipboard write failed: {}", e);
                        Json(ApiResponse::error(format!("Clipboard write failed: {}", e)))
                    }
                }
            }
        }
        Err(e) => {
            error!("UI Bridge API: Clipboard init failed: {}", e);
            Json(ApiResponse::error(format!(
                "Clipboard initialization failed: {}",
                e
            )))
        }
    }
}

/// Get undo/redo state from the UI Bridge.
pub async fn ui_bridge_get_undo_state_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting undo state");

    match ui_bridge_request_sync(&state, "get_undo_state", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute undo via the UI Bridge.
pub async fn ui_bridge_undo_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Undo");

    match ui_bridge_request_sync(&state, "undo", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Undo failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute redo via the UI Bridge.
pub async fn ui_bridge_redo_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Redo");

    match ui_bridge_request_sync(&state, "redo", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Redo failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get form state awareness data from the UI Bridge.
pub async fn ui_bridge_get_forms_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting form state");

    match ui_bridge_request_sync(&state, "get_forms", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Smart form fill action via the UI Bridge.
pub async fn ui_bridge_fill_form_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Fill form");

    match ui_bridge_request_sync(&state, "fill_form", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Fill form failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Capture a form state snapshot via the UI Bridge.
pub async fn ui_bridge_snapshot_forms_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Snapshot forms");

    match ui_bridge_request_sync(&state, "snapshot_forms", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Snapshot forms failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Diff two form snapshots via the UI Bridge.
pub async fn ui_bridge_diff_forms_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Diff forms");

    match ui_bridge_request_sync(&state, "diff_forms", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Diff forms failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get console errors captured by the UI Bridge ConsoleCapture.
pub async fn ui_bridge_get_console_errors_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ConsoleErrorsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting console errors");

    let payload = serde_json::json!({
        "params": {
            "since": query.since,
            "limit": query.limit
        }
    });

    match ui_bridge_request_sync(&state, "get_console_errors", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Clear console errors captured by the UI Bridge ConsoleCapture.
pub async fn ui_bridge_clear_console_errors_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Clearing console errors");

    match ui_bridge_request_sync(&state, "clear_console_errors", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Network Request Monitoring Handlers
// ============================================================================

/// List network requests with optional filters.
pub async fn ui_bridge_get_network_requests_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<NetworkRequestsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting network requests");

    let payload = serde_json::json!({
        "params": {
            "status": query.status,
            "method": query.method,
            "urlPattern": query.url_pattern,
            "failuresOnly": query.failures_only,
            "since": query.since,
            "limit": query.limit
        }
    });

    match ui_bridge_request_sync(&state, "get_network_requests", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get currently in-flight network requests.
pub async fn ui_bridge_get_network_requests_in_flight_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting in-flight network requests");

    match ui_bridge_request_sync(
        &state,
        "get_network_requests_in_flight",
        serde_json::json!({}),
    )
    .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Wait for a specific network request matching criteria.
pub async fn ui_bridge_wait_for_network_request_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Wait for network request");

    match ui_bridge_request_sync(&state, "wait_for_network_request", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a specific network request by ID.
pub async fn ui_bridge_get_network_request_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting network request {}", id);

    match ui_bridge_request_sync(
        &state,
        "get_network_request",
        serde_json::json!({ "id": id }),
    )
    .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get all loaded specs from the SpecStore.
pub async fn ui_bridge_get_specs_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting all specs");

    match ui_bridge_request_sync(&state, "get_specs", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a specific spec by ID from the SpecStore.
pub async fn ui_bridge_get_spec_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting spec {}", id);

    match ui_bridge_request_sync(&state, "get_spec", serde_json::json!({ "specId": id })).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Page Navigation Handlers
// ============================================================================

/// Query parameters for console errors endpoint
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleErrorsQuery {
    #[serde(default)]
    since: Option<f64>,
    #[serde(default)]
    limit: Option<u32>,
}

/// Query parameters for network requests endpoint
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkRequestsQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    url_pattern: Option<String>,
    #[serde(default)]
    failures_only: Option<bool>,
    #[serde(default)]
    since: Option<f64>,
    #[serde(default)]
    limit: Option<u32>,
}

/// Request for page navigation
#[derive(Debug, Deserialize)]
pub struct PageNavigateRequest {
    url: String,
}

/// Request for CSS selector query
#[derive(Debug, Deserialize)]
pub struct QuerySelectorRequest {
    pub selector: String,
    /// Optional action to perform on matched element(s): "click"
    pub action: Option<String>,
    /// Index of the matched element to perform the action on (default: 0)
    pub index: Option<u32>,
}

/// Request for page evaluation
#[derive(Debug, Deserialize)]
pub struct PageEvaluateRequest {
    pub expression: String,
}

/// Refresh the page.
pub async fn ui_bridge_page_refresh_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Page refresh");

    match ui_bridge_request_sync(&state, "page_refresh", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Navigate to a URL.
pub async fn ui_bridge_page_navigate_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<PageNavigateRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Page navigate to {}", request.url);

    let payload = serde_json::json!({ "url": request.url });

    match ui_bridge_request_sync(&state, "page_navigate", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Go back in browser history.
pub async fn ui_bridge_page_go_back_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Page go back");

    match ui_bridge_request_sync(&state, "page_go_back", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Go forward in browser history.
pub async fn ui_bridge_page_go_forward_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Page go forward");

    match ui_bridge_request_sync(&state, "page_go_forward", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Query elements by CSS selector, optionally performing an action.
pub async fn ui_bridge_query_selector_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<QuerySelectorRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Query selector '{}'", request.selector);

    let payload = serde_json::json!({
        "selector": request.selector,
        "index": request.index,
        "params": {
            "action": request.action,
        },
    });

    match ui_bridge_request_sync(&state, "query_selector", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Evaluate a JavaScript expression in the webview.
pub async fn ui_bridge_page_evaluate_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<PageEvaluateRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: Page evaluate ({}...)",
        &request.expression[..request.expression.len().min(80)]
    );

    let payload = serde_json::json!({ "expression": request.expression });

    match ui_bridge_request_sync(&state, "page_evaluate", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Exploration Handlers
// ============================================================================

/// Start UI Bridge exploration using qontinui library
/// Returns a job_id that can be used to poll for status and results
pub async fn start_ui_bridge_exploration(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartUIBridgeExplorationRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
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
// Window Listing & App-Specific Screenshots (xcap)
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

/// Query parameters for annotated screenshot
#[derive(Debug, Deserialize)]
pub struct AnnotatedScreenshotQuery {
    /// Monitor index (0-based), None for primary monitor. Used for full-screen capture.
    #[serde(default)]
    monitor: Option<i32>,
    /// Capture a specific window by title (case-insensitive substring match)
    #[serde(default)]
    window_title: Option<String>,
    /// Capture a specific window by app name (case-insensitive substring match)
    #[serde(default)]
    app_name: Option<String>,
    /// Capture a specific window by its ID (HWND as u32)
    #[serde(default)]
    window_id: Option<u32>,
    /// Capture the runner's own window
    #[serde(default)]
    runner: Option<bool>,
}

/// Annotated screenshot response
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotatedScreenshotData {
    screenshot: String,
    width: i32,
    height: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    monitor: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_app_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_id: Option<u32>,
}

/// Encode a DynamicImage as base64 PNG.
fn encode_image_to_base64(image: &image::DynamicImage) -> Result<String, String> {
    use base64::Engine;
    let mut png_bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut png_bytes, image::ImageFormat::Png)
        .map_err(|e| format!("Failed to encode PNG: {}", e))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(png_bytes.into_inner()))
}

/// Capture a specific window by matching criteria.
fn capture_window_screenshot(
    window_title: Option<String>,
    app_name: Option<String>,
    window_id: Option<u32>,
) -> Result<AnnotatedScreenshotData, String> {
    use xcap::Window;

    let windows = Window::all().map_err(|e| format!("Failed to enumerate windows: {}", e))?;

    let target = if let Some(id) = window_id {
        windows
            .iter()
            .find(|w| w.id().unwrap_or(0) == id)
            .ok_or_else(|| format!("No window found with id {}", id))?
    } else if let Some(ref title_query) = window_title {
        let query_lower = title_query.to_lowercase();
        windows
            .iter()
            .find(|w| {
                w.title()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query_lower)
            })
            .ok_or_else(|| {
                let available: Vec<String> = windows
                    .iter()
                    .filter_map(|w| {
                        let t = w.title().unwrap_or_default();
                        if t.is_empty() {
                            None
                        } else {
                            Some(t)
                        }
                    })
                    .take(10)
                    .collect();
                format!(
                    "No window found matching title '{}'. Available: {:?}",
                    title_query, available
                )
            })?
    } else if let Some(ref app_query) = app_name {
        let query_lower = app_query.to_lowercase();
        windows
            .iter()
            .find(|w| {
                w.app_name()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query_lower)
            })
            .ok_or_else(|| {
                let available: Vec<String> = windows
                    .iter()
                    .filter_map(|w| {
                        let a = w.app_name().unwrap_or_default();
                        if a.is_empty() {
                            None
                        } else {
                            Some(a)
                        }
                    })
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .take(10)
                    .collect();
                format!(
                    "No window found matching app_name '{}'. Available: {:?}",
                    app_query, available
                )
            })?
    } else {
        return Err("No window selection criteria provided".to_string());
    };

    let title = target.title().unwrap_or_default();
    let app = target.app_name().unwrap_or_default();
    let id = target.id().unwrap_or(0);

    if target.is_minimized().unwrap_or(false) {
        return Err(format!(
            "Window '{}' ({}) is minimized — cannot capture",
            title, app
        ));
    }

    let image = target
        .capture_image()
        .map_err(|e| format!("Failed to capture window '{}': {}", title, e))?;

    let width = image.width() as i32;
    let height = image.height() as i32;
    let dynamic = image::DynamicImage::ImageRgba8(image);
    let b64 = encode_image_to_base64(&dynamic)?;

    Ok(AnnotatedScreenshotData {
        screenshot: b64,
        width,
        height,
        monitor: None,
        window_title: Some(title),
        window_app_name: Some(app),
        window_id: Some(id),
    })
}

/// Capture the runner's own window by cropping from a monitor screenshot.
/// xcap skips same-process windows, so we capture the monitor and crop.
///
/// DPI handling:
/// - Tauri `outer_position()` / `outer_size()` return physical pixels.
/// - xcap `Monitor::x()` / `y()` return logical coordinates (dmPosition).
/// - xcap `Monitor::width()` / `height()` return physical pixels (dmPelsWidth/Height).
/// - The captured image is at physical resolution.
///
/// To match monitors: convert Tauri physical position to logical using scale_factor.
/// To crop the image: work in physical pixels (image coords = physical).
fn capture_runner_window(
    phys_x: i32,
    phys_y: i32,
    phys_w: u32,
    phys_h: u32,
    scale: f64,
    title: &str,
) -> Result<AnnotatedScreenshotData, String> {
    use xcap::Monitor;

    let monitors = Monitor::all().map_err(|e| format!("Failed to enumerate monitors: {}", e))?;
    if monitors.is_empty() {
        return Err("No monitors found".to_string());
    }

    // Convert Tauri physical position to logical for monitor matching.
    // xcap monitor x/y are logical (dmPosition), width/height are physical (dmPelsWidth).
    let logical_x = (phys_x as f64 / scale) as i32;
    let logical_y = (phys_y as f64 / scale) as i32;
    let logical_center_x = logical_x + (phys_w as f64 / scale / 2.0) as i32;
    let logical_center_y = logical_y + (phys_h as f64 / scale / 2.0) as i32;

    let (monitor, mon_logical_x, mon_logical_y) = monitors
        .iter()
        .find_map(|m| {
            let mx = m.x().unwrap_or(0);
            let my = m.y().unwrap_or(0);
            let mon_scale = m.scale_factor().unwrap_or(1.0) as f64;
            // Monitor logical dimensions = physical / scale
            let mw_logical = (m.width().unwrap_or(0) as f64 / mon_scale) as i32;
            let mh_logical = (m.height().unwrap_or(0) as f64 / mon_scale) as i32;
            if logical_center_x >= mx
                && logical_center_x < mx + mw_logical
                && logical_center_y >= my
                && logical_center_y < my + mh_logical
            {
                Some((m, mx, my))
            } else {
                None
            }
        })
        .ok_or_else(|| "Runner window not on any monitor".to_string())?;

    let mon_scale = monitor.scale_factor().unwrap_or(1.0) as f64;
    let full_image = monitor
        .capture_image()
        .map_err(|e| format!("Failed to capture monitor: {}", e))?;

    // Convert logical window position to physical pixel offset in the captured image.
    // Offset in logical coords relative to monitor origin, then scale to physical.
    let rel_logical_x = logical_x - mon_logical_x;
    let rel_logical_y = logical_y - mon_logical_y;
    let rel_phys_x = (rel_logical_x as f64 * mon_scale) as i32;
    let rel_phys_y = (rel_logical_y as f64 * mon_scale) as i32;

    // Handle negative offsets (window partially off-screen)
    let crop_x = rel_phys_x.max(0) as u32;
    let crop_y = rel_phys_y.max(0) as u32;
    let crop_w = if rel_phys_x < 0 {
        phys_w.saturating_sub((-rel_phys_x) as u32)
    } else {
        phys_w
    }
    .min(full_image.width().saturating_sub(crop_x));
    let crop_h = if rel_phys_y < 0 {
        phys_h.saturating_sub((-rel_phys_y) as u32)
    } else {
        phys_h
    }
    .min(full_image.height().saturating_sub(crop_y));

    if crop_w == 0 || crop_h == 0 {
        return Err(format!(
            "Runner window has zero visible area (crop: {}x{} at ({}, {}), image: {}x{}, scale: {})",
            crop_w, crop_h, crop_x, crop_y, full_image.width(), full_image.height(), mon_scale
        ));
    }

    let full_dynamic = image::DynamicImage::ImageRgba8(full_image);
    let cropped = full_dynamic.crop_imm(crop_x, crop_y, crop_w, crop_h);
    let b64 = encode_image_to_base64(&cropped)?;

    Ok(AnnotatedScreenshotData {
        screenshot: b64,
        width: crop_w as i32,
        height: crop_h as i32,
        monitor: None,
        window_title: Some(title.to_string()),
        window_app_name: Some("Qontinui Runner".to_string()),
        window_id: None,
    })
}

/// Capture a full monitor screenshot.
fn capture_monitor_screenshot(
    monitor_index: Option<i32>,
) -> Result<AnnotatedScreenshotData, String> {
    use xcap::Monitor;

    let monitors = Monitor::all().map_err(|e| format!("Failed to enumerate monitors: {}", e))?;
    if monitors.is_empty() {
        return Err("No monitors found".to_string());
    }

    let monitor = if let Some(idx) = monitor_index {
        if idx < 0 {
            return Err(format!("Monitor index must be non-negative, got {}", idx));
        }
        monitors
            .into_iter()
            .nth(idx as usize)
            .ok_or_else(|| format!("Monitor index {} out of range", idx))?
    } else {
        monitors.into_iter().next().unwrap()
    };

    let image = monitor
        .capture_image()
        .map_err(|e| format!("Failed to capture monitor: {}", e))?;

    let width = image.width() as i32;
    let height = image.height() as i32;
    let dynamic = image::DynamicImage::ImageRgba8(image);
    let b64 = encode_image_to_base64(&dynamic)?;

    Ok(AnnotatedScreenshotData {
        screenshot: b64,
        width,
        height,
        monitor: monitor_index,
        window_title: None,
        window_app_name: None,
        window_id: None,
    })
}

/// GET /ui-bridge/control/annotated-screenshot — Screenshot with metadata
///
/// Captures natively via xcap (Rust). No Python executor dependency.
///
/// Query params (all optional, first match wins):
/// - `runner=true` — capture the runner's own Tauri window
/// - `window_title=...` — case-insensitive substring match on window title
/// - `app_name=...` — case-insensitive substring match on app name
/// - `window_id=N` — exact window ID (HWND)
/// - `monitor=N` — full monitor capture (0-based index, default: primary)
/// - (none) — captures primary monitor
pub async fn ui_bridge_annotated_screenshot_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AnnotatedScreenshotQuery>,
) -> Json<ApiResponse<AnnotatedScreenshotData>> {
    let is_window_capture = query.runner.unwrap_or(false)
        || query.window_title.is_some()
        || query.app_name.is_some()
        || query.window_id.is_some();

    if is_window_capture {
        info!(
            runner = ?query.runner,
            window_title = ?query.window_title,
            app_name = ?query.app_name,
            window_id = ?query.window_id,
            "UI Bridge API: Capturing window screenshot (native)"
        );

        // For runner's own window, xcap skips same-process windows,
        // so we capture the monitor and crop to the window bounds.
        if query.runner.unwrap_or(false) {
            use tauri::Manager;
            let window = state.app_handle.get_webview_window("main");
            if let Some(win) = window {
                let scale = win.scale_factor().unwrap_or(1.0);
                let pos = win.outer_position().unwrap_or_default();
                let size = win.outer_size().unwrap_or_default();
                let x = pos.x;
                let y = pos.y;
                let w = size.width;
                let h = size.height;
                let title = win
                    .title()
                    .unwrap_or_else(|_| "Qontinui Runner".to_string());

                return match tokio::task::spawn_blocking(move || {
                    capture_runner_window(x, y, w, h, scale, &title)
                })
                .await
                {
                    Ok(Ok(data)) => {
                        info!(
                            "UI Bridge screenshot: Captured runner window ({}x{})",
                            data.width, data.height
                        );
                        Json(ApiResponse::success(data))
                    }
                    Ok(Err(e)) => {
                        error!("UI Bridge screenshot: Runner capture failed: {}", e);
                        Json(ApiResponse::error(format!(
                            "Runner screenshot failed: {}",
                            e
                        )))
                    }
                    Err(e) => {
                        error!("UI Bridge screenshot: Task join error: {}", e);
                        Json(ApiResponse::error(format!(
                            "Screenshot capture task failed: {}",
                            e
                        )))
                    }
                };
            } else {
                return Json(ApiResponse::error("Runner window not found".to_string()));
            }
        }

        let window_title = query.window_title;
        let app_name = query.app_name;
        let window_id = query.window_id;

        match tokio::task::spawn_blocking(move || {
            capture_window_screenshot(window_title, app_name, window_id)
        })
        .await
        {
            Ok(Ok(data)) => {
                info!(
                    "UI Bridge screenshot: Captured window '{}' ({}x{}, id={})",
                    data.window_title.as_deref().unwrap_or("?"),
                    data.width,
                    data.height,
                    data.window_id.unwrap_or(0),
                );
                Json(ApiResponse::success(data))
            }
            Ok(Err(e)) => {
                error!("UI Bridge screenshot: Window capture failed: {}", e);
                Json(ApiResponse::error(format!(
                    "Window screenshot failed: {}",
                    e
                )))
            }
            Err(e) => {
                error!("UI Bridge screenshot: Task join error: {}", e);
                Json(ApiResponse::error(format!(
                    "Screenshot capture task failed: {}",
                    e
                )))
            }
        }
    } else {
        // Full monitor capture (existing behavior)
        info!(
            monitor = ?query.monitor,
            "UI Bridge API: Capturing monitor screenshot (native)"
        );

        let monitor = query.monitor;
        match tokio::task::spawn_blocking(move || capture_monitor_screenshot(monitor)).await {
            Ok(Ok(data)) => {
                info!(
                    "UI Bridge screenshot: Captured {}x{} from monitor {:?}",
                    data.width, data.height, data.monitor
                );
                Json(ApiResponse::success(data))
            }
            Ok(Err(e)) => {
                error!("UI Bridge screenshot: Monitor capture failed: {}", e);
                Json(ApiResponse::error(format!(
                    "Screenshot capture failed: {}",
                    e
                )))
            }
            Err(e) => {
                error!("UI Bridge screenshot: Task join error: {}", e);
                Json(ApiResponse::error(format!(
                    "Screenshot capture task failed: {}",
                    e
                )))
            }
        }
    }
}

// ============================================================================
// Design Review Handlers (Control Mode)
// ============================================================================

/// Get extended computed styles for a single element.
pub async fn ui_bridge_design_element_styles_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get element styles for {}", id);

    let payload = serde_json::json!({
        "elementId": id
    });

    match ui_bridge_request_sync(&state, "design_get_element_styles", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design element styles failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get styles across interaction states (hover, focus, active, disabled).
pub async fn ui_bridge_design_state_styles_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get state styles for {}", id);

    let mut payload = serde_json::json!({
        "elementId": id
    });

    if let Some(Json(body)) = body {
        if let (Some(base), Some(extra)) = (payload.as_object_mut(), body.as_object()) {
            for (key, value) in extra {
                base.insert(key.clone(), value.clone());
            }
        }
    }

    match ui_bridge_request_sync(&state, "design_get_state_styles", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design state styles failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get design snapshot for all or filtered elements.
pub async fn ui_bridge_design_snapshot_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get design snapshot");

    let payload = body.map(|Json(b)| b).unwrap_or(serde_json::json!({}));

    match ui_bridge_request_sync(&state, "design_get_snapshot", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design snapshot failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Capture responsive snapshots at multiple viewport widths.
pub async fn ui_bridge_design_responsive_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get responsive snapshots");

    match ui_bridge_request_sync(&state, "design_get_responsive", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design responsive failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Run a style audit against a loaded or provided style guide.
pub async fn ui_bridge_design_audit_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - run style audit");

    let payload = body.map(|Json(b)| b).unwrap_or(serde_json::json!({}));

    match ui_bridge_request_sync(&state, "design_run_audit", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design audit failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Load a style guide for subsequent audits.
pub async fn ui_bridge_design_load_style_guide_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - load style guide");

    match ui_bridge_request_sync(&state, "design_load_style_guide", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design load style guide failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get the currently loaded style guide.
pub async fn ui_bridge_design_get_style_guide_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get style guide");

    let payload = serde_json::json!({});

    match ui_bridge_request_sync(&state, "design_get_style_guide", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design get style guide failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Clear the currently loaded style guide.
pub async fn ui_bridge_design_clear_style_guide_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - clear style guide");

    let payload = serde_json::json!({});

    match ui_bridge_request_sync(&state, "design_clear_style_guide", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design clear style guide failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ── Change tracking handlers ─────────────────────────────────────────

/// Save a bookmark (snapshot) by name.
pub async fn ui_bridge_save_bookmark_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Save bookmark");
    match ui_bridge_request_sync(&state, "save_bookmark", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Save bookmark failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a bookmark by name.
pub async fn ui_bridge_get_bookmark_handler(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get bookmark '{}'", name);
    match ui_bridge_request_sync(&state, "get_bookmark", serde_json::json!({"name": name})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Get bookmark failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Delete a bookmark by name.
pub async fn ui_bridge_delete_bookmark_handler(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Delete bookmark '{}'", name);
    match ui_bridge_request_sync(&state, "delete_bookmark", serde_json::json!({"name": name})).await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Delete bookmark failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// List all bookmarks.
pub async fn ui_bridge_list_bookmarks_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: List bookmarks");
    match ui_bridge_request_sync(&state, "list_bookmarks", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: List bookmarks failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Diff current state from a named bookmark.
pub async fn ui_bridge_diff_from_bookmark_handler(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Diff from bookmark '{}'", name);
    match ui_bridge_request_sync(
        &state,
        "diff_from_bookmark",
        serde_json::json!({"name": name}),
    )
    .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Diff from bookmark failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute an action and return the diff.
pub async fn ui_bridge_execute_with_diff_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Execute with diff");
    match ui_bridge_request_sync(&state, "execute_with_diff", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Execute with diff failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Wait for a change to occur.
pub async fn ui_bridge_wait_for_change_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Wait for change");
    match ui_bridge_request_sync(&state, "wait_for_change", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Wait for change failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Categorize the last diff.
pub async fn ui_bridge_categorize_last_diff_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Categorize last diff");
    match ui_bridge_request_sync(&state, "categorize_last_diff", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Categorize last diff failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Compute a scoped diff.
pub async fn ui_bridge_scoped_diff_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Scoped diff");
    match ui_bridge_request_sync(&state, "scoped_diff", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Scoped diff failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Summarize a diff.
pub async fn ui_bridge_summarize_diff_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Summarize diff");
    match ui_bridge_request_sync(&state, "summarize_diff", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Summarize diff failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get structured changes.
pub async fn ui_bridge_structured_changes_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Structured changes");
    match ui_bridge_request_sync(&state, "structured_changes", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Structured changes failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Enable the change buffer.
pub async fn ui_bridge_enable_change_buffer_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Enable change buffer");
    match ui_bridge_request_sync(&state, "enable_change_buffer", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Enable change buffer failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Disable the change buffer.
pub async fn ui_bridge_disable_change_buffer_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Disable change buffer");
    match ui_bridge_request_sync(&state, "disable_change_buffer", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Disable change buffer failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Drain the change buffer.
pub async fn ui_bridge_drain_change_buffer_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Drain change buffer");
    match ui_bridge_request_sync(&state, "drain_change_buffer", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Drain change buffer failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get the change buffer size.
pub async fn ui_bridge_get_change_buffer_size_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get change buffer size");
    match ui_bridge_request_sync(&state, "get_change_buffer_size", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Get change buffer size failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Keyboard Shortcuts Handler
// ============================================================================

/// Get discovered keyboard shortcuts.
pub async fn ui_bridge_get_keyboard_shortcuts_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting keyboard shortcuts");

    match ui_bridge_request_sync(&state, "get_keyboard_shortcuts", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Idle Detection Handlers
// ============================================================================

/// Get composite idle status.
pub async fn ui_bridge_get_idle_status_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting idle status");

    match ui_bridge_request_sync(&state, "get_idle_status", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Wait for composite idle state.
pub async fn ui_bridge_wait_for_idle_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Wait for idle");

    let payload = serde_json::json!({
        "params": {
            "timeout": body.get("timeout").and_then(|v| v.as_i64()).unwrap_or(30000),
            "minStableMs": body.get("minStableMs").and_then(|v| v.as_i64()).unwrap_or(500),
            "exclude": body.get("exclude")
        }
    });

    match ui_bridge_request_sync(&state, "wait_for_idle", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Wait for idle failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// AI Search & Find Handlers
// ============================================================================

/// AI-powered element search.
pub async fn ui_bridge_ai_search_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI search");

    let payload = serde_json::json!({ "params": body });

    match ui_bridge_request_sync(&state, "ai_search", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: AI search failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Natural language element find.
pub async fn ui_bridge_ai_find_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI find");

    let payload = serde_json::json!({ "params": body });

    match ui_bridge_request_sync(&state, "ai_find", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: AI find failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/ui-bridge/control/elements",
            get(ui_bridge_get_elements_handler),
        )
        .route(
            "/ui-bridge/control/element/:id",
            get(ui_bridge_get_element_handler),
        )
        .route(
            "/ui-bridge/control/element/:id/action",
            post(ui_bridge_execute_action_handler),
        )
        .route(
            "/ui-bridge/control/components",
            get(ui_bridge_get_components_handler),
        )
        .route(
            "/ui-bridge/control/component/:id",
            get(ui_bridge_get_component_handler),
        )
        .route(
            "/ui-bridge/control/component/:id/action/:action_id",
            post(ui_bridge_execute_component_action_handler),
        )
        .route(
            "/ui-bridge/control/discover",
            post(ui_bridge_discover_handler),
        )
        .route(
            "/ui-bridge/control/snapshot",
            get(ui_bridge_get_snapshot_handler),
        )
        .route(
            "/ui-bridge/control/windows",
            get(ui_bridge_list_windows_handler),
        )
        .route(
            "/ui-bridge/control/annotated-screenshot",
            get(ui_bridge_annotated_screenshot_handler),
        )
        .route(
            "/ui-bridge/control/console-errors",
            get(ui_bridge_get_console_errors_handler),
        )
        .route(
            "/ui-bridge/control/console-errors/clear",
            post(ui_bridge_clear_console_errors_handler),
        )
        // Undo/Redo awareness
        .route(
            "/ui-bridge/control/undo-state",
            get(ui_bridge_get_undo_state_handler),
        )
        .route("/ui-bridge/control/undo", post(ui_bridge_undo_handler))
        .route("/ui-bridge/control/redo", post(ui_bridge_redo_handler))
        // Form state awareness
        .route("/ui-bridge/control/forms", get(ui_bridge_get_forms_handler))
        .route("/ui-bridge/control/fill", post(ui_bridge_fill_form_handler))
        .route(
            "/ui-bridge/control/forms/snapshot",
            post(ui_bridge_snapshot_forms_handler),
        )
        .route(
            "/ui-bridge/control/forms/diff",
            post(ui_bridge_diff_forms_handler),
        )
        // Clipboard
        .route(
            "/ui-bridge/control/clipboard",
            get(ui_bridge_clipboard_read_handler).post(ui_bridge_clipboard_write_handler),
        )
        // Network request monitoring
        .route(
            "/ui-bridge/control/network-requests",
            get(ui_bridge_get_network_requests_handler),
        )
        .route(
            "/ui-bridge/control/network-requests/in-flight",
            get(ui_bridge_get_network_requests_in_flight_handler),
        )
        .route(
            "/ui-bridge/control/network-requests/wait",
            post(ui_bridge_wait_for_network_request_handler),
        )
        .route(
            "/ui-bridge/control/network-request/:id",
            get(ui_bridge_get_network_request_handler),
        )
        .route("/ui-bridge/control/specs", get(ui_bridge_get_specs_handler))
        .route(
            "/ui-bridge/control/spec/:id",
            get(ui_bridge_get_spec_handler),
        )
        .route(
            "/ui-bridge/control/page/refresh",
            post(ui_bridge_page_refresh_handler),
        )
        .route(
            "/ui-bridge/control/page/navigate",
            post(ui_bridge_page_navigate_handler),
        )
        .route(
            "/ui-bridge/control/page/back",
            post(ui_bridge_page_go_back_handler),
        )
        .route(
            "/ui-bridge/control/page/forward",
            post(ui_bridge_page_go_forward_handler),
        )
        .route(
            "/ui-bridge/control/query-selector",
            post(ui_bridge_query_selector_handler),
        )
        .route(
            "/ui-bridge/control/page/evaluate",
            post(ui_bridge_page_evaluate_handler),
        )
        // Design Review
        .route(
            "/ui-bridge/control/design/element/:id/styles",
            get(ui_bridge_design_element_styles_handler),
        )
        .route(
            "/ui-bridge/control/design/element/:id/state-styles",
            post(ui_bridge_design_state_styles_handler),
        )
        .route(
            "/ui-bridge/control/design/snapshot",
            post(ui_bridge_design_snapshot_handler),
        )
        .route(
            "/ui-bridge/control/design/responsive",
            post(ui_bridge_design_responsive_handler),
        )
        .route(
            "/ui-bridge/control/design/audit",
            post(ui_bridge_design_audit_handler),
        )
        .route(
            "/ui-bridge/control/design/style-guide/load",
            post(ui_bridge_design_load_style_guide_handler),
        )
        .route(
            "/ui-bridge/control/design/style-guide",
            get(ui_bridge_design_get_style_guide_handler),
        )
        .route(
            "/ui-bridge/control/design/style-guide/clear",
            post(ui_bridge_design_clear_style_guide_handler),
        )
        // Change tracking
        .route(
            "/ui-bridge/control/ai/bookmarks",
            get(ui_bridge_list_bookmarks_handler).post(ui_bridge_save_bookmark_handler),
        )
        .route(
            "/ui-bridge/control/ai/bookmark/:name",
            get(ui_bridge_get_bookmark_handler).delete(ui_bridge_delete_bookmark_handler),
        )
        .route(
            "/ui-bridge/control/ai/bookmark/:name/diff",
            get(ui_bridge_diff_from_bookmark_handler),
        )
        .route(
            "/ui-bridge/control/ai/execute-with-diff",
            post(ui_bridge_execute_with_diff_handler),
        )
        .route(
            "/ui-bridge/control/ai/wait-for-change",
            post(ui_bridge_wait_for_change_handler),
        )
        .route(
            "/ui-bridge/control/ai/categorize-last-diff",
            get(ui_bridge_categorize_last_diff_handler),
        )
        .route(
            "/ui-bridge/control/ai/scoped-diff",
            post(ui_bridge_scoped_diff_handler),
        )
        .route(
            "/ui-bridge/control/ai/summarize-diff",
            post(ui_bridge_summarize_diff_handler),
        )
        .route(
            "/ui-bridge/control/ai/structured-changes",
            post(ui_bridge_structured_changes_handler),
        )
        .route(
            "/ui-bridge/control/ai/change-buffer/enable",
            post(ui_bridge_enable_change_buffer_handler),
        )
        .route(
            "/ui-bridge/control/ai/change-buffer/disable",
            post(ui_bridge_disable_change_buffer_handler),
        )
        .route(
            "/ui-bridge/control/ai/change-buffer/drain",
            post(ui_bridge_drain_change_buffer_handler),
        )
        .route(
            "/ui-bridge/control/ai/change-buffer/size",
            get(ui_bridge_get_change_buffer_size_handler),
        )
        // Keyboard shortcuts
        .route(
            "/ui-bridge/control/keyboard-shortcuts",
            get(ui_bridge_get_keyboard_shortcuts_handler),
        )
        // Idle detection
        .route(
            "/ui-bridge/control/idle-status",
            get(ui_bridge_get_idle_status_handler),
        )
        .route(
            "/ui-bridge/control/wait-for-idle",
            post(ui_bridge_wait_for_idle_handler),
        )
        // AI search & find
        .route(
            "/ui-bridge/control/ai/search",
            post(ui_bridge_ai_search_handler),
        )
        .route(
            "/ui-bridge/control/ai/find",
            post(ui_bridge_ai_find_handler),
        )
        // Exploration
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
}
