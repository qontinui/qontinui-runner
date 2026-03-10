//! SDK Client — Direct HTTP communication with UI Bridge SDK apps
//!
//! Instead of going through the Chrome extension relay (WebSocket → extension → browser),
//! this module communicates directly with SDK-integrated apps via HTTP:
//!
//! ```text
//! Frontend → POST /ui-bridge/sdk/* → Runner (this module) → HTTP → SDK App
//! ```
//!
//! This is faster and more reliable for apps that have the UI Bridge SDK embedded.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use super::types::{ApiResponse, ApiState};
use super::ui_bridge::ui_bridge_request_sync;

// =============================================================================
// Types
// =============================================================================

/// Information about a connected SDK app
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SdkAppInfo {
    pub app_id: String,
    pub app_name: String,
    pub app_type: String,
    pub framework: Option<String>,
    pub version: Option<String>,
    pub capabilities: Vec<String>,
    pub port: u16,
}

/// Active connection to an SDK app
pub struct SdkConnection {
    pub app_url: String,
    pub base_path: String,
    pub app_info: SdkAppInfo,
    pub client: reqwest::Client,
    pub connected_at: i64,
}

/// Manages multiple simultaneous SDK connections with one active connection
pub struct SdkConnectionManager {
    pub connections: HashMap<String, SdkConnection>,
    pub active_url: Option<String>,
}

impl SdkConnectionManager {
    pub fn new() -> Self {
        Self {
            connections: HashMap::new(),
            active_url: None,
        }
    }

    /// Get the currently active connection
    pub fn active_connection(&self) -> Option<&SdkConnection> {
        self.active_url
            .as_ref()
            .and_then(|url| self.connections.get(url))
    }
}

/// Request body for connecting to an SDK app
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectRequest {
    url: String,
    app_id: Option<String>,
    app_name: Option<String>,
    app_type: Option<String>,
    framework: Option<String>,
    port: Option<u16>,
}

/// Status response for the SDK connection
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SdkStatusResponse {
    connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    app: Option<SdkAppInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    connected_at: Option<i64>,
    /// All active connections (not just the active one)
    all_connections: Vec<ConnectionInfo>,
}

/// Information about a single connection in the manager
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionInfo {
    url: String,
    app: SdkAppInfo,
    connected_at: i64,
    is_active: bool,
}

/// Request body for switching active connection
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SwitchRequest {
    url: String,
}

/// Request body for disconnecting a specific connection
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DisconnectRequest {
    /// If provided, disconnect this specific URL. If not provided, disconnect the active connection.
    #[serde(default)]
    url: Option<String>,
}

/// Health check result for a single connection
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionHealthResult {
    url: String,
    healthy: bool,
    app_name: String,
}

/// Connect response
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectResponse {
    pub app: SdkAppInfo,
    pub url: String,
    pub base_path: String,
}

// =============================================================================
// Core Client
// =============================================================================

/// Send an HTTP request to the connected SDK app
pub async fn sdk_request(
    state: &Arc<ApiState>,
    method: Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let conn_guard = state.sdk_connection.lock().await;
    let conn = conn_guard
        .active_connection()
        .ok_or_else(|| "No active SDK app connection".to_string())?;

    let url = format!("{}{}{}", conn.app_url, conn.base_path, path);
    debug!(url = %url, method = %method, "SDK request");

    let mut request = conn.client.request(method.clone(), &url);

    if let Some(body) = body {
        request = request.json(&body);
    }

    let response = request.send().await.map_err(|e| {
        format!(
            "Failed to reach SDK app at {}: {}",
            url,
            if e.is_timeout() {
                "Request timed out".to_string()
            } else if e.is_connect() {
                "Connection refused — app may have stopped".to_string()
            } else {
                e.to_string()
            }
        )
    })?;

    let status = response.status();
    let response_text = response
        .text()
        .await
        .map_err(|e| format!("Failed to read response body: {}", e))?;

    // Try to parse as JSON
    let json: serde_json::Value = serde_json::from_str(&response_text).map_err(|_| {
        if !status.is_success() {
            format!("SDK app returned HTTP {}: {}", status, response_text)
        } else {
            format!(
                "SDK app returned non-JSON response: {}",
                &response_text[..response_text.len().min(200)]
            )
        }
    })?;

    // Check if the response has a success field
    if let Some(success) = json.get("success").and_then(|v| v.as_bool()) {
        if !success {
            let error = json
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error from SDK app");
            return Err(error.to_string());
        }
    } else if !status.is_success() {
        return Err(format!("SDK app returned HTTP {}", status));
    }

    Ok(json)
}

// =============================================================================
// Handlers
// =============================================================================

/// Normalize localhost to 127.0.0.1 to avoid IPv6 resolution issues on Windows.
/// Windows often resolves `localhost` to `::1` (IPv6) first, but many dev servers
/// (Next.js, FastAPI, etc.) only bind to IPv4 `0.0.0.0`. This causes reqwest to
/// hang on the IPv6 connect attempt before falling back to IPv4, exceeding timeouts.
fn normalize_localhost_url(url: &str) -> String {
    url.replace("://localhost:", "://127.0.0.1:")
        .replace("://localhost/", "://127.0.0.1/")
        .replace("://localhost", "://127.0.0.1") // handle bare localhost without port
}

/// POST /ui-bridge/sdk/connect — Connect to an SDK app
async fn handle_connect(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<ConnectRequest>,
) -> Json<ApiResponse<ConnectResponse>> {
    let url = normalize_localhost_url(req.url.trim_end_matches('/'));
    info!(url = %url, "Connecting to SDK app");

    // Check if we're already connected to this URL
    {
        let mut manager = state.sdk_connection.lock().await;
        if manager.connections.contains_key(&url) {
            // Already connected — just make it active
            manager.active_url = Some(url.clone());
            let conn = manager.connections.get(&url).unwrap();
            let response = ConnectResponse {
                app: conn.app_info.clone(),
                url: conn.app_url.clone(),
                base_path: conn.base_path.clone(),
            };
            info!(
                "Switched active connection to already-connected SDK app: {}",
                response.app.app_name
            );
            return Json(ApiResponse::success(response));
        }
    }

    // Delegate to the shared connect_sdk_app function
    match connect_sdk_app(
        &state.sdk_connection,
        &url,
        req.port,
        req.app_id,
        req.app_name,
        req.app_type,
        req.framework,
    )
    .await
    {
        Ok(response) => Json(ApiResponse::success(response)),
        Err(e) => Json(ApiResponse::error(e)),
    }
}

