//! Screenshot capture and upload commands.
//!
//! This module provides commands for:
//! - Capturing screenshots from connected monitors
//! - Uploading screenshots directly to qontinui-web projects
//! - Getting monitor information for screenshot capture
//!
//! All screenshot capture uses IPC to the Python bridge (qontinui library).

use super::compartments::BridgeCompartment;
use super::CommandResponse;
use crate::auth::AuthManager;
use crate::error::AppError;
use crate::executor::with_default_bridge_compartment;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;
use tracing::{error, info};

use crate::api_config::get_api_base_url;

/// Monitor information.
/// Matches the Monitor type from qontinui-schemas/geometry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub index: i32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// Spatial position: "left", "center", or "right"
    pub position: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_primary: Option<bool>,
    /// DPI scale factor (1.0 = 100%, 1.5 = 150%, 2.0 = 200%)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale_factor: Option<f64>,
    /// Legacy scale field for backward compatibility
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Response from screenshot capture
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotCaptureResult {
    pub success: bool,
    pub screenshot_base64: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub monitor: Option<i32>,
    pub error: Option<String>,
}

/// Configuration for screenshot upload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotUploadConfig {
    pub project_id: String,
    pub name: Option<String>,
    pub monitor: Option<i32>,
}

/// Response from screenshot upload to web project
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotUploadResult {
    pub success: bool,
    pub screenshot_id: Option<String>,
    pub presigned_url: Option<String>,
    pub error: Option<String>,
}

/// Get available monitors for screenshot capture via IPC to Python bridge.
///
/// This calls the Python executor's get_monitors command to get
/// information about all connected displays at **physical resolution**.
///
/// Note: This is distinct from `get_monitors` in execution.rs, which uses
/// Tauri's window API for logical (DPI-scaled) coordinates. Use this command
/// when you need physical pixel coordinates for screenshot capture, and
/// `get_monitors` for workflow execution monitor selection.
#[tauri::command]
pub async fn get_screenshot_monitors(
    bridge: State<'_, BridgeCompartment>,
) -> Result<CommandResponse, String> {
    info!("Getting available monitors for screenshot capture via IPC");

    let bridge_clone = bridge.inner().clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge_compartment(&bridge_clone, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = Duration::from_secs(30);
            bridge.send_command_and_wait("get_monitors", None, timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        String::from(AppError::ProcessError(format!(
            "spawn_blocking error: {}",
            e
        )))
    })?;

    match result {
        Ok(response) => {
            if response.success {
                let data = response
                    .data
                    .unwrap_or(json!({"success": true, "monitors": [], "count": 0}));
                let count = data.get("count").and_then(|c| c.as_i64()).unwrap_or(0);
                info!("Retrieved {} monitors via IPC", count);
                Ok(CommandResponse {
                    success: true,
                    message: Some("Monitors retrieved".to_string()),
                    data: Some(data),
                })
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Get monitors failed".to_string());
                error!("Get monitors via IPC failed: {}", error_msg);
                Err(error_msg)
            }
        }
        Err(e) => {
            error!("Failed to get monitors via IPC: {}", e);
            Err(e)
        }
    }
}

