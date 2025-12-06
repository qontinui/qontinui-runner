//! Screenshot capture and upload commands.
//!
//! This module provides commands for:
//! - Capturing screenshots from connected monitors
//! - Uploading screenshots directly to qontinui-web projects
//! - Getting monitor information for screenshot capture

use super::{AppState, CommandResponse};
use crate::auth::AuthManager;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tauri::State;
use tracing::{error, info};

/// Get API base URL for qontinui-web backend
fn get_api_base_url() -> String {
    std::env::var("QONTINUI_API_URL").unwrap_or_else(|_| {
        if cfg!(debug_assertions) {
            "http://localhost:8000".to_string()
        } else {
            "https://qontinui-prod-py.eba-km2u4s23.eu-central-1.elasticbeanstalk.com".to_string()
        }
    })
}

/// Get qontinui-api URL for screenshot capture
fn get_qontinui_api_url() -> String {
    std::env::var("QONTINUI_API_URL_VISION").unwrap_or_else(|_| {
        if cfg!(debug_assertions) {
            "http://localhost:8001".to_string()
        } else {
            // Production qontinui-api URL (when deployed)
            "http://localhost:8001".to_string()
        }
    })
}

/// Monitor information from qontinui-api
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub index: i32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub scale: f64,
    pub is_primary: bool,
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
    pub image_id: Option<String>,
    pub url: Option<String>,
    pub error: Option<String>,
}

