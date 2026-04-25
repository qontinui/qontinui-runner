//! App Discovery Module
//!
//! Discovers UI Bridge-enabled applications by scanning localhost ports
//! for health endpoints that include `uiBridge` metadata.
//!
//! Supports three discovery modes:
//! - Web: Scans common web dev ports (3000-3010, 4200, 5173-5175, 8080)
//! - Desktop: Scans common desktop app ports (1420, 9876-9878)
//! - Mobile: Lists ADB devices and checks for UI Bridge via port forwarding

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;
use tracing::debug;

use crate::mcp::types::{ApiResponse, ApiState};

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredApp {
    pub app_id: String,
    pub app_name: String,
    pub app_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub framework: Option<String>,
    pub url: String,
    pub port: u16,
    pub base_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_count: Option<u32>,
    pub discovered_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileDevice {
    pub device_id: String,
    pub device_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_bridge: Option<DiscoveredApp>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryResult {
    pub web: Vec<DiscoveredApp>,
    pub desktop: Vec<DiscoveredApp>,
    pub mobile: Vec<MobileDevice>,
    pub scanned_at: i64,
    pub duration_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterAppRequest {
    pub app_id: String,
    pub app_name: String,
    pub app_type: String,
    #[serde(default)]
    pub framework: Option<String>,
    #[serde(default)]
    pub transport: Option<String>,
    /// Absolute URL where the runner can reach this app's UI Bridge endpoints,
    /// e.g. "http://127.0.0.1:9875/supervisor-bridge". Preferred.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Legacy — used when `base_url` is absent.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub base_path: Option<String>,
    #[serde(default)]
    pub capabilities: Option<Vec<String>>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub origin: Option<String>,
    /// Optional WebSocket connection id, used when an app is promoting an
    /// already-open `/ui-bridge/ws` connection into the registry via a
    /// subsequent phone-home POST. Normally set by the WS handler itself,
    /// but accepting it here keeps the payload symmetric.
    #[serde(default)]
    pub websocket_conn_id: Option<u64>,
}

/// Response returned from `POST /ui-bridge/apps/register`. Mirrors
/// `DiscoveredApp` plus the runner's canonical view of the app's transport
/// so the SDK can confirm it registered as expected.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterAppResponse {
    #[serde(flatten)]
    pub app: DiscoveredApp,
    pub transport: crate::mcp::app_registry::AppTransport,
}

// ============================================================================
// Port Lists
// ============================================================================

const WEB_DEV_PORTS: &[u16] = &[
    3000, 3001, 3002, 3003, 3004, 3005, 3006, 3007, 3008, 3009, 3010, 4200, // Angular
    5173, 5174, 5175, // Vite
    8080, 8081, // Generic
    4000, // Phoenix/misc
];

const DESKTOP_APP_PORTS: &[u16] = &[
    1420, // Tauri dev server
    9875, // Qontinui Supervisor
    9876, // Qontinui Runner
    9877, 9878, // Runner fallback ports
    8888, // Electron common
    3333, // Desktop misc
];

const HEALTH_PATHS: &[(&str, &str)] = &[
    ("/api/ui-bridge/health", "/api/ui-bridge"),
    ("/ui-bridge/health", "/ui-bridge"),
    ("/supervisor-bridge/health", "/supervisor-bridge"),
    ("/health", ""),
];
const SCAN_TIMEOUT_MS: u64 = 1500;

// ============================================================================
// Scanner Implementation
// ============================================================================

/// Build a shared HTTP client for scanning (avoids creating one per port check)
fn build_scan_client() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_millis(SCAN_TIMEOUT_MS))
        .pool_max_idle_per_host(2)
        .build()
        .ok()
}

/// Check a single port for UI Bridge health endpoint
async fn check_port(client: &reqwest::Client, port: u16) -> Option<DiscoveredApp> {
    for (health_path, base_path) in HEALTH_PATHS {
        match check_health(client, port, health_path, base_path).await {
            Some(app) => return Some(app),
            None => continue,
        }
    }
    None
}

