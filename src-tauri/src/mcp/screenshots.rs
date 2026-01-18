//! Screenshot capture handlers for MCP API
//!
//! Provides screenshot capture capabilities for AI automation.
//! Uses IPC to the Python bridge for screenshot capture.

use axum::{extract::State, http::StatusCode, Json};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

use super::sessions::emit_ai_output;
use super::types::{api_error, ApiResponse, ApiState, CaptureScreenshotRequest, CaptureScreenshotResponse};

/// Capture a screenshot via IPC to Python bridge
async fn capture_screenshot_ipc(
    app_state: Arc<crate::AppState>,
    monitor: Option<i32>,
) -> Result<serde_json::Value, String> {
    let params = serde_json::json!({
        "monitor": monitor,
        "format": "png",
    });

    let result = tokio::task::spawn_blocking(move || {
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            warn!("IPC: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = Duration::from_secs(30);
            bridge.send_command_and_wait("capture_screenshot", Some(params), timeout_duration)
        } else {
            Err("Python executor not initialized".to_string())
        }
    })
    .await
    .map_err(|e| format!("spawn_blocking error: {}", e))?;

    match result {
        Ok(response) => {
            if response.success {
                Ok(response.data.unwrap_or(serde_json::json!({"success": true})))
            } else {
                Err(response.error.unwrap_or_else(|| "Screenshot capture failed".to_string()))
            }
        }
        Err(e) => Err(e),
    }
}

