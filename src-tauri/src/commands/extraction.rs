//! Extraction-related commands for web GUI extraction.
//!
//! This module provides commands for:
//! - Starting web extraction processes
//! - Monitoring extraction progress
//! - Exporting training data
//! - Managing screenshots

use super::{AppState, CommandResponse};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::State;
use tracing::info;

/// Configuration for web extraction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebExtractionConfig {
    pub urls: Vec<String>,
    pub viewports: Option<Vec<(i32, i32)>>,
    pub capture_hover_states: Option<bool>,
    pub capture_focus_states: Option<bool>,
    pub capture_scroll_states: Option<bool>,
    pub max_depth: Option<i32>,
    pub max_pages: Option<i32>,
    pub auth_cookies: Option<std::collections::HashMap<String, String>>,
}

/// Start a web extraction process
#[tauri::command]
pub fn start_web_extraction(
    state: State<AppState>,
    config: WebExtractionConfig,
) -> Result<CommandResponse, String> {
    info!("Starting web extraction for URLs: {:?}", config.urls);

    let mut bridge_lock = state
        .python_bridge
        .lock()
        .map_err(|e| format!("Failed to acquire lock: {}", e))?;

    if let Some(ref mut bridge) = *bridge_lock {
        if !bridge.is_running() {
            return Err("Python executor not running".to_string());
        }

        let params = json!({
            "config": {
                "urls": config.urls,
                "viewports": config.viewports.unwrap_or_else(|| vec![(1920, 1080)]),
                "capture_hover_states": config.capture_hover_states.unwrap_or(true),
                "capture_focus_states": config.capture_focus_states.unwrap_or(true),
                "capture_scroll_states": config.capture_scroll_states.unwrap_or(true),
                "max_depth": config.max_depth.unwrap_or(5),
                "max_pages": config.max_pages.unwrap_or(100),
                "auth_cookies": config.auth_cookies.unwrap_or_default(),
            }
        });

        bridge
            .send_command("start_web_extraction", Some(params))
            .map_err(|e| e.to_string())?;

        Ok(CommandResponse {
            success: true,
            message: Some("Web extraction started".to_string()),
            data: None,
        })
    } else {
        Err("Python executor not initialized".to_string())
    }
}

/// Stop the current web extraction process
#[tauri::command]
pub fn stop_web_extraction(state: State<AppState>) -> Result<CommandResponse, String> {
    info!("Stopping web extraction");

    let mut bridge_lock = state
        .python_bridge
        .lock()
        .map_err(|e| format!("Failed to acquire lock: {}", e))?;

    if let Some(ref mut bridge) = *bridge_lock {
        if !bridge.is_running() {
            return Err("Python executor not running".to_string());
        }

        bridge
            .send_command("stop_web_extraction", None)
            .map_err(|e| e.to_string())?;

        Ok(CommandResponse {
            success: true,
            message: Some("Web extraction stopped".to_string()),
            data: None,
        })
    } else {
        Err("Python executor not initialized".to_string())
    }
}

/// Get the current extraction status
#[tauri::command]
pub fn get_extraction_status(state: State<AppState>) -> Result<CommandResponse, String> {
    info!("Getting extraction status");

    let mut bridge_lock = state
        .python_bridge
        .lock()
        .map_err(|e| format!("Failed to acquire lock: {}", e))?;

    if let Some(ref mut bridge) = *bridge_lock {
        if !bridge.is_running() {
            return Err("Python executor not running".to_string());
        }

        bridge
            .send_command("get_extraction_status", None)
            .map_err(|e| e.to_string())?;

        Ok(CommandResponse {
            success: true,
            message: Some("Status request sent".to_string()),
            data: None,
        })
    } else {
        Err("Python executor not initialized".to_string())
    }
}

/// Request a screenshot from the extraction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotRequest {
    pub screenshot_id: String,
    pub resolution: String, // "thumbnail" or "full"
}

#[tauri::command]
pub fn request_extraction_screenshot(
    state: State<AppState>,
    request: ScreenshotRequest,
) -> Result<CommandResponse, String> {
    info!(
        "Requesting screenshot {} at {} resolution",
        request.screenshot_id, request.resolution
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
            "screenshot_id": request.screenshot_id,
            "resolution": request.resolution,
        });

        bridge
            .send_command("get_extraction_screenshot", Some(params))
            .map_err(|e| e.to_string())?;

        Ok(CommandResponse {
            success: true,
            message: Some("Screenshot request sent".to_string()),
            data: None,
        })
    } else {
        Err("Python executor not initialized".to_string())
    }
}

/// Export training data configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingDataExportConfig {
    pub extraction_id: String,
    pub format: String, // "coco", "yolo", "jsonl"
    pub output_path: String,
    pub annotations: Option<Value>, // Updated annotations from web backend
    pub include_states: Option<bool>,
}

/// Export extraction results as training data
#[tauri::command]
pub fn export_training_data(
    state: State<AppState>,
    config: TrainingDataExportConfig,
) -> Result<CommandResponse, String> {
    info!(
        "Exporting training data for extraction {} as {} to {}",
        config.extraction_id, config.format, config.output_path
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
            "extraction_id": config.extraction_id,
            "format": config.format,
            "output_path": config.output_path,
            "annotations": config.annotations,
            "include_states": config.include_states.unwrap_or(true),
        });

        bridge
            .send_command("export_training_data", Some(params))
            .map_err(|e| e.to_string())?;

        Ok(CommandResponse {
            success: true,
            message: Some("Export started".to_string()),
            data: None,
        })
    } else {
        Err("Python executor not initialized".to_string())
    }
}

/// Export extraction results as state structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateStructureExportConfig {
    pub extraction_id: String,
    pub output_path: String,
    pub include_screenshots: Option<bool>,
}

#[tauri::command]
pub fn export_state_structure(
    state: State<AppState>,
    config: StateStructureExportConfig,
) -> Result<CommandResponse, String> {
    info!(
        "Exporting state structure for extraction {} to {}",
        config.extraction_id, config.output_path
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
            "extraction_id": config.extraction_id,
            "output_path": config.output_path,
            "include_screenshots": config.include_screenshots.unwrap_or(true),
        });

        bridge
            .send_command("export_state_structure", Some(params))
            .map_err(|e| e.to_string())?;

        Ok(CommandResponse {
            success: true,
            message: Some("Export started".to_string()),
            data: None,
        })
    } else {
        Err("Python executor not initialized".to_string())
    }
}

/// Get list of available extractions
#[tauri::command]
pub fn list_extractions(state: State<AppState>) -> Result<CommandResponse, String> {
    info!("Listing available extractions");

    let mut bridge_lock = state
        .python_bridge
        .lock()
        .map_err(|e| format!("Failed to acquire lock: {}", e))?;

    if let Some(ref mut bridge) = *bridge_lock {
        if !bridge.is_running() {
            return Err("Python executor not running".to_string());
        }

        bridge
            .send_command("list_extractions", None)
            .map_err(|e| e.to_string())?;

        Ok(CommandResponse {
            success: true,
            message: Some("List request sent".to_string()),
            data: None,
        })
    } else {
        Err("Python executor not initialized".to_string())
    }
}