/// Check a specific health endpoint on a port
async fn check_health(
    client: &reqwest::Client,
    port: u16,
    path: &str,
    base_path: &str,
) -> Option<DiscoveredApp> {
    // Use 127.0.0.1 instead of localhost to avoid IPv6 resolution delays on Windows
    let url = format!("http://127.0.0.1:{}{}", port, path);

    let response = match timeout(
        Duration::from_millis(SCAN_TIMEOUT_MS + 200),
        client.get(&url).send(),
    )
    .await
    {
        Ok(Ok(resp)) => resp,
        _ => return None,
    };

    let body: serde_json::Value = match response.json().await {
        Ok(v) => v,
        Err(_) => return None,
    };

    // Check for uiBridge field in the response
    let ui_bridge = body.get("uiBridge")?;

    let now = chrono::Utc::now().timestamp_millis();

    Some(DiscoveredApp {
        app_id: ui_bridge
            .get("appId")
            .and_then(|v| v.as_str())
            .unwrap_or(&format!("app-{}", port))
            .to_string(),
        app_name: ui_bridge
            .get("appName")
            .and_then(|v| v.as_str())
            .unwrap_or(&format!("App on port {}", port))
            .to_string(),
        app_type: ui_bridge
            .get("appType")
            .and_then(|v| v.as_str())
            .unwrap_or("other")
            .to_string(),
        framework: ui_bridge
            .get("framework")
            .and_then(|v| v.as_str())
            .map(String::from),
        url: format!("http://127.0.0.1:{}", port),
        port,
        base_path: base_path.to_string(),
        version: ui_bridge
            .get("version")
            .and_then(|v| v.as_str())
            .map(String::from),
        capabilities: ui_bridge
            .get("capabilities")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        element_count: ui_bridge
            .get("elementCount")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        component_count: ui_bridge
            .get("componentCount")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        discovered_at: now,
    })
}

/// Scan a list of ports in parallel using a shared HTTP client
async fn scan_ports(ports: &[u16]) -> Vec<DiscoveredApp> {
    let client = match build_scan_client() {
        Some(c) => Arc::new(c),
        None => return Vec::new(),
    };

    let handles: Vec<_> = ports
        .iter()
        .map(|&port| {
            let client = client.clone();
            tokio::spawn(async move { check_port(&client, port).await })
        })
        .collect();

    let mut apps = Vec::new();
    for handle in handles {
        if let Ok(Some(app)) = handle.await {
            apps.push(app);
        }
    }
    apps
}

/// Load user-configured discovery ports from `settings.json`. Performs the
/// blocking disk read off the async runtime.
async fn user_discovery_ports() -> Vec<u16> {
    tokio::task::spawn_blocking(|| crate::settings::load_settings().discovery_ports)
        .await
        .unwrap_or_default()
}

/// Merge the hardcoded desktop port list with user-configured ports, dedup
/// while preserving the original order (defaults first, user entries after).
async fn desktop_ports_merged() -> Vec<u16> {
    let mut seen = std::collections::HashSet::new();
    let mut merged: Vec<u16> = Vec::with_capacity(DESKTOP_APP_PORTS.len());
    for &p in DESKTOP_APP_PORTS {
        if seen.insert(p) {
            merged.push(p);
        }
    }
    for p in user_discovery_ports().await {
        if seen.insert(p) {
            merged.push(p);
        }
    }
    merged
}

// ============================================================================
// ADB Utilities (minimal, for discovery only)
// ============================================================================

/// Find ADB path
fn find_adb() -> Option<std::path::PathBuf> {
    // Check ANDROID_HOME
    if let Ok(android_home) = std::env::var("ANDROID_HOME") {
        let p = std::path::PathBuf::from(&android_home)
            .join("platform-tools")
            .join(if cfg!(windows) { "adb.exe" } else { "adb" });
        if p.exists() {
            return Some(p);
        }
    }

    // Check ANDROID_SDK_ROOT
    if let Ok(sdk_root) = std::env::var("ANDROID_SDK_ROOT") {
        let p = std::path::PathBuf::from(&sdk_root)
            .join("platform-tools")
            .join(if cfg!(windows) { "adb.exe" } else { "adb" });
        if p.exists() {
            return Some(p);
        }
    }

    // Common Windows paths
    if cfg!(windows) {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            let p = std::path::PathBuf::from(local_app_data)
                .join("Android")
                .join("Sdk")
                .join("platform-tools")
                .join("adb.exe");
            if p.exists() {
                return Some(p);
            }
        }
    }

    // Fallback to PATH
    Some(std::path::PathBuf::from(if cfg!(windows) {
        "adb.exe"
    } else {
        "adb"
    }))
}