/// Internal function to capture a screenshot via IPC.
///
/// This is the core implementation used by both the Tauri command and other
/// functions that need to capture screenshots.
async fn capture_screenshot_internal(
    bridge_compartment: BridgeCompartment,
    monitor: Option<i32>,
    delay_seconds: Option<f64>,
) -> Result<ScreenshotCaptureResult, String> {
    // Apply delay if specified (clamped to 0-30 seconds)
    if let Some(delay) = delay_seconds {
        if delay > 0.0 {
            let clamped_delay = delay.clamp(0.0, 30.0);
            info!("Waiting {}s before screenshot capture", clamped_delay);
            tokio::time::sleep(tokio::time::Duration::from_secs_f64(clamped_delay)).await;
        }
    }

    let params = json!({
        "monitor": monitor,
        "format": "png",
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge_compartment(&bridge_compartment, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = Duration::from_secs(30);
            bridge.send_command_and_wait("capture_screenshot", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        String::from(AppError::ProcessError(format!(
            "spawn_blocking error: {}",
            e
        )))
    })?;

    match result {
        Ok(response) => {
            if response.success {
                let data = response.data.unwrap_or(json!({}));

                let screenshot_base64 = data
                    .get("screenshot_base64")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());

                let width = data.get("width").and_then(|w| w.as_i64()).map(|w| w as i32);

                let height = data
                    .get("height")
                    .and_then(|h| h.as_i64())
                    .map(|h| h as i32);

                info!(
                    "Screenshot captured via IPC: {}x{} pixels",
                    width.unwrap_or(0),
                    height.unwrap_or(0)
                );

                Ok(ScreenshotCaptureResult {
                    success: true,
                    screenshot_base64,
                    width,
                    height,
                    monitor,
                    error: None,
                })
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Screenshot capture failed".to_string());
                error!("Screenshot capture via IPC failed: {}", error_msg);
                Ok(ScreenshotCaptureResult {
                    success: false,
                    screenshot_base64: None,
                    width: None,
                    height: None,
                    monitor,
                    error: Some(error_msg),
                })
            }
        }
        Err(e) => {
            error!("Failed to capture screenshot via IPC: {}", e);
            Ok(ScreenshotCaptureResult {
                success: false,
                screenshot_base64: None,
                width: None,
                height: None,
                monitor,
                error: Some(e),
            })
        }
    }
}

/// Capture a screenshot from the specified monitor via IPC to Python bridge.
///
/// This uses the qontinui library's HAL layer for capture, ensuring
/// screenshots are taken at physical pixel resolution.
///
/// # Arguments
/// * `monitor` - Monitor index (0-based), None for all monitors combined
/// * `delay_seconds` - Optional delay in seconds before capturing (0-30).
///   Useful for waiting for UI animations to settle.
/// * `state` - Application state containing the Python bridge
#[tauri::command]
pub async fn capture_screenshot(
    monitor: Option<i32>,
    delay_seconds: Option<f64>,
    bridge: State<'_, BridgeCompartment>,
) -> Result<ScreenshotCaptureResult, String> {
    info!(
        "Capturing screenshot from monitor: {:?}, delay: {:?}s via IPC",
        monitor, delay_seconds
    );

    capture_screenshot_internal(bridge.inner().clone(), monitor, delay_seconds).await
}

/// Capture a screenshot and upload it directly to a qontinui-web project.
///
/// This is the main function for the runner's "capture to web" feature.
/// It captures at physical resolution and uploads to the user's project.
///
/// # Arguments
/// * `config` - Upload configuration including project_id and optional name
/// * `state` - Application state containing the Python bridge
#[tauri::command]
pub async fn capture_and_upload_screenshot(
    config: ScreenshotUploadConfig,
    bridge: State<'_, BridgeCompartment>,
) -> Result<ScreenshotUploadResult, String> {
    info!(
        "Capturing and uploading screenshot to project: {}",
        config.project_id
    );

    // 1. Capture the screenshot via IPC (no delay for upload workflow)
    let capture_result =
        capture_screenshot_internal(bridge.inner().clone(), config.monitor, None).await?;

    if !capture_result.success || capture_result.screenshot_base64.is_none() {
        return Ok(ScreenshotUploadResult {
            success: false,
            screenshot_id: None,
            presigned_url: None,
            error: capture_result
                .error
                .or_else(|| Some("Capture failed".to_string())),
        });
    }

    let screenshot_base64 = capture_result.screenshot_base64.unwrap();

    // 2. Get auth token for upload
    let auth_manager = AuthManager::new();
    if !auth_manager.has_tokens() {
        return Ok(ScreenshotUploadResult {
            success: false,
            screenshot_id: None,
            presigned_url: None,
            error: Some("Not authenticated. Please log in first.".to_string()),
        });
    }

    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        String::from(AppError::AuthError(format!("{}", e)))
    })?;

    // 3. Convert base64 to binary for upload
    let image_bytes = base64::engine::general_purpose::STANDARD
        .decode(&screenshot_base64)
        .map_err(|e| {
            error!("Failed to decode screenshot: {}", e);
            String::from(AppError::EncodingError(format!(
                "Invalid screenshot data: {}",
                e
            )))
        })?;

    // 4. Create multipart form for upload
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string();
    let screenshot_name = config
        .name
        .clone()
        .unwrap_or_else(|| format!("screenshot_{}", timestamp));
    let filename = format!("{}.png", screenshot_name);

    let part = reqwest::multipart::Part::bytes(image_bytes)
        .file_name(filename.clone())
        .mime_str("image/png")
        .map_err(|e| {
            String::from(AppError::NetworkError(format!(
                "Failed to create upload part: {}",
                e
            )))
        })?;

    let form = reqwest::multipart::Form::new().part("file", part);

    // 5. Build upload URL with query parameters
    let client = reqwest::Client::new();
    let api_url = get_api_base_url();

    let mut upload_url = format!(
        "{}/api/v1/projects/{}/screenshots/upload?name={}&source=runner_capture",
        api_url,
        config.project_id,
        urlencoding::encode(&screenshot_name)
    );

    // Add monitor index if provided
    if let Some(monitor_idx) = config.monitor {
        upload_url = format!("{}&monitor_index={}", upload_url, monitor_idx);
    }

    let response = client
        .post(&upload_url)
        .bearer_auth(&access_token)
        .multipart(form)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to upload screenshot: {}", e);
            String::from(AppError::NetworkError(format!("Upload failed: {}", e)))
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!(
            "Screenshot upload failed with status {}: {}",
            status, error_text
        );
        return Ok(ScreenshotUploadResult {
            success: false,
            screenshot_id: None,
            presigned_url: None,
            error: Some(format!("Upload failed: {}", error_text)),
        });
    }

    let upload_response: serde_json::Value = response.json().await.map_err(|e| {
        error!("Failed to parse upload response: {}", e);
        String::from(AppError::ParseError(format!(
            "Invalid response from server: {}",
            e
        )))
    })?;

    let screenshot_id = upload_response
        .get("id")
        .and_then(|id| id.as_str())
        .map(|s| s.to_string());

    let presigned_url = upload_response
        .get("presigned_url")
        .and_then(|u| u.as_str())
        .map(|s| s.to_string());

    info!(
        "Screenshot uploaded successfully: {:?}",
        screenshot_id.as_ref().unwrap_or(&"unknown".to_string())
    );

    Ok(ScreenshotUploadResult {
        success: true,
        screenshot_id,
        presigned_url,
        error: None,
    })
}

