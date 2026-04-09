//! Streaming API for Sunshine/Moonlight integration
//!
//! Provides endpoints for:
//! - Sunshine health check (detect if Sunshine is running)
//! - Quick screenshot capture (native xcap, JPEG response)
//! - Pairing relay (generate Sunshine PIN for Moonlight pairing)
//! - Focus control (configure Sunshine to stream specific window or desktop)

use axum::{
    body::Body,
    extract::{Query, State as AxumState},
    http::{header, StatusCode},
    response::{IntoResponse, Json, Response},
};
use image::codecs::jpeg::JpegEncoder;
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::mcp::types::ApiState;

/// Default Sunshine admin API port
const SUNSHINE_API_PORT: u16 = 47990;
/// Default Sunshine streaming port (for Moonlight connection)
const SUNSHINE_STREAM_PORT: u16 = 47989;

// ── Response Types ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct StreamingStatus {
    pub sunshine_available: bool,
    pub sunshine_url: Option<String>,
    pub sunshine_version: Option<String>,
    pub encoder: Option<String>,
    pub streams_active: u32,
    pub host_ip: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PairResponse {
    pub success: bool,
    pub pin: Option<String>,
    pub sunshine_host: Option<String>,
    pub sunshine_port: u16,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FocusRequest {
    /// "runner" to stream the runner's Tauri window, "desktop" for full desktop
    pub target: String,
    /// Optional window title substring for targeting a specific window
    pub window_title: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FocusResponse {
    pub success: bool,
    pub target: String,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct ScreenshotQuery {
    /// JPEG quality (1-100, default 80)
    pub quality: Option<u8>,
    /// Monitor index (0-based, default primary)
    pub monitor: Option<usize>,
    /// If true, capture a specific window by title substring
    pub window_title: Option<String>,
}

// ── Sunshine API Client Helpers ─────────────────────────────────────────────

/// Get the local machine's LAN IP address for Moonlight to connect to.
fn get_local_ip() -> Option<String> {
    // Use a UDP socket trick to find the default outbound IP
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let addr = socket.local_addr().ok()?;
    Some(addr.ip().to_string())
}

/// Build the Sunshine admin API base URL.
fn sunshine_base_url() -> String {
    let port = std::env::var("SUNSHINE_API_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(SUNSHINE_API_PORT);
    format!("https://localhost:{}", port)
}

/// Check if Sunshine is running and return client info.
async fn check_sunshine() -> Option<(String, Option<String>)> {
    let url = format!("{}/api/currentclients", sunshine_base_url());

    // Sunshine uses a self-signed cert, so we must accept invalid certs
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()?;

    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let version = resp
                .headers()
                .get("x-sunshine-version")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.to_string());
            Some((sunshine_base_url(), version))
        }
        Ok(resp) => {
            warn!(
                status = %resp.status(),
                "Sunshine API returned non-success status"
            );
            None
        }
        Err(e) => {
            // Connection refused = Sunshine not running, which is expected
            if !e.is_connect() {
                warn!(error = %e, "Sunshine health check failed");
            }
            None
        }
    }
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// GET /api/v1/streaming/status
///
/// Check if Sunshine is running and return streaming capability info.
pub async fn get_streaming_status(
    AxumState(_state): AxumState<Arc<ApiState>>,
) -> Json<StreamingStatus> {
    let sunshine = check_sunshine().await;
    let host_ip = get_local_ip();

    match sunshine {
        Some((url, version)) => {
            info!("Sunshine detected at {}", url);
            Json(StreamingStatus {
                sunshine_available: true,
                sunshine_url: Some(url),
                sunshine_version: version,
                encoder: Some("nvenc".to_string()),
                streams_active: 0,
                host_ip,
            })
        }
        None => Json(StreamingStatus {
            sunshine_available: false,
            sunshine_url: None,
            sunshine_version: None,
            encoder: None,
            streams_active: 0,
            host_ip,
        }),
    }
}

/// GET /api/v1/streaming/screenshot
///
/// Capture a screenshot and return it as JPEG binary.
/// Uses xcap for native screen capture — no Python dependency.
pub async fn get_screenshot(Query(query): Query<ScreenshotQuery>) -> impl IntoResponse {
    let quality = query.quality.unwrap_or(80).clamp(1, 100);
    let monitor_index = query.monitor;
    let window_title = query.window_title.clone();

    // xcap capture must run on a blocking thread (Windows COM/GDI)
    let result = tokio::task::spawn_blocking(move || {
        capture_screenshot(monitor_index, window_title.as_deref(), quality)
    })
    .await;

    match result {
        Ok(Ok(jpeg_bytes)) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/jpeg")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from(jpeg_bytes))
            .unwrap_or_else(|_| {
                Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::from("Failed to build response"))
                    .unwrap()
            }),
        Ok(Err(e)) => {
            error!(error = %e, "Screenshot capture failed");
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"error": e}).to_string(),
                ))
                .unwrap()
        }
        Err(e) => {
            error!(error = %e, "Screenshot task panicked");
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"error": "Screenshot capture panicked"}).to_string(),
                ))
                .unwrap()
        }
    }
}