/// List connected ADB devices via the pure-Rust `adb_client` crate.
async fn list_adb_devices() -> Vec<MobileDevice> {
    crate::mcp::adb_helper::list_devices()
        .await
        .into_iter()
        .map(|d| {
            let device_type = if d.serial.starts_with("emulator-") {
                "emulator"
            } else {
                "device"
            }
            .to_string();

            let status = match d.state.as_str() {
                "device" => "online",
                "offline" => "offline",
                "unauthorized" => "unauthorized",
                _ => "offline",
            }
            .to_string();

            MobileDevice {
                device_id: d.serial,
                device_type,
                model: d.model,
                status,
                ui_bridge: None,
            }
        })
        .collect()
}

/// Check if a mobile device has UI Bridge by port forwarding
async fn check_device_ui_bridge(device_id: &str) -> Option<DiscoveredApp> {
    let adb_path = find_adb()?;
    let client = build_scan_client()?;

    // Forward a local port to the device's UI Bridge port
    // Use tcp:0 to let the OS pick a free local port
    let output = crate::process_helpers::tokio_no_window(&adb_path)
        .args(["-s", device_id, "forward", "tcp:0", "tcp:9876"])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    // Parse the local port from output
    let local_port_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let local_port: u16 = local_port_str.parse().ok()?;

    debug!(
        "Port forwarded device {} to local port {}",
        device_id, local_port
    );

    // Check the forwarded port
    let result = check_port(&client, local_port).await;

    // Remove the port forward
    let _ = crate::process_helpers::tokio_no_window(&adb_path)
        .args([
            "-s",
            device_id,
            "forward",
            "--remove",
            &format!("tcp:{}", local_port),
        ])
        .output()
        .await;

    result
}

// ============================================================================
// HTTP Handlers
// ============================================================================

/// Full scan -- web + desktop + mobile
async fn scan_all(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<DiscoveryResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    let start = std::time::Instant::now();

    let desktop_ports = desktop_ports_merged().await;

    // Run web and desktop scans in parallel
    let (web_apps, desktop_apps, mut mobile_devices) = tokio::join!(
        scan_ports(WEB_DEV_PORTS),
        scan_ports(&desktop_ports),
        list_adb_devices()
    );

    // Check UI Bridge on each online mobile device
    for device in &mut mobile_devices {
        if device.status == "online" {
            device.ui_bridge = check_device_ui_bridge(&device.device_id).await;
        }
    }

    // Merge physical device registry entries (LAN/Cloud devices not found by ADB scan)
    let registry_devices = state.physical_device_registry.list_all().await;
    for registry_device in registry_devices {
        // Skip if already in the ADB list
        let already_present = mobile_devices
            .iter()
            .any(|m| m.device_id == registry_device.info.id);
        if already_present {
            continue;
        }
        // Only include devices with an active transport
        if let Some(proxy_url) = registry_device.active_proxy_url() {
            let device_type = registry_device.info.device_kind.clone();
            let model = registry_device.info.model.clone();
            let transport_kind = registry_device
                .transports
                .first()
                .map(|t| format!("{}", t.kind))
                .unwrap_or_else(|| "unknown".to_string());
            let status = match registry_device.health_state {
                crate::mcp::physical_device::HealthState::Healthy => "online",
                crate::mcp::physical_device::HealthState::Degraded(_) => "degraded",
                crate::mcp::physical_device::HealthState::Unreachable => "offline",
            }
            .to_string();
            mobile_devices.push(MobileDevice {
                device_id: registry_device.info.id.clone(),
                device_type: format!("{}-{}", transport_kind.to_lowercase(), device_type),
                model,
                status,
                ui_bridge: None, // populated lazily; proxy_url available separately
            });
        }
    }

    let duration = start.elapsed();

    Ok(Json(ApiResponse::success(DiscoveryResult {
        web: web_apps,
        desktop: desktop_apps,
        mobile: mobile_devices,
        scanned_at: chrono::Utc::now().timestamp_millis(),
        duration_ms: duration.as_millis() as u64,
    })))
}

/// Scan web apps only
async fn scan_web(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<DiscoveredApp>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let apps = scan_ports(WEB_DEV_PORTS).await;
    Ok(Json(ApiResponse::success(apps)))
}

/// Scan desktop apps only
async fn scan_desktop(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<DiscoveredApp>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let ports = desktop_ports_merged().await;
    let apps = scan_ports(&ports).await;
    Ok(Json(ApiResponse::success(apps)))
}