/// Connect to an SDK app by URL. Shared logic used by both `handle_connect`
/// (user-initiated) and auto-connect (proxy creation).
///
/// When called from auto-connect, the `app_*` / `framework` hints will be `None`
/// and values are inferred from the health response.
pub async fn connect_sdk_app(
    sdk_connection: &tokio::sync::Mutex<SdkConnectionManager>,
    url: &str,
    port_hint: Option<u16>,
    app_id: Option<String>,
    app_name: Option<String>,
    app_type: Option<String>,
    framework: Option<String>,
) -> Result<ConnectResponse, String> {
    // Create HTTP client with timeout
    // 30s request timeout matches the UI Bridge IPC timeout — the app's snapshot/elements
    // endpoints proxy through the IPC layer and can take >10s on cold pages.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    // Try to hit the health endpoint to validate the app
    let (base_path, health_data) = try_health_check(&client, url).await?;

    // Extract app info from health response or from provided hints
    let ui_bridge_meta = health_data
        .get("uiBridge")
        .or_else(|| health_data.get("data").and_then(|d| d.get("uiBridge")));

    let port = port_hint.unwrap_or_else(|| {
        url.split(':')
            .next_back()
            .and_then(|p| p.parse().ok())
            .unwrap_or(0)
    });

    let app_info = SdkAppInfo {
        app_id: app_id.unwrap_or_else(|| {
            ui_bridge_meta
                .and_then(|m| m.get("appId").and_then(|v| v.as_str()))
                .unwrap_or("unknown")
                .to_string()
        }),
        app_name: app_name.unwrap_or_else(|| {
            ui_bridge_meta
                .and_then(|m| m.get("appName").and_then(|v| v.as_str()))
                .unwrap_or("Unknown App")
                .to_string()
        }),
        app_type: app_type.unwrap_or_else(|| {
            ui_bridge_meta
                .and_then(|m| m.get("appType").and_then(|v| v.as_str()))
                .unwrap_or("web")
                .to_string()
        }),
        framework: framework.or_else(|| {
            ui_bridge_meta
                .and_then(|m| m.get("framework").and_then(|v| v.as_str()))
                .map(|s| s.to_string())
        }),
        version: ui_bridge_meta
            .and_then(|m| m.get("version").and_then(|v| v.as_str()))
            .map(|s| s.to_string()),
        capabilities: ui_bridge_meta
            .and_then(|m| m.get("capabilities"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        port,
    };

    let connected_at = chrono::Utc::now().timestamp_millis();

    let response = ConnectResponse {
        app: app_info.clone(),
        url: url.to_string(),
        base_path: base_path.clone(),
    };

    // Store connection in manager and set as active
    let mut manager = sdk_connection.lock().await;
    manager.connections.insert(
        url.to_string(),
        SdkConnection {
            app_url: url.to_string(),
            base_path,
            app_info,
            client,
            connected_at,
        },
    );
    manager.active_url = Some(url.to_string());

    info!("Connected to SDK app: {}", response.app.app_name);
    Ok(response)
}

/// Try health check on an SDK app, returning (base_path, health_json)
async fn try_health_check(
    client: &reqwest::Client,
    url: &str,
) -> Result<(String, serde_json::Value), String> {
    // Try paths in order: Next.js API route, standard SDK path, root-level
    let paths: &[(&str, &str)] = &[
        ("/api/ui-bridge/health", "/api/ui-bridge"),
        ("/ui-bridge/health", "/ui-bridge"),
        ("/__ui-bridge/health", "/__ui-bridge"),
        ("/health", ""),
    ];

    let mut errors: Vec<String> = Vec::new();

    for (health_path, base_path) in paths {
        let health_url = format!("{}{}", url, health_path);
        match client.get(&health_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<serde_json::Value>().await {
                    Ok(json) => return Ok((base_path.to_string(), json)),
                    Err(e) => {
                        errors.push(format!("{} → invalid JSON: {}", health_path, e));
                    }
                }
            }
            Ok(resp) => {
                errors.push(format!("{} → HTTP {}", health_path, resp.status()));
            }
            Err(e) => {
                let reason = if e.is_connect() {
                    "connection refused".to_string()
                } else if e.is_timeout() {
                    "timed out".to_string()
                } else {
                    e.to_string()
                };
                errors.push(format!("{} → {}", health_path, reason));
            }
        }
    }

    Err(format!(
        "No UI Bridge health endpoint found at {}. Tried: {}",
        url,
        errors.join(", ")
    ))
}

/// POST /ui-bridge/sdk/disconnect — Disconnect from SDK app
///
/// If a URL is provided in the body, disconnect that specific connection.
/// If no URL is provided, disconnect the active connection.
async fn handle_disconnect(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<DisconnectRequest>>,
) -> Json<ApiResponse<String>> {
    let mut manager = state.sdk_connection.lock().await;

    let target_url = body
        .and_then(|b| b.0.url)
        .or_else(|| manager.active_url.clone());

    match target_url {
        Some(url) => {
            if let Some(conn) = manager.connections.remove(&url) {
                let name = conn.app_info.app_name.clone();
                // If we disconnected the active connection, clear active_url
                if manager.active_url.as_deref() == Some(&url) {
                    manager.active_url = None;
                }
                info!("Disconnected from SDK app: {} ({})", name, url);
                Json(ApiResponse::success(format!("Disconnected from {}", name)))
            } else {
                Json(ApiResponse::success(format!(
                    "No connection found for {}",
                    url
                )))
            }
        }
        None => Json(ApiResponse::success("Already disconnected".to_string())),
    }
}

/// GET /ui-bridge/sdk/status — Check connection status
async fn handle_status(State(state): State<Arc<ApiState>>) -> Json<ApiResponse<SdkStatusResponse>> {
    let manager = state.sdk_connection.lock().await;

    let all_connections: Vec<ConnectionInfo> = manager
        .connections
        .iter()
        .map(|(url, conn)| ConnectionInfo {
            url: url.clone(),
            app: conn.app_info.clone(),
            connected_at: conn.connected_at,
            is_active: manager.active_url.as_deref() == Some(url.as_str()),
        })
        .collect();

    match manager.active_connection() {
        Some(conn) => Json(ApiResponse::success(SdkStatusResponse {
            connected: true,
            app: Some(conn.app_info.clone()),
            url: Some(conn.app_url.clone()),
            connected_at: Some(conn.connected_at),
            all_connections,
        })),
        None => Json(ApiResponse::success(SdkStatusResponse {
            connected: false,
            app: None,
            url: None,
            connected_at: None,
            all_connections,
        })),
    }
}

/// GET /ui-bridge/sdk/health — Proxy health check to SDK app
async fn handle_health(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/health", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/elements — List all elements
///
/// Supports optional query parameters for filtering:
/// - `contentOnly=true` — return only content elements (exclude interactive)
/// - `contentTypes=heading` — filter to specific element types (comma-separated)
/// - `includeContent=false` — exclude content elements
/// - `contentRole=metric` — filter to a specific content role
///
/// Filtering is applied proxy-side since the frontend `/control/elements` returns all elements.
async fn handle_elements(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    match sdk_request(&state, Method::GET, "/control/elements", None).await {
        Ok(mut data) => {
            // Add helpful note if no elements found
            if let Some(arr) = data.get("data").and_then(|d| d.as_array()) {
                if arr.is_empty() {
                    if let Some(obj) = data.as_object_mut() {
                        obj.insert(
                            "note".to_string(),
                            serde_json::json!("No instrumented elements found. Ensure the UI Bridge SDK provider is mounted on the active page and elements are registered."),
                        );
                    }
                }
            }

            // Always add a `total` field for easier verification (e.g. jq -e '.total > 0')
            if let Some(arr) = data.get("data").and_then(|d| d.as_array()) {
                let total = arr.len();
                if let Some(obj) = data.as_object_mut() {
                    obj.insert("total".to_string(), serde_json::json!(total));
                }
            }

            // Apply proxy-side filters if query parameters are present.
            // The backend /control/elements endpoint doesn't support filtering,
            // so we fetch all elements and filter here.
            if !query.is_empty() {
                if let Some(elements) = data.get("data").and_then(|d| d.as_array()).cloned() {
                    let content_only = query
                        .get("contentOnly")
                        .map(|v| v == "true")
                        .unwrap_or(false);
                    let content_types: Option<Vec<&str>> = query
                        .get("contentTypes")
                        .map(|v| v.split(',').map(|s| s.trim()).collect());
                    let include_content = query
                        .get("includeContent")
                        .map(|v| v != "false")
                        .unwrap_or(true);
                    let content_role = query.get("contentRole");

                    let filtered: Vec<_> = elements
                        .into_iter()
                        .filter(|el| {
                            let category = el
                                .get("category")
                                .and_then(|v| v.as_str())
                                .unwrap_or("interactive");
                            let el_type = el.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            let el_role = el.get("contentRole").and_then(|v| v.as_str());

                            // contentOnly: keep only content category elements,
                            // but also include elements matching explicit contentTypes
                            if content_only && category != "content" {
                                if let Some(ref types) = content_types {
                                    if !types.contains(&el_type) {
                                        return false;
                                    }
                                } else {
                                    return false;
                                }
                            }

                            // includeContent=false: exclude content elements
                            if !include_content && category == "content" {
                                return false;
                            }

                            // contentTypes: filter by element type
                            if let Some(ref types) = content_types {
                                if !types.contains(&el_type) {
                                    return false;
                                }
                            }

                            // contentRole: filter by content role
                            if let Some(role) = content_role {
                                if el_role != Some(role.as_str()) {
                                    return false;
                                }
                            }

                            true
                        })
                        .collect();

                    if let Some(obj) = data.as_object_mut() {
                        let total = filtered.len();
                        obj.insert("data".to_string(), serde_json::json!(filtered));
                        obj.insert("total".to_string(), serde_json::json!(total));
                    }
                }
            }

            (StatusCode::OK, Json(data))
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "success": false, "error": e })),
        ),
    }
}

