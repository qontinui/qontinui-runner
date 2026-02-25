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
    extract::State,
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
    pub url: String,
    pub port: u16,
    #[serde(default)]
    pub base_path: String,
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
    9876, // Qontinui Runner
    9877, 9878, // Runner fallback ports
    8888, // Electron common
    3333, // Desktop misc
];

const HEALTH_PATHS: &[(&str, &str)] = &[
    ("/api/ui-bridge/health", "/api/ui-bridge"),
    ("/ui-bridge/health", "/ui-bridge"),
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

/// List connected ADB devices
async fn list_adb_devices() -> Vec<MobileDevice> {
    let adb_path = match find_adb() {
        Some(p) => p,
        None => return Vec::new(),
    };

    let output = match tokio::process::Command::new(&adb_path)
        .args(["devices", "-l"])
        .output()
        .await
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return Vec::new(),
    };

    let mut devices = Vec::new();
    for line in output.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }

        let device_id = parts[0].to_string();
        let state = parts[1].to_string();

        // Determine device type
        let device_type = if device_id.starts_with("emulator-") {
            "emulator"
        } else {
            "device"
        }
        .to_string();

        // Extract model from properties
        let model = parts
            .iter()
            .find(|p| p.starts_with("model:"))
            .map(|p| p.trim_start_matches("model:").to_string());

        let status = match state.as_str() {
            "device" => "online",
            "offline" => "offline",
            "unauthorized" => "unauthorized",
            _ => "offline",
        }
        .to_string();

        devices.push(MobileDevice {
            device_id,
            device_type,
            model,
            status,
            ui_bridge: None,
        });
    }

    devices
}

/// Check if a mobile device has UI Bridge by port forwarding
async fn check_device_ui_bridge(device_id: &str) -> Option<DiscoveredApp> {
    let adb_path = find_adb()?;
    let client = build_scan_client()?;

    // Forward a local port to the device's UI Bridge port
    // Use tcp:0 to let the OS pick a free local port
    let output = tokio::process::Command::new(&adb_path)
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
    let _ = tokio::process::Command::new(&adb_path)
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
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<DiscoveryResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    let start = std::time::Instant::now();

    // Run web and desktop scans in parallel
    let (web_apps, desktop_apps, mut mobile_devices) = tokio::join!(
        scan_ports(WEB_DEV_PORTS),
        scan_ports(DESKTOP_APP_PORTS),
        list_adb_devices()
    );

    // Check UI Bridge on each online mobile device
    for device in &mut mobile_devices {
        if device.status == "online" {
            device.ui_bridge = check_device_ui_bridge(&device.device_id).await;
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
    let apps = scan_ports(DESKTOP_APP_PORTS).await;
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
    let output = tokio::process::Command::new(&adb_path)
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

/// Manual app registration
async fn register_app(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<RegisterAppRequest>,
) -> Result<Json<ApiResponse<DiscoveredApp>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app = DiscoveredApp {
        app_id: req.app_id,
        app_name: req.app_name,
        app_type: req.app_type,
        framework: req.framework,
        url: req.url,
        port: req.port,
        base_path: req.base_path,
        version: None,
        capabilities: Vec::new(),
        element_count: None,
        component_count: None,
        discovered_at: chrono::Utc::now().timestamp_millis(),
    };
    Ok(Json(ApiResponse::success(app)))
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
        .route("/ui-bridge/apps/register", post(register_app))
        .route("/ui-bridge/apps/forward-device", post(forward_device))
}