/// Capture a screenshot using xcap and encode as JPEG.
fn capture_screenshot(
    monitor_index: Option<usize>,
    window_title: Option<&str>,
    quality: u8,
) -> Result<Vec<u8>, String> {
    // If a window title is specified, try to capture that window
    if let Some(title) = window_title {
        use xcap::Window;
        let windows = Window::all().map_err(|e| format!("Failed to enumerate windows: {}", e))?;
        let title_lower = title.to_lowercase();
        let target = windows.iter().find(|w| {
            w.title()
                .unwrap_or_default()
                .to_lowercase()
                .contains(&title_lower)
        });

        if let Some(win) = target {
            let image = win
                .capture_image()
                .map_err(|e| format!("Failed to capture window '{}': {}", title, e))?;
            return encode_rgba_to_jpeg(&image, quality);
        } else {
            return Err(format!("No window found matching title '{}'", title));
        }
    }

    // Otherwise capture a monitor
    use xcap::Monitor;
    let monitors = Monitor::all().map_err(|e| format!("Failed to enumerate monitors: {}", e))?;

    if monitors.is_empty() {
        return Err("No monitors found".to_string());
    }

    let monitor = if let Some(idx) = monitor_index {
        monitors
            .get(idx)
            .ok_or_else(|| format!("Monitor index {} out of range ({})", idx, monitors.len()))?
    } else {
        // Find primary monitor, fall back to first
        monitors
            .iter()
            .find(|m| m.is_primary().unwrap_or(false))
            .unwrap_or(&monitors[0])
    };

    let image = monitor
        .capture_image()
        .map_err(|e| format!("Failed to capture monitor: {}", e))?;

    encode_rgba_to_jpeg(&image, quality)
}

/// Encode an RGBA image buffer to JPEG bytes.
fn encode_rgba_to_jpeg(image: &image::RgbaImage, quality: u8) -> Result<Vec<u8>, String> {
    // Convert RGBA to RGB via DynamicImage (JPEG doesn't support alpha)
    let dynamic = image::DynamicImage::ImageRgba8(image.clone());
    let mut buf = Cursor::new(Vec::new());
    let encoder = JpegEncoder::new_with_quality(&mut buf, quality);
    dynamic
        .write_with_encoder(encoder)
        .map_err(|e| format!("JPEG encoding failed: {}", e))?;
    Ok(buf.into_inner())
}

/// POST /api/v1/streaming/pair
///
/// Request a pairing PIN from Sunshine for Moonlight to use.
/// The runner relays the PIN request to Sunshine's local admin API.
pub async fn post_pair(
    AxumState(_state): AxumState<Arc<ApiState>>,
) -> Json<PairResponse> {
    let host_ip = get_local_ip();

    let client = match reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return Json(PairResponse {
                success: false,
                pin: None,
                sunshine_host: host_ip,
                sunshine_port: SUNSHINE_STREAM_PORT,
                error: Some(format!("Failed to create HTTP client: {}", e)),
            });
        }
    };

    let url = format!("{}/api/pin", sunshine_base_url());
    info!("Requesting Sunshine pairing PIN from {}", url);

    match client.post(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            // Sunshine returns the PIN in the response body
            match resp.json::<serde_json::Value>().await {
                Ok(body) => {
                    let pin = body
                        .get("pin")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| {
                            // Some Sunshine versions return just the PIN as a string
                            body.as_str().map(|s| s.to_string())
                        });
                    Json(PairResponse {
                        success: pin.is_some(),
                        pin,
                        sunshine_host: host_ip,
                        sunshine_port: SUNSHINE_STREAM_PORT,
                        error: None,
                    })
                }
                Err(e) => Json(PairResponse {
                    success: false,
                    pin: None,
                    sunshine_host: host_ip,
                    sunshine_port: SUNSHINE_STREAM_PORT,
                    error: Some(format!("Failed to parse Sunshine response: {}", e)),
                }),
            }
        }
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Json(PairResponse {
                success: false,
                pin: None,
                sunshine_host: host_ip,
                sunshine_port: SUNSHINE_STREAM_PORT,
                error: Some(format!("Sunshine returned {}: {}", status, body)),
            })
        }
        Err(e) => {
            let msg = if e.is_connect() {
                "Sunshine is not running or not reachable".to_string()
            } else {
                format!("Failed to contact Sunshine: {}", e)
            };
            Json(PairResponse {
                success: false,
                pin: None,
                sunshine_host: host_ip,
                sunshine_port: SUNSHINE_STREAM_PORT,
                error: Some(msg),
            })
        }
    }
}