/// Capture a screenshot and save it to .dev-logs/screenshots/ with task-identifiable naming.
/// Also logs the screenshot to ai-output.jsonl for AI analysis.
///
/// This endpoint is used by both:
/// 1. Dedicated screenshot actions in the AI Automation Builder
/// 2. Post-step screenshots (takeScreenshot toggle on other step types)
pub async fn capture_screenshot_step(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CaptureScreenshotRequest>,
) -> Result<Json<ApiResponse<CaptureScreenshotResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    use base64::Engine;
    use std::fs;
    use std::io::Write;

    info!(
        "MCP API: Capturing screenshot via IPC (monitor: {:?}, delay: {:?}s, task: {:?}, step: {:?})",
        request.monitor, request.delay_seconds, request.task_id, request.step_index
    );

    // Apply delay if specified (clamped to 0-30 seconds)
    if let Some(delay) = request.delay_seconds {
        if delay > 0.0 {
            let clamped_delay = delay.clamp(0.0, 30.0);
            info!(
                "MCP API: Waiting {}s before screenshot capture",
                clamped_delay
            );
            tokio::time::sleep(tokio::time::Duration::from_secs_f64(clamped_delay)).await;
        }
    }

    // Capture screenshot via Python IPC
    let capture_response = match capture_screenshot_ipc(state.app_state.clone(), request.monitor).await {
        Ok(response) => response,
        Err(e) => {
            error!("MCP API: Failed to capture screenshot via IPC: {}", e);
            return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
                success: false,
                screenshot_path: None,
                absolute_path: None,
                width: None,
                height: None,
                monitor: request.monitor,
                error: Some(format!("IPC error: {}", e)),
            })));
        }
    };

    let screenshot_base64 = match capture_response
        .get("screenshot_base64")
        .and_then(|s| s.as_str())
    {
        Some(s) => s.to_string(),
        None => {
            error!("MCP API: No screenshot_base64 in IPC response");
            return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
                success: false,
                screenshot_path: None,
                absolute_path: None,
                width: None,
                height: None,
                monitor: request.monitor,
                error: Some("No screenshot data in response".to_string()),
            })));
        }
    };

    let width = capture_response
        .get("width")
        .and_then(|w| w.as_i64())
        .map(|w| w as i32);
    let height = capture_response
        .get("height")
        .and_then(|h| h.as_i64())
        .map(|h| h as i32);

    // Decode base64 to bytes
    let image_bytes = match base64::engine::general_purpose::STANDARD.decode(&screenshot_base64) {
        Ok(bytes) => bytes,
        Err(e) => {
            error!("MCP API: Failed to decode screenshot base64: {}", e);
            return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
                success: false,
                screenshot_path: None,
                absolute_path: None,
                width: None,
                height: None,
                monitor: request.monitor,
                error: Some(format!("Failed to decode screenshot: {}", e)),
            })));
        }
    };

    // Generate filename with task/run identification
    let timestamp = chrono::Utc::now().timestamp_millis();
    let task_id = request.task_id.as_deref().unwrap_or("manual");
    let step_part = request
        .step_index
        .map(|i| format!("step{:02}", i))
        .unwrap_or_else(|| "step00".to_string());
    let monitor_part = request
        .monitor
        .map(|m| format!("m{}", m))
        .unwrap_or_else(|| "all".to_string());
    let filename = format!(
        "screenshot-{}-{}-{}-{}.png",
        task_id, step_part, timestamp, monitor_part
    );

    // Save to .dev-logs/screenshots/
    let screenshots_dir = crate::paths::get_screenshots_dir();
    if let Err(e) = fs::create_dir_all(&screenshots_dir) {
        error!("MCP API: Failed to create screenshots directory: {}", e);
        return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
            success: false,
            screenshot_path: None,
            absolute_path: None,
            width: None,
            height: None,
            monitor: request.monitor,
            error: Some(format!("Failed to create directory: {}", e)),
        })));
    }

    let screenshot_path = screenshots_dir.join(&filename);
    let mut file = match fs::File::create(&screenshot_path) {
        Ok(f) => f,
        Err(e) => {
            error!("MCP API: Failed to create screenshot file: {}", e);
            return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
                success: false,
                screenshot_path: None,
                absolute_path: None,
                width: None,
                height: None,
                monitor: request.monitor,
                error: Some(format!("Failed to create file: {}", e)),
            })));
        }
    };

    if let Err(e) = file.write_all(&image_bytes) {
        error!("MCP API: Failed to write screenshot file: {}", e);
        return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
            success: false,
            screenshot_path: None,
            absolute_path: None,
            width: None,
            height: None,
            monitor: request.monitor,
            error: Some(format!("Failed to write file: {}", e)),
        })));
    }

    let relative_path = format!("screenshots/{}", filename);
    let absolute_path_str = screenshot_path.to_string_lossy().to_string();

    info!(
        "MCP API: Screenshot saved: {} ({}x{})",
        relative_path,
        width.unwrap_or(0),
        height.unwrap_or(0)
    );

    // Log to ai-output.jsonl
    let ai_output_entry = crate::commands::logging::AiOutputEntry {
        id: format!("ss-{}-{}", timestamp, rand::random::<u32>()),
        timestamp,
        line: format!(
            "[SCREENSHOT] {} ({}x{})",
            filename,
            width.unwrap_or(0),
            height.unwrap_or(0)
        ),
        source: "runner".to_string(),
        action_id: Some(format!("screenshot-{}", step_part)),
        session_id: None,
        session_name: None,
        screenshot_path: Some(relative_path.clone()),
        screenshot_width: width,
        screenshot_height: height,
    };

    // Append to ai-output.jsonl
    let _ = crate::commands::logging::append_ai_output_log(ai_output_entry);

    // Also emit to frontend for real-time display
    emit_ai_output(
        &state.app_handle,
        &format!(
            "[SCREENSHOT] Captured: {} ({}x{})",
            filename,
            width.unwrap_or(0),
            height.unwrap_or(0)
        ),
        "runner",
        Some(&format!("screenshot-{}", step_part)),
        None,
    );

    Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
        success: true,
        screenshot_path: Some(relative_path),
        absolute_path: Some(absolute_path_str),
        width,
        height,
        monitor: request.monitor,
        error: None,
    })))
}