/// Scan mobile devices only
async fn scan_mobile(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<MobileDevice>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mut devices = list_adb_devices().await;
    for device in &mut devices {
        if device.status == "online" {
            device.ui_bridge = check_device_ui_bridge(&device.device_id).await;
        }
    }
    Ok(Json(ApiResponse::success(devices)))
}

/// Scan iOS devices only (Plan 2) — spawns ios_bridge sidecar on first call.
async fn scan_ios(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<MobileDevice>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let ios_devices = match state.ios_transport.list_devices().await {
        Ok(d) => d,
        Err(e) => {
            return Ok(Json(ApiResponse::error(format!("iOS scan failed: {e}"))));
        }
    };

    let mobile: Vec<MobileDevice> = ios_devices
        .into_iter()
        .map(|d| MobileDevice {
            device_id: d.udid,
            device_type: format!("ios-{}", d.connection),
            model: d
                .name
                .clone()
                .or(d.product_type.clone())
                .or_else(|| Some("iOS device".to_string())),
            status: if d.paired { "online" } else { "unauthorized" }.to_string(),
            ui_bridge: None,
        })
        .collect();

    Ok(Json(ApiResponse::success(mobile)))
}

// ============================================================================
// ADB Port Forwarding (persistent, for connecting to mobile devices)
// ============================================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardDeviceRequest {
    pub device_id: String,
    #[serde(default = "default_remote_port")]
    pub remote_port: u16,
}

fn default_remote_port() -> u16 {
    9876
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardDeviceResponse {
    pub local_port: u16,
    pub device_id: String,
    pub remote_port: u16,
}

/// Set up persistent ADB port forwarding for a mobile device.
/// Unlike the scan which creates a temporary forward, this one stays active
/// so the SDK client can connect to the device's UI Bridge.
async fn forward_device(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<ForwardDeviceRequest>,
) -> Result<Json<ApiResponse<ForwardDeviceResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let adb_path = find_adb().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::error(
                "ADB not found. Install Android SDK platform-tools.",
            )),
        )
    })?;

    // Use tcp:0 to let the OS pick a free local port
    let output = crate::process_helpers::tokio_no_window(&adb_path)
        .args([
            "-s",
            &req.device_id,
            "forward",
            "tcp:0",
            &format!("tcp:{}", req.remote_port),
        ])
        .output()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::error(format!("Failed to run adb: {}", e))),
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::error(format!(
                "ADB forward failed: {}",
                stderr.trim()
            ))),
        ));
    }

    let local_port_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let local_port: u16 = local_port_str.parse().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::error(format!(
                "Failed to parse local port from ADB output: '{}'",
                local_port_str
            ))),
        )
    })?;

    debug!(
        "Persistent port forward: device {} remote {} -> local {}",
        req.device_id, req.remote_port, local_port
    );

    Ok(Json(ApiResponse::success(ForwardDeviceResponse {
        local_port,
        device_id: req.device_id,
        remote_port: req.remote_port,
    })))
}

/// iOS USB port forward (Plan 2) — routes through the pymobiledevice3 sidecar.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IosForwardRequest {
    pub udid: String,
    #[serde(default = "default_remote_port")]
    pub device_port: u16,
}

async fn ios_forward(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<IosForwardRequest>,
) -> Result<Json<ApiResponse<ForwardDeviceResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state
        .ios_transport
        .forward_port(&req.udid, req.device_port)
        .await
    {
        Ok(local_port) => Ok(Json(ApiResponse::success(ForwardDeviceResponse {
            local_port,
            device_id: req.udid,
            remote_port: req.device_port,
        }))),
        Err(e) => Ok(Json(ApiResponse::error(format!("iOS forward: {e}")))),
    }
}