/// POST /api/v1/streaming/focus
///
/// Configure Sunshine to stream a specific target (runner window or full desktop).
/// This uses Sunshine's application configuration to set the capture target.
pub async fn post_focus(
    AxumState(state): AxumState<Arc<ApiState>>,
    Json(request): Json<FocusRequest>,
) -> Json<FocusResponse> {
    match request.target.as_str() {
        "runner" => {
            // Get the runner's window handle to tell Sunshine what to capture
            use tauri::Manager;
            let window = state.app_handle.get_webview_window(qontinui_runner_lib::get_main_window_label());
            match window {
                Some(win) => {
                    let title = win
                        .title()
                        .unwrap_or_else(|_| "Qontinui Runner".to_string());
                    info!(title = %title, "Setting Sunshine focus to runner window");

                    // Configure Sunshine via its API to capture this specific app
                    match configure_sunshine_app(&title).await {
                        Ok(_) => Json(FocusResponse {
                            success: true,
                            target: "runner".to_string(),
                            message: format!("Sunshine focused on runner window: {}", title),
                        }),
                        Err(e) => Json(FocusResponse {
                            success: false,
                            target: "runner".to_string(),
                            message: format!("Failed to configure Sunshine: {}", e),
                        }),
                    }
                }
                None => Json(FocusResponse {
                    success: false,
                    target: "runner".to_string(),
                    message: "Runner window not found".to_string(),
                }),
            }
        }
        "window" => {
            let title = request.window_title.as_deref().unwrap_or("");
            if title.is_empty() {
                return Json(FocusResponse {
                    success: false,
                    target: "window".to_string(),
                    message: "window_title is required for target 'window'".to_string(),
                });
            }
            info!(title = %title, "Setting Sunshine focus to specific window");
            match configure_sunshine_app(title).await {
                Ok(_) => Json(FocusResponse {
                    success: true,
                    target: "window".to_string(),
                    message: format!("Sunshine focused on window: {}", title),
                }),
                Err(e) => Json(FocusResponse {
                    success: false,
                    target: "window".to_string(),
                    message: format!("Failed to configure Sunshine: {}", e),
                }),
            }
        }
        "desktop" => {
            info!("Setting Sunshine focus to full desktop");
            match configure_sunshine_desktop().await {
                Ok(_) => Json(FocusResponse {
                    success: true,
                    target: "desktop".to_string(),
                    message: "Sunshine focused on full desktop".to_string(),
                }),
                Err(e) => Json(FocusResponse {
                    success: false,
                    target: "desktop".to_string(),
                    message: format!("Failed to configure Sunshine: {}", e),
                }),
            }
        }
        other => Json(FocusResponse {
            success: false,
            target: other.to_string(),
            message: format!(
                "Unknown target '{}'. Use 'runner', 'window', or 'desktop'.",
                other
            ),
        }),
    }
}

/// Configure Sunshine to capture a specific application by window title.
async fn configure_sunshine_app(window_title: &str) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    // Sunshine's app configuration endpoint
    let url = format!("{}/api/apps", sunshine_base_url());

    let body = serde_json::json!({
        "name": format!("Qontinui: {}", window_title),
        "output": "",
        "cmd": "",
        "index": -1,
        "exclude-global-prep-cmd": false,
        "elevated": false,
        "auto-detach": true,
        "wait-all": true,
        "exit-timeout": 5,
        "prep-cmd": [],
        "detached": [],
        "image-path": "",
        "app-name": window_title,
    });

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                "Sunshine is not running".to_string()
            } else {
                format!("Failed to contact Sunshine: {}", e)
            }
        })?;

    if resp.status().is_success() {
        info!("Configured Sunshine app capture for '{}'", window_title);
        Ok(())
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(format!("Sunshine returned {}: {}", status, body))
    }
}

/// Configure Sunshine to capture the full desktop (remove app-specific capture).
async fn configure_sunshine_desktop() -> Result<(), String> {
    // When no specific app is configured, Sunshine defaults to desktop capture.
    // We can also explicitly set it via the apps API with an empty app name.
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let url = format!("{}/api/apps/close", sunshine_base_url());

    let resp = client
        .post(&url)
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                "Sunshine is not running".to_string()
            } else {
                format!("Failed to contact Sunshine: {}", e)
            }
        })?;

    if resp.status().is_success() {
        info!("Configured Sunshine for full desktop capture");
        Ok(())
    } else {
        // Non-success is acceptable — may mean no app was running
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        warn!(
            "Sunshine close-app returned {}: {}",
            status, body
        );
        Ok(())
    }
}

// ── Routes ──────────────────────────────────────────────────────────────────

/// Create routes for the streaming module.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/api/v1/streaming/status", get(get_streaming_status))
        .route("/api/v1/streaming/screenshot", get(get_screenshot))
        .route("/api/v1/streaming/pair", post(post_pair))
        .route("/api/v1/streaming/focus", post(post_focus))
}
