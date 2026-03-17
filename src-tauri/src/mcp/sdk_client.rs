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
    response::{
        sse::{Event as SseEvent, KeepAlive, Sse},
        IntoResponse, Json,
    },
    routing::{get, post},
    Router,
};
use futures_util::{stream::Stream, StreamExt};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

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
    /// Cached responsiveness status of the active app (from health endpoint)
    pub active_responsive: Option<bool>,
    /// Timestamp (ms) of the last responsiveness check
    pub active_responsive_checked_at: i64,
}

impl SdkConnectionManager {
    pub fn new() -> Self {
        Self {
            connections: HashMap::new(),
            active_url: None,
            active_responsive: None,
            active_responsive_checked_at: 0,
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
    /// Whether the active SDK app is responsive (based on heartbeat freshness)
    healthy: Option<bool>,
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

/// Send an HTTP request to the connected SDK app.
///
/// Before making the request, checks if the SDK app is responsive (cached for
/// 10 seconds). If the app reports `responsive: false` (no active browser tab),
/// returns an error immediately so handlers can fall back to IPC.
pub async fn sdk_request(
    state: &Arc<ApiState>,
    method: Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    // 1. Read connection info and check cached responsiveness
    let (app_url, base_path, client) = {
        let conn_guard = state.sdk_connection.lock().await;
        let conn = conn_guard
            .active_connection()
            .ok_or_else(|| "No active SDK app connection".to_string())?;

        // If we recently checked and the app is not responsive, fail fast
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let cache_age = now_ms - conn_guard.active_responsive_checked_at;
        if cache_age < 10_000 && conn_guard.active_responsive == Some(false) {
            return Err("SDK app is not responsive (no active browser tab)".to_string());
        }

        (
            conn.app_url.clone(),
            conn.base_path.clone(),
            conn.client.clone(),
        )
    };
    // Mutex released — safe to make HTTP requests

    // 2. Refresh responsiveness cache if stale (>10s or never checked)
    {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let conn_guard = state.sdk_connection.lock().await;
        let cache_age = now_ms - conn_guard.active_responsive_checked_at;
        if cache_age >= 10_000 {
            // Drop the lock before making HTTP request
            drop(conn_guard);

            let health_url = format!("{}{}/health", app_url, base_path);
            let responsive = match client
                .get(&health_url)
                .timeout(std::time::Duration::from_secs(2))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => resp
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|h| h.get("data")?.get("responsive")?.as_bool())
                    .unwrap_or(true),
                _ => true, // Assume responsive if health check fails
            };

            // Re-acquire lock to update cache
            let mut conn_guard = state.sdk_connection.lock().await;
            conn_guard.active_responsive = Some(responsive);
            conn_guard.active_responsive_checked_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64;

            if !responsive {
                debug!("SDK app at {} is not responsive, skipping", app_url);
                return Err("SDK app is not responsive (no active browser tab)".to_string());
            }
        }
    }

    // 3. Make the actual SDK request
    let url = format!("{}{}{}", app_url, base_path, path);
    debug!(url = %url, method = %method, "SDK request");

    let mut request = client.request(method.clone(), &url);

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
            manager.active_responsive = None;
            manager.active_responsive_checked_at = 0;
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
    manager.active_responsive = None;
    manager.active_responsive_checked_at = 0;

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
                    manager.active_responsive = None;
                    manager.active_responsive_checked_at = 0;
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

    // Check health via the SDK app's /health endpoint (quick, non-blocking)
    let healthy = if let Some(conn) = manager.active_connection() {
        let health_url = format!("{}{}/health", conn.app_url, conn.base_path);
        match conn
            .client
            .get(&health_url)
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
        {
            Ok(resp) => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    json.get("healthy").and_then(|v| v.as_bool())
                } else {
                    Some(true) // Health endpoint responded but no "healthy" field — assume ok
                }
            }
            Err(_) => Some(false),
        }
    } else {
        None
    };

    match manager.active_connection() {
        Some(conn) => Json(ApiResponse::success(SdkStatusResponse {
            connected: true,
            app: Some(conn.app_info.clone()),
            url: Some(conn.app_url.clone()),
            connected_at: Some(conn.connected_at),
            all_connections,
            healthy,
        })),
        None => Json(ApiResponse::success(SdkStatusResponse {
            connected: false,
            app: None,
            url: None,
            connected_at: None,
            all_connections,
            healthy: None,
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

/// GET /ui-bridge/sdk/capabilities — Get SDK app capabilities
async fn handle_capabilities(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/capabilities", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/heartbeat — Forward heartbeat to SDK app
async fn handle_heartbeat(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    match sdk_request(
        &state,
        Method::POST,
        "/heartbeat",
        Some(serde_json::json!({ "timestamp": now })),
    )
    .await
    {
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
            // Normalize: if data.data is an object with an "elements" array, flatten it
            // so the response is { "success": true, "data": [...elements...] }.
            // The Tauri IPC handler returns { elements: [...], count: N } but SDK
            // consumers expect a flat array.
            if let Some(inner) = data
                .get("data")
                .and_then(|d| d.as_object())
                .and_then(|obj| obj.get("elements"))
                .and_then(|e| e.as_array())
                .cloned()
            {
                if let Some(obj) = data.as_object_mut() {
                    obj.insert("data".to_string(), serde_json::json!(inner));
                }
            }

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
        Err(_sdk_err) => {
            // No SDK app connected — fall back to the runner's own UI via IPC
            debug!("SDK elements unavailable, falling back to IPC control endpoint");
            match ui_bridge_request_sync(&state, "get_elements", serde_json::json!({})).await {
                Ok(mut data) => {
                    // Normalize: IPC returns { elements: [...], count: N }
                    if let Some(inner) = data
                        .as_object()
                        .and_then(|obj| obj.get("elements"))
                        .and_then(|e| e.as_array())
                        .cloned()
                    {
                        data = serde_json::json!(inner);
                    }
                    let total = data.as_array().map(|a| a.len()).unwrap_or(0);
                    (
                        StatusCode::OK,
                        Json(serde_json::json!({ "success": true, "data": data, "total": total })),
                    )
                }
                Err(e) => (
                    StatusCode::BAD_GATEWAY,
                    Json(serde_json::json!({ "success": false, "error": e })),
                ),
            }
        }
    }
}

/// GET /ui-bridge/sdk/element/:id — Get a single element
async fn handle_element(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let id = id.trim().to_string();
    let path = format!("/control/element/{}", id);
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(_) => {
            // Fall back to IPC
            match ui_bridge_request_sync(&state, "get_element", serde_json::json!({ "id": id }))
                .await
            {
                Ok(data) => Json(serde_json::json!({ "success": true, "data": data })),
                Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
            }
        }
    }
}

/// POST /ui-bridge/sdk/element/:id/action — Execute an action on an element
async fn handle_element_action(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let id = id.trim().to_string();
    let path = format!("/control/element/{}/action", id);
    match sdk_request(&state, Method::POST, &path, Some(body.clone())).await {
        Ok(data) => Json(data),
        Err(_) => {
            // Fall back to IPC — wrap action in an object to match the format
            // expected by the TypeScript handler (action.action, action.params, etc.)
            let action_name = body
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("click");
            let params = body
                .get("params")
                .cloned()
                .unwrap_or(serde_json::json!(null));
            let wait_options = body
                .get("waitOptions")
                .cloned()
                .unwrap_or(serde_json::json!(null));
            let payload = serde_json::json!({
                "elementId": id,
                "action": {
                    "action": action_name,
                    "params": params,
                    "waitOptions": wait_options
                }
            });
            match ui_bridge_request_sync(&state, "execute_action", payload).await {
                Ok(data) => {
                    // If the inner response already indicates failure, return it directly
                    // instead of wrapping in { success: true, data } which masks the error
                    if data.get("success") == Some(&serde_json::json!(false)) {
                        Json(data)
                    } else {
                        Json(serde_json::json!({ "success": true, "data": data }))
                    }
                }
                Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
            }
        }
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
    match sdk_request(&state, Method::POST, "/control/find", Some(body.clone())).await {
        Ok(data) => Json(data),
        Err(_) => {
            // Fall back to IPC — get all elements and return as discovery result
            match ui_bridge_request_sync(&state, "get_elements", serde_json::json!({})).await {
                Ok(mut data) => {
                    // Normalize IPC response
                    if let Some(inner) = data
                        .as_object()
                        .and_then(|obj| obj.get("elements"))
                        .and_then(|e| e.as_array())
                        .cloned()
                    {
                        data = serde_json::json!(inner);
                    }
                    Json(serde_json::json!({ "success": true, "data": data }))
                }
                Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
            }
        }
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
        Err(_) => {
            // Fall back to IPC
            match ui_bridge_request_sync(&state, "get_components", serde_json::json!({})).await {
                Ok(data) => Json(serde_json::json!({ "success": true, "data": data })),
                Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
            }
        }
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
        Err(_) => {
            // Fall back to IPC
            match ui_bridge_request_sync(&state, "get_console_errors", serde_json::json!({})).await
            {
                Ok(data) => Json(serde_json::json!({ "success": true, "data": data })),
                Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
            }
        }
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
    match sdk_request(&state, Method::POST, "/ai/search", Some(body.clone())).await {
        Ok(data) => {
            // Unwrap ApiResponse {success, data} wrapper from control endpoint
            if let Some(inner) = data.get("data") {
                Json(inner.clone())
            } else {
                Json(data)
            }
        },
        Err(_sdk_err) => {
            // No SDK app connected — fall back to the runner's own UI via IPC
            debug!("SDK ai/search unavailable, falling back to IPC control endpoint");
            let payload = serde_json::json!({ "params": body });
            match ui_bridge_request_sync(&state, "ai_search", payload).await {
                Ok(data) => {
                    // The control endpoint wraps in ApiResponse, extract the data
                    Json(data)
                }
                Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
            }
        }
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
        Err(_) => {
            // Fall back to IPC
            match ui_bridge_request_sync(&state, "get_forms", serde_json::json!({})).await {
                Ok(data) => Json(serde_json::json!({ "success": true, "data": data })),
                Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
            }
        }
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
        Err(_) => {
            // Fall back to IPC
            match ui_bridge_request_sync(&state, "get_network_requests", serde_json::json!({}))
                .await
            {
                Ok(data) => Json(serde_json::json!({ "success": true, "data": data })),
                Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
            }
        }
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

/// POST /ui-bridge/sdk/diagnose-stuck — Diagnose stuck screen in SDK app
async fn handle_diagnose_stuck(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/control/diagnose-stuck", Some(body)).await {
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

/// GET /ui-bridge/sdk/windows — List capturable windows
async fn handle_windows(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<super::ui_bridge::WindowInfo>>> {
    super::ui_bridge::ui_bridge_list_windows_handler(axum::extract::State(state)).await
}

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
        Err(_) => {
            // Fall back to IPC
            match ui_bridge_request_sync(&state, "page_refresh", serde_json::json!({})).await {
                Ok(data) => Json(serde_json::json!({ "success": true, "data": data })),
                Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
            }
        }
    }
}

/// POST /ui-bridge/sdk/page/navigate — Navigate to a URL
async fn handle_page_navigate(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(
        &state,
        Method::POST,
        "/control/page/navigate",
        Some(body.clone()),
    )
    .await
    {
        Ok(data) => Json(data),
        Err(_) => {
            // Fall back to IPC
            match ui_bridge_request_sync(&state, "page_navigate", body).await {
                Ok(data) => Json(serde_json::json!({ "success": true, "data": data })),
                Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
            }
        }
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
        manager.active_responsive = None;
        manager.active_responsive_checked_at = 0;
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
            manager.active_responsive = None;
            manager.active_responsive_checked_at = 0;
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

/// GET /ui-bridge/sdk/screenshot — Capture screenshot for SDK app
///
/// Uses xcap for native screen capture. Supports window-specific and monitor capture.
/// Same query parameters as the control screenshot endpoint.
///
/// Query parameters:
/// - `window_title`: Case-insensitive substring match on window title
/// - `app_name`: Case-insensitive substring match on app name
/// - `window_id`: Exact window ID (HWND)
/// - `monitor`: Monitor index (0-based). If not provided, captures the primary monitor.
async fn handle_screenshot(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<super::ui_bridge::AnnotatedScreenshotQuery>,
) -> Json<ApiResponse<super::ui_bridge::AnnotatedScreenshotData>> {
    // Delegate to the control screenshot handler (same xcap-based implementation)
    super::ui_bridge::ui_bridge_annotated_screenshot_handler(
        axum::extract::State(state),
        axum::extract::Query(query),
    )
    .await
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
// Undo/Redo Awareness
// =============================================================================

/// GET /ui-bridge/sdk/undo-state — Get undo/redo state
async fn handle_undo_state(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/control/undo-state", None).await {
        Ok(data) => Json(data),
        Err(_) => {
            // Fall back to IPC
            match ui_bridge_request_sync(&state, "get_undo_state", serde_json::json!({})).await {
                Ok(data) => Json(serde_json::json!({ "success": true, "data": data })),
                Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
            }
        }
    }
}

/// POST /ui-bridge/sdk/undo — Execute undo
async fn handle_undo(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/control/undo", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/redo — Execute redo
async fn handle_redo(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/control/redo", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// Render Log Handlers
// =============================================================================

/// GET /ui-bridge/sdk/render-log — Get render log entries
async fn handle_render_log(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/render-log", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// DELETE /ui-bridge/sdk/render-log — Clear render log
async fn handle_clear_render_log(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::DELETE, "/render-log", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/render-log/snapshot — Capture render log snapshot
async fn handle_render_log_snapshot(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/render-log/snapshot", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/render-log/path — Get render log path
async fn handle_render_log_path(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/render-log/path", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// Element/Component State Handlers
// =============================================================================

/// GET /ui-bridge/sdk/element/:id/state — Get element state
async fn handle_element_state(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let id = id.trim().to_string();
    let path = format!("/control/element/{}/state", id);
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(_) => {
            // Fall back to IPC — get element details which include state
            match ui_bridge_request_sync(&state, "get_element", serde_json::json!({ "id": id }))
                .await
            {
                Ok(data) => Json(serde_json::json!({ "success": true, "data": data })),
                Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
            }
        }
    }
}

/// GET /ui-bridge/sdk/component/:id/state — Get component state
async fn handle_component_state(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let path = format!("/control/component/{}/state", id);
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/component/:id/action/:actionId — Execute component action
async fn handle_component_action(
    State(state): State<Arc<ApiState>>,
    Path((id, action_id)): Path<(String, String)>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let path = format!("/control/component/{}/action/{}", id, action_id);
    match sdk_request(&state, Method::POST, &path, Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// Workflow Handlers
// =============================================================================

/// GET /ui-bridge/sdk/workflows — List workflows
async fn handle_workflows(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/control/workflows", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/workflow/:id/run — Run a workflow
async fn handle_workflow_run(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    body: Option<Json<serde_json::Value>>,
) -> Json<serde_json::Value> {
    let path = format!("/control/workflow/{}/run", id);
    match sdk_request(&state, Method::POST, &path, body.map(|b| b.0)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/workflow/:runId/status — Get workflow run status
async fn handle_workflow_status(
    State(state): State<Arc<ApiState>>,
    Path(run_id): Path<String>,
) -> Json<serde_json::Value> {
    let path = format!("/control/workflow/{}/status", run_id);
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// Debug Handlers (additional)
// =============================================================================

/// GET /ui-bridge/sdk/debug/action-history — Get action history
async fn handle_action_history(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/debug/action-history", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/debug/element-tree — Get element tree
async fn handle_element_tree(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/debug/element-tree", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// AI Handlers (additional)
// =============================================================================

/// POST /ui-bridge/sdk/ai/find — AI-powered element finding
async fn handle_ai_find(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/find", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/assert/batch — Batch AI assertions
async fn handle_ai_assert_batch(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/assert/batch", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/ai/diff — Get semantic diff
async fn handle_ai_diff(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/ai/diff", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/semantic-search — Semantic search
async fn handle_ai_semantic_search(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/semantic-search", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// State Management Handlers
// =============================================================================

/// GET /ui-bridge/sdk/states — Get all states
async fn handle_states(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/control/states", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/states/active — Get active states
async fn handle_active_states(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/control/states/active", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/states/snapshot — Get state snapshot
async fn handle_state_snapshot(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/control/states/snapshot", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/states/find-path — Find path between states
async fn handle_find_path(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(
        &state,
        Method::POST,
        "/control/states/find-path",
        Some(body),
    )
    .await
    {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/states/navigate — Navigate to a state
async fn handle_navigate_to(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/control/states/navigate", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/state/:id — Get a specific state
async fn handle_get_state(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let path = format!("/control/state/{}", id);
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/state/:id/activate — Activate a state
async fn handle_activate_state(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let path = format!("/control/state/{}/activate", id);
    match sdk_request(&state, Method::POST, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/state/:id/deactivate — Deactivate a state
async fn handle_deactivate_state(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let path = format!("/control/state/{}/deactivate", id);
    match sdk_request(&state, Method::POST, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/state-groups — Get all state groups
async fn handle_state_groups(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/control/state-groups", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/state-group/:id/activate — Activate a state group
async fn handle_activate_state_group(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let path = format!("/control/state-group/{}/activate", id);
    match sdk_request(&state, Method::POST, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/state-group/:id/deactivate — Deactivate a state group
async fn handle_deactivate_state_group(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let path = format!("/control/state-group/{}/deactivate", id);
    match sdk_request(&state, Method::POST, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/transitions — Get all transitions
async fn handle_transitions(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/control/transitions", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/transition/:id/can-execute — Check if transition can execute
async fn handle_can_execute_transition(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let path = format!("/control/transition/{}/can-execute", id);
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/transition/:id/execute — Execute a transition
async fn handle_execute_transition(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let path = format!("/control/transition/{}/execute", id);
    match sdk_request(&state, Method::POST, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// Intent Handlers
// =============================================================================

/// GET /ui-bridge/sdk/ai/intents — List all intents
async fn handle_list_intents(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/ai/intents", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/intents/execute — Execute an intent
async fn handle_execute_intent(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/intents/execute", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/intents/find — Find matching intents
async fn handle_find_intent(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/intents/find", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/intents/register — Register an intent
async fn handle_register_intent(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/intents/register", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/intents/execute-from-query — Execute intent from natural language
async fn handle_execute_intent_from_query(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(
        &state,
        Method::POST,
        "/ai/intents/execute-from-query",
        Some(body),
    )
    .await
    {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// Recovery Handler
// =============================================================================

/// POST /ui-bridge/sdk/ai/recovery/attempt — Attempt error recovery
async fn handle_recovery_attempt(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/recovery/attempt", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// Annotation Handlers
// =============================================================================

/// GET /ui-bridge/sdk/annotations — Get all annotations
async fn handle_annotations(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/annotations", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/annotations/export — Export annotations
async fn handle_annotations_export(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/annotations/export", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/annotations/coverage — Get annotation coverage
async fn handle_annotations_coverage(
    State(state): State<Arc<ApiState>>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/annotations/coverage", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/annotations/import — Import annotations
async fn handle_annotations_import(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/annotations/import", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/annotations/:id — Get a specific annotation
async fn handle_annotation_get(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let path = format!("/annotations/{}", id);
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// PUT /ui-bridge/sdk/annotations/:id — Set/update an annotation
async fn handle_annotation_set(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let path = format!("/annotations/{}", id);
    match sdk_request(&state, Method::PUT, &path, Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// DELETE /ui-bridge/sdk/annotations/:id — Delete an annotation
async fn handle_annotation_delete(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let path = format!("/annotations/{}", id);
    match sdk_request(&state, Method::DELETE, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// Performance & Error Handlers (additional)
// =============================================================================

/// GET /ui-bridge/sdk/performance-entries — Get performance entries
async fn handle_performance_entries(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/control/performance-entries", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/performance-entries/clear — Clear performance entries
async fn handle_clear_performance_entries(
    State(state): State<Arc<ApiState>>,
) -> Json<serde_json::Value> {
    match sdk_request(
        &state,
        Method::POST,
        "/control/performance-entries/clear",
        None,
    )
    .await
    {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/error-snapshots — Get error snapshots
async fn handle_error_snapshots(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/control/error-snapshots", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/error-report — Get error report
async fn handle_error_report(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/control/error-report", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// Quality Evaluation Handlers
// =============================================================================

/// POST /ui-bridge/sdk/design/evaluate — Evaluate quality
async fn handle_evaluate_quality(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/design/evaluate", body.map(|b| b.0)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// GET /ui-bridge/sdk/design/evaluate/contexts — Get quality evaluation contexts
async fn handle_quality_contexts(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::GET, "/design/evaluate/contexts", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/design/evaluate/baseline — Save quality baseline
async fn handle_save_baseline(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Json<serde_json::Value> {
    match sdk_request(
        &state,
        Method::POST,
        "/design/evaluate/baseline",
        body.map(|b| b.0),
    )
    .await
    {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/design/evaluate/diff — Diff quality baseline
async fn handle_diff_baseline(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Json<serde_json::Value> {
    match sdk_request(
        &state,
        Method::POST,
        "/design/evaluate/diff",
        body.map(|b| b.0),
    )
    .await
    {
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
        .route("/ui-bridge/sdk/capabilities", get(handle_capabilities))
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
        .route("/ui-bridge/sdk/find", post(handle_discover))
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
        .route("/ui-bridge/sdk/diagnose-stuck", post(handle_diagnose_stuck))
        // Windows
        .route("/ui-bridge/sdk/windows", get(handle_windows))
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
        // Undo/Redo awareness
        .route("/ui-bridge/sdk/undo-state", get(handle_undo_state))
        .route("/ui-bridge/sdk/undo", post(handle_undo))
        .route("/ui-bridge/sdk/redo", post(handle_redo))
        // Render log
        .route(
            "/ui-bridge/sdk/render-log",
            get(handle_render_log).delete(handle_clear_render_log),
        )
        .route(
            "/ui-bridge/sdk/render-log/snapshot",
            post(handle_render_log_snapshot),
        )
        .route(
            "/ui-bridge/sdk/render-log/path",
            get(handle_render_log_path),
        )
        // Element/Component state
        .route(
            "/ui-bridge/sdk/element/:id/state",
            get(handle_element_state),
        )
        .route(
            "/ui-bridge/sdk/component/:id/state",
            get(handle_component_state),
        )
        .route(
            "/ui-bridge/sdk/component/:id/action/:actionId",
            post(handle_component_action),
        )
        // Workflows
        .route("/ui-bridge/sdk/workflows", get(handle_workflows))
        .route("/ui-bridge/sdk/workflow/:id/run", post(handle_workflow_run))
        .route(
            "/ui-bridge/sdk/workflow/:runId/status",
            get(handle_workflow_status),
        )
        // Debug (additional)
        .route(
            "/ui-bridge/sdk/debug/action-history",
            get(handle_action_history),
        )
        .route(
            "/ui-bridge/sdk/debug/element-tree",
            get(handle_element_tree),
        )
        // AI (additional)
        .route("/ui-bridge/sdk/ai/find", post(handle_ai_find))
        .route(
            "/ui-bridge/sdk/ai/assert/batch",
            post(handle_ai_assert_batch),
        )
        .route("/ui-bridge/sdk/ai/diff", get(handle_ai_diff))
        .route(
            "/ui-bridge/sdk/ai/semantic-search",
            post(handle_ai_semantic_search),
        )
        // State management
        .route("/ui-bridge/sdk/states", get(handle_states))
        .route("/ui-bridge/sdk/states/active", get(handle_active_states))
        .route("/ui-bridge/sdk/states/snapshot", get(handle_state_snapshot))
        .route("/ui-bridge/sdk/states/find-path", post(handle_find_path))
        .route("/ui-bridge/sdk/states/navigate", post(handle_navigate_to))
        .route("/ui-bridge/sdk/state/:id", get(handle_get_state))
        .route(
            "/ui-bridge/sdk/state/:id/activate",
            post(handle_activate_state),
        )
        .route(
            "/ui-bridge/sdk/state/:id/deactivate",
            post(handle_deactivate_state),
        )
        .route("/ui-bridge/sdk/state-groups", get(handle_state_groups))
        .route(
            "/ui-bridge/sdk/state-group/:id/activate",
            post(handle_activate_state_group),
        )
        .route(
            "/ui-bridge/sdk/state-group/:id/deactivate",
            post(handle_deactivate_state_group),
        )
        .route("/ui-bridge/sdk/transitions", get(handle_transitions))
        .route(
            "/ui-bridge/sdk/transition/:id/can-execute",
            get(handle_can_execute_transition),
        )
        .route(
            "/ui-bridge/sdk/transition/:id/execute",
            post(handle_execute_transition),
        )
        // Intents
        .route("/ui-bridge/sdk/ai/intents", get(handle_list_intents))
        .route(
            "/ui-bridge/sdk/ai/intents/execute",
            post(handle_execute_intent),
        )
        .route("/ui-bridge/sdk/ai/intents/find", post(handle_find_intent))
        .route(
            "/ui-bridge/sdk/ai/intents/register",
            post(handle_register_intent),
        )
        .route(
            "/ui-bridge/sdk/ai/intents/execute-from-query",
            post(handle_execute_intent_from_query),
        )
        // Recovery
        .route(
            "/ui-bridge/sdk/ai/recovery/attempt",
            post(handle_recovery_attempt),
        )
        // Annotations
        .route("/ui-bridge/sdk/annotations", get(handle_annotations))
        .route(
            "/ui-bridge/sdk/annotations/export",
            get(handle_annotations_export),
        )
        .route(
            "/ui-bridge/sdk/annotations/coverage",
            get(handle_annotations_coverage),
        )
        .route(
            "/ui-bridge/sdk/annotations/import",
            post(handle_annotations_import),
        )
        .route(
            "/ui-bridge/sdk/annotations/:id",
            get(handle_annotation_get)
                .put(handle_annotation_set)
                .delete(handle_annotation_delete),
        )
        // Performance entries
        .route(
            "/ui-bridge/sdk/performance-entries",
            get(handle_performance_entries),
        )
        .route(
            "/ui-bridge/sdk/performance-entries/clear",
            post(handle_clear_performance_entries),
        )
        // Error snapshots & report
        .route(
            "/ui-bridge/sdk/error-snapshots",
            get(handle_error_snapshots),
        )
        .route("/ui-bridge/sdk/error-report", get(handle_error_report))
        // Quality evaluation
        .route(
            "/ui-bridge/sdk/design/evaluate",
            post(handle_evaluate_quality),
        )
        .route(
            "/ui-bridge/sdk/design/evaluate/contexts",
            get(handle_quality_contexts),
        )
        .route(
            "/ui-bridge/sdk/design/evaluate/baseline",
            post(handle_save_baseline),
        )
        .route(
            "/ui-bridge/sdk/design/evaluate/diff",
            post(handle_diff_baseline),
        )
        // SSE event stream
        .route("/ui-bridge/sdk/events/stream", get(handle_sse_event_stream))
        // =====================================================================
        // /control/-prefixed aliases (canonical SDK paths for path consistency)
        // =====================================================================
        .route("/ui-bridge/sdk/control/elements", get(handle_elements))
        .route("/ui-bridge/sdk/control/element/:id", get(handle_element))
        .route(
            "/ui-bridge/sdk/control/element/:id/action",
            post(handle_element_action),
        )
        .route("/ui-bridge/sdk/control/components", get(handle_components))
        .route(
            "/ui-bridge/sdk/control/component/:id",
            get(handle_component),
        )
        .route("/ui-bridge/sdk/control/find", post(handle_discover))
        .route("/ui-bridge/sdk/control/discover", post(handle_discover))
        .route("/ui-bridge/sdk/control/snapshot", get(handle_snapshot))
        .route(
            "/ui-bridge/sdk/control/console-errors",
            get(handle_console_errors),
        )
        .route(
            "/ui-bridge/sdk/control/console-errors/clear",
            post(handle_clear_console_errors),
        )
        .route("/ui-bridge/sdk/control/forms", get(handle_forms))
        .route("/ui-bridge/sdk/control/fill", post(handle_fill_form))
        .route(
            "/ui-bridge/sdk/control/forms/snapshot",
            post(handle_snapshot_forms),
        )
        .route("/ui-bridge/sdk/control/forms/diff", post(handle_diff_forms))
        .route(
            "/ui-bridge/sdk/control/clipboard",
            get(handle_clipboard_read).post(handle_clipboard_write),
        )
        .route(
            "/ui-bridge/sdk/control/network-requests",
            get(handle_network_requests),
        )
        .route(
            "/ui-bridge/sdk/control/network-requests/in-flight",
            get(handle_network_requests_in_flight),
        )
        .route(
            "/ui-bridge/sdk/control/network-requests/wait",
            post(handle_wait_for_network_request),
        )
        .route(
            "/ui-bridge/sdk/control/network-request/:id",
            get(handle_network_request),
        )
        .route(
            "/ui-bridge/sdk/control/idle-status",
            get(handle_idle_status),
        )
        .route(
            "/ui-bridge/sdk/control/idle-status/:signal",
            get(handle_idle_status_signal),
        )
        .route(
            "/ui-bridge/sdk/control/wait-for-idle",
            post(handle_wait_for_idle),
        )
        .route(
            "/ui-bridge/sdk/control/wait-for-idle/:signal",
            post(handle_wait_for_signal),
        )
        .route(
            "/ui-bridge/sdk/control/wait-for-targets",
            post(handle_wait_for_targets),
        )
        .route(
            "/ui-bridge/sdk/control/page/refresh",
            post(handle_page_refresh),
        )
        .route(
            "/ui-bridge/sdk/control/page/navigate",
            post(handle_page_navigate),
        )
        .route(
            "/ui-bridge/sdk/control/page/back",
            post(handle_page_go_back),
        )
        .route(
            "/ui-bridge/sdk/control/page/forward",
            post(handle_page_go_forward),
        )
        .route(
            "/ui-bridge/sdk/control/browser-events",
            get(handle_console_browser_events),
        )
        .route(
            "/ui-bridge/sdk/control/timeline",
            get(handle_console_timeline),
        )
        .route("/ui-bridge/sdk/control/health", get(handle_console_health))
        .route(
            "/ui-bridge/sdk/control/network-chains",
            get(handle_console_network_chains),
        )
        .route(
            "/ui-bridge/sdk/control/error-sessions/start",
            post(handle_console_error_session_start),
        )
        .route(
            "/ui-bridge/sdk/control/error-sessions/end",
            post(handle_console_error_session_end),
        )
        .route(
            "/ui-bridge/sdk/control/error-sessions",
            get(handle_console_error_sessions_list),
        )
        .route(
            "/ui-bridge/sdk/control/error-baselines/capture",
            post(handle_console_error_baseline_capture),
        )
        .route(
            "/ui-bridge/sdk/control/error-baselines/compare",
            post(handle_console_error_baseline_compare),
        )
        .route("/ui-bridge/sdk/control/undo-state", get(handle_undo_state))
        .route("/ui-bridge/sdk/control/undo", post(handle_undo))
        .route("/ui-bridge/sdk/control/redo", post(handle_redo))
        .route(
            "/ui-bridge/sdk/control/events/stream",
            get(handle_sse_event_stream),
        )
        // Element/Component state aliases
        .route(
            "/ui-bridge/sdk/control/element/:id/state",
            get(handle_element_state),
        )
        .route(
            "/ui-bridge/sdk/control/component/:id/state",
            get(handle_component_state),
        )
        .route(
            "/ui-bridge/sdk/control/component/:id/action/:actionId",
            post(handle_component_action),
        )
        // Workflow aliases
        .route("/ui-bridge/sdk/control/workflows", get(handle_workflows))
        .route(
            "/ui-bridge/sdk/control/workflow/:id/run",
            post(handle_workflow_run),
        )
        .route(
            "/ui-bridge/sdk/control/workflow/:runId/status",
            get(handle_workflow_status),
        )
        // State management aliases
        .route("/ui-bridge/sdk/control/states", get(handle_states))
        .route(
            "/ui-bridge/sdk/control/states/active",
            get(handle_active_states),
        )
        .route(
            "/ui-bridge/sdk/control/states/snapshot",
            get(handle_state_snapshot),
        )
        .route(
            "/ui-bridge/sdk/control/states/find-path",
            post(handle_find_path),
        )
        .route(
            "/ui-bridge/sdk/control/states/navigate",
            post(handle_navigate_to),
        )
        .route("/ui-bridge/sdk/control/state/:id", get(handle_get_state))
        .route(
            "/ui-bridge/sdk/control/state/:id/activate",
            post(handle_activate_state),
        )
        .route(
            "/ui-bridge/sdk/control/state/:id/deactivate",
            post(handle_deactivate_state),
        )
        .route(
            "/ui-bridge/sdk/control/state-groups",
            get(handle_state_groups),
        )
        .route(
            "/ui-bridge/sdk/control/state-group/:id/activate",
            post(handle_activate_state_group),
        )
        .route(
            "/ui-bridge/sdk/control/state-group/:id/deactivate",
            post(handle_deactivate_state_group),
        )
        .route(
            "/ui-bridge/sdk/control/transitions",
            get(handle_transitions),
        )
        .route(
            "/ui-bridge/sdk/control/transition/:id/can-execute",
            get(handle_can_execute_transition),
        )
        .route(
            "/ui-bridge/sdk/control/transition/:id/execute",
            post(handle_execute_transition),
        )
        // Performance & error aliases
        .route(
            "/ui-bridge/sdk/control/performance-entries",
            get(handle_performance_entries),
        )
        .route(
            "/ui-bridge/sdk/control/performance-entries/clear",
            post(handle_clear_performance_entries),
        )
        .route(
            "/ui-bridge/sdk/control/error-snapshots",
            get(handle_error_snapshots),
        )
        .route(
            "/ui-bridge/sdk/control/error-report",
            get(handle_error_report),
        )
        // Media discovery & analysis
        .route("/ui-bridge/sdk/ai/media/find", post(handle_media_find))
        .route(
            "/ui-bridge/sdk/ai/media/audit/accessibility",
            post(handle_media_audit_accessibility),
        )
        .route(
            "/ui-bridge/sdk/ai/media/audit/performance",
            post(handle_media_audit_performance),
        )
        .route(
            "/ui-bridge/sdk/ai/media/snapshot",
            post(handle_media_snapshot),
        )
        .route(
            "/ui-bridge/sdk/ai/media/compare",
            post(handle_media_compare),
        )
        .route(
            "/ui-bridge/sdk/ai/media/analyze",
            post(handle_media_analyze),
        )
        .route(
            "/ui-bridge/sdk/ai/media/analyze/batch",
            post(handle_media_analyze_batch),
        )
        .route(
            "/ui-bridge/sdk/ai/media/analyze/page",
            post(handle_media_analyze_page),
        )
        // Heartbeat
        .route("/ui-bridge/sdk/heartbeat", post(handle_heartbeat))
        // Query selector & page evaluate (IPC relay)
        .route(
            "/ui-bridge/sdk/control/query-selector",
            post(handle_query_selector),
        )
        .route(
            "/ui-bridge/sdk/control/page-evaluate",
            post(handle_page_evaluate),
        )
}

// =============================================================================
// Query Selector & Page Evaluate (IPC relay)
// =============================================================================

/// POST /ui-bridge/sdk/control/query-selector — Query DOM elements by CSS selector
async fn handle_query_selector(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match ui_bridge_request_sync(&state, "query_selector", body).await {
        Ok(data) => Json(serde_json::json!({ "success": true, "data": data })),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/control/page-evaluate — Evaluate JS expression in the webview
async fn handle_page_evaluate(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match ui_bridge_request_sync(&state, "page_evaluate", body).await {
        Ok(data) => Json(serde_json::json!({ "success": true, "data": data })),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// Media Discovery & Analysis
// =============================================================================

/// POST /ui-bridge/sdk/ai/media/find — Find media elements with filters
async fn handle_media_find(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/media/find", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/media/audit/accessibility — Alt text audit
async fn handle_media_audit_accessibility(
    State(state): State<Arc<ApiState>>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/media/audit/accessibility", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/media/audit/performance — Performance audit
async fn handle_media_audit_performance(
    State(state): State<Arc<ApiState>>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/media/audit/performance", None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/media/snapshot — Capture media snapshot
async fn handle_media_snapshot(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/media/snapshot", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/media/compare — Compare two media snapshots
async fn handle_media_compare(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/media/compare", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/media/analyze — AI analysis of single media element
async fn handle_media_analyze(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/media/analyze", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/media/analyze/batch — AI analysis of multiple media elements
async fn handle_media_analyze_batch(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/media/analyze/batch", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// POST /ui-bridge/sdk/ai/media/analyze/page — AI analysis of all visible media on page
async fn handle_media_analyze_page(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match sdk_request(&state, Method::POST, "/ai/media/analyze/page", Some(body)).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

// =============================================================================
// SSE Event Stream Proxy
// =============================================================================

/// Query params for SSE stream
#[derive(Debug, Deserialize)]
struct SseStreamQuery {
    /// Comma-separated event types to filter
    #[serde(default)]
    types: Option<String>,
    /// Comma-separated element IDs to filter
    #[serde(default)]
    elements: Option<String>,
}

/// GET /ui-bridge/sdk/events/stream — SSE proxy to the SDK app's event stream
///
/// Connects to the SDK app's SSE endpoint and forwards events to the caller.
async fn handle_sse_event_stream(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<SseStreamQuery>,
) -> Sse<impl Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let stream = async_stream::stream! {
        // Build the SSE URL for the SDK app
        let sdk_url = {
            let conn_guard = state.sdk_connection.lock().await;
            conn_guard.active_connection().map(|conn| {
                let mut url = format!("{}{}/control/events/stream", conn.app_url, conn.base_path);
                let mut sep = '?';
                if let Some(ref types) = query.types {
                    url.push_str(&format!("{}types={}", sep, types));
                    sep = '&';
                }
                if let Some(ref elements) = query.elements {
                    url.push_str(&format!("{}elements={}", sep, elements));
                }
                (url, conn.client.clone())
            })
        };

        match sdk_url {
            Some((url, client)) => {
                debug!(url = %url, "Connecting to SDK SSE stream");

                match client.get(&url).send().await {
                    Ok(response) => {
                        let mut byte_stream = response.bytes_stream();
                        let mut buffer = String::new();

                        while let Some(chunk_result) = byte_stream.next().await {
                            match chunk_result {
                                Ok(bytes) => {
                                    buffer.push_str(&String::from_utf8_lossy(&bytes));

                                    // Parse complete SSE events from buffer
                                    while let Some(end) = buffer.find("\n\n") {
                                        let event_block = buffer[..end].to_string();
                                        buffer = buffer[end + 2..].to_string();

                                        // Parse SSE fields
                                        let mut event_type = String::new();
                                        let mut data = String::new();
                                        let mut id = String::new();

                                        for line in event_block.lines() {
                                            if let Some(val) = line.strip_prefix("event: ") {
                                                event_type = val.to_string();
                                            } else if let Some(val) = line.strip_prefix("data: ") {
                                                data = val.to_string();
                                            } else if let Some(val) = line.strip_prefix("id: ") {
                                                id = val.to_string();
                                            }
                                        }

                                        if !data.is_empty() {
                                            let mut event = SseEvent::default().data(data);
                                            if !event_type.is_empty() {
                                                event = event.event(event_type);
                                            }
                                            if !id.is_empty() {
                                                event = event.id(id);
                                            }
                                            yield Ok(event);
                                        }
                                    }
                                }
                                Err(e) => {
                                    warn!("SSE stream error: {}", e);
                                    yield Ok(SseEvent::default()
                                        .event("error")
                                        .data(format!(r#"{{"error":"Stream error: {}"}}"#, e)));
                                    break;
                                }
                            }
                        }

                        // Flush any remaining complete events in the buffer
                        while let Some(end) = buffer.find("\n\n") {
                            let event_block = buffer[..end].to_string();
                            buffer = buffer[end + 2..].to_string();

                            let mut event_type = String::new();
                            let mut data = String::new();
                            let mut id = String::new();

                            for line in event_block.lines() {
                                if let Some(val) = line.strip_prefix("event: ") {
                                    event_type = val.to_string();
                                } else if let Some(val) = line.strip_prefix("data: ") {
                                    data = val.to_string();
                                } else if let Some(val) = line.strip_prefix("id: ") {
                                    id = val.to_string();
                                }
                            }

                            if !data.is_empty() {
                                let mut event = SseEvent::default().data(data);
                                if !event_type.is_empty() {
                                    event = event.event(event_type);
                                }
                                if !id.is_empty() {
                                    event = event.id(id);
                                }
                                yield Ok(event);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to connect to SDK SSE: {}", e);
                        yield Ok(SseEvent::default()
                            .event("error")
                            .data(format!(r#"{{"error":"Failed to connect: {}"}}"#, e)));
                    }
                }
            }
            None => {
                yield Ok(SseEvent::default()
                    .event("error")
                    .data(r#"{"error":"No active SDK app connection"}"#));
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
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
