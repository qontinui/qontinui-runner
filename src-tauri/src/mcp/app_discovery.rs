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
use reqwest::Method;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;
use tracing::debug;

use crate::mcp::app_dispatch::DispatchError;
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
    /// Optional per-entry TTL override (in seconds). Tests, scripts, and
    /// synthetic injections that need an entry to stay alive longer than the
    /// default 30-second heartbeat window can pass this to avoid sending
    /// repeated heartbeats. Capped server-side at one day; values above the
    /// cap are rejected with 400.
    #[serde(default)]
    pub keep_alive_secs: Option<u64>,
}

/// Server-side cap on per-entry `keep_alive_secs` (1 day, expressed in ms).
/// Prevents misbehaving callers from leaving zombie entries in the registry
/// indefinitely.
pub const MAX_KEEP_ALIVE_MS: i64 = 24 * 3600 * 1_000;

/// Validate the optional `keep_alive_secs` override and convert it to the
/// `Option<i64>` ms shape used by the registry. Extracted so tests can
/// exercise the cap without building a full `ApiState`.
pub(crate) fn validate_keep_alive_secs(secs: Option<u64>) -> Result<Option<i64>, String> {
    match secs {
        Some(s) => {
            let ms = (s as i64).saturating_mul(1_000);
            if ms > MAX_KEEP_ALIVE_MS {
                return Err(format!(
                    "keepAliveSecs {} exceeds cap of {} (one day)",
                    s,
                    MAX_KEEP_ALIVE_MS / 1_000
                ));
            }
            Ok(Some(ms))
        }
        None => Ok(None),
    }
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

    // Validate and normalize the optional keep_alive override. We convert
    // seconds → milliseconds via i64 to match the registry's freshness math
    // and reject values above the server-side cap so misbehaving callers
    // can't leave zombie entries in the registry indefinitely.
    let keep_alive_ms: Option<i64> = match validate_keep_alive_secs(req.keep_alive_secs) {
        Ok(v) => v,
        Err(msg) => {
            return Err((StatusCode::BAD_REQUEST, Json(ApiResponse::error(msg))));
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
            keep_alive_ms,
        )
        .await;

    debug!(
        "[app-registry] registered appId={} origin={} port={} basePath={} transport={:?} keepAliveMs={:?}",
        app.app_id, app.url, app.port, app.base_path, transport, keep_alive_ms
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
// POST /ui-bridge/control/wait-for-app
// ============================================================================

/// Server-side ceiling on `timeoutMs`. Mirrors the cap used by the snapshot-
/// based `/ui-bridge/ai/wait-for*` endpoints — anything above this is almost
/// certainly a misconfigured caller.
const WAIT_FOR_APP_MAX_TIMEOUT_MS: u64 = 60_000;
const WAIT_FOR_APP_DEFAULT_TIMEOUT_MS: u64 = 5_000;
const WAIT_FOR_APP_DEFAULT_POLL_MS: u64 = 100;
const WAIT_FOR_APP_MIN_POLL_MS: u64 = 50;
const WAIT_FOR_APP_MAX_POLL_MS: u64 = 1_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitForAppRequest {
    pub app_id: String,
    /// Optional transport filter — when set, the predicate is satisfied only
    /// if the live entry's transport matches.
    #[serde(default)]
    pub transport: Option<crate::mcp::app_registry::AppTransport>,
    /// Default `true`. If `false`, wait for the entry to disappear (or age
    /// past freshness) instead of appear.
    #[serde(default = "default_wait_for_app_present")]
    pub present: bool,
    #[serde(default = "default_wait_for_app_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_wait_for_app_poll_ms")]
    pub poll_ms: u64,
}

fn default_wait_for_app_present() -> bool {
    true
}
fn default_wait_for_app_timeout_ms() -> u64 {
    WAIT_FOR_APP_DEFAULT_TIMEOUT_MS
}
fn default_wait_for_app_poll_ms() -> u64 {
    WAIT_FOR_APP_DEFAULT_POLL_MS
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitForAppResponse {
    pub satisfied: bool,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timed_out: Option<bool>,
    /// The matched registry entry. Populated when `satisfied: true` AND
    /// `present: true` — the app the caller was waiting for is now visible
    /// and they typically want to act on its `transport`/`url` immediately.
    /// Omitted on disappearance waits (`present: false`) since there is no
    /// entry to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<RegisterAppResponse>,
}

/// Validate the request and run the polling loop. Extracted so tests can
/// drive it with a bare `AppRegistry` instead of building a full `ApiState`.
pub(crate) async fn wait_for_app_inner(
    registry: &crate::mcp::app_registry::AppRegistry,
    req: &WaitForAppRequest,
) -> Result<WaitForAppResponse, String> {
    use crate::mcp::app_registry::AppTransport;

    if req.app_id.is_empty() {
        return Err("appId must be non-empty".to_string());
    }
    if req.timeout_ms == 0 || req.timeout_ms > WAIT_FOR_APP_MAX_TIMEOUT_MS {
        return Err(format!(
            "timeoutMs must be in (0, {}]",
            WAIT_FOR_APP_MAX_TIMEOUT_MS
        ));
    }
    if req.poll_ms < WAIT_FOR_APP_MIN_POLL_MS || req.poll_ms > WAIT_FOR_APP_MAX_POLL_MS {
        return Err(format!(
            "pollMs must be in [{}, {}]",
            WAIT_FOR_APP_MIN_POLL_MS, WAIT_FOR_APP_MAX_POLL_MS
        ));
    }

    let start = std::time::Instant::now();
    let deadline = start + Duration::from_millis(req.timeout_ms);
    let target_transport: Option<AppTransport> = req.transport;

    // Helper for evaluating the predicate against the registry's current
    // view. Uses `list_live` so that "absent" means "not registered OR aged
    // past freshness," matching the natural semantics of present=false.
    let evaluate = |live: &[crate::mcp::app_registry::RegisteredApp]| -> bool {
        let matched = live.iter().find(|e| e.app.app_id == req.app_id);
        match (req.present, matched, target_transport) {
            // present=true, found, transport unrestricted -> satisfied
            (true, Some(_), None) => true,
            // present=true, found, transport must match
            (true, Some(entry), Some(t)) => entry.transport == t,
            // present=true, missing -> not satisfied
            (true, None, _) => false,
            // present=false, missing -> satisfied (absent or stale)
            (false, None, _) => true,
            // present=false, present and any transport -> not satisfied
            (false, Some(_), None) => false,
            // present=false, present but transport mismatch -> satisfied
            // (the *targeted* registration is absent)
            (false, Some(entry), Some(t)) => entry.transport != t,
        }
    };

    // Build the matched-app payload only on present:true satisfactions —
    // disappearance waits have no entry to surface.
    let pick_app =
        |live: &[crate::mcp::app_registry::RegisteredApp]| -> Option<RegisterAppResponse> {
            if !req.present {
                return None;
            }
            live.iter()
                .find(|e| e.app.app_id == req.app_id)
                .map(|e| RegisterAppResponse {
                    app: e.app.clone(),
                    transport: e.transport,
                })
        };

    loop {
        let live = registry.list_live().await;
        if evaluate(&live) {
            let app = pick_app(&live);
            return Ok(WaitForAppResponse {
                satisfied: true,
                elapsed_ms: start.elapsed().as_millis() as u64,
                timed_out: None,
                app,
            });
        }

        // Outer-deadline check before sleeping so we don't overshoot the
        // budget by ~poll_ms on the last iteration.
        let now = std::time::Instant::now();
        if now + Duration::from_millis(req.poll_ms) > deadline {
            // One last evaluation right at the boundary, in case the
            // registry changed during the read above.
            tokio::time::sleep(deadline.saturating_duration_since(now)).await;
            let live = registry.list_live().await;
            if evaluate(&live) {
                let app = pick_app(&live);
                return Ok(WaitForAppResponse {
                    satisfied: true,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    timed_out: None,
                    app,
                });
            }
            return Ok(WaitForAppResponse {
                satisfied: false,
                elapsed_ms: start.elapsed().as_millis() as u64,
                timed_out: Some(true),
                app: None,
            });
        }

        tokio::time::sleep(Duration::from_millis(req.poll_ms)).await;
    }
}

// ============================================================================
// POST /ui-bridge/apps/:appId/dispatch
// ============================================================================

/// Body for `POST /ui-bridge/apps/:appId/dispatch` — transport-agnostic
/// dispatch entry point for the wrapper framework.
#[derive(Debug, Deserialize)]
pub struct AppDispatchRequest {
    /// Action name. For WebSocket-transport apps this is the `action` field
    /// of the outbound command frame; for HTTP-transport apps it is sent
    /// verbatim in the request body alongside `params`.
    pub action: String,
    /// Caller-supplied payload. For WS apps this becomes the frame's
    /// `payload`; for HTTP apps it is wrapped in `{action, params}` in the
    /// POST body so the wrapper sees the same envelope on both transports.
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Build the dispatch payload for the transport-agnostic endpoint.
///
/// Extracted from `dispatch_to_app` so unit tests can verify the
/// payload shape without spinning up a full `ApiState`. WebSocket-transport
/// apps receive the user's `params` verbatim (the wrapper SDK's relay
/// already forwards `action` separately in the command frame); HTTP apps
/// receive an `{action, params}` envelope so the wrapper sees the same
/// shape on both transports.
fn build_dispatch_payload(
    transport: crate::mcp::app_registry::AppTransport,
    action: &str,
    params: &serde_json::Value,
) -> serde_json::Value {
    match transport {
        crate::mcp::app_registry::AppTransport::Websocket => params.clone(),
        crate::mcp::app_registry::AppTransport::Http => serde_json::json!({
            "action": action,
            "params": params,
        }),
    }
}

/// Core dispatch path for `POST /ui-bridge/apps/:appId/dispatch`. Pulled out
/// of the axum handler so unit tests can drive it with a hand-built
/// `(AppRegistry, AppDispatcher)` pair instead of the full `ApiState`.
async fn dispatch_to_app_inner(
    registry: &crate::mcp::app_registry::AppRegistry,
    dispatcher: &crate::mcp::app_dispatch::AppDispatcher,
    app_id: &str,
    req: &AppDispatchRequest,
) -> (StatusCode, ApiResponse<serde_json::Value>) {
    let entry = match registry.get(app_id).await {
        Some(e) => e,
        None => {
            return (
                StatusCode::NOT_FOUND,
                ApiResponse::error("app not registered"),
            );
        }
    };

    let payload = build_dispatch_payload(entry.transport, &req.action, &req.params);

    match dispatcher
        .dispatch(app_id, &req.action, Method::POST, "/dispatch", payload)
        .await
    {
        Ok(data) => (StatusCode::OK, ApiResponse::success(data)),
        Err(DispatchError::NotRegistered(_)) => (
            StatusCode::NOT_FOUND,
            ApiResponse::error("app not registered"),
        ),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            ApiResponse::error(e.to_user_message()),
        ),
    }
}

/// `POST /ui-bridge/apps/:appId/dispatch` — transport-agnostic dispatch.
///
/// Looks up `app_id` in `AppRegistry` and routes through `AppDispatcher`,
/// which picks WebSocket or HTTP based on the registered transport. The
/// HTTP arm targets the app's `<base_url><base_path>/dispatch` endpoint
/// with body `{action, params}` — wrappers expose the same envelope on
/// both transports so the runner does not have to special-case either
/// side.
///
/// Returns the dispatched call's response wrapped in `ApiResponse::success`.
/// Failures map to:
///   - `404` when `app_id` is not in the registry,
///   - `502` for transport-level errors (HTTP non-2xx, WS disconnect, etc.).
async fn dispatch_to_app(
    State(state): State<Arc<ApiState>>,
    Path(app_id): Path<String>,
    Json(req): Json<AppDispatchRequest>,
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    let (status, body) =
        dispatch_to_app_inner(&state.app_registry, &state.app_dispatcher, &app_id, &req).await;
    (status, Json(body))
}

/// Poll the app registry until `appId` reaches the desired presence/transport
/// state, or `timeoutMs` elapses. Always returns 200; callers branch on
/// `satisfied` / `timedOut` in the body. Validation failures return 400.
async fn wait_for_app(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<WaitForAppRequest>,
) -> Result<Json<ApiResponse<WaitForAppResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    match wait_for_app_inner(&state.app_registry, &req).await {
        Ok(resp) => Ok(Json(ApiResponse::success(resp))),
        Err(msg) => Err((StatusCode::BAD_REQUEST, Json(ApiResponse::error(msg)))),
    }
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
        .route("/ui-bridge/apps/{app_id}/dispatch", post(dispatch_to_app))
        .route("/ui-bridge/apps/forward-device", post(forward_device))
        .route("/ui-bridge/ios/forward", post(ios_forward))
        .route("/ui-bridge/control/wait-for-app", post(wait_for_app))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::app_registry::{AppRegistry, AppTransport};

    fn sample_app(app_id: &str) -> DiscoveredApp {
        DiscoveredApp {
            app_id: app_id.to_string(),
            app_name: "Sample".to_string(),
            app_type: "web".to_string(),
            framework: None,
            url: "http://127.0.0.1:3000".to_string(),
            port: 3000,
            base_path: "".to_string(),
            version: None,
            capabilities: vec![],
            element_count: None,
            component_count: None,
            discovered_at: 0,
        }
    }

    // ------------------------------------------------------------------
    // keep_alive_secs validation (Item A — register-side path)
    // ------------------------------------------------------------------

    #[test]
    fn validate_keep_alive_none_passes() {
        assert_eq!(validate_keep_alive_secs(None).unwrap(), None);
    }

    #[test]
    fn validate_keep_alive_60s_converts_to_60000ms() {
        assert_eq!(
            validate_keep_alive_secs(Some(60)).unwrap(),
            Some(60_000_i64)
        );
    }

    #[test]
    fn validate_keep_alive_at_cap_passes() {
        let cap_secs = (MAX_KEEP_ALIVE_MS / 1_000) as u64;
        assert_eq!(
            validate_keep_alive_secs(Some(cap_secs)).unwrap(),
            Some(MAX_KEEP_ALIVE_MS)
        );
    }

    #[test]
    fn validate_keep_alive_above_cap_rejects() {
        let cap_secs = (MAX_KEEP_ALIVE_MS / 1_000) as u64;
        let err = validate_keep_alive_secs(Some(cap_secs + 1)).unwrap_err();
        assert!(err.contains("exceeds cap"), "unexpected error: {}", err);
    }

    /// Item A end-to-end through the registry: simulates what `register_app`
    /// does with a `keep_alive_secs: 60` payload and confirms the resulting
    /// entry honors the override (entry survives past the global 30s TTL).
    #[tokio::test]
    async fn register_keep_alive_path_honors_override() {
        // Mirror the conversion register_app performs.
        let keep_alive_ms = validate_keep_alive_secs(Some(60))
            .expect("60s must be under the cap")
            .expect("Some input must yield Some output");
        assert_eq!(keep_alive_ms, 60_000);

        let registry = AppRegistry::new();
        registry
            .upsert(
                sample_app("registered-with-keep-alive"),
                Some("http://127.0.0.1:9999".to_string()),
                AppTransport::Http,
                None,
                Some(keep_alive_ms),
            )
            .await;

        // Backdate past the global TTL but inside the 60s override.
        let aged = registry
            .test_age_entry(
                "registered-with-keep-alive",
                crate::mcp::app_registry::REGISTRATION_TTL_MS + 5_000,
            )
            .await;
        assert!(aged, "test_age_entry should find the just-inserted entry");

        let live = registry.list_live().await;
        assert_eq!(live.len(), 1, "60s override must keep entry live");
        assert_eq!(live[0].keep_alive_ms, Some(60_000));
    }

    // ------------------------------------------------------------------
    // wait_for_app handler (Item B)
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn wait_for_app_resolves_immediately_when_present() {
        let registry = AppRegistry::new();
        registry
            .upsert(
                sample_app("already-here"),
                None,
                AppTransport::Http,
                None,
                None,
            )
            .await;

        let req = WaitForAppRequest {
            app_id: "already-here".to_string(),
            transport: None,
            present: true,
            timeout_ms: 5_000,
            poll_ms: 100,
        };
        let resp = wait_for_app_inner(&registry, &req).await.unwrap();
        assert!(resp.satisfied, "registered app must satisfy present=true");
        assert!(
            resp.elapsed_ms < 200,
            "first-poll resolution should be near-instant (got {}ms)",
            resp.elapsed_ms
        );
        assert_eq!(resp.timed_out, None);
    }

    #[tokio::test]
    async fn wait_for_app_filters_by_transport() {
        let registry = AppRegistry::new();
        registry
            .upsert(
                sample_app("http-only"),
                None,
                AppTransport::Http,
                None,
                None,
            )
            .await;

        // Wait for websocket-transport — should time out because the entry
        // is HTTP, not websocket.
        let req = WaitForAppRequest {
            app_id: "http-only".to_string(),
            transport: Some(AppTransport::Websocket),
            present: true,
            timeout_ms: 250,
            poll_ms: 50,
        };
        let resp = wait_for_app_inner(&registry, &req).await.unwrap();
        assert!(!resp.satisfied);
        assert_eq!(resp.timed_out, Some(true));
    }

    #[tokio::test]
    async fn wait_for_app_times_out_for_unregistered_app() {
        let registry = AppRegistry::new();
        let req = WaitForAppRequest {
            app_id: "nope-not-here".to_string(),
            transport: None,
            present: true,
            timeout_ms: 200,
            poll_ms: 50,
        };
        let resp = wait_for_app_inner(&registry, &req).await.unwrap();
        assert!(!resp.satisfied);
        assert_eq!(resp.timed_out, Some(true));
        // Should respect the requested timeout — within poll-budget margin.
        assert!(
            resp.elapsed_ms >= 200 && resp.elapsed_ms < 500,
            "elapsed {}ms should be near 200ms timeout",
            resp.elapsed_ms
        );
    }

    #[tokio::test]
    async fn wait_for_app_present_false_resolves_when_absent() {
        let registry = AppRegistry::new();
        // Register, then remove — ensures the registry sees absence.
        registry
            .upsert(
                sample_app("transient"),
                None,
                AppTransport::Http,
                None,
                None,
            )
            .await;
        registry.remove("transient").await;

        let req = WaitForAppRequest {
            app_id: "transient".to_string(),
            transport: None,
            present: false,
            timeout_ms: 1_000,
            poll_ms: 50,
        };
        let resp = wait_for_app_inner(&registry, &req).await.unwrap();
        assert!(resp.satisfied);
        assert_eq!(resp.timed_out, None);
    }

    #[tokio::test]
    async fn wait_for_app_resolves_when_app_appears_during_poll() {
        let registry = std::sync::Arc::new(AppRegistry::new());

        // Spawn a task that registers the app after a 150ms delay.
        let reg_clone = registry.clone();
        let registrar = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            reg_clone
                .upsert(
                    sample_app("late-arrival"),
                    None,
                    AppTransport::Websocket,
                    Some(7),
                    None,
                )
                .await;
        });

        let req = WaitForAppRequest {
            app_id: "late-arrival".to_string(),
            transport: Some(AppTransport::Websocket),
            present: true,
            timeout_ms: 2_000,
            poll_ms: 50,
        };
        let resp = wait_for_app_inner(&registry, &req).await.unwrap();
        assert!(resp.satisfied, "should pick up the late registration");
        assert_eq!(resp.timed_out, None);
        // Should resolve well before the 2s timeout.
        assert!(resp.elapsed_ms >= 100 && resp.elapsed_ms < 1_500);
        registrar.await.unwrap();
    }

    // ------------------------------------------------------------------
    // Validation rejects (handler-level via inner)
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn wait_for_app_rejects_zero_timeout() {
        let registry = AppRegistry::new();
        let req = WaitForAppRequest {
            app_id: "x".to_string(),
            transport: None,
            present: true,
            timeout_ms: 0,
            poll_ms: 100,
        };
        let err = wait_for_app_inner(&registry, &req).await.unwrap_err();
        assert!(err.contains("timeoutMs"), "unexpected error: {}", err);
    }

    #[tokio::test]
    async fn wait_for_app_rejects_excessive_timeout() {
        let registry = AppRegistry::new();
        let req = WaitForAppRequest {
            app_id: "x".to_string(),
            transport: None,
            present: true,
            timeout_ms: WAIT_FOR_APP_MAX_TIMEOUT_MS + 1,
            poll_ms: 100,
        };
        let err = wait_for_app_inner(&registry, &req).await.unwrap_err();
        assert!(err.contains("timeoutMs"), "unexpected error: {}", err);
    }

    #[tokio::test]
    async fn wait_for_app_rejects_poll_too_low() {
        let registry = AppRegistry::new();
        let req = WaitForAppRequest {
            app_id: "x".to_string(),
            transport: None,
            present: true,
            timeout_ms: 1_000,
            poll_ms: WAIT_FOR_APP_MIN_POLL_MS - 1,
        };
        let err = wait_for_app_inner(&registry, &req).await.unwrap_err();
        assert!(err.contains("pollMs"), "unexpected error: {}", err);
    }

    #[tokio::test]
    async fn wait_for_app_rejects_empty_app_id() {
        let registry = AppRegistry::new();
        let req = WaitForAppRequest {
            app_id: String::new(),
            transport: None,
            present: true,
            timeout_ms: 1_000,
            poll_ms: 100,
        };
        let err = wait_for_app_inner(&registry, &req).await.unwrap_err();
        assert!(err.contains("appId"), "unexpected error: {}", err);
    }

    // ------------------------------------------------------------------
    // POST /ui-bridge/apps/:appId/dispatch (transport-agnostic dispatch)
    // ------------------------------------------------------------------

    #[test]
    fn build_dispatch_payload_websocket_returns_params_verbatim() {
        let params = serde_json::json!({"x": 1, "y": "two"});
        let payload = build_dispatch_payload(AppTransport::Websocket, "doThing", &params);
        assert_eq!(
            payload, params,
            "WS-transport apps must receive the user's params verbatim"
        );
    }

    #[test]
    fn build_dispatch_payload_http_wraps_action_and_params() {
        let params = serde_json::json!({"x": 1});
        let payload = build_dispatch_payload(AppTransport::Http, "doThing", &params);
        assert_eq!(payload["action"], "doThing");
        assert_eq!(payload["params"]["x"], 1);
        // The HTTP envelope must have *exactly* the two keys.
        let obj = payload.as_object().expect("HTTP payload must be an object");
        assert_eq!(obj.len(), 2, "HTTP envelope must be {{action, params}}");
    }

    #[tokio::test]
    async fn dispatch_to_app_inner_returns_404_for_unregistered_app() {
        use crate::mcp::app_dispatch::AppDispatcher;
        use crate::mcp::command_relay::CommandRelay;
        use crate::mcp::ws_relay::WsConnectionManager;

        let registry = AppRegistry::new();
        let ws = WsConnectionManager::new();
        let relay = CommandRelay::new(ws);
        let dispatcher = AppDispatcher::new(registry.clone(), relay);

        let req = AppDispatchRequest {
            action: "anything".into(),
            params: serde_json::json!({}),
        };
        let (status, body) = dispatch_to_app_inner(&registry, &dispatcher, "ghost-app", &req).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body.success, false);
        assert_eq!(body.error.as_deref(), Some("app not registered"));
    }

    #[tokio::test]
    async fn dispatch_to_app_inner_routes_to_websocket_arm() {
        use crate::mcp::app_dispatch::AppDispatcher;
        use crate::mcp::command_relay::{CommandRelay, CommandResponse};
        use crate::mcp::ws_relay::WsConnectionManager;
        use std::time::Duration;

        let registry = AppRegistry::new();
        let ws = WsConnectionManager::new();
        let (_conn_id, mut outbound_rx) = ws.test_register("wapp").await;
        registry
            .upsert(
                sample_app("wapp"),
                None,
                AppTransport::Websocket,
                Some(1),
                None,
            )
            .await;
        let relay = CommandRelay::with_timeout(ws.clone(), Duration::from_secs(2));
        let dispatcher = AppDispatcher::new(registry.clone(), relay.clone());

        let req = AppDispatchRequest {
            action: "doThing".into(),
            params: serde_json::json!({"x": 1}),
        };

        // Drive the dispatch on a separate task so we can play the role of
        // the wrapper and pull the outbound frame off the channel.
        let registry_for_task = registry.clone();
        let dispatcher_for_task = dispatcher.clone();
        let handle = tokio::spawn(async move {
            dispatch_to_app_inner(&registry_for_task, &dispatcher_for_task, "wapp", &req).await
        });

        let frame = outbound_rx
            .recv()
            .await
            .expect("relay must send an outbound frame");
        let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
        let command_id = v["commandId"].as_str().unwrap().to_string();
        // WS-transport: action goes in the frame's `action` field, the
        // user's `params` go verbatim into `payload` (NOT wrapped).
        assert_eq!(v["action"], "doThing");
        assert_eq!(v["payload"], serde_json::json!({"x": 1}));

        relay
            .resolve(CommandResponse {
                command_id,
                success: true,
                result: Some(serde_json::json!({"ok": true})),
                error: None,
            })
            .await;

        let (status, body) = handle.await.unwrap();
        assert_eq!(status, StatusCode::OK);
        assert!(body.success);
        assert_eq!(body.data, Some(serde_json::json!({"ok": true})));
    }

    #[tokio::test]
    async fn dispatch_to_app_inner_routes_to_http_arm() {
        use crate::mcp::app_dispatch::AppDispatcher;
        use crate::mcp::command_relay::CommandRelay;
        use crate::mcp::ws_relay::WsConnectionManager;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Spin up a tiny in-process HTTP server that records the inbound body
        // and replies with a canned JSON response. axum's `serve` is more
        // ergonomic than reqwest's mock APIs and avoids pulling extra
        // dev-dependencies.
        let captured_body = Arc::new(tokio::sync::Mutex::new(serde_json::Value::Null));
        let captured_path = Arc::new(tokio::sync::Mutex::new(String::new()));
        let hits = Arc::new(AtomicUsize::new(0));

        let body_for_handler = captured_body.clone();
        let path_for_handler = captured_path.clone();
        let hits_for_handler = hits.clone();
        let app = Router::new().route(
            "/dispatch",
            post(move |req: axum::extract::Request<axum::body::Body>| {
                let body_slot = body_for_handler.clone();
                let path_slot = path_for_handler.clone();
                let hits = hits_for_handler.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    *path_slot.lock().await = req.uri().path().to_string();
                    let bytes = axum::body::to_bytes(req.into_body(), 64 * 1024)
                        .await
                        .unwrap_or_default();
                    let parsed: serde_json::Value =
                        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
                    *body_slot.lock().await = parsed;
                    Json(serde_json::json!({"echoed": true}))
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server_port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Register the test server as an HTTP-transport app.
        let registry = AppRegistry::new();
        let mut http_app = sample_app("happ");
        http_app.url = format!("http://127.0.0.1:{}", server_port);
        http_app.port = server_port;
        http_app.base_path = String::new();
        registry
            .upsert(http_app, None, AppTransport::Http, None, None)
            .await;
        let ws = WsConnectionManager::new();
        let relay = CommandRelay::new(ws);
        let dispatcher = AppDispatcher::new(registry.clone(), relay);

        let req = AppDispatchRequest {
            action: "doThing".into(),
            params: serde_json::json!({"alpha": 42}),
        };
        let (status, body) = dispatch_to_app_inner(&registry, &dispatcher, "happ", &req).await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.success);
        assert_eq!(body.data, Some(serde_json::json!({"echoed": true})));

        // Verify the dispatcher hit `/dispatch` with the {action, params} envelope.
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        let observed_path = captured_path.lock().await.clone();
        assert_eq!(observed_path, "/dispatch");
        let observed_body = captured_body.lock().await.clone();
        assert_eq!(observed_body["action"], "doThing");
        assert_eq!(observed_body["params"]["alpha"], 42);
    }
}