/// Get available monitors for screenshot capture via qontinui-api.
///
/// This calls the qontinui-api screenshot/monitors endpoint to get
/// information about all connected displays at physical resolution.
#[tauri::command]
pub async fn get_screenshot_monitors() -> Result<CommandResponse, String> {
    info!("Getting available monitors for screenshot capture");

    let client = reqwest::Client::new();
    let api_url = get_qontinui_api_url();

    let response = client
        .get(format!("{}/api/capture/screenshot/monitors", api_url))
        .send()
        .await
        .map_err(|e| {
            error!("Failed to get monitors: {}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!("Get monitors failed with status {}: {}", status, error_text);
        return Err(format!("Failed to get monitors: {}", error_text));
    }

    let monitors_response: serde_json::Value = response.json().await.map_err(|e| {
        error!("Failed to parse monitors response: {}", e);
        format!("Invalid response from API: {}", e)
    })?;

    info!(
        "Retrieved {} monitors",
        monitors_response
            .get("count")
            .and_then(|c| c.as_i64())
            .unwrap_or(0)
    );

    Ok(CommandResponse {
        success: true,
        message: Some("Monitors retrieved".to_string()),
        data: Some(monitors_response),
    })
}

/// Capture a screenshot from the specified monitor via qontinui-api.
///
/// This uses the qontinui library's HAL layer for capture, ensuring
/// screenshots are taken at physical pixel resolution.
///
/// # Arguments
/// * `monitor` - Monitor index (0-based), None for all monitors combined
#[tauri::command]
pub async fn capture_screenshot(monitor: Option<i32>) -> Result<ScreenshotCaptureResult, String> {
    info!("Capturing screenshot from monitor: {:?}", monitor);

    let client = reqwest::Client::new();
    let api_url = get_qontinui_api_url();

    let mut url = format!("{}/api/capture/screenshot/current", api_url);
    if let Some(mon) = monitor {
        url = format!("{}?monitor={}", url, mon);
    }

    let response = client.get(&url).send().await.map_err(|e| {
        error!("Failed to capture screenshot: {}", e);
        format!("Network error: {}", e)
    })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!(
            "Screenshot capture failed with status {}: {}",
            status, error_text
        );
        return Ok(ScreenshotCaptureResult {
            success: false,
            screenshot_base64: None,
            width: None,
            height: None,
            monitor,
            error: Some(format!("Capture failed: {}", error_text)),
        });
    }

    let capture_response: serde_json::Value = response.json().await.map_err(|e| {
        error!("Failed to parse screenshot response: {}", e);
        format!("Invalid response from API: {}", e)
    })?;

    let screenshot_base64 = capture_response
        .get("screenshot_base64")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());

    let width = capture_response
        .get("width")
        .and_then(|w| w.as_i64())
        .map(|w| w as i32);

    let height = capture_response
        .get("height")
        .and_then(|h| h.as_i64())
        .map(|h| h as i32);

    info!(
        "Screenshot captured: {}x{} pixels",
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
}

/// Capture a screenshot and upload it directly to a qontinui-web project.
///
/// This is the main function for the runner's "capture to web" feature.
/// It captures at physical resolution and uploads to the user's project.
///
/// # Arguments
/// * `config` - Upload configuration including project_id and optional name
#[tauri::command]
pub async fn capture_and_upload_screenshot(
    config: ScreenshotUploadConfig,
) -> Result<ScreenshotUploadResult, String> {
    info!(
        "Capturing and uploading screenshot to project: {}",
        config.project_id
    );

    // 1. Capture the screenshot via qontinui-api
    let capture_result = capture_screenshot(config.monitor).await?;

    if !capture_result.success || capture_result.screenshot_base64.is_none() {
        return Ok(ScreenshotUploadResult {
            success: false,
            image_id: None,
            url: None,
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
            image_id: None,
            url: None,
            error: Some("Not authenticated. Please log in first.".to_string()),
        });
    }

    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Authentication error: {}", e)
    })?;

    // 3. Convert base64 to binary for upload
    let image_bytes = base64::engine::general_purpose::STANDARD
        .decode(&screenshot_base64)
        .map_err(|e| {
            error!("Failed to decode screenshot: {}", e);
            format!("Invalid screenshot data: {}", e)
        })?;

    // 4. Create multipart form for upload
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string();
    let filename = config
        .name
        .unwrap_or_else(|| format!("screenshot_{}.png", timestamp));

    let part = reqwest::multipart::Part::bytes(image_bytes)
        .file_name(filename.clone())
        .mime_str("image/png")
        .map_err(|e| format!("Failed to create upload part: {}", e))?;

    let form = reqwest::multipart::Form::new().part("file", part);

    // 5. Upload to qontinui-web backend
    let client = reqwest::Client::new();
    let api_url = get_api_base_url();

    let response = client
        .post(format!(
            "{}/api/v1/projects/{}/images/upload",
            api_url, config.project_id
        ))
        .bearer_auth(&access_token)
        .multipart(form)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to upload screenshot: {}", e);
            format!("Upload failed: {}", e)
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
            image_id: None,
            url: None,
            error: Some(format!("Upload failed: {}", error_text)),
        });
    }

    let upload_response: serde_json::Value = response.json().await.map_err(|e| {
        error!("Failed to parse upload response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    let image_id = upload_response
        .get("image_id")
        .and_then(|id| id.as_str())
        .map(|s| s.to_string());

    let url = upload_response
        .get("url")
        .and_then(|u| u.as_str())
        .map(|s| s.to_string());

    info!(
        "Screenshot uploaded successfully: {:?}",
        image_id.as_ref().unwrap_or(&"unknown".to_string())
    );

    Ok(ScreenshotUploadResult {
        success: true,
        image_id,
        url,
        error: None,
    })
}

/// Capture a screenshot using Python bridge (alternative to API).
///
/// Uses the Python executor's capture functionality instead of the qontinui-api.
/// This requires the Python executor to be running.
///
/// # Arguments
/// * `monitor` - Monitor index (0-based)
/// * `state` - Application state containing the Python bridge
#[tauri::command]
pub fn capture_screenshot_via_python(
    monitor: Option<i32>,
    state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!(
        "Capturing screenshot via Python bridge for monitor: {:?}",
        monitor
    );

    let mut bridge_lock = state
        .python_bridge
        .lock()
        .map_err(|e| format!("Failed to acquire lock: {}", e))?;

    if let Some(ref mut bridge) = *bridge_lock {
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
    } else {
        Err("Python executor not initialized".to_string())
    }
}