/// GET /ui-bridge/sdk/element/:id — Get a single element
async fn handle_element(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let path = format!("/control/element/{}", id);
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/element/:id/action — Execute an action on an element
async fn handle_element_action(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let path = format!("/control/element/{}/action", id);
    match sdk_request(&state, Method::POST, &path, Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/snapshot — Full UI snapshot
///
/// Falls back to the runner's own control endpoint when no SDK app is connected.
async fn handle_snapshot(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    match sdk_request(&state, Method::GET, "/control/snapshot", None).await {
        Ok(data) => (StatusCode::OK, Json(data)),
        Err(_sdk_err) => {
            // No SDK app connected — fall back to the runner's own UI via control endpoint
            debug!("SDK snapshot unavailable, falling back to control endpoint");
            match ui_bridge_request_sync(&state, "get_snapshot", serde_json::json!({})).await {
                Ok(data) => (
                    StatusCode::OK,
                    Json(serde_json::json!({ "success": true, "data": data })),
                ),
                Err(e) => (
                    StatusCode::BAD_GATEWAY,
                    Json(serde_json::json!({ "success": false, "error": e })),
                ),
            }
        }
    }
}

/// POST /ui-bridge/sdk/discover — Find/discover elements
async fn handle_discover(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/control/find", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/components — List components
async fn handle_components(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/control/components", None).await {
        Ok(mut data) => {
            // Add helpful note if no components found
            if let Some(arr) = data.get("data").and_then(|d| d.as_array()) {
                if arr.is_empty() {
                    if let Some(obj) = data.as_object_mut() {
                        obj.insert(
                            "note".to_string(),
                            serde_json::json!("No registered components found. Ensure the UI Bridge SDK provider is mounted and components are registered with useUIBridge or equivalent."),
                        );
                    }
                }
            }
            Json(data)
        }
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/component/:id — Get a single component
async fn handle_component(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let path = format!("/control/component/{}", id);
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/console-errors — Get console errors from SDK app
async fn handle_console_errors(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<serde_json::Value>,
) -> Json<serde_json::Value> {
    let mut path = "/control/console-errors".to_string();
    let mut params = vec![];
    if let Some(since) = query.get("since").and_then(|v| v.as_f64()) {
        params.push(format!("since={}", since));
    }
    if let Some(limit) = query.get("limit").and_then(|v| v.as_u64()) {
        params.push(format!("limit={}", limit));
    }
    if !params.is_empty() {
        path = format!("{}?{}", path, params.join("&"));
    }
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/console-errors/clear — Clear console errors in SDK app
async fn handle_clear_console_errors(
    State(state): State<Arc<ApiState>>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/control/console-errors/clear", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/search — AI-powered element search
async fn handle_ai_search(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/search", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/execute — AI-powered action execution
async fn handle_ai_execute(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/execute", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/assert — AI-powered assertions
async fn handle_ai_assert(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/assert", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/clipboard — Read clipboard (system-level, same as control)
async fn handle_clipboard_read(State(_state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    // Clipboard is a system resource — read directly via arboard, not through the SDK app
    match arboard::Clipboard::new() {
        Ok(mut clipboard) => {
            let text = clipboard.get_text().ok();
            let has_text = text.is_some();
            Json(serde_json::json!({
                "success": true,
                "data": {
                    "text": text,
                    "formats": if has_text { vec!["text/plain"] } else { vec![] as Vec<&str> },
                }
            }))
        }
        Err(e) => Json(
            serde_json::json!({ "success": false, "error": format!("Clipboard read failed: {}", e) }),
        ),
    }
}

/// POST /ui-bridge/sdk/clipboard — Write to clipboard (system-level, same as control)
async fn handle_clipboard_write(
    State(_state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let html = body.get("html").and_then(|v| v.as_str());

    match arboard::Clipboard::new() {
        Ok(mut clipboard) => {
            let result = if let Some(html_content) = html {
                clipboard.set_html(html_content, Some(text))
            } else {
                clipboard.set_text(text)
            };
            match result {
                Ok(()) => Json(serde_json::json!({
                    "success": true,
                    "data": {
                        "written": true,
                        "formats": if html.is_some() { vec!["text/html", "text/plain"] } else { vec!["text/plain"] },
                    }
                })),
                Err(e) => Json(
                    serde_json::json!({ "success": false, "error": format!("Clipboard write failed: {}", e) }),
                ),
            }
        }
        Err(e) => Json(
            serde_json::json!({ "success": false, "error": format!("Clipboard init failed: {}", e) }),
        ),
    }
}

/// GET /ui-bridge/sdk/forms — Form state awareness
async fn handle_forms(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/control/forms", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/fill — Smart form fill
async fn handle_fill_form(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/control/fill", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/forms/snapshot — Capture form state snapshot
async fn handle_snapshot_forms(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/control/forms/snapshot", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/forms/diff — Diff two form snapshots
async fn handle_diff_forms(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/control/forms/diff", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// Network Request Monitoring
// =============================================================================

/// GET /ui-bridge/sdk/network-requests — List network requests with optional filters
///
/// Optional query parameters:
/// - `status`: filter by status (string)
/// - `method`: filter by HTTP method (string)
/// - `urlPattern`: filter by URL pattern (string)
/// - `failuresOnly`: only return failed requests (boolean)
/// - `since`: timestamp (number)
/// - `limit`: max results (number)
async fn handle_network_requests(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let mut path = "/control/network-requests".to_string();
    let mut params = vec![];
    if let Some(status) = query.get("status") {
        params.push(format!("status={}", status));
    }
    if let Some(method) = query.get("method") {
        params.push(format!("method={}", method));
    }
    if let Some(url_pattern) = query.get("urlPattern") {
        params.push(format!("urlPattern={}", url_pattern));
    }
    if let Some(failures_only) = query.get("failuresOnly") {
        params.push(format!("failuresOnly={}", failures_only));
    }
    if let Some(since) = query.get("since") {
        params.push(format!("since={}", since));
    }
    if let Some(limit) = query.get("limit") {
        params.push(format!("limit={}", limit));
    }
    if !params.is_empty() {
        path = format!("{}?{}", path, params.join("&"));
    }
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/network-requests/in-flight — Currently in-flight requests
async fn handle_network_requests_in_flight(
    State(state): State<Arc<ApiState>>,
) -> Json<serde_json::Value> {
    match sdk_request(
        &state,
        Method::GET,
        "/control/network-requests/in-flight",
        None,
    )
    .await
    {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/network-requests/wait — Wait for a specific request matching criteria
async fn handle_wait_for_network_request(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(
        &state,
        Method::POST,
        "/control/network-requests/wait",
        Some(body),
    )
    .await
    {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/network-request/:id — Get specific request by ID
async fn handle_network_request(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let path = format!("/control/network-request/{}", id);
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/ai/snapshot — Semantic AI snapshot
async fn handle_ai_snapshot(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/ai/snapshot", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/ai/summary — Page summary
async fn handle_ai_summary(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/ai/summary", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// Idle Detection Relay Handlers
// =============================================================================

/// GET /ui-bridge/sdk/idle-status — Get idle status from SDK app
async fn handle_idle_status(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/control/idle-status", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/idle-status/:signal — Get single signal idle status from SDK app
async fn handle_idle_status_signal(
    State(state): State<Arc<ApiState>>,
    Path(signal): Path<String>,
) -> Json<serde_json::Value> {
    match sdk_request(
        &state,
        Method::GET,
        &format!("/control/idle-status/{}", signal),
        None,
    )
    .await
    {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/wait-for-idle — Block until SDK app is idle
async fn handle_wait_for_idle(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/control/wait-for-idle", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/wait-for-idle/:signal — Block until specific signal is idle in SDK app
async fn handle_wait_for_signal(
    State(state): State<Arc<ApiState>>,
    Path(signal): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(
        &state,
        Method::POST,
        &format!("/control/wait-for-idle/{}", signal),
        Some(body),
    )
    .await
    {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/wait-for-targets — Wait for specific targets in SDK app
async fn handle_wait_for_targets(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(
        &state,
        Method::POST,
        "/control/wait-for-targets",
        Some(body),
    )
    .await
    {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// Page Navigation Relay Handlers
// =============================================================================

/// GET /ui-bridge/sdk/tabs — List connected browser tabs
async fn handle_tabs(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/tabs", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/page/refresh — Refresh the page
async fn handle_page_refresh(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Json<serde_json::Value> {
    match sdk_request(
        &state,
        Method::POST,
        "/control/page/refresh",
        body.map(|b| b.0),
    )
    .await
    {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/page/navigate — Navigate to a URL
async fn handle_page_navigate(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/control/page/navigate", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/page/back — Go back in history
async fn handle_page_go_back(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Json<serde_json::Value> {
    match sdk_request(
        &state,
        Method::POST,
        "/control/page/back",
        body.map(|b| b.0),
    )
    .await
    {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/page/forward — Go forward in history
async fn handle_page_go_forward(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Json<serde_json::Value> {
    match sdk_request(
        &state,
        Method::POST,
        "/control/page/forward",
        body.map(|b| b.0),
    )
    .await
    {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/debug/metrics — Debug metrics
async fn handle_debug_metrics(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/debug/metrics", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/debug/highlight/:id — Highlight an element
async fn handle_debug_highlight(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let path = format!("/debug/highlight/{}", id);
    match sdk_request(&state, Method::POST, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/switch — Switch active connection to a different URL
async fn handle_switch(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<SwitchRequest>,
) -> Json<ApiResponse<ConnectResponse>> {
    let url = req.url.trim_end_matches('/').to_string();
    let mut manager = state.sdk_connection.lock().await;

    if let Some(conn) = manager.connections.get(&url) {
        let response = ConnectResponse {
            app: conn.app_info.clone(),
            url: conn.app_url.clone(),
            base_path: conn.base_path.clone(),
        };
        manager.active_url = Some(url.clone());
        info!(
            "Switched active SDK connection to: {} ({})",
            response.app.app_name, url
        );
        Json(ApiResponse::success(response))
    } else {
        let available: Vec<&String> = manager.connections.keys().collect();
        Json(ApiResponse::error(format!(
            "No connection found for {}. Available connections: {:?}",
            url, available
        )))
    }
}

/// GET /ui-bridge/sdk/connections — List all connections
async fn handle_connections(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<ConnectionInfo>>> {
    let manager = state.sdk_connection.lock().await;

    let connections: Vec<ConnectionInfo> = manager
        .connections
        .iter()
        .map(|(url, conn)| ConnectionInfo {
            url: url.clone(),
            app: conn.app_info.clone(),
            connected_at: conn.connected_at,
            is_active: manager.active_url.as_deref() == Some(url.as_str()),
        })
        .collect();

    Json(ApiResponse::success(connections))
}

/// GET /ui-bridge/sdk/check-health — Check health of all connections, remove stale ones
async fn handle_check_health(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    let mut manager = state.sdk_connection.lock().await;
    let mut results = Vec::new();
    let mut stale_urls = Vec::new();

    // Create a short-timeout client for health checks
    let health_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .connect_timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap_or_default();

    for (url, conn) in &manager.connections {
        let health_url = format!("{}{}/health", conn.app_url, conn.base_path);
        let healthy = match health_client.get(&health_url).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        };

        results.push(ConnectionHealthResult {
            url: url.clone(),
            healthy,
            app_name: conn.app_info.app_name.clone(),
        });

        if !healthy {
            stale_urls.push(url.clone());
        }
    }

    // Remove stale connections
    for url in &stale_urls {
        if let Some(conn) = manager.connections.remove(url) {
            warn!(
                "Removed stale SDK connection: {} ({})",
                conn.app_info.app_name, url
            );
        }
        if manager.active_url.as_deref() == Some(url.as_str()) {
            manager.active_url = None;
        }
    }

    let active_healthy =
        manager.active_url.is_some() && !stale_urls.contains(manager.active_url.as_ref().unwrap());

    Json(serde_json::json!({
        "success": true,
        "data": {
            "activeHealthy": active_healthy,
            "activeUrl": manager.active_url,
            "results": results,
            "staleRemoved": stale_urls,
            "totalConnections": manager.connections.len(),
        }
    }))
}

// =============================================================================
// Screenshot Capture
// =============================================================================

/// Query parameters for screenshot capture
#[derive(Debug, Deserialize)]
struct ScreenshotQuery {
    /// Monitor index (0-based), None for primary monitor
    #[serde(default)]
    monitor: Option<i32>,
}

/// Screenshot capture response data
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScreenshotData {
    /// Base64 encoded PNG screenshot data
    screenshot: String,
    /// Screenshot width in pixels
    width: i32,
    /// Screenshot height in pixels
    height: i32,
    /// Monitor that was captured
    #[serde(skip_serializing_if = "Option::is_none")]
    monitor: Option<i32>,
}

/// GET /ui-bridge/sdk/screenshot — Capture screenshot of the app's monitor
///
/// Since SDK apps (browser-based) cannot take screenshots of their own window
/// due to browser security restrictions, this endpoint uses the runner's
/// Python IPC screenshot capture infrastructure to capture the monitor where
/// the SDK app is running.
///
/// Query parameters:
/// - `monitor`: Monitor index (0-based). If not provided, captures the primary monitor.
async fn handle_screenshot(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ScreenshotQuery>,
) -> Json<ApiResponse<ScreenshotData>> {
    info!(
        monitor = ?query.monitor,
        "SDK screenshot: Capturing monitor screenshot for SDK app"
    );

    // Use the existing IPC-based screenshot capture from misc.rs
    match super::misc::capture_screenshot_ipc(state.app_state.clone(), query.monitor, "png").await {
        Ok(capture_data) => {
            // Extract base64 data, width, and height from the IPC response
            let screenshot_base64 = match capture_data
                .get("screenshot_base64")
                .and_then(|s| s.as_str())
            {
                Some(s) => s.to_string(),
                None => {
                    error!("SDK screenshot: No screenshot_base64 in IPC response");
                    return Json(ApiResponse::error(
                        "Screenshot captured but no image data returned",
                    ));
                }
            };

            let width = capture_data
                .get("width")
                .and_then(|w| w.as_i64())
                .unwrap_or(0) as i32;
            let height = capture_data
                .get("height")
                .and_then(|h| h.as_i64())
                .unwrap_or(0) as i32;

            info!(
                "SDK screenshot: Captured {}x{} from monitor {:?}",
                width, height, query.monitor
            );

            Json(ApiResponse::success(ScreenshotData {
                screenshot: screenshot_base64,
                width,
                height,
                monitor: query.monitor,
            }))
        }
        Err(e) => {
            error!("SDK screenshot: Failed to capture: {}", e);
            Json(ApiResponse::error(format!(
                "Screenshot capture failed: {}. Ensure the Python executor is running.",
                e
            )))
        }
    }
}

// =============================================================================
// Cross-App Analysis Proxies
// =============================================================================

/// GET /ui-bridge/sdk/ai/analyze/data — Extract page data
async fn handle_ai_analyze_data(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/ai/analyze/data", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/ai/analyze/regions — Segment page regions
async fn handle_ai_analyze_regions(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/ai/analyze/regions", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/ai/analyze/structured-data — Extract tables/lists
async fn handle_ai_analyze_structured_data(
    State(state): State<Arc<ApiState>>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/ai/analyze/structured-data", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/analyze/cross-app-compare — Compare two snapshots
async fn handle_ai_analyze_cross_app_compare(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(
        &state,
        Method::POST,
        "/ai/analyze/cross-app-compare",
        Some(body),
    )
    .await
    {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// Cached Specs
// =============================================================================

/// Request body for discover-and-cache
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiscoverAndCacheRequest {
    url: String,
    #[serde(default)]
    app_name: Option<String>,
}

/// GET /ui-bridge/sdk/cached-specs — Return all cached external specs
async fn handle_cached_specs_all(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<serde_json::Value>>> {
    let db = &state.app_state.checkpoint_db;

    match db.get_all_cached_specs() {
        Ok(specs) => {
            let data: Vec<serde_json::Value> = specs
                .into_iter()
                .map(|s| {
                    serde_json::json!({
                        "id": s.id,
                        "app_url": s.app_url,
                        "app_name": s.app_name,
                        "spec_id": s.spec_id,
                        "spec_json": s.spec_json,
                        "discovered_at": s.discovered_at,
                        "page_url": s.page_url,
                    })
                })
                .collect();
            Json(ApiResponse::success(data))
        }
        Err(e) => Json(ApiResponse::error(format!(
            "Failed to get cached specs: {}",
            e
        ))),
    }
}

/// GET /ui-bridge/sdk/cached-specs/:app_url — Return cached specs for one app
async fn handle_cached_specs_for_app(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Json<ApiResponse<Vec<serde_json::Value>>> {
    let app_url = match query.get("app_url") {
        Some(url) => url.clone(),
        None => return Json(ApiResponse::error("Missing app_url query parameter")),
    };

    let db = &state.app_state.checkpoint_db;

    match db.get_cached_specs_for_app(&app_url) {
        Ok(specs) => {
            let data: Vec<serde_json::Value> = specs
                .into_iter()
                .map(|s| {
                    serde_json::json!({
                        "id": s.id,
                        "app_url": s.app_url,
                        "app_name": s.app_name,
                        "spec_id": s.spec_id,
                        "spec_json": s.spec_json,
                        "discovered_at": s.discovered_at,
                        "page_url": s.page_url,
                    })
                })
                .collect();
            Json(ApiResponse::success(data))
        }
        Err(e) => Json(ApiResponse::error(format!(
            "Failed to get cached specs: {}",
            e
        ))),
    }
}

/// POST /ui-bridge/sdk/discover-and-cache — Connect, discover specs, cache them
async fn handle_discover_and_cache(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<DiscoverAndCacheRequest>,
) -> Json<ApiResponse<Vec<serde_json::Value>>> {
    let url = normalize_localhost_url(req.url.trim_end_matches('/'));
    let app_name = req.app_name.unwrap_or_else(|| "Unknown App".to_string());

    info!(url = %url, app_name = %app_name, "Discover-and-cache specs");

    // Step 1: Ensure connected to the app
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return Json(ApiResponse::error(format!(
                "Failed to create HTTP client: {}",
                e
            )))
        }
    };

    let (base_path, _health_data) = match try_health_check(&client, &url).await {
        Ok(result) => result,
        Err(e) => return Json(ApiResponse::error(format!("Health check failed: {}", e))),
    };

    // Step 2: Call getSpecs via the SDK
    let specs_url = format!("{}{}/control/specs", url, base_path);
    let specs_response = match client.get(&specs_url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(json) => json,
            Err(e) => {
                return Json(ApiResponse::error(format!(
                    "Failed to parse specs response: {}",
                    e
                )))
            }
        },
        Ok(resp) => {
            let status = resp.status();
            let hint = match status.as_u16() {
                404 => " — the app may not have UI Bridge SDK integrated",
                401 | 403 => " — authentication required",
                500..=599 => " — the app's specs endpoint encountered an internal error",
                _ => "",
            };
            return Json(ApiResponse::error(format!(
                "Could not fetch specs from {}{}/control/specs (HTTP {}){}",
                url, base_path, status, hint
            )));
        }
        Err(e) => return Json(ApiResponse::error(format!("Failed to fetch specs: {}", e))),
    };

    // Step 3: Parse and cache each spec
    let db = &state.app_state.checkpoint_db;
    let specs_data = specs_response.get("data").or(Some(&specs_response));

    let specs_array = match specs_data.and_then(|d| d.as_array()) {
        Some(arr) => arr.clone(),
        None => {
            // Maybe it's a single spec object with groups
            if specs_data.is_some_and(|d| d.get("groups").is_some()) {
                vec![specs_data.unwrap().clone()]
            } else {
                return Json(ApiResponse::error(
                    "No specs found in response (expected array or object with groups)",
                ));
            }
        }
    };

    let mut cached: Vec<serde_json::Value> = Vec::new();

    for spec in &specs_array {
        let spec_id = spec
            .get("specId")
            .or_else(|| spec.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let page_url = spec
            .get("metadata")
            .and_then(|m| m.get("pageUrl"))
            .and_then(|v| v.as_str());

        let spec_json_str = serde_json::to_string(spec).unwrap_or_default();

        if let Err(e) = db.upsert_cached_spec(&url, &app_name, spec_id, &spec_json_str, page_url) {
            warn!("Failed to cache spec {}: {}", spec_id, e);
            continue;
        }

        cached.push(serde_json::json!({
            "id": format!("{}:{}", url, spec_id),
            "app_url": url,
            "app_name": app_name,
            "spec_id": spec_id,
            "spec_json": spec_json_str,
            "page_url": page_url,
        }));
    }

    info!("Cached {} specs for {}", cached.len(), url);
    Json(ApiResponse::success(cached))
}

/// DELETE /ui-bridge/sdk/cached-specs — Clear cached specs for an app
async fn handle_delete_cached_specs(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Json<ApiResponse<serde_json::Value>> {
    let app_url = match query.get("app_url") {
        Some(url) => url.clone(),
        None => return Json(ApiResponse::error("Missing app_url query parameter")),
    };

    let db = &state.app_state.checkpoint_db;

    match db.delete_cached_specs_for_app(&app_url) {
        Ok(count) => Json(ApiResponse::success(serde_json::json!({
            "deleted": count,
            "app_url": app_url,
        }))),
        Err(e) => Json(ApiResponse::error(format!(
            "Failed to delete cached specs: {}",
            e
        ))),
    }
}

// =============================================================================
// Design Review Handlers
// =============================================================================

/// GET /ui-bridge/sdk/design/element/:id/styles — Extended computed styles for one element
async fn handle_design_element_styles(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let path = format!("/design/element/{}/styles", id);
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/design/element/:id/state-styles — Styles across interaction states
async fn handle_design_element_state_styles(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    body: Option<Json<serde_json::Value>>,
) -> Json<serde_json::Value> {
    let path = format!("/design/element/{}/state-styles", id);
    match sdk_request(&state, Method::POST, &path, body.map(|b| b.0)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/design/snapshot — Design data for all/filtered elements
async fn handle_design_snapshot(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/design/snapshot", body.map(|b| b.0)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/design/responsive — Snapshots at multiple viewports
async fn handle_design_responsive(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/design/responsive", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/design/audit — Run style audit against style guide
async fn handle_design_audit(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/design/audit", body.map(|b| b.0)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/design/style-guide/load — Load a style guide
async fn handle_design_load_style_guide(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/design/style-guide/load", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/design/style-guide — Get loaded style guide
async fn handle_design_get_style_guide(
    State(state): State<Arc<ApiState>>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/design/style-guide", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// DELETE /ui-bridge/sdk/design/style-guide — Clear loaded style guide
async fn handle_design_clear_style_guide(
    State(state): State<Arc<ApiState>>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::DELETE, "/design/style-guide", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// Console Capture / Error Tracking Handlers
// =============================================================================

/// GET /ui-bridge/sdk/console/health — Check UI health status
///
/// Returns a health score (0-100), status (healthy/degraded/broken),
/// error breakdown, and top issue.
async fn handle_console_health(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/control/health", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/console/browser-events — Get browser events with filtering
///
/// Optional query parameters:
/// - `severity`: crash, error, warning, noise
/// - `deduplicate`: boolean
/// - `since`: timestamp (number)
/// - `limit`: max results (number)
async fn handle_console_browser_events(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let mut path = "/control/browser-events".to_string();
    let mut params = vec![];
    if let Some(severity) = query.get("severity") {
        params.push(format!("severity={}", severity));
    }
    if let Some(deduplicate) = query.get("deduplicate") {
        params.push(format!("deduplicate={}", deduplicate));
    }
    if let Some(since) = query.get("since") {
        params.push(format!("since={}", since));
    }
    if let Some(limit) = query.get("limit") {
        params.push(format!("limit={}", limit));
    }
    if !params.is_empty() {
        path = format!("{}?{}", path, params.join("&"));
    }
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/console/timeline — Get interleaved action/error timeline
///
/// Optional query parameters:
/// - `since`: timestamp (number)
/// - `limit`: max results (number)
/// - `minSeverity`: minimum severity level (string)
async fn handle_console_timeline(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let mut path = "/control/timeline".to_string();
    let mut params = vec![];
    if let Some(since) = query.get("since") {
        params.push(format!("since={}", since));
    }
    if let Some(limit) = query.get("limit") {
        params.push(format!("limit={}", limit));
    }
    if let Some(min_severity) = query.get("minSeverity") {
        params.push(format!("minSeverity={}", min_severity));
    }
    if !params.is_empty() {
        path = format!("{}?{}", path, params.join("&"));
    }
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/console/network-chains — Get network request-response chains
///
/// Optional query parameters:
/// - `failuresOnly`: boolean
/// - `limit`: max results (number)
/// - `url`: filter by URL pattern (string)
async fn handle_console_network_chains(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let mut path = "/control/network-chains".to_string();
    let mut params = vec![];
    if let Some(failures_only) = query.get("failuresOnly") {
        params.push(format!("failuresOnly={}", failures_only));
    }
    if let Some(limit) = query.get("limit") {
        params.push(format!("limit={}", limit));
    }
    if let Some(url) = query.get("url") {
        params.push(format!("url={}", url));
    }
    if !params.is_empty() {
        path = format!("{}?{}", path, params.join("&"));
    }
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/console/error-sessions/start — Start error tracking session
///
/// Optional body: `{ "label": "..." }`
async fn handle_console_error_session_start(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Json<serde_json::Value> {
    match sdk_request(
        &state,
        Method::POST,
        "/control/error-sessions/start",
        body.map(|b| b.0),
    )
    .await
    {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/console/error-sessions/end — End error tracking session
///
/// Returns a summary of captured errors during the session.
async fn handle_console_error_session_end(
    State(state): State<Arc<ApiState>>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/control/error-sessions/end", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/console/error-sessions — List all error session summaries
async fn handle_console_error_sessions_list(
    State(state): State<Arc<ApiState>>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/control/error-sessions", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/console/error-baselines/capture — Capture error baseline
///
/// Required body: `{ "label": "..." }`
async fn handle_console_error_baseline_capture(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(
        &state,
        Method::POST,
        "/control/error-baselines/capture",
        Some(body),
    )
    .await
    {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/console/error-baselines/compare — Compare against baseline
///
/// Required body: `{ "label": "..." }`
/// Returns new errors (regressions) and fixed errors.
async fn handle_console_error_baseline_compare(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(
        &state,
        Method::POST,
        "/control/error-baselines/compare",
        Some(body),
    )
    .await
    {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// Change tracking (SDK proxy)
// =============================================================================

/// GET /ui-bridge/sdk/ai/bookmarks — List all bookmarks
async fn handle_ct_list_bookmarks(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/ai/bookmarks", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/bookmarks — Save a bookmark
async fn handle_ct_save_bookmark(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/bookmarks", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/ai/bookmark/:name — Get a specific bookmark
async fn handle_ct_get_bookmark(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Json<serde_json::Value> {
    let path = format!("/ai/bookmark/{}", name);
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// DELETE /ui-bridge/sdk/ai/bookmark/:name — Delete a bookmark
async fn handle_ct_delete_bookmark(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Json<serde_json::Value> {
    let path = format!("/ai/bookmark/{}", name);
    match sdk_request(&state, Method::DELETE, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/ai/bookmark/:name/diff — Diff from a bookmark
async fn handle_ct_diff_from_bookmark(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Json<serde_json::Value> {
    let path = format!("/ai/bookmark/{}/diff", name);
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/execute-with-diff — Execute action and return diff
async fn handle_ct_execute_with_diff(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/execute-with-diff", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/wait-for-change — Wait for a change matching predicate
async fn handle_ct_wait_for_change(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/wait-for-change", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/ai/categorize-last-diff — Categorize the last diff
async fn handle_ct_categorize_last_diff(
    State(state): State<Arc<ApiState>>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/ai/categorize-last-diff", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/scoped-diff — Get scoped diff
async fn handle_ct_scoped_diff(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/scoped-diff", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/summarize-diff — Summarize a diff with budget
async fn handle_ct_summarize_diff(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/summarize-diff", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/structured-changes — Analyze structured changes
async fn handle_ct_structured_changes(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/structured-changes", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/change-buffer/enable — Enable change buffer
async fn handle_ct_enable_change_buffer(
    State(state): State<Arc<ApiState>>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/change-buffer/enable", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/change-buffer/disable — Disable change buffer
async fn handle_ct_disable_change_buffer(
    State(state): State<Arc<ApiState>>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/change-buffer/disable", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/change-buffer/drain — Drain change buffer
async fn handle_ct_drain_change_buffer(
    State(state): State<Arc<ApiState>>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/change-buffer/drain", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/ai/change-buffer/size — Get change buffer size
async fn handle_ct_get_change_buffer_size(
    State(state): State<Arc<ApiState>>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/ai/change-buffer/size", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// Router
// =============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        // Connection management
        .route("/ui-bridge/sdk/connect", post(handle_connect))
        .route("/ui-bridge/sdk/disconnect", post(handle_disconnect))
        .route("/ui-bridge/sdk/status", get(handle_status))
        .route("/ui-bridge/sdk/switch", post(handle_switch))
        .route("/ui-bridge/sdk/connections", get(handle_connections))
        .route("/ui-bridge/sdk/check-health", get(handle_check_health))
        // Health
        .route("/ui-bridge/sdk/health", get(handle_health))
        // Elements
        .route("/ui-bridge/sdk/elements", get(handle_elements))
        .route("/ui-bridge/sdk/element/:id", get(handle_element))
        .route(
            "/ui-bridge/sdk/element/:id/action",
            post(handle_element_action),
        )
        // Snapshot & discovery
        .route("/ui-bridge/sdk/snapshot", get(handle_snapshot))
        .route("/ui-bridge/sdk/discover", post(handle_discover))
        // Console errors
        .route("/ui-bridge/sdk/console-errors", get(handle_console_errors))
        .route(
            "/ui-bridge/sdk/console-errors/clear",
            post(handle_clear_console_errors),
        )
        // Screenshot (monitor capture for SDK apps that can't self-screenshot)
        .route("/ui-bridge/sdk/screenshot", get(handle_screenshot))
        // Components
        .route("/ui-bridge/sdk/components", get(handle_components))
        .route("/ui-bridge/sdk/component/:id", get(handle_component))
        // AI endpoints
        .route("/ui-bridge/sdk/ai/search", post(handle_ai_search))
        .route("/ui-bridge/sdk/ai/execute", post(handle_ai_execute))
        .route("/ui-bridge/sdk/ai/assert", post(handle_ai_assert))
        .route("/ui-bridge/sdk/ai/snapshot", get(handle_ai_snapshot))
        // Clipboard
        .route(
            "/ui-bridge/sdk/clipboard",
            get(handle_clipboard_read).post(handle_clipboard_write),
        )
        // Form state awareness
        .route("/ui-bridge/sdk/forms", get(handle_forms))
        .route("/ui-bridge/sdk/fill", post(handle_fill_form))
        .route("/ui-bridge/sdk/forms/snapshot", post(handle_snapshot_forms))
        .route("/ui-bridge/sdk/forms/diff", post(handle_diff_forms))
        // Network request monitoring
        .route(
            "/ui-bridge/sdk/network-requests",
            get(handle_network_requests),
        )
        .route(
            "/ui-bridge/sdk/network-requests/in-flight",
            get(handle_network_requests_in_flight),
        )
        .route(
            "/ui-bridge/sdk/network-requests/wait",
            post(handle_wait_for_network_request),
        )
        .route(
            "/ui-bridge/sdk/network-request/:id",
            get(handle_network_request),
        )
        .route("/ui-bridge/sdk/ai/summary", get(handle_ai_summary))
        // AI analysis endpoints
        .route(
            "/ui-bridge/sdk/ai/analyze/data",
            get(handle_ai_analyze_data),
        )
        .route(
            "/ui-bridge/sdk/ai/analyze/regions",
            get(handle_ai_analyze_regions),
        )
        .route(
            "/ui-bridge/sdk/ai/analyze/structured-data",
            get(handle_ai_analyze_structured_data),
        )
        .route(
            "/ui-bridge/sdk/ai/analyze/cross-app-compare",
            post(handle_ai_analyze_cross_app_compare),
        )
        // Idle detection
        .route("/ui-bridge/sdk/idle-status", get(handle_idle_status))
        .route(
            "/ui-bridge/sdk/idle-status/:signal",
            get(handle_idle_status_signal),
        )
        .route("/ui-bridge/sdk/wait-for-idle", post(handle_wait_for_idle))
        .route(
            "/ui-bridge/sdk/wait-for-idle/:signal",
            post(handle_wait_for_signal),
        )
        .route(
            "/ui-bridge/sdk/wait-for-targets",
            post(handle_wait_for_targets),
        )
        // Tab registry
        .route("/ui-bridge/sdk/tabs", get(handle_tabs))
        // Page navigation
        .route("/ui-bridge/sdk/page/refresh", post(handle_page_refresh))
        .route("/ui-bridge/sdk/page/navigate", post(handle_page_navigate))
        .route("/ui-bridge/sdk/page/back", post(handle_page_go_back))
        .route("/ui-bridge/sdk/page/forward", post(handle_page_go_forward))
        // Debug
        .route("/ui-bridge/sdk/debug/metrics", get(handle_debug_metrics))
        .route(
            "/ui-bridge/sdk/debug/highlight/:id",
            post(handle_debug_highlight),
        )
        // Cached specs
        .route(
            "/ui-bridge/sdk/cached-specs",
            get(handle_cached_specs_all).delete(handle_delete_cached_specs),
        )
        .route(
            "/ui-bridge/sdk/cached-specs/by-app",
            get(handle_cached_specs_for_app),
        )
        .route(
            "/ui-bridge/sdk/discover-and-cache",
            post(handle_discover_and_cache),
        )
        // Design review
        .route(
            "/ui-bridge/sdk/design/element/:id/styles",
            get(handle_design_element_styles),
        )
        .route(
            "/ui-bridge/sdk/design/element/:id/state-styles",
            post(handle_design_element_state_styles),
        )
        .route(
            "/ui-bridge/sdk/design/snapshot",
            post(handle_design_snapshot),
        )
        .route(
            "/ui-bridge/sdk/design/responsive",
            post(handle_design_responsive),
        )
        .route("/ui-bridge/sdk/design/audit", post(handle_design_audit))
        .route(
            "/ui-bridge/sdk/design/style-guide/load",
            post(handle_design_load_style_guide),
        )
        .route(
            "/ui-bridge/sdk/design/style-guide",
            get(handle_design_get_style_guide).delete(handle_design_clear_style_guide),
        )
        // Console capture / error tracking
        .route("/ui-bridge/sdk/console/health", get(handle_console_health))
        .route(
            "/ui-bridge/sdk/console/browser-events",
            get(handle_console_browser_events),
        )
        .route(
            "/ui-bridge/sdk/console/timeline",
            get(handle_console_timeline),
        )
        .route(
            "/ui-bridge/sdk/console/network-chains",
            get(handle_console_network_chains),
        )
        .route(
            "/ui-bridge/sdk/console/error-sessions/start",
            post(handle_console_error_session_start),
        )
        .route(
            "/ui-bridge/sdk/console/error-sessions/end",
            post(handle_console_error_session_end),
        )
        .route(
            "/ui-bridge/sdk/console/error-sessions",
            get(handle_console_error_sessions_list),
        )
        .route(
            "/ui-bridge/sdk/console/error-baselines/capture",
            post(handle_console_error_baseline_capture),
        )
        .route(
            "/ui-bridge/sdk/console/error-baselines/compare",
            post(handle_console_error_baseline_compare),
        )
        // Change tracking
        .route(
            "/ui-bridge/sdk/ai/bookmarks",
            get(handle_ct_list_bookmarks).post(handle_ct_save_bookmark),
        )
        .route(
            "/ui-bridge/sdk/ai/bookmark/:name",
            get(handle_ct_get_bookmark).delete(handle_ct_delete_bookmark),
        )
        .route(
            "/ui-bridge/sdk/ai/bookmark/:name/diff",
            get(handle_ct_diff_from_bookmark),
        )
        .route(
            "/ui-bridge/sdk/ai/execute-with-diff",
            post(handle_ct_execute_with_diff),
        )
        .route(
            "/ui-bridge/sdk/ai/wait-for-change",
            post(handle_ct_wait_for_change),
        )
        .route(
            "/ui-bridge/sdk/ai/categorize-last-diff",
            get(handle_ct_categorize_last_diff),
        )
        .route("/ui-bridge/sdk/ai/scoped-diff", post(handle_ct_scoped_diff))
        .route(
            "/ui-bridge/sdk/ai/summarize-diff",
            post(handle_ct_summarize_diff),
        )
        .route(
            "/ui-bridge/sdk/ai/structured-changes",
            post(handle_ct_structured_changes),
        )
        .route(
            "/ui-bridge/sdk/ai/change-buffer/enable",
            post(handle_ct_enable_change_buffer),
        )
        .route(
            "/ui-bridge/sdk/ai/change-buffer/disable",
            post(handle_ct_disable_change_buffer),
        )
        .route(
            "/ui-bridge/sdk/ai/change-buffer/drain",
            post(handle_ct_drain_change_buffer),
        )
        .route(
            "/ui-bridge/sdk/ai/change-buffer/size",
            get(handle_ct_get_change_buffer_size),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_localhost_with_port() {
        assert_eq!(
            normalize_localhost_url("http://localhost:3000/path"),
            "http://127.0.0.1:3000/path"
        );
    }

    #[test]
    fn normalize_bare_localhost() {
        assert_eq!(
            normalize_localhost_url("http://localhost"),
            "http://127.0.0.1"
        );
    }

    #[test]
    fn normalize_localhost_with_trailing_slash() {
        assert_eq!(
            normalize_localhost_url("http://localhost/"),
            "http://127.0.0.1/"
        );
    }

    #[test]
    fn normalize_non_localhost_unchanged() {
        let url = "http://192.168.1.10:3000/api";
        assert_eq!(normalize_localhost_url(url), url);
    }

    #[test]
    fn normalize_already_ip_unchanged() {
        let url = "http://127.0.0.1:5000";
        assert_eq!(normalize_localhost_url(url), url);
    }

    #[test]
    fn normalize_preserves_path_and_query() {
        assert_eq!(
            normalize_localhost_url("http://localhost:8080/api/v1?key=val"),
            "http://127.0.0.1:8080/api/v1?key=val"
        );
    }
}