/// Phone-home registration from the SDK's `CommandRelayListener`.
/// Also accepts the legacy manual `{url, port, basePath}` shape when `baseUrl`
/// is absent, so existing callers keep working.
async fn register_app(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<RegisterAppRequest>,
) -> Result<Json<ApiResponse<RegisterAppResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    use crate::mcp::app_registry::AppTransport;

    // Parse the declared transport (defaults to HTTP when absent). Unknown
    // values are rejected so wrappers get a clear signal rather than being
    // silently downgraded.
    let transport = match req.transport.as_deref() {
        None | Some("") | Some("http") => AppTransport::Http,
        Some("websocket") => AppTransport::Websocket,
        Some(other) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::error(format!(
                    "unknown transport '{}' — expected 'http' or 'websocket'",
                    other
                ))),
            ));
        }
    };

    // Derive a canonical (origin, port, base_path) triple from either the
    // preferred `base_url` or the legacy `url` + `port` + `base_path` shape.
    //
    // WebSocket-transport apps do not need a reachable `baseUrl` (the runner
    // pushes commands over the socket), so we synthesize placeholder values
    // when they are absent. This lets third-party browser tabs and headless
    // wrappers register without exposing an HTTP server of their own.
    let (origin_url, port, base_path) = match &req.base_url {
        Some(raw) if !raw.is_empty() => match url::Url::parse(raw) {
            Ok(parsed) => {
                let port = parsed
                    .port_or_known_default()
                    .unwrap_or(match parsed.scheme() {
                        "https" => 443,
                        _ => 80,
                    });
                let host = parsed.host_str().unwrap_or("127.0.0.1");
                let origin = format!("{}://{}:{}", parsed.scheme(), host, port);
                let path = parsed.path().trim_end_matches('/').to_string();
                (origin, port, path)
            }
            Err(e) => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(ApiResponse::error(format!(
                        "invalid baseUrl '{}': {}",
                        raw, e
                    ))),
                ));
            }
        },
        _ => {
            // Legacy shape: url + port (+ optional basePath).
            // For WebSocket-transport apps these fields are optional — the
            // runner reaches them over the socket, not their own HTTP port.
            let url_val = req.url.clone().unwrap_or_default();
            let port_val = req.port.unwrap_or(0);
            if transport == AppTransport::Http && (url_val.is_empty() || port_val == 0) {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(ApiResponse::error(
                        "register_app requires either `baseUrl` or (`url` + `port`) for HTTP transport",
                    )),
                ));
            }
            let base = req.base_path.clone().unwrap_or_default();
            let base = base.trim_end_matches('/').to_string();
            (url_val, port_val, base)
        }
    };

    let app = DiscoveredApp {
        app_id: req.app_id.clone(),
        app_name: req.app_name,
        app_type: req.app_type,
        framework: req.framework,
        url: origin_url,
        port,
        base_path,
        version: req.version,
        capabilities: req.capabilities.unwrap_or_default(),
        element_count: None,
        component_count: None,
        discovered_at: chrono::Utc::now().timestamp_millis(),
    };

    state
        .app_registry
        .upsert(
            app.clone(),
            req.origin,
            transport,
            req.websocket_conn_id,
        )
        .await;

    debug!(
        "[app-registry] registered appId={} origin={} port={} basePath={} transport={:?}",
        app.app_id, app.url, app.port, app.base_path, transport
    );

    Ok(Json(ApiResponse::success(RegisterAppResponse {
        app,
        transport,
    })))
}

/// List apps that have phoned home and are still within the TTL window.
/// Returns the transport discriminator per entry so the integration tool
/// can distinguish HTTP-phone-home apps from WebSocket-registered wrappers.
async fn list_registered_apps(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<RegisterAppResponse>>> {
    let apps = state
        .app_registry
        .list_live()
        .await
        .into_iter()
        .map(|e| RegisterAppResponse {
            app: e.app,
            transport: e.transport,
        })
        .collect();
    Json(ApiResponse::success(apps))
}

/// Explicit deregistration — useful on `beforeunload` when the SDK can still
/// send a beacon. Returns `true` if an entry was removed.
async fn deregister_app(
    State(state): State<Arc<ApiState>>,
    Path(app_id): Path<String>,
) -> Json<ApiResponse<bool>> {
    let removed = state.app_registry.remove(&app_id).await;
    Json(ApiResponse::success(removed))
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/ui-bridge/apps/scan", post(scan_all))
        .route("/ui-bridge/apps/scan/web", get(scan_web))
        .route("/ui-bridge/apps/scan/desktop", get(scan_desktop))
        .route("/ui-bridge/apps/scan/mobile", get(scan_mobile))
        .route("/ui-bridge/apps/scan/ios", get(scan_ios))
        .route("/ui-bridge/apps/register", post(register_app))
        .route("/ui-bridge/apps/registered", get(list_registered_apps))
        .route(
            "/ui-bridge/apps/register/{app_id}",
            axum::routing::delete(deregister_app),
        )
        .route("/ui-bridge/apps/forward-device", post(forward_device))
        .route("/ui-bridge/ios/forward", post(ios_forward))
}