/// Capture a screenshot using Python bridge.
///
/// Uses the Python executor's capture functionality.
/// This requires the Python executor to be running.
///
/// # Arguments
/// * `monitor` - Monitor index (0-based)
/// * `state` - Application state containing the Python bridge
#[tauri::command]
pub fn capture_screenshot_via_python(
    monitor: Option<i32>,
    bridge: State<BridgeCompartment>,
) -> Result<CommandResponse, String> {
    info!(
        "Capturing screenshot via Python bridge for monitor: {:?}",
        monitor
    );

    let bridge_clone = bridge.inner().clone();

    with_default_bridge_compartment(&bridge_clone, |bridge| {
        if !bridge.is_running() {
            return Err("Python executor not running".to_string());
        }

        let params = json!({
            "monitor": monitor.unwrap_or(0),
        });

        bridge
            .send_command("capture_screenshot", Some(params))
            .map_err(|e| e.to_string())?;

        Ok(CommandResponse {
            success: true,
            message: Some("Screenshot capture requested".to_string()),
            data: None,
        })
    })?
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_screenshot")
        .invoke_handler(tauri::generate_handler![
            get_screenshot_monitors,
            capture_screenshot,
            capture_and_upload_screenshot,
            capture_screenshot_via_python,
        ])
        .build()
}
