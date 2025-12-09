//! Extraction-related commands for web GUI extraction.
//!
//! This module provides commands for:
//! - Starting web extraction processes
//! - Monitoring extraction progress
//! - Exporting training data
//! - Managing screenshots
//! - Creating extraction sessions in qontinui-web

use super::{AppState, CommandResponse};
use crate::auth::AuthManager;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::State;
use tracing::{error, info, warn};

/// Configuration for web extraction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebExtractionConfig {
    pub urls: Vec<String>,
    /// Multiple viewports to test (optional)
    pub viewports: Option<Vec<(i32, i32)>>,
    /// Single viewport from frontend (preferred)
    pub viewport: Option<(i32, i32)>,
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
    state: State<Arc<AppState>>,
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
pub fn stop_web_extraction(state: State<Arc<AppState>>) -> Result<CommandResponse, String> {
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
pub fn get_extraction_status(state: State<Arc<AppState>>) -> Result<CommandResponse, String> {
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
    state: State<Arc<AppState>>,
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
    state: State<Arc<AppState>>,
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
    state: State<Arc<AppState>>,
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
pub fn list_extractions(state: State<Arc<AppState>>) -> Result<CommandResponse, String> {
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

// ============================================================================
// Web Backend Integration Commands
// ============================================================================

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

/// Request to create an extraction session in qontinui-web
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateExtractionSessionRequest {
    pub project_id: String,
    pub source_urls: Vec<String>,
    pub config: WebExtractionConfig,
}

/// Response from creating an extraction session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionSessionResponse {
    pub id: String,
    pub project_id: String,
    pub source_urls: Vec<String>,
    pub config: Value,
    pub status: String,
    pub stats: Value,
    pub error_message: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_by: Option<String>,
}

/// Create an extraction session in qontinui-web backend
///
/// This creates a record of the extraction session on the web backend,
/// which can then be used to track progress and store results.
#[tauri::command]
pub async fn create_extraction_session(
    request: CreateExtractionSessionRequest,
) -> Result<ExtractionSessionResponse, String> {
    info!(
        "Creating extraction session for project {} with {} URLs",
        request.project_id,
        request.source_urls.len()
    );

    let auth_manager = AuthManager::new();

    // Check authentication
    if !auth_manager.has_tokens() {
        return Err("Not authenticated. Please log in first.".to_string());
    }

    // Get access token
    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    // Prepare the request body for the API
    #[derive(Serialize)]
    struct ApiRequest {
        source_urls: Vec<String>,
        config: ApiConfig,
    }

    #[derive(Serialize)]
    struct ApiConfig {
        viewports: Vec<(i32, i32)>,
        capture_hover_states: bool,
        capture_focus_states: bool,
        max_depth: i32,
        max_pages: i32,
        auth_cookies: std::collections::HashMap<String, String>,
    }

    let api_request = ApiRequest {
        source_urls: request.source_urls,
        config: ApiConfig {
            viewports: request
                .config
                .viewports
                .unwrap_or_else(|| vec![(1920, 1080)]),
            capture_hover_states: request.config.capture_hover_states.unwrap_or(true),
            capture_focus_states: request.config.capture_focus_states.unwrap_or(true),
            max_depth: request.config.max_depth.unwrap_or(5),
            max_pages: request.config.max_pages.unwrap_or(100),
            auth_cookies: request.config.auth_cookies.unwrap_or_default(),
        },
    };

    // Call the backend API
    let client = reqwest::Client::new();
    let response = client
        .post(format!(
            "{}/api/v1/projects/{}/extractions",
            get_api_base_url(),
            request.project_id
        ))
        .bearer_auth(&access_token)
        .json(&api_request)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to create extraction session: {}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!(
            "Create extraction session failed with status {}: {}",
            status, error_text
        );
        return Err(format!(
            "Failed to create extraction session: {}",
            error_text
        ));
    }

    let session: ExtractionSessionResponse = response.json().await.map_err(|e| {
        error!("Failed to parse extraction session response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    info!("Extraction session created: {}", session.id);
    Ok(session)
}

/// Update extraction session status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateExtractionSessionRequest {
    pub extraction_id: String,
    pub status: Option<String>,
    pub stats: Option<Value>,
    pub error_message: Option<String>,
}

/// Update an extraction session's status in qontinui-web
#[tauri::command]
pub async fn update_extraction_session(
    request: UpdateExtractionSessionRequest,
) -> Result<ExtractionSessionResponse, String> {
    info!("Updating extraction session: {}", request.extraction_id);

    let auth_manager = AuthManager::new();

    if !auth_manager.has_tokens() {
        return Err("Not authenticated. Please log in first.".to_string());
    }

    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    #[derive(Serialize)]
    struct ApiUpdateRequest {
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        stats: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
    }

    let api_request = ApiUpdateRequest {
        status: request.status,
        stats: request.stats,
        error_message: request.error_message,
    };

    let client = reqwest::Client::new();
    let response = client
        .patch(format!(
            "{}/api/v1/extractions/{}",
            get_api_base_url(),
            request.extraction_id
        ))
        .bearer_auth(&access_token)
        .json(&api_request)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to update extraction session: {}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!(
            "Update extraction session failed with status {}: {}",
            status, error_text
        );
        return Err(format!(
            "Failed to update extraction session: {}",
            error_text
        ));
    }

    let session: ExtractionSessionResponse = response.json().await.map_err(|e| {
        error!("Failed to parse extraction session response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    info!("Extraction session updated: {}", session.id);
    Ok(session)
}

/// Upload extraction annotations to qontinui-web
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadAnnotationsRequest {
    pub extraction_id: String,
    pub screenshot_id: String,
    pub source_url: String,
    pub viewport_width: i32,
    pub viewport_height: i32,
    pub elements: Vec<Value>,
    pub states: Vec<Value>,
}

/// Annotation response from qontinui-web
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationResponse {
    pub id: String,
    pub session_id: String,
    pub screenshot_id: String,
    pub source_url: String,
    pub viewport_width: i32,
    pub viewport_height: i32,
    pub elements: Vec<Value>,
    pub states: Vec<Value>,
    pub created_at: String,
    pub updated_at: String,
}

/// Upload annotations for a screenshot to qontinui-web
#[tauri::command]
pub async fn upload_extraction_annotations(
    request: UploadAnnotationsRequest,
) -> Result<AnnotationResponse, String> {
    info!(
        "Uploading annotations for extraction {} screenshot {}",
        request.extraction_id, request.screenshot_id
    );

    let auth_manager = AuthManager::new();

    if !auth_manager.has_tokens() {
        return Err("Not authenticated. Please log in first.".to_string());
    }

    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    #[derive(Serialize)]
    struct ApiAnnotationRequest {
        screenshot_id: String,
        elements: Vec<Value>,
        states: Vec<Value>,
    }

    let api_request = ApiAnnotationRequest {
        screenshot_id: request.screenshot_id,
        elements: request.elements,
        states: request.states,
    };

    let client = reqwest::Client::new();
    let response = client
        .put(format!(
            "{}/api/v1/extractions/{}/annotations",
            get_api_base_url(),
            request.extraction_id
        ))
        .bearer_auth(&access_token)
        .json(&api_request)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to upload annotations: {}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!(
            "Upload annotations failed with status {}: {}",
            status, error_text
        );
        return Err(format!("Failed to upload annotations: {}", error_text));
    }

    let annotation: AnnotationResponse = response.json().await.map_err(|e| {
        error!("Failed to parse annotation response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    info!("Annotations uploaded successfully");
    Ok(annotation)
}

/// Upload state structure to qontinui-web
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadStateStructureRequest {
    pub extraction_id: String,
    pub state_structure: Value,
}

/// Response from uploading state structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateStructureResponse {
    pub id: String,
    pub session_id: String,
    pub state_structure: Value,
    pub created_at: String,
    pub updated_at: String,
}

/// Upload the generated state structure to qontinui-web backend
///
/// This uploads the state machine structure discovered during extraction
/// to the web backend for storage and further processing.
#[tauri::command]
pub async fn upload_state_structure(
    request: UploadStateStructureRequest,
) -> Result<StateStructureResponse, String> {
    info!(
        "Uploading state structure for extraction {}",
        request.extraction_id
    );

    let auth_manager = AuthManager::new();

    if !auth_manager.has_tokens() {
        return Err("Not authenticated. Please log in first.".to_string());
    }

    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    #[derive(Serialize)]
    struct ApiStateStructureRequest {
        state_structure: Value,
    }

    let api_request = ApiStateStructureRequest {
        state_structure: request.state_structure,
    };

    let client = reqwest::Client::new();
    let response = client
        .put(format!(
            "{}/api/v1/extractions/{}/state-structure",
            get_api_base_url(),
            request.extraction_id
        ))
        .bearer_auth(&access_token)
        .json(&api_request)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to upload state structure: {}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!(
            "Upload state structure failed with status {}: {}",
            status, error_text
        );
        return Err(format!("Failed to upload state structure: {}", error_text));
    }

    let state_response: StateStructureResponse = response.json().await.map_err(|e| {
        error!("Failed to parse state structure response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    info!("State structure uploaded successfully");
    Ok(state_response)
}

/// Get extraction sessions for a project from qontinui-web
#[tauri::command]
pub async fn get_project_extractions(
    project_id: String,
) -> Result<Vec<ExtractionSessionResponse>, String> {
    info!("Getting extractions for project: {}", project_id);

    let auth_manager = AuthManager::new();

    if !auth_manager.has_tokens() {
        return Err("Not authenticated. Please log in first.".to_string());
    }

    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    let client = reqwest::Client::new();
    let response = client
        .get(format!(
            "{}/api/v1/projects/{}/extractions",
            get_api_base_url(),
            project_id
        ))
        .bearer_auth(&access_token)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to get extractions: {}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!(
            "Get extractions failed with status {}: {}",
            status, error_text
        );
        return Err(format!("Failed to get extractions: {}", error_text));
    }

    let sessions: Vec<ExtractionSessionResponse> = response.json().await.map_err(|e| {
        error!("Failed to parse extractions response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    info!("Retrieved {} extractions", sessions.len());
    Ok(sessions)
}
